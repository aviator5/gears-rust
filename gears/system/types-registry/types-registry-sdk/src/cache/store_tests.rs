use std::sync::Arc;

use gts::GtsId;
use serde_json::json;
use tokio::time::Instant;
use uuid::Uuid;

use super::{RepKey, Store, canonical_uuid, weight};
use crate::models::{
    Entity, EntityField, EntityKey, EntityKind, FieldSelection, JsonDocument, LifecycleStatus,
    Origin, Projection, Provenance, Validator,
};

const TYPE: &str = "gts.cf.cache.store.entity.v1~";

fn id(s: &str) -> GtsId {
    GtsId::try_new(s).expect("valid GTS identifier")
}

fn entity(gts_id: &str, content: Option<JsonDocument>) -> Arc<Entity> {
    let gts_id = id(gts_id);
    let gts_uuid = gts_id.to_uuid();
    let kind = EntityKind::of(&gts_id);
    Arc::new(Entity {
        gts_id,
        gts_uuid,
        kind,
        lifecycle_status: LifecycleStatus::Active,
        origin: None,
        content,
        resolved_schema: None,
        effective_traits: None,
        effective_traits_schema: None,
        provenance: None,
    })
}

fn instance_id(n: usize) -> String {
    format!("{TYPE}cf.cache.store.item{n}.v1")
}

fn key(snapshot: &Entity, projection: &Projection) -> RepKey {
    RepKey::new(snapshot.gts_uuid, projection)
}

fn etag(s: &str) -> Validator {
    Validator::from_bytes(s.as_bytes())
}

fn put(store: &mut Store, snapshot: &Arc<Entity>, projection: &Projection) {
    store.insert(
        key(snapshot, projection),
        Arc::clone(snapshot),
        etag("v1"),
        Instant::now(),
    );
}

#[test]
fn either_key_form_names_one_identity() {
    let gts_id = id(TYPE);
    let uuid = gts_id.to_uuid();
    assert_eq!(canonical_uuid(&EntityKey::GtsId(gts_id)), uuid);
    assert_eq!(canonical_uuid(&EntityKey::GtsUuid(uuid)), uuid);
}

#[test]
fn default_and_explicit_default_selections_share_one_representation() {
    let uuid = Uuid::nil();
    let explicit = Projection::Select(FieldSelection::with(&EntityField::DEFAULT));
    assert_eq!(
        RepKey::new(uuid, &Projection::Default),
        RepKey::new(uuid, &explicit)
    );
    let reordered = Projection::Select(FieldSelection::with(&[
        EntityField::Content,
        EntityField::Origin,
    ]));
    let ordered = Projection::Select(FieldSelection::with(&[
        EntityField::Origin,
        EntityField::Content,
        EntityField::GtsId,
    ]));
    assert_eq!(RepKey::new(uuid, &reordered), RepKey::new(uuid, &ordered));
    assert_ne!(
        RepKey::new(uuid, &Projection::Default),
        RepKey::new(uuid, &reordered),
        "a different field set is a different representation"
    );
}

/// The compact length of `expected`, an independently written REST entity payload.
fn wire_len(expected: &serde_json::Value) -> usize {
    serde_json::to_string(expected).expect("serializable").len()
}

#[test]
fn a_light_snapshot_weighs_its_compact_rest_payload() {
    let light = entity(TYPE, None);
    let expected = json!({
        "gts_id": TYPE,
        "gts_uuid": light.gts_uuid.to_string(),
        "kind": "type_schema",
        "lifecycle_status": "active",
    });
    assert_eq!(weight(&light), wire_len(&expected));
}

#[test]
fn a_full_snapshot_weighs_its_compact_rest_payload() {
    const AT: &str = "2026-11-20T10:00:00.5Z";
    let at = time::OffsetDateTime::parse(AT, &time::format_description::well_known::Rfc3339)
        .expect("timestamp");
    let mut full = (*entity(TYPE, Some(json!({ "type": "object", "title": "T" })))).clone();
    full.lifecycle_status = LifecycleStatus::Deleted;
    full.origin = Some(Origin::Managed {
        resource_version: 1234,
        created_at: at,
        updated_at: at,
    });
    full.resolved_schema = Some(json!([1, 2]));
    full.effective_traits = Some(json!({}));
    full.effective_traits_schema = Some(json!("x\"y"));
    full.provenance = Some(Provenance {
        compatibility_check_skipped: true,
    });
    let expected = json!({
        "gts_id": TYPE,
        "gts_uuid": full.gts_uuid.to_string(),
        "kind": "type_schema",
        "origin": {
            "type": "managed",
            "resource_version": 1234,
            "created_at": AT,
            "updated_at": AT,
        },
        "lifecycle_status": "deleted",
        "content": { "type": "object", "title": "T" },
        "resolved_schema": [1, 2],
        "effective_traits": {},
        "effective_traits_schema": "x\"y",
        "provenance": { "compatibility_check_skipped": true },
    });
    assert_eq!(weight(&full), wire_len(&expected));
}

#[test]
fn a_selected_null_is_charged_and_an_unselected_field_is_not() {
    let id = instance_id(1);
    let selected_null = entity(&id, Some(JsonDocument::Null));
    let expected = json!({
        "gts_id": id,
        "gts_uuid": selected_null.gts_uuid.to_string(),
        "kind": "instance",
        "lifecycle_status": "active",
        "content": null,
    });
    assert_eq!(weight(&selected_null), wire_len(&expected));
    assert_eq!(
        weight(&selected_null) - weight(&entity(&id, None)),
        r#","content":null"#.len()
    );
}

#[test]
fn a_snapshot_exactly_the_bound_fits_and_one_byte_more_does_not() {
    let snapshot = entity(TYPE, Some(json!({ "a": 1 })));
    let mut exact = Store::new(weight(&snapshot));
    put(&mut exact, &snapshot, &Projection::Default);
    assert!(exact.get(&key(&snapshot, &Projection::Default)).is_some());

    let mut short = Store::new(weight(&snapshot) - 1);
    put(&mut short, &snapshot, &Projection::Default);
    assert!(short.get(&key(&snapshot, &Projection::Default)).is_none());
}

#[test]
fn a_hit_returns_the_stored_snapshot_validator_and_confirmation() {
    let mut store = Store::new(1024 * 1024);
    let snapshot = entity(TYPE, None);
    let confirmed_at = Instant::now();
    let rep = key(&snapshot, &Projection::Default);
    store.insert(rep.clone(), Arc::clone(&snapshot), etag("v1"), confirmed_at);

    let hit = store.get(&rep).expect("resident");
    assert!(Arc::ptr_eq(&hit.snapshot, &snapshot));
    assert_eq!(hit.etag, etag("v1"));
    assert_eq!(hit.confirmed_at, confirmed_at);
    assert!(
        store
            .get(&key(&snapshot, &Projection::Select(FieldSelection::full())))
            .is_none(),
        "a narrow representation never answers a wider selection"
    );
}

#[test]
fn replacing_and_removing_keep_the_byte_count_exact() {
    let mut store = Store::new(1024 * 1024);
    let small = entity(TYPE, None);
    let large = entity(TYPE, Some(json!({ "a": "b".repeat(100) })));
    let rep = key(&small, &Projection::Default);

    put(&mut store, &small, &Projection::Default);
    assert_eq!(store.used(), weight(&small));
    store.insert(rep.clone(), Arc::clone(&large), etag("v2"), Instant::now());
    assert_eq!(
        store.used(),
        weight(&large),
        "a replacement is counted once"
    );

    store.remove(&rep);
    assert_eq!(store.used(), 0);
    assert!(store.get(&rep).is_none());
}

#[test]
fn removing_an_entity_drops_every_projection_of_it_and_nothing_else() {
    let mut store = Store::new(1024 * 1024);
    let target = entity(TYPE, Some(json!({})));
    let other = entity(&instance_id(1), None);
    let full = Projection::Select(FieldSelection::full());
    put(&mut store, &target, &Projection::Default);
    put(&mut store, &target, &full);
    put(&mut store, &other, &Projection::Default);

    store.remove_entity(target.gts_uuid);

    assert!(store.get(&key(&target, &Projection::Default)).is_none());
    assert!(store.get(&key(&target, &full)).is_none());
    assert!(store.get(&key(&other, &Projection::Default)).is_some());
    assert_eq!(store.used(), weight(&other));
}

#[test]
fn the_bound_is_bytes_a_thousand_small_entries_fit_and_one_megabyte_evicts() {
    const BOUND: usize = 1024 * 1024;
    let mut store = Store::new(BOUND);
    let small: Vec<_> = (0..1000).map(|n| entity(&instance_id(n), None)).collect();
    for snapshot in &small {
        put(&mut store, snapshot, &Projection::Default);
    }
    // The old entry-count bound of 1024 would also admit these; bytes are the point.
    assert!(
        small
            .iter()
            .all(|s| store.get(&key(s, &Projection::Default)).is_some()),
        "a thousand small snapshots fit within one megabyte"
    );
    // Promote the first one: it is now the most recently used of the small ones.
    assert!(store.get(&key(&small[0], &Projection::Default)).is_some());

    let large = entity(TYPE, Some(json!("x".repeat(1_000_000))));
    assert!(weight(&large) <= BOUND);
    put(&mut store, &large, &Projection::Default);

    assert!(store.get(&key(&large, &Projection::Default)).is_some());
    assert!(store.used() <= BOUND);
    assert!(
        store.get(&key(&small[1], &Projection::Default)).is_none(),
        "the least recently used entry is evicted first"
    );
    assert!(
        store.get(&key(&small[0], &Projection::Default)).is_some(),
        "the promoted entry outlives the eviction"
    );
}

#[test]
fn an_entry_larger_than_the_bound_is_not_retained_and_evicts_nothing() {
    let mut store = Store::new(4096);
    let resident = entity(&instance_id(1), None);
    put(&mut store, &resident, &Projection::Default);
    let stale = entity(TYPE, None);
    put(&mut store, &stale, &Projection::Default);

    let oversized = entity(TYPE, Some(json!("x".repeat(8192))));
    put(&mut store, &oversized, &Projection::Default);

    assert!(
        store.get(&key(&oversized, &Projection::Default)).is_none(),
        "neither the oversized snapshot nor the one it replaces stays resident"
    );
    assert!(store.get(&key(&resident, &Projection::Default)).is_some());
    assert_eq!(store.used(), weight(&resident));
}

#[test]
fn an_evicted_projection_leaves_no_trace_in_the_entity_index() {
    let first = entity(&instance_id(1), None);
    let mut store = Store::new(weight(&first) * 3);
    put(&mut store, &first, &Projection::Default);
    for n in 2..10 {
        put(
            &mut store,
            &entity(&instance_id(n), None),
            &Projection::Default,
        );
    }
    assert!(store.get(&key(&first, &Projection::Default)).is_none());
    assert!(!store.indexes(first.gts_uuid), "eviction cleans the index");
}

#[test]
fn evicting_one_projection_keeps_its_sibling_until_the_entity_is_invalidated() {
    // Heavier than the filler, so evicting one projection makes room for it.
    let target = entity(TYPE, Some(json!({ "pad": "x".repeat(100) })));
    let other = entity(&instance_id(1), None);
    let narrow = Projection::Default;
    let wide = Projection::Select(FieldSelection::full());
    let mut store = Store::new(weight(&target) * 2 + weight(&other));
    put(&mut store, &target, &narrow);
    put(&mut store, &target, &wide);
    put(&mut store, &other, &narrow);
    // Promote everything except the narrow projection, then make room for one more.
    assert!(store.get(&key(&target, &wide)).is_some());
    assert!(store.get(&key(&other, &narrow)).is_some());
    let filler = entity(&instance_id(2), None);
    put(&mut store, &filler, &narrow);

    assert!(store.get(&key(&target, &narrow)).is_none(), "evicted");
    assert!(
        store.get(&key(&target, &wide)).is_some(),
        "the sibling stays"
    );

    store.remove_entity(target.gts_uuid);
    assert!(
        store.get(&key(&target, &wide)).is_none(),
        "invalidation still reaches the remaining sibling"
    );
    assert_eq!(store.used(), weight(&other) + weight(&filler));
}
