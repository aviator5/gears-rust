//! The cache's bounded store: one entry per representation of an entity, evicted
//! least-recently-used by the bytes it holds.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use lru::LruCache;
use tokio::time::Instant;
use uuid::Uuid;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::models::{
    Entity, EntityKey, EntityKind, FieldSelection, JsonDocument, LifecycleStatus, Origin,
    Projection, Validator,
};

/// The entity a key names, whichever form it takes. A GTS identifier's UUID is derived
/// from it, so both forms address the same entries without a lookup table.
pub fn canonical_uuid(key: &EntityKey) -> Uuid {
    match key {
        EntityKey::GtsId(id) => id.to_uuid(),
        EntityKey::GtsUuid(uuid) => *uuid,
    }
}

/// Who may see a representation. P0 has one value.
// ponytail: C7 — visibility context and Context Tenant are fixed for the platform plane
// in P0. P1 tenancy replaces this marker with the subject tenant and Context Tenant
// without reshaping the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Unrestricted visibility, no Context Tenant.
    Platform,
}

/// One cached representation: an entity under one normalized field selection and scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepKey {
    pub uuid: Uuid,
    pub selection: FieldSelection,
    pub scope: Scope,
}

impl RepKey {
    /// `projection`'s effective field set: `Default` and its explicit equivalent share
    /// one entry, and a narrower set never answers a wider one.
    pub fn new(uuid: Uuid, projection: &Projection) -> Self {
        Self {
            uuid,
            selection: projection.normalized(),
            scope: Scope::Platform,
        }
    }
}

/// A resident representation.
#[derive(Debug, Clone)]
pub struct Entry {
    pub snapshot: Arc<Entity>,
    pub etag: Validator,
    /// When the registry last confirmed `etag` as current.
    pub confirmed_at: Instant,
    weight: usize,
}

/// The bytes a snapshot is charged: its compact JSON as the REST plane writes an entity
/// (TR's `EntityDto`) — identity, kind and lifecycle always, each selected field otherwise,
/// a selected JSON `null` as `null`. Key order does not change the length. This is the
/// payload's size, not retained heap.
pub fn weight(snapshot: &Entity) -> usize {
    let mut fields = vec![
        field("gts_id", str_len(snapshot.gts_id.as_ref())),
        field("gts_uuid", UUID_LEN),
        field(
            "kind",
            quoted(match snapshot.kind {
                EntityKind::TypeSchema => "type_schema",
                EntityKind::Instance => "instance",
            }),
        ),
        field(
            "lifecycle_status",
            quoted(match snapshot.lifecycle_status {
                LifecycleStatus::Active => "active",
                LifecycleStatus::Deleted => "deleted",
            }),
        ),
    ];
    if let Some(Origin::Managed {
        resource_version,
        created_at,
        updated_at,
    }) = &snapshot.origin
    {
        fields.push(field(
            "origin",
            object(&[
                field("type", quoted("managed")),
                field("resource_version", resource_version.to_string().len()),
                field("created_at", timestamp_len(*created_at)),
                field("updated_at", timestamp_len(*updated_at)),
            ]),
        ));
    }
    for (name, document) in [
        ("content", &snapshot.content),
        ("resolved_schema", &snapshot.resolved_schema),
        ("effective_traits", &snapshot.effective_traits),
        ("effective_traits_schema", &snapshot.effective_traits_schema),
    ] {
        if let Some(document) = document {
            fields.push(field(name, json_len(document)));
        }
    }
    if let Some(provenance) = &snapshot.provenance {
        let skipped = if provenance.compatibility_check_skipped {
            "true"
        } else {
            "false"
        };
        fields.push(field(
            "provenance",
            object(&[field("compatibility_check_skipped", skipped.len())]),
        ));
    }
    object(&fields)
}

/// A hyphenated UUID, quoted.
const UUID_LEN: usize = 38;

/// `"name":value`.
fn field(name: &str, value: usize) -> usize {
    quoted(name).saturating_add(1).saturating_add(value)
}

/// `{a,b,...}`.
fn object(fields: &[usize]) -> usize {
    fields
        .iter()
        .fold(2 + fields.len().saturating_sub(1), |sum, f| {
            sum.saturating_add(*f)
        })
}

/// A string needing no escapes, quoted.
fn quoted(s: &str) -> usize {
    s.len() + 2
}

/// RFC 3339, quoted. A timestamp RFC 3339 cannot write cannot be serialized either; it
/// is charged as oversized so that its entity is never retained.
fn timestamp_len(at: OffsetDateTime) -> usize {
    at.format(&Rfc3339).map_or(usize::MAX, |s| quoted(&s))
}

fn str_len(s: &str) -> usize {
    counted(|w| serde_json::to_writer(w, s))
}

fn json_len(document: &JsonDocument) -> usize {
    counted(|w| serde_json::to_writer(w, document))
}

/// Bytes `write` produces. Writing to a counter cannot fail, and neither can a `Value`
/// or `str`; were it to, the value is charged as oversized so that it is never retained.
fn counted(write: impl FnOnce(&mut ByteCounter) -> serde_json::Result<()>) -> usize {
    let mut counter = ByteCounter(0);
    match write(&mut counter) {
        Ok(()) => counter.0,
        Err(_) => usize::MAX,
    }
}

struct ByteCounter(usize);

impl io::Write for ByteCounter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(buf.len());
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Representations bounded by the bytes they hold, with an index from each entity to its
/// resident representations so that an invalidation drops all of them.
pub struct Store {
    entries: LruCache<RepKey, Entry>,
    by_entity: HashMap<Uuid, Vec<RepKey>>,
    used: usize,
    bound: usize,
}

impl Store {
    pub fn new(bound: usize) -> Self {
        Self {
            entries: LruCache::unbounded(),
            by_entity: HashMap::new(),
            used: 0,
            bound,
        }
    }

    /// The entry under `key`, marked most recently used.
    pub fn get(&mut self, key: &RepKey) -> Option<&Entry> {
        self.entries.get(key)
    }

    /// Stores `snapshot` under `key`, replacing what was there, and evicts the least
    /// recently used entries until it fits. A snapshot larger than the whole bound is not
    /// retained, and the entry it would replace is dropped rather than left stale.
    pub fn insert(
        &mut self,
        key: RepKey,
        snapshot: Arc<Entity>,
        etag: Validator,
        confirmed_at: Instant,
    ) {
        self.remove(&key);
        let weight = weight(&snapshot);
        if weight > self.bound {
            return;
        }
        while self.used + weight > self.bound {
            let Some((evicted, entry)) = self.entries.pop_lru() else {
                break;
            };
            self.forget(&evicted, entry.weight);
        }
        self.used += weight;
        self.by_entity
            .entry(key.uuid)
            .or_default()
            .push(key.clone());
        self.entries.put(
            key,
            Entry {
                snapshot,
                etag,
                confirmed_at,
                weight,
            },
        );
    }

    pub fn remove(&mut self, key: &RepKey) {
        if let Some(entry) = self.entries.pop(key) {
            self.forget(key, entry.weight);
        }
    }

    /// Drops every representation of the entity.
    pub fn remove_entity(&mut self, uuid: Uuid) {
        for key in self.by_entity.remove(&uuid).unwrap_or_default() {
            if let Some(entry) = self.entries.pop(&key) {
                self.used -= entry.weight;
            }
        }
    }

    fn forget(&mut self, key: &RepKey, weight: usize) {
        self.used -= weight;
        if let Some(keys) = self.by_entity.get_mut(&key.uuid) {
            keys.retain(|k| k != key);
            if keys.is_empty() {
                self.by_entity.remove(&key.uuid);
            }
        }
    }

    #[cfg(test)]
    pub fn used(&self) -> usize {
        self.used
    }

    #[cfg(test)]
    pub fn indexes(&self, uuid: Uuid) -> bool {
        self.by_entity.contains_key(&uuid)
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod store_tests;
