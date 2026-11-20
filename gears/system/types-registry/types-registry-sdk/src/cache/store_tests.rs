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
        weight(snapshot),
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
    store.insert(
        rep.clone(),
        Arc::clone(&snapshot),
        weight(&snapshot),
        etag("v1"),
        confirmed_at,
    );

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
    store.insert(
        rep.clone(),
        Arc::clone(&large),
        weight(&large),
        etag("v2"),
        Instant::now(),
    );
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

    store.invalidate(target.gts_uuid);

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

    store.invalidate(target.gts_uuid);
    assert!(
        store.get(&key(&target, &wide)).is_none(),
        "invalidation still reaches the remaining sibling"
    );
    assert_eq!(store.used(), weight(&other) + weight(&filler));
}

#[test]
fn an_invalidation_fences_a_ticket_taken_before_it_even_with_nothing_resident() {
    let mut store = Store::new(1024 * 1024);
    let uuid = Uuid::nil();
    let before = store.ticket(uuid);
    store.invalidate(uuid);
    assert!(!store.quiet_since(uuid, before));
    let after = store.ticket(uuid);
    assert!(store.quiet_since(uuid, after));
    store.release(uuid);
    store.release(uuid);
    assert!(
        !store.indexes(uuid),
        "released tickets leave no bookkeeping"
    );
}

#[test]
fn a_ticket_keeps_its_entitys_history_through_eviction() {
    let target = entity(TYPE, None);
    let filler = entity(&instance_id(1), None);
    // Room for either fixture alone, never both.
    let mut store = Store::new(weight(&target).max(weight(&filler)));
    put(&mut store, &target, &Projection::Default);
    let ticket = store.ticket(target.gts_uuid);
    store.invalidate(target.gts_uuid);
    put(&mut store, &target, &Projection::Default);
    put(&mut store, &filler, &Projection::Default);
    assert!(
        store.get(&key(&target, &Projection::Default)).is_none(),
        "evicted"
    );
    assert!(
        store.get(&key(&filler, &Projection::Default)).is_some(),
        "retained"
    );

    let later = store.ticket(target.gts_uuid);
    assert!(store.quiet_since(target.gts_uuid, later));
    assert!(
        !store.quiet_since(target.gts_uuid, ticket),
        "the slot kept the invalidation the old ticket must not see past"
    );
}

#[test]
fn a_confirmation_only_refreshes_the_validator_it_names() {
    let snapshot = entity(TYPE, None);
    let mut store = Store::new(1024 * 1024);
    let rep = key(&snapshot, &Projection::Default);
    let at = Instant::now();
    store.insert(
        rep.clone(),
        Arc::clone(&snapshot),
        weight(&snapshot),
        etag("v2"),
        at,
    );
    let later = at + std::time::Duration::from_secs(5);
    store.confirm(&rep, &etag("v1"), later);
    assert_eq!(store.peek(&rep).expect("resident").confirmed_at, at);
    store.confirm(&rep, &etag("v2"), later);
    assert_eq!(store.peek(&rep).expect("resident").confirmed_at, later);
    store.confirm(&rep, &etag("v2"), at);
    assert_eq!(
        store.peek(&rep).expect("resident").confirmed_at,
        later,
        "never moved back"
    );
}

#[test]
fn an_unmeasurable_snapshot_is_never_retained_even_under_an_unlimited_bound() {
    let mut store = Store::new(usize::MAX);
    let ordinary = entity(&instance_id(1), None);
    put(&mut store, &ordinary, &Projection::Default);
    let mut unmeasurable = (*entity(TYPE, None)).clone();
    let year_minus_one = time::OffsetDateTime::UNIX_EPOCH
        .replace_year(-1)
        .expect("a representable year");
    unmeasurable.origin = Some(Origin::Managed {
        resource_version: 1,
        created_at: year_minus_one,
        updated_at: year_minus_one,
    });
    let unmeasurable = Arc::new(unmeasurable);
    assert_eq!(
        weight(&unmeasurable),
        usize::MAX,
        "RFC 3339 cannot write year -1"
    );

    put(&mut store, &unmeasurable, &Projection::Default);
    assert!(
        store
            .get(&key(&unmeasurable, &Projection::Default))
            .is_none()
    );
    assert_eq!(store.used(), weight(&ordinary));
}

mod weight_matches_the_wire {
    use proptest::prelude::*;
    use serde_json::{Map, Value, json};
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;

    use super::{TYPE, entity, instance_id, weight, wire_len};
    use crate::models::{LifecycleStatus, Origin, Provenance};

    /// Arbitrary documents; strings draw on quotes, backslashes, control characters
    /// and multi-byte text, the inputs where a hand count and a serializer part ways.
    fn document() -> impl Strategy<Value = Value> {
        let text = prop_oneof![
            any::<String>(),
            "[\\x00-\\x1f\"\\\\/a-z\u{e9}\u{4e2d}\u{1f600}]{0,12}",
        ];
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(|n| json!(n)),
            text.clone().prop_map(Value::String),
        ];
        leaf.prop_recursive(3, 24, 4, move |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
                prop::collection::vec((text.clone(), inner), 0..4)
                    .prop_map(|pairs| Value::Object(pairs.into_iter().collect())),
            ]
        })
    }

    fn at() -> impl Strategy<Value = OffsetDateTime> {
        (0i64..4_000_000_000, 0u32..1_000_000_000).prop_map(|(secs, nanos)| {
            OffsetDateTime::from_unix_timestamp(secs)
                .expect("in range")
                .replace_nanosecond(nanos)
                .expect("nanoseconds")
        })
    }

    proptest! {
        #[test]
        fn any_snapshot_weighs_its_compact_serialization(
            instance in any::<bool>(),
            deleted in any::<bool>(),
            origin in prop::option::of((any::<u64>(), at(), at())),
            docs in prop::collection::vec(prop::option::of(document()), 4),
            skipped in prop::option::of(any::<bool>()),
        ) {
            let id = if instance { instance_id(1) } else { TYPE.to_owned() };
            let mut snapshot = (*entity(&id, None)).clone();
            let mut expected = Map::new();
            expected.insert("gts_id".into(), json!(id));
            expected.insert("gts_uuid".into(), json!(snapshot.gts_uuid.to_string()));
            expected.insert("kind".into(), json!(if instance { "instance" } else { "type_schema" }));
            expected.insert("lifecycle_status".into(), json!(if deleted { "deleted" } else { "active" }));
            if deleted {
                snapshot.lifecycle_status = LifecycleStatus::Deleted;
            }
            if let Some((resource_version, created_at, updated_at)) = origin {
                snapshot.origin = Some(Origin::Managed { resource_version, created_at, updated_at });
                expected.insert("origin".into(), json!({
                    "type": "managed",
                    "resource_version": resource_version,
                    "created_at": created_at.format(&Rfc3339).expect("format"),
                    "updated_at": updated_at.format(&Rfc3339).expect("format"),
                }));
            }
            let [content, resolved, traits, traits_schema] = <[_; 4]>::try_from(docs).expect("four");
            for (name, doc, slot) in [
                ("content", content, &mut snapshot.content),
                ("resolved_schema", resolved, &mut snapshot.resolved_schema),
                ("effective_traits", traits, &mut snapshot.effective_traits),
                ("effective_traits_schema", traits_schema, &mut snapshot.effective_traits_schema),
            ] {
                if let Some(doc) = doc {
                    expected.insert(name.into(), doc.clone());
                    *slot = Some(doc);
                }
            }
            if let Some(compatibility_check_skipped) = skipped {
                snapshot.provenance = Some(Provenance { compatibility_check_skipped });
                expected.insert("provenance".into(), json!({
                    "compatibility_check_skipped": compatibility_check_skipped,
                }));
            }
            prop_assert_eq!(weight(&snapshot), wire_len(&Value::Object(expected)));
        }
    }
}
