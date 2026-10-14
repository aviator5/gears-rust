#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! Startup seeding through the real outbox admission (T31 slice 2).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit_db::{DBProvider, DbError};
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::OperationDispatch;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::registry_service::{
    BatchGetItem, EntityKey, EntityLookup, RegistryService,
};
use types_registry::domain::seeding::{SeedError, SeedSet, seed};
use types_registry::domain::selection::FieldSelection;
use types_registry::infra::outbox::OutboxDispatch;

mod common;

const BUDGET: Duration = Duration::from_secs(30);

/// Two registry instances: one may sequence a message into a partition whose lease
/// the other's processor holds, and that processor finds it only on its idle poll
/// (one minute under the `low_latency` outbox profile). The production seed budget
/// is five minutes; the races get more than one poll.
const RACE_BUDGET: Duration = Duration::from_mins(3);

const AM_BASE: &str = "gts.cf.core.am.tenant_type.v1~";
const CUSTOMER: &str = "gts.cf.core.am.tenant_type.v1~cf.core.am.customer.v1~";
const PLUGIN_SPEC: &str = "gts.cf.toolkit.plugins.plugin.v1~cf.core.seed_test.plugin.v1~";
const PERMISSION: &str = "gts.cf.toolkit.authz.permission.v1~cf.core.seed_test.read.v1";

/// One registry instance with its own running outbox pipeline.
struct Registry {
    service: Arc<RegistryService>,
    _outbox: Box<dyn std::any::Any + Send>,
}

async fn registry(db: &Arc<DBProvider<DbError>>, config: TypesRegistryConfig) -> Registry {
    let dispatch = Arc::new(OutboxDispatch::new());
    let service = Arc::new(RegistryService::new(
        db.db(),
        common::stores(),
        RegistrationPolicy::default(),
        config,
        Arc::clone(&dispatch) as Arc<dyn OperationDispatch>,
        common::metrics(),
    ));
    let outbox = types_registry::infra::outbox::start(db.db(), &service, &dispatch)
        .await
        .expect("start the admission outbox");
    Registry {
        service,
        _outbox: Box::new(outbox),
    }
}

async fn database(dir: &common::TestDir) -> Arc<DBProvider<DbError>> {
    let dsn = format!(
        "sqlite://{}?mode=rwc&journal_mode=wal",
        dir.path().join("seed.db").display()
    );
    common::provider_for_with_outbox(&dsn, 8).await
}

/// AM's linked base, copied verbatim from the e2e feature set's inventory.
fn am_base() -> Value {
    json!({
        "$id": format!("gts://{AM_BASE}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "description": "Base tenant type schema for Account Management.",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "id": {"allOf": [{"type": "string", "format": "gts-instance-id", "x-gts-ref": "gts.*"}]},
            "payload": {"type": "object"}
        },
        "required": ["payload"],
        "x-gts-traits-schema": {
            "type": "object",
            "properties": {
                "allowed_parent_types": {
                    "type": "array",
                    "items": {"type": "string", "x-gts-ref": AM_BASE},
                    "default": []
                },
                "idp_provisioning": {"type": "boolean", "default": false}
            },
            "additionalProperties": false
        },
        "x-gts-traits": {"allowed_parent_types": [], "idp_provisioning": false},
        "x-gts-abstract": true
    })
}

/// `config/e2e-local.yaml`'s customer: its platform-root parent is only an `x-gts-ref`.
fn customer(description: &str) -> Value {
    json!({
        "$id": format!("gts://{CUSTOMER}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "allOf": [{"$ref": format!("gts://{AM_BASE}")}],
        "description": description,
        "type": "object",
        "x-gts-traits": {
            "allowed_parent_types": [
                "gts.cf.core.am.tenant_type.v1~cf.core.am.platform.v1~",
                CUSTOMER
            ],
            "idp_provisioning": false
        }
    })
}

fn plugin_spec() -> Value {
    json!({
        "$id": format!("gts://{PLUGIN_SPEC}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "allOf": [{"$ref": "gts://gts.cf.toolkit.plugins.plugin.v1~"}]
    })
}

fn permission() -> Value {
    json!({
        "id": PERMISSION,
        "resource_type": "gts.cf.core.seed_test.thing.v1~",
        "action": "read",
        "display_name": "Read things"
    })
}

/// The real `toolkit-gts` inventory linked into this binary, plus documents of a
/// second crate depending on it, plus the configured customer.
fn seeds(customer_description: &str) -> SeedSet {
    let mut documents = toolkit_gts::all_inventory_type_schemas().expect("inventory schemas");
    documents.extend(toolkit_gts::all_inventory_instances().expect("inventory instances"));
    assert!(
        documents
            .iter()
            .any(|d| d["$id"] == "gts://gts.cf.toolkit.plugins.plugin.v1~"),
        "the toolkit-gts plugin base must be linked into this test binary"
    );
    documents.extend([am_base(), plugin_spec(), permission()]);
    documents.push(customer(customer_description));
    SeedSet::collect(documents, &TypesRegistryConfig::default().entity_id_fields)
        .expect("a valid seed set")
}

/// `gts_id -> (resource_version, content)` of every key, through the database read.
async fn stored(service: &RegistryService, ids: &[&str]) -> Vec<Option<(i64, Value)>> {
    let items: Vec<BatchGetItem> = ids
        .iter()
        .map(|id| BatchGetItem::from(EntityKey::GtsId((*id).to_owned())))
        .collect();
    let answers = service
        .batch_get(common::caller(), &items, FieldSelection::full())
        .await
        .expect("read seeds");
    ids.iter()
        .map(|id| {
            let lookup = common::answer_for(&answers, &EntityKey::GtsId((*id).to_owned()));
            match lookup {
                EntityLookup::Found { record, .. } => Some((
                    record.origin.expect("managed").resource_version,
                    common::doc(record.content.as_deref()).expect("content"),
                )),
                EntityLookup::NotFound => None,
                EntityLookup::Unchanged { .. } => panic!("unconditional read answered unchanged"),
            }
        })
        .collect()
}

const ALL: [&str; 6] = [
    "gts.cf.toolkit.plugins.plugin.v1~",
    "gts.cf.toolkit.authz.permission.v1~",
    AM_BASE,
    CUSTOMER,
    PLUGIN_SPEC,
    PERMISSION,
];

#[tokio::test]
async fn cold_start_admits_inventory_and_configured_entities_in_one_operation() {
    let dir = common::TestDir::new("tr-seed-cold");
    let db = database(&dir).await;
    let r = registry(&db, TypesRegistryConfig::default()).await;
    let set = seeds("Customer tenant under the platform root (E2E).");

    let summary = seed(&r.service, &set, BUDGET, &CancellationToken::new())
        .await
        .expect("a cold start seeds every document");

    assert_eq!(summary.documents, set.len());
    assert_eq!(summary.admitted, set.len());
    assert_eq!(summary.unchanged, 0);
    assert_eq!(summary.passes, 1, "one operation admits the whole graph");
    let read = stored(&r.service, &ALL).await;
    for (id, row) in ALL.iter().zip(&read) {
        assert_eq!(
            row.as_ref().map(|(rv, _)| *rv),
            Some(1),
            "{id} admitted once"
        );
    }
    assert_eq!(
        read[3].as_ref().unwrap().1,
        customer("Customer tenant under the platform root (E2E).")
    );
}

#[tokio::test]
async fn restart_with_equal_seeds_allocates_no_revision() {
    let dir = common::TestDir::new("tr-seed-restart");
    let db = database(&dir).await;
    let set = seeds("first");
    {
        let r = registry(&db, TypesRegistryConfig::default()).await;
        seed(&r.service, &set, BUDGET, &CancellationToken::new())
            .await
            .expect("first boot");
    }
    let r = registry(&db, TypesRegistryConfig::default()).await;

    let summary = seed(&r.service, &set, BUDGET, &CancellationToken::new())
        .await
        .expect("second boot");

    assert_eq!(summary.unchanged, set.len());
    assert_eq!(summary.admitted, 0);
    let read = stored(&r.service, &ALL).await;
    assert!(
        read.iter()
            .all(|row| row.as_ref().map(|(rv, _)| *rv) == Some(1))
    );
}

#[tokio::test]
async fn permitted_configured_change_revises_through_cas() {
    let dir = common::TestDir::new("tr-seed-update");
    let db = database(&dir).await;
    let r = registry(&db, TypesRegistryConfig::default()).await;
    seed(
        &r.service,
        &seeds("first"),
        BUDGET,
        &CancellationToken::new(),
    )
    .await
    .expect("first boot");

    let summary = seed(
        &r.service,
        &seeds("second"),
        BUDGET,
        &CancellationToken::new(),
    )
    .await
    .expect("a permitted change converges");

    assert_eq!(summary.admitted, 1);
    let read = stored(&r.service, &[CUSTOMER, AM_BASE]).await;
    assert_eq!(read[0], Some((2, customer("second"))));
    assert_eq!(
        read[1].as_ref().map(|(rv, _)| *rv),
        Some(1),
        "equal seeds keep their revision"
    );
}

#[tokio::test]
async fn a_missing_derivation_target_is_refused_by_identifier() {
    let dir = common::TestDir::new("tr-seed-missing");
    let db = database(&dir).await;
    let r = registry(&db, TypesRegistryConfig::default()).await;
    // The customer without its AM base: a real derivation/`$ref` dependency.
    let set = SeedSet::collect(
        [customer("orphan")],
        &TypesRegistryConfig::default().entity_id_fields,
    )
    .unwrap();

    let error = seed(&r.service, &set, BUDGET, &CancellationToken::new())
        .await
        .expect_err("a missing base must fail the seed");

    let SeedError::Rejected(failures) = &error else {
        panic!("expected a rejection, got {error}");
    };
    assert_eq!(failures.len(), 1, "{error}");
    assert_eq!(failures[0].gts_id, CUSTOMER);
    assert_eq!(failures[0].reason, "dependency_not_found", "{error}");
}

#[tokio::test]
async fn an_oversized_set_is_refused_before_any_write() {
    let dir = common::TestDir::new("tr-seed-oversized");
    let db = database(&dir).await;
    let mut config = TypesRegistryConfig::default();
    config.limits.batch_candidates = 2;
    let r = registry(&db, config).await;
    let set = seeds("first");

    let error = seed(&r.service, &set, BUDGET, &CancellationToken::new())
        .await
        .expect_err("the limit applies to the whole seed set");

    assert!(
        matches!(error, SeedError::TooLarge { count, limit: 2 } if count == set.len()),
        "{error}"
    );
    assert!(stored(&r.service, &ALL).await.iter().all(Option::is_none));
}

#[tokio::test]
async fn an_expired_budget_fails_naming_the_undecided_seeds() {
    let dir = common::TestDir::new("tr-seed-deadline");
    let db = database(&dir).await;
    let r = registry(&db, TypesRegistryConfig::default()).await;
    let set = seeds("first");

    let error = seed(&r.service, &set, Duration::ZERO, &CancellationToken::new())
        .await
        .expect_err("no budget, no seed");

    let SeedError::Deadline { undecided, .. } = &error else {
        panic!("expected a deadline, got {error}");
    };
    assert!(undecided.iter().any(|f| f.gts_id == CUSTOMER), "{error}");
}

#[tokio::test]
async fn cancellation_stops_seeding() {
    let dir = common::TestDir::new("tr-seed-cancel");
    let db = database(&dir).await;
    let r = registry(&db, TypesRegistryConfig::default()).await;
    let cancel = CancellationToken::new();
    cancel.cancel();

    let error = seed(&r.service, &seeds("first"), BUDGET, &cancel)
        .await
        .expect_err("a cancelled startup does not seed");

    assert!(matches!(error, SeedError::Cancelled), "{error}");
}

#[tokio::test]
async fn concurrent_seeders_converge_on_an_empty_database() {
    let dir = common::TestDir::new("tr-seed-race-empty");
    let db = database(&dir).await;
    let a = registry(&db, TypesRegistryConfig::default()).await;
    let b = registry(&db, TypesRegistryConfig::default()).await;
    let set = seeds("first");
    let cancel = CancellationToken::new();

    let (left, right) = tokio::join!(
        seed(&a.service, &set, RACE_BUDGET, &cancel),
        seed(&b.service, &set, RACE_BUDGET, &cancel),
    );

    left.expect("left seeder");
    right.expect("right seeder");
    let read = stored(&a.service, &ALL).await;
    assert!(
        read.iter()
            .all(|row| row.as_ref().map(|(rv, _)| *rv) == Some(1)),
        "one admitted version of each document: {read:?}"
    );
}

#[tokio::test]
async fn concurrent_seeders_converge_on_one_permitted_update() {
    let dir = common::TestDir::new("tr-seed-race-update");
    let db = database(&dir).await;
    let a = registry(&db, TypesRegistryConfig::default()).await;
    let b = registry(&db, TypesRegistryConfig::default()).await;
    let cancel = CancellationToken::new();
    seed(&a.service, &seeds("first"), RACE_BUDGET, &cancel)
        .await
        .expect("first boot");
    let set = seeds("second");

    let (left, right) = tokio::join!(
        seed(&a.service, &set, RACE_BUDGET, &cancel),
        seed(&b.service, &set, RACE_BUDGET, &cancel),
    );

    left.expect("left seeder");
    right.expect("right seeder");
    let read = stored(&a.service, &[CUSTOMER]).await;
    assert_eq!(
        read[0],
        Some((2, customer("second"))),
        "exactly one revision"
    );
}

#[tokio::test]
async fn a_change_only_in_a_signed_zero_is_revised() {
    let dir = common::TestDir::new("tr-seed-signed-zero");
    let db = database(&dir).await;
    let r = registry(&db, TypesRegistryConfig::default()).await;
    let id = "gts.cf.core.seed_test.zero.v1~";
    let with_default = |default: Value| {
        SeedSet::collect(
            [json!({
                "$id": format!("gts://{id}"),
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {"v": {"type": "number", "default": default}}
            })],
            &TypesRegistryConfig::default().entity_id_fields,
        )
        .unwrap()
    };
    seed(
        &r.service,
        &with_default(json!(0.0)),
        BUDGET,
        &CancellationToken::new(),
    )
    .await
    .expect("first boot");

    let summary = seed(
        &r.service,
        &with_default(json!(-0.0)),
        BUDGET,
        &CancellationToken::new(),
    )
    .await
    .expect("the change converges");

    assert_eq!(
        summary.admitted, 1,
        "-0.0 is a change, not an equal restart"
    );
    let (rv, content) = stored(&r.service, &[id]).await.remove(0).expect("stored");
    assert_eq!(rv, 2);
    assert_eq!(content["properties"]["v"]["default"].to_string(), "-0.0");
}

#[tokio::test]
async fn an_empty_set_still_honours_the_budget_and_cancellation() {
    let dir = common::TestDir::new("tr-seed-empty");
    let db = database(&dir).await;
    let r = registry(&db, TypesRegistryConfig::default()).await;
    let empty = SeedSet::default();

    let expired = seed(
        &r.service,
        &empty,
        Duration::ZERO,
        &CancellationToken::new(),
    )
    .await;
    assert!(
        matches!(expired, Err(SeedError::Deadline { .. })),
        "{expired:?}"
    );

    let cancel = CancellationToken::new();
    cancel.cancel();
    let cancelled = seed(&r.service, &empty, BUDGET, &cancel).await;
    assert!(
        matches!(cancelled, Err(SeedError::Cancelled)),
        "{cancelled:?}"
    );
}
