//! The client-side representation cache (SPEC §8.3, DESIGN "The client-side cache").

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::Instant;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use uuid::Uuid;

use crate::contract::PlatformTypesRegistryApi;
use crate::models::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, BatchGetItem, DeleteEntitiesRequest,
    DeletionOperation, DeletionOutcome, Entity, EntityField, EntityKey, EntityKind, EntityLookup,
    FieldSelection, IdempotencyKey, ListEntitiesRequest, ListEntitiesResponse, MAX_BATCH_GET_KEYS,
    Operation, RegisterEntitiesRequest, RegistrationOperation, RegistrationOutcome, Validator,
};

mod store;

use store::{RepKey, Store, Ticket, canonical_uuid};

/// How long a confirmed entry is served without asking the registry again, and how
/// many bytes of snapshots the cache holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheSettings {
    /// `0` is meaningful: every read revalidates.
    pub freshness_window: Duration,
    /// Bytes of cached snapshots, evicted least-recently-used.
    pub store_bound: usize,
}

impl CacheSettings {
    /// DESIGN's freshness window.
    pub const DEFAULT_FRESHNESS_WINDOW: Duration = Duration::from_secs(30);
    /// DESIGN's store bound: 64 MB.
    pub const DEFAULT_STORE_BOUND: usize = 64 * 1024 * 1024;
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            freshness_window: Self::DEFAULT_FRESHNESS_WINDOW,
            store_bound: Self::DEFAULT_STORE_BOUND,
        }
    }
}

/// A [`PlatformTypesRegistryApi`] that serves exact batch reads from a bounded,
/// per-client representation cache and forwards everything else.
pub struct CachedPlatformTypesRegistry {
    inner: Arc<dyn PlatformTypesRegistryApi>,
    freshness_window: Duration,
    /// Every critical section is a lookup or a few inserts and removals that leave the
    /// store consistent at each step, so a poisoned lock is recovered.
    store: Mutex<Store>,
}

/// The registry's bound on a key's identifier and on a validator (TR `MAX_KEY_LEN`, the
/// REST client's entity-tag bound). A request over it is the registry's to refuse.
const TOKEN_BOUND: usize = 1024;

/// Whether a validator stays within the bound on every transport. REST measures the
/// quoted entity-tag, so a validator two bytes short of the bound is already over it.
fn validator_fits(validator: &Validator) -> bool {
    validator.as_bytes().len() + 2 <= TOKEN_BOUND
}

/// How one distinct key of a batch is answered.
enum Plan {
    /// From the cache, inside the window.
    Local {
        snapshot: Arc<Entity>,
        etag: Validator,
        caller: Option<Validator>,
    },
    /// From the registry, conditionally on `sent`: the cache's validator for `pinned`,
    /// or else the caller's own.
    Remote {
        rep: RepKey,
        caller: Option<Validator>,
        pinned: Option<(Arc<Entity>, Validator)>,
        sent: Option<Validator>,
        /// This read's ticket, taken with its lookup.
        ticket: Ticket,
    },
}

/// The tickets one batch read holds, released when it completes, fails or is dropped.
struct Tickets<'a> {
    store: &'a Mutex<Store>,
    uuids: Vec<Uuid>,
}

impl Drop for Tickets<'_> {
    fn drop(&mut self) {
        let mut store = self.store.lock().unwrap_or_else(PoisonError::into_inner);
        for uuid in &self.uuids {
            store.release(*uuid);
        }
    }
}

/// A registry answer checked against what was sent, ready to commit without further work.
enum Verified {
    Found {
        snapshot: Arc<Entity>,
        etag: Validator,
        weight: usize,
    },
    /// `Unchanged` to the cache's own validator: the pinned snapshot is current.
    Confirmed,
    /// `Unchanged` to the caller's validator on a cold key: nothing to cache.
    Matched(Validator),
    NotFound,
}

impl CachedPlatformTypesRegistry {
    #[must_use]
    pub fn new(inner: Arc<dyn PlatformTypesRegistryApi>, settings: CacheSettings) -> Self {
        Self {
            inner,
            freshness_window: settings.freshness_window,
            store: Mutex::new(Store::new(settings.store_bound)),
        }
    }

    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Decides, per distinct key, whether the cache answers it or the registry does, and
    /// with which condition. A repeated key keeps its first mention, as the registry does.
    /// Each key sent to the registry takes a ticket in the same critical section as its
    /// lookup, so a mutation observed after the lookup fences its answer.
    fn plan(
        &self,
        request: &BatchGetEntitiesRequest,
        now: Instant,
    ) -> (Vec<(EntityKey, Plan)>, Tickets<'_>) {
        let mut store = self.store();
        let mut tickets = Tickets {
            store: &self.store,
            uuids: Vec::new(),
        };
        let mut seen = HashSet::new();
        let mut plans = Vec::with_capacity(request.items.len());
        for item in &request.items {
            if !seen.insert(&item.key) {
                continue;
            }
            let rep = RepKey::new(canonical_uuid(&item.key), &request.projection);
            let caller = item.if_none_match.clone();
            let plan = match store.get(&rep) {
                Some(entry)
                    if !request.fresh
                        && now.duration_since(entry.confirmed_at) < self.freshness_window =>
                {
                    Plan::Local {
                        snapshot: Arc::clone(&entry.snapshot),
                        etag: entry.etag.clone(),
                        caller,
                    }
                }
                entry => {
                    let pinned = entry.map(|e| (Arc::clone(&e.snapshot), e.etag.clone()));
                    tickets.uuids.push(rep.uuid);
                    Plan::Remote {
                        ticket: store.ticket(rep.uuid),
                        sent: pinned
                            .as_ref()
                            .map(|(_, ours)| ours.clone())
                            .or_else(|| caller.clone()),
                        rep,
                        caller,
                        pinned,
                    }
                }
            };
            plans.push((item.key.clone(), plan));
        }
        (plans, tickets)
    }

    /// Records verified answers. Only map operations run under the lock. An answer to a
    /// read during which anything else was recorded for its entity changes nothing (the
    /// late-fill guard); the caller still receives it.
    fn commit(&self, plans: &[(EntityKey, Plan)], verified: &[Option<Verified>], sent_at: Instant) {
        let mut store = self.store();
        // Judged before any of this batch is applied: its keys were ticketed together, so
        // what one of them records is not news to another.
        let quiet: Vec<bool> = plans
            .iter()
            .map(|(_, plan)| match plan {
                Plan::Remote { rep, ticket, .. } => store.quiet_since(rep.uuid, *ticket),
                Plan::Local { .. } => false,
            })
            .collect();
        for (((_, plan), verified), quiet) in plans.iter().zip(verified).zip(quiet) {
            let (
                Plan::Remote {
                    rep, sent, pinned, ..
                },
                Some(verified),
            ) = (plan, verified)
            else {
                continue;
            };
            if !quiet {
                // Something was recorded for this entity while the read was in flight — an
                // observed mutation, or another read's answer — and nothing orders revisions,
                // so this answer fills nothing. Unless it agrees with what is resident, one
                // of the two is stale: drop the entity's entries and let the next read refetch.
                let agrees = match verified {
                    Verified::Found { etag, .. } | Verified::Matched(etag) => {
                        store.peek(rep).is_some_and(|entry| entry.etag == *etag)
                    }
                    // The registry confirmed the snapshot this read held: still agreement
                    // only if that is what is resident now.
                    Verified::Confirmed => pinned.as_ref().is_some_and(|(_, ours)| {
                        store.peek(rep).is_some_and(|entry| entry.etag == *ours)
                    }),
                    Verified::NotFound => false,
                };
                if !agrees {
                    store.invalidate(rep.uuid);
                }
                continue;
            }
            match verified {
                Verified::Found {
                    snapshot,
                    etag,
                    weight,
                } => {
                    if sent.as_ref().is_some_and(|sent| sent != etag) {
                        // A validator this read held is no longer current: a new revision,
                        // so every projection of it is stale, and reads of it started
                        // earlier must not fill.
                        store.invalidate(rep.uuid);
                    }
                    store.insert(
                        rep.clone(),
                        Arc::clone(snapshot),
                        *weight,
                        etag.clone(),
                        sent_at,
                    );
                }
                Verified::Confirmed => {
                    if let Some((_, ours)) = pinned {
                        store.confirm(rep, ours, sent_at);
                    }
                }
                Verified::Matched(_) => {}
                Verified::NotFound => store.invalidate(rep.uuid),
            }
            store.record(rep.uuid);
        }
    }

    /// Invalidates every entity a terminal successful registration committed.
    fn observe_registration(&self, operation: &RegistrationOperation) {
        let committed: Vec<Uuid> = operation
            .items
            .iter()
            .filter(|item| matches!(item.outcome, RegistrationOutcome::Succeeded { .. }))
            .map(|item| item.gts_id.to_uuid())
            .collect();
        self.invalidate(&committed);
    }

    /// Invalidates every entity a terminal successful deletion committed.
    fn observe_deletion(&self, operation: &DeletionOperation) {
        let committed: Vec<Uuid> = operation
            .items
            .iter()
            .filter(|item| matches!(item.outcome, DeletionOutcome::Succeeded { .. }))
            .map(|item| canonical_uuid(&item.entity_key))
            .collect();
        self.invalidate(&committed);
    }

    fn invalidate(&self, uuids: &[Uuid]) {
        if uuids.is_empty() {
            return;
        }
        let mut store = self.store();
        for uuid in uuids {
            store.invalidate(*uuid);
        }
    }
}

/// Checks every answer against the request that was sent before any of it is cached:
/// one answer per sent key and no other, the asked entity in a consistent shape, and
/// `Unchanged` only to the exact validator sent.
fn verify(
    plans: &[(EntityKey, Plan)],
    mut answers: HashMap<EntityKey, EntityLookup>,
) -> Result<Vec<Option<Verified>>, CanonicalError> {
    let mut verified = Vec::with_capacity(plans.len());
    for (key, plan) in plans {
        let Plan::Remote {
            rep, pinned, sent, ..
        } = plan
        else {
            verified.push(None);
            continue;
        };
        let answer = match answers.remove(key) {
            None => return Err(protocol_fault(key, "no answer")),
            Some(EntityLookup::Found { entity, etag }) => {
                if !names(&entity, key) || !crate::ext::consistent(&entity) {
                    return Err(protocol_fault(key, "another or an inconsistent entity"));
                }
                if !shaped(&entity, &rep.selection) {
                    return Err(protocol_fault(key, "fields other than those selected"));
                }
                if !validator_fits(&etag) {
                    return Err(protocol_fault(key, "a validator over the bound"));
                }
                let weight = store::weight(&entity);
                Verified::Found {
                    snapshot: Arc::new(*entity),
                    etag,
                    weight,
                }
            }
            Some(EntityLookup::Unchanged { etag }) if sent.as_ref() == Some(&etag) => {
                if pinned.is_some() {
                    Verified::Confirmed
                } else {
                    Verified::Matched(etag)
                }
            }
            Some(EntityLookup::Unchanged { .. }) => {
                return Err(protocol_fault(key, "unchanged to a validator not sent"));
            }
            Some(EntityLookup::NotFound) => Verified::NotFound,
        };
        verified.push(Some(answer));
    }
    if let Some(key) = answers.keys().next() {
        return Err(protocol_fault(key, "an answer for a key not asked"));
    }
    Ok(verified)
}

fn protocol_fault(key: &EntityKey, what: &str) -> CanonicalError {
    CanonicalError::internal(format!(
        "the registry answered the batch read of {key} with {what}"
    ))
    .create()
}

/// The caller's response, built outside the lock: `Unchanged` only where the caller
/// itself sent the matching validator.
fn respond(
    plans: Vec<(EntityKey, Plan)>,
    verified: Vec<Option<Verified>>,
) -> BatchGetEntitiesResponse {
    let mut out = HashMap::with_capacity(plans.len());
    for ((key, plan), verified) in plans.into_iter().zip(verified) {
        let lookup = match (plan, verified) {
            (
                Plan::Local {
                    snapshot,
                    etag,
                    caller,
                },
                _,
            )
            | (Plan::Remote { caller, .. }, Some(Verified::Found { snapshot, etag, .. }))
            | (
                Plan::Remote {
                    caller,
                    pinned: Some((snapshot, etag)),
                    ..
                },
                Some(Verified::Confirmed),
            ) => answer(&snapshot, etag, caller.as_ref()),
            (Plan::Remote { .. }, Some(Verified::Matched(etag))) => {
                EntityLookup::Unchanged { etag }
            }
            // `verify` pairs every remote plan with an answer, and `Confirmed` with a pin.
            (Plan::Remote { .. }, _) => EntityLookup::NotFound,
        };
        out.insert(key, lookup);
    }
    BatchGetEntitiesResponse(out)
}

fn answer(snapshot: &Entity, etag: Validator, caller: Option<&Validator>) -> EntityLookup {
    if caller == Some(&etag) {
        EntityLookup::Unchanged { etag }
    } else {
        EntityLookup::Found {
            entity: Box::new(snapshot.clone()),
            etag,
        }
    }
}

/// Whether `entity` is the one `key` names, with a consistent identity.
fn names(entity: &Entity, key: &EntityKey) -> bool {
    let consistent = entity.gts_id.to_uuid() == entity.gts_uuid;
    consistent
        && match key {
            EntityKey::GtsId(id) => *id == entity.gts_id,
            EntityKey::GtsUuid(uuid) => *uuid == entity.gts_uuid,
        }
}

/// Whether the snapshot carries exactly the fields `selection` asks for that apply to its
/// kind, as the registry answers them: content and origin on either kind, the derived
/// documents and provenance on a Type Schema only. A selected JSON `null` is present.
fn shaped(entity: &Entity, selection: &FieldSelection) -> bool {
    let schema = entity.kind == EntityKind::TypeSchema;
    let expected = |field: EntityField, applicable: bool, present: bool| {
        present == (applicable && selection.contains(field))
    };
    expected(EntityField::Origin, true, entity.origin.is_some())
        && expected(EntityField::Content, true, entity.content.is_some())
        && expected(
            EntityField::ResolvedSchema,
            schema,
            entity.resolved_schema.is_some(),
        )
        && expected(
            EntityField::EffectiveTraits,
            schema,
            entity.effective_traits.is_some(),
        )
        && expected(
            EntityField::EffectiveTraitsSchema,
            schema,
            entity.effective_traits_schema.is_some(),
        )
        && expected(EntityField::Provenance, schema, entity.provenance.is_some())
}

/// A key or validator over the registry's bounds, whatever the cache holds: forwarded
/// intact, so the registry refuses it with its own error.
fn over_token_bound(request: &BatchGetEntitiesRequest) -> bool {
    request.items.iter().any(|item| {
        matches!(&item.key, EntityKey::GtsId(id) if id.as_ref().len() > TOKEN_BOUND)
            || item
                .if_none_match
                .as_ref()
                .is_some_and(|v| !validator_fits(v))
    })
}

#[async_trait]
impl PlatformTypesRegistryApi for CachedPlatformTypesRegistry {
    async fn batch_get_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        if request.items.is_empty() || request.items.len() > MAX_BATCH_GET_KEYS {
            // Refused for its size whatever it names.
            return self.inner.batch_get_entities(ctx, request).await;
        }
        if over_token_bound(&request) {
            // The registry decides; its answers are not checked against a plan, so a
            // successful one records nothing and drops what it may contradict.
            let named: Vec<Uuid> = request
                .items
                .iter()
                .map(|item| canonical_uuid(&item.key))
                .collect();
            let response = self.inner.batch_get_entities(ctx, request).await?;
            self.invalidate(&named);
            return Ok(response);
        }
        let now = Instant::now();
        let (plans, tickets) = self.plan(&request, now);
        let remote: Vec<BatchGetItem> = plans
            .iter()
            .filter_map(|(key, plan)| match plan {
                Plan::Local { .. } => None,
                Plan::Remote { sent, .. } => Some(BatchGetItem {
                    key: key.clone(),
                    if_none_match: sent.clone(),
                }),
            })
            .collect();
        let answers = if remote.is_empty() {
            HashMap::new()
        } else {
            let inner = BatchGetEntitiesRequest {
                items: remote,
                projection: request.projection,
                fresh: request.fresh,
            };
            self.inner.batch_get_entities(ctx, inner).await?.0
        };
        let verified = verify(&plans, answers)?;
        self.commit(&plans, &verified, now);
        drop(tickets);
        Ok(respond(plans, verified))
    }

    async fn list_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        self.inner.list_entities(ctx, request).await
    }

    async fn register_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: RegisterEntitiesRequest,
    ) -> Result<RegistrationOperation, CanonicalError> {
        let operation = self.inner.register_entities(ctx, key, request).await?;
        self.observe_registration(&operation);
        Ok(operation)
    }

    async fn delete_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: DeleteEntitiesRequest,
    ) -> Result<DeletionOperation, CanonicalError> {
        let operation = self.inner.delete_entities(ctx, key, request).await?;
        self.observe_deletion(&operation);
        Ok(operation)
    }

    async fn get_operation(
        &self,
        ctx: &PlatformSecurityContext,
        operation_id: Uuid,
    ) -> Result<Operation, CanonicalError> {
        let operation = self.inner.get_operation(ctx, operation_id).await?;
        match &operation {
            Operation::Registration(registration) => self.observe_registration(registration),
            Operation::Deletion(deletion) => self.observe_deletion(deletion),
        }
        Ok(operation)
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;
