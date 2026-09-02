//! Reverse-impact artifact refresh (T14): a revision re-materializes every
//! dependent's effective artifacts in its own commit transaction.
//!
//! Driven through the real worker — submit, then `run_operation` — because the
//! claim is about what one commit leaves behind, and only a commit can show it.
//! No `sleep`, no timer, no polling (SPEC §13).
//!
//! # What a refresh moves, and what it must not
//!
//! A dependent's authored document did not change, so it gets **no new
//! revision** and its `resource_version` stays where it is — that column is
//! reserved for optimistic writes. What moves is `type_schema`'s materialized
//! artifacts and their `resolution_fingerprint`, which is why the fingerprint
//! supports equality and never ordering (`database.sql`).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

mod common;

use std::sync::Arc;

use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::{DBProvider, DbError, DbTx};
use toolkit_gts::gts_id;
use uuid::Uuid;

use common::{allow_all, run_operation, stores, test_db};
use types_registry::config::{Limits, TypesRegistryConfig};
use types_registry::domain::admission::acceptance::{AcceptanceContext, AcceptanceError, accept};
use types_registry::domain::admission::refresh::RefreshOutcome;
use types_registry::domain::admission::worker::{OperationOutcome, WorkerError};
use types_registry::domain::admission::{Candidate, OperationDispatch, SubmitRequest};
use types_registry::domain::enums as domain_enums;
use types_registry::domain::enums::OperationItemStatus;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::ports::{CurrentTypeSchemaRow, EntityRow};
use types_registry::infra::storage::repo::{EntityRepo, InstanceRepo, TypeSchemaRepo};

const NOW: OffsetDateTime = datetime!(2026-08-19 09:15:30 UTC);
const LATER: OffsetDateTime = datetime!(2026-08-19 10:20:40 UTC);

/// Its own family prefix, for the reason `dependency_test.rs` records: the
/// `SQLite` family lock is keyed per DSN across processes, so two binaries
/// admitting one family key contend even with isolated databases.
const BASE: &str = gts_id!("cf.core.ref.thing.v1~");
/// Derived from [`BASE`] — its resolution composes the base, so a base revision
/// changes its effective artifacts.
const DERIVED: &str = gts_id!("cf.core.ref.thing.v1~cf.core.ref.leaf.v1~");
/// Outside every chain here, and reaches [`BASE`] only through a `$ref`.
const REFERRER: &str = gts_id!("cf.core.ref.referrer.v1~");
/// A second referrer, so an over-bound case has two dependents to exceed one.
const SECOND: &str = gts_id!("cf.core.ref.second.v1~");
/// An Instance of [`BASE`]: a dependent with no materialized artifacts at all.
const INSTANCE: &str = gts_id!("cf.core.ref.thing.v1~cf.core.ref.first.v1");

type Provider = Arc<DBProvider<DbError>>;

struct NoDispatch;

#[async_trait::async_trait]
impl OperationDispatch for NoDispatch {
    async fn enqueue(&self, _tx: &DbTx<'_>, _operation_id: Uuid) -> anyhow::Result<()> {
        Ok(())
    }
}

fn worker(db: &Provider) -> DBProvider<WorkerError> {
    DBProvider::new(db.db())
}

/// A base schema with one named property, so a revision can change the shape its
/// dependents compose.
fn base_schema(property: &str) -> Value {
    json!({
        "$id": format!("gts://{BASE}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { property: { "type": "string" } },
    })
}

/// Derived from [`BASE`] through the identifier chain **and** an explicit `$ref`,
/// which is the shape whose resolved schema inlines the base.
fn derived_schema() -> Value {
    json!({
        "$id": format!("gts://{DERIVED}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "allOf": [
            { "$ref": format!("gts://{BASE}") },
            { "type": "object", "properties": { "tier": { "type": "string" } } },
        ],
    })
}

/// A schema outside the chain whose one property is a `$ref` to [`BASE`].
fn referencing_schema(id: &str) -> Value {
    json!({
        "$id": format!("gts://{id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "subject": { "$ref": format!("gts://{BASE}") } },
    })
}

async fn submit(
    db: &Provider,
    key: &str,
    gts_id: &str,
    content: Value,
    expected_resource_version: Option<i64>,
) -> Result<Uuid, AcceptanceError> {
    let provider: DBProvider<AcceptanceError> = DBProvider::new(db.db());
    let policy = RegistrationPolicy::default();
    let config = TypesRegistryConfig::default();
    let dispatch: Arc<dyn OperationDispatch> = Arc::new(NoDispatch);
    accept(
        &stores(),
        &provider,
        &allow_all(),
        &AcceptanceContext {
            policy: &policy,
            config: &config,
        },
        &dispatch,
        &SubmitRequest {
            idempotency_key: key.to_owned(),
            kind: domain_enums::OperationKind::Registration,
            dry_run: false,
            candidates: vec![Candidate {
                gts_id: gts_id.to_owned(),
                content: Some(content),
                expected_resource_version,
                force: false,
            }],
        },
        NOW,
    )
    .await
    .map(|accepted| accepted.operation_id)
}

/// Submit one candidate and admit it under `limits`.
async fn admit_with(
    db: &Provider,
    limits: &Limits,
    key: &str,
    gts_id: &str,
    content: Value,
    expected_resource_version: Option<i64>,
) -> OperationOutcome {
    let operation_id = submit(db, key, gts_id, content, expected_resource_version)
        .await
        .expect("acceptance");
    run_operation(
        &stores(),
        &worker(db),
        &allow_all(),
        limits,
        operation_id,
        LATER,
    )
    .await
    .expect("the worker must not fail on infrastructure")
}

/// The same, under P0's configured defaults.
async fn admit(
    db: &Provider,
    key: &str,
    gts_id: &str,
    content: Value,
    expected_resource_version: Option<i64>,
) -> OperationOutcome {
    admit_with(
        db,
        &common::limits(),
        key,
        gts_id,
        content,
        expected_resource_version,
    )
    .await
}

async fn entity(db: &Provider, gts_id: &str) -> EntityRow {
    let conn = db.conn().expect("conn");
    EntityRepo::find_by_gts_id(&conn, &allow_all(), gts_id)
        .await
        .expect("read")
        .unwrap_or_else(|| panic!("{gts_id} must exist"))
}

async fn current(db: &Provider, gts_id: &str) -> CurrentTypeSchemaRow {
    let entity_id = entity(db, gts_id).await.id;
    let conn = db.conn().expect("conn");
    TypeSchemaRepo::find_current(&conn, &allow_all(), entity_id)
        .await
        .expect("read")
        .unwrap_or_else(|| panic!("{gts_id} must have a current row"))
}

/// [`BASE`] plus the two dependents that carry artifacts, all three admitted.
async fn seed_base_and_dependents(db: &Provider) {
    admit(db, "k-base", BASE, base_schema("name"), None).await;
    admit(db, "k-derived", DERIVED, derived_schema(), None).await;
    admit(
        db,
        "k-referrer",
        REFERRER,
        referencing_schema(REFERRER),
        None,
    )
    .await;
}

fn succeeded(
    outcome: &OperationOutcome,
) -> &types_registry::domain::admission::worker::ItemOutcome {
    let item = &outcome.items[0];
    assert_eq!(
        item.status,
        OperationItemStatus::Succeeded,
        "expected a succeeded item, got {item:?}"
    );
    item
}

// ---------------------------------------------------------------------------
// the refresh
// ---------------------------------------------------------------------------

/// The criterion: revising a base refreshes exactly its N dependents, in the same
/// transaction as the new revision — and moves neither their revision pointer nor
/// their `resource_version`.
#[tokio::test]
async fn revising_a_base_refreshes_every_dependent_schema() {
    let db = test_db().await;
    seed_base_and_dependents(&db).await;

    let before_derived = current(&db, DERIVED).await;
    let before_referrer = current(&db, REFERRER).await;
    let derived_version = entity(&db, DERIVED).await.resource_version;

    let outcome = admit(&db, "k-base-2", BASE, base_schema("label"), Some(1)).await;
    succeeded(&outcome);

    let after_derived = current(&db, DERIVED).await;
    let after_referrer = current(&db, REFERRER).await;

    assert_ne!(
        before_derived.resolution_fingerprint, after_derived.resolution_fingerprint,
        "the derived type composes the base, so its artifacts moved"
    );
    assert_ne!(
        before_referrer.resolution_fingerprint, after_referrer.resolution_fingerprint,
        "the referrer inlines the base through its `$ref`, so its artifacts moved"
    );
    assert!(
        after_derived.resolved_schema.contains("label"),
        "the refreshed artifacts carry the base's new property, got {}",
        after_derived.resolved_schema
    );

    assert_eq!(
        after_derived.revision_no, before_derived.revision_no,
        "a dependency-driven change writes no revision"
    );
    assert_eq!(
        entity(&db, DERIVED).await.resource_version,
        derived_version,
        "`resource_version` is reserved for optimistic writes; the fingerprint is \
         what a dependency-driven change moves"
    );
}

/// The early stop: a dependent whose recomputation is byte-identical is not
/// written at all. Shown by refreshing twice — the second pass sees the state the
/// first left, recomputes the same artifacts, and touches nothing. A redelivered
/// commit is exactly this case.
#[tokio::test]
async fn a_dependent_whose_artifacts_are_identical_is_not_rewritten() {
    let db = test_db().await;
    seed_base_and_dependents(&db).await;
    admit(&db, "k-base-2", BASE, base_schema("label"), Some(1)).await;

    let after_first = current(&db, DERIVED).await;

    let roots = vec![entity(&db, BASE).await.id];
    let outcome = refresh_directly(&db, &roots, &common::limits()).await;

    assert_eq!(
        outcome.examined, 2,
        "both dependents were reached and recomputed; an empty write set over an \
         empty impact set would prove nothing"
    );
    assert!(
        outcome.refreshed.is_empty(),
        "nothing recomputed to different bytes, so nothing was written: {:?}",
        outcome.refreshed
    );
    let after_second = current(&db, DERIVED).await;
    assert_eq!(
        after_first.updated_at, after_second.updated_at,
        "a skipped dependent is not touched at all, not even its timestamp"
    );
}

/// An Instance carries no materialized artifacts (`instance` holds a revision
/// pointer and nothing else), so the walk reaches it and the refresh has nothing
/// to do for it. The claim is that this is a no-op rather than a failure.
#[tokio::test]
async fn an_instance_dependent_is_reached_and_left_alone() {
    let db = test_db().await;
    admit(&db, "k-base", BASE, base_schema("name"), None).await;
    admit(
        &db,
        "k-instance",
        INSTANCE,
        json!({ "name": "first" }),
        None,
    )
    .await;

    let entity_id = entity(&db, INSTANCE).await.id;
    let conn = db.conn().expect("conn");
    let before = InstanceRepo::find_current(&conn, &allow_all(), entity_id)
        .await
        .expect("read")
        .expect("the instance has a current row");

    let outcome = admit(&db, "k-base-2", BASE, base_schema("label"), Some(1)).await;
    succeeded(&outcome);

    let conn = db.conn().expect("conn");
    let after = InstanceRepo::find_current(&conn, &allow_all(), entity_id)
        .await
        .expect("read")
        .expect("the instance still has a current row");
    assert_eq!(
        (before.revision_no, before.updated_at),
        (after.revision_no, after.updated_at),
        "the Instance's current row is untouched by a refresh of its type"
    );
}

// ---------------------------------------------------------------------------
// the bound
// ---------------------------------------------------------------------------

/// Over `limits.activation_write_set` the candidate fails with a structured
/// reason and **nothing** is committed — not the new revision, not a partial
/// refresh.
#[tokio::test]
async fn an_over_bound_write_set_commits_nothing() {
    let db = test_db().await;
    seed_base_and_dependents(&db).await;
    admit(&db, "k-second", SECOND, referencing_schema(SECOND), None).await;

    let before_base = entity(&db, BASE).await;
    let before_base_current = current(&db, BASE).await;
    let before_derived = current(&db, DERIVED).await;
    let before_referrer = current(&db, REFERRER).await;

    let tight = Limits {
        activation_write_set: 1,
        ..Limits::default()
    };
    let outcome = admit_with(&db, &tight, "k-base-2", BASE, base_schema("label"), Some(1)).await;

    let item = &outcome.items[0];
    assert_eq!(item.status, OperationItemStatus::Failed, "got {item:?}");
    let failure = item
        .failure
        .as_ref()
        .expect("a failed item names its reason");
    assert_eq!(failure.reason, "activation_write_set_exceeded");

    assert_eq!(
        entity(&db, BASE).await.resource_version,
        before_base.resource_version,
        "the base's own revision rolled back with the refresh"
    );
    assert_eq!(
        current(&db, BASE).await,
        before_base_current,
        "the base's current row is byte-identical to the one it had"
    );
    assert_eq!(
        current(&db, DERIVED).await.resolution_fingerprint,
        before_derived.resolution_fingerprint,
        "no dependent was refreshed"
    );
    assert_eq!(
        current(&db, REFERRER).await.resolution_fingerprint,
        before_referrer.resolution_fingerprint,
    );
}

/// A creation refreshes nothing, and that is a claim about the *base*: admitting a
/// new derived type cannot move the artifacts of the type it composes. Nothing
/// depended on the candidate a moment ago, so there is no reverse impact to have.
#[tokio::test]
async fn a_creation_leaves_its_base_untouched() {
    let db = test_db().await;
    admit(&db, "k-base", BASE, base_schema("name"), None).await;
    let before_base = current(&db, BASE).await;

    let outcome = admit(&db, "k-derived", DERIVED, derived_schema(), None).await;
    succeeded(&outcome);

    assert_eq!(
        current(&db, BASE).await,
        before_base,
        "the base is a dependency of the new candidate, not a dependent of it"
    );
}

// ---------------------------------------------------------------------------
// helpers that reach the refresh step on its own
// ---------------------------------------------------------------------------

/// Run the refresh step alone, in its own commit transaction, and report which
/// dependents it wrote. Used by the early-stop test: the claim there is about a
/// *second* refresh over unchanged state, which no second admission can express —
/// identical content is `unchanged` and never reaches a commit.
async fn refresh_directly(db: &Provider, roots: &[i64], limits: &Limits) -> RefreshOutcome {
    use types_registry::domain::admission::refresh::refresh_dependents;
    use types_registry::domain::ports::commit_write;

    let provider: DBProvider<WorkerError> = DBProvider::new(db.db());
    let roots = roots.to_vec();
    let bound = limits.activation_write_set;
    provider
        .transaction_with_config(commit_write(&provider.db()), move |tx| {
            let roots = roots.clone();
            Box::pin(async move {
                Ok(
                    refresh_dependents(stores().as_ref(), tx, &allow_all(), &roots, bound, LATER)
                        .await?
                        .expect("the refresh must not refuse under the default bound"),
                )
            })
        })
        .await
        .expect("refresh transaction")
}
