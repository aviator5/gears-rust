//! The commit-time revision-vector guard and the bounded revalidation loop
//! (T15, D4, SPEC §8.1 steps 3 and 4.3).
//!
//! # Why the guard needs its own suite
//!
//! Every other admission test runs one pass at a time, and a single pass can
//! never drift: it evaluates and commits against the same state. The claim here is
//! about what happens when something moves *between* those two, which needs a
//! mutation placed exactly in that gap.
//!
//! Two shapes do that, and both are deterministic — no `sleep`, no timer, no
//! polling (SPEC §13):
//!
//! * **Split by hand.** `evaluate` closes its snapshot transaction before it
//!   returns, so a test can call it, commit a real mutation, and then call
//!   `commit_revision`. Nothing overlaps, so `SQLite` is enough, and the mutation
//!   is an ordinary committed admission rather than an injection.
//! * **Held at the boundary.** `common::PausingStores` holds the pass at
//!   `PausePoint::RevisionEntityRead` — the commit transaction's *first* statement,
//!   which means the transaction is open and holding nothing, so a second
//!   connection can commit underneath it even on `SQLite`. That is what makes the
//!   loop itself observable: one drift, one rollback, one fresh evaluation.
//!
//! The file-backed database is deliberate for the second shape: `sqlite::memory:`
//! gives every pooled connection its own empty database, so a second connection
//! would see no tables.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

mod common;

use std::sync::Arc;

use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::{DBProvider, DbError, DbTx};
use toolkit_gts::gts_id;
use uuid::Uuid;

use common::{
    PausePoint, PausingStores, TestDir, allow_all, stores, test_db, test_db_file, worker_settings,
};
use types_registry::config::{TypesRegistryConfig, WorkerSettings};
use types_registry::domain::admission::acceptance::{AcceptanceContext, AcceptanceError, accept};
use types_registry::domain::admission::unit::{
    EvaluatedUnit, RevisionCommit, commit_creation, commit_revision, evaluate,
};
use types_registry::domain::admission::vector::{VectorDrift, VectorRole};
use types_registry::domain::admission::worker::{OperationOutcome, WorkerError, run_operation};
use types_registry::domain::admission::{Candidate, OperationDispatch, SubmitRequest};
use types_registry::domain::enums as domain_enums;
use types_registry::domain::enums::OperationItemStatus;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::ports::{
    CurrentTypeSchemaRow, EntityRow, NewCurrentTypeSchema, Stores, commit_write,
};
use types_registry::domain::registry_service::{
    AdmissionMode, EntityKey, RegistryService, ServiceError,
};
use types_registry::infra::storage::repo::{EntityRepo, OperationRepo, TypeSchemaRepo};

const NOW: OffsetDateTime = datetime!(2026-08-20 09:15:30 UTC);
const LATER: OffsetDateTime = datetime!(2026-08-20 10:20:40 UTC);

/// Its own family prefix, for the reason `dependency_test.rs` records: the
/// `SQLite` family lock is keyed per DSN across processes, so two binaries
/// admitting one family key contend even with isolated databases.
const BASE: &str = gts_id!("cf.core.reval.thing.v1~");
/// Derived from [`BASE`] through the identifier chain and an explicit `$ref`.
const DERIVED: &str = gts_id!("cf.core.reval.thing.v1~cf.core.reval.leaf.v1~");
/// Outside every chain here, and reaches [`BASE`] only through a `$ref`.
const REFERRER: &str = gts_id!("cf.core.reval.referrer.v1~");

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

fn base_schema(property: &str) -> Value {
    json!({
        "$id": format!("gts://{BASE}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { property: { "type": "string" } },
    })
}

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

/// A schema outside the chain whose one property is a `$ref` to [`BASE`], with a
/// second property so a revision of it has something to change.
fn referencing_schema(marker: &str) -> Value {
    json!({
        "$id": format!("gts://{REFERRER}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "subject": { "$ref": format!("gts://{BASE}") },
            marker: { "type": "string" },
        },
    })
}

// ---------------------------------------------------------------------------
// Driving one admission
// ---------------------------------------------------------------------------

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

/// Submit one candidate and admit it through the real worker and the real ports.
async fn admit(
    db: &Provider,
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
        &common::limits(),
        &worker_settings(),
        operation_id,
        LATER,
    )
    .await
    .expect("the worker must not fail on infrastructure")
}

/// Submit a candidate and run only step 3 against it: the evaluation whose
/// verdict — and whose revision vector — the commit will be handed.
async fn evaluated(
    db: &Provider,
    key: &str,
    gts_id: &str,
    content: Value,
    expected_resource_version: Option<i64>,
) -> EvaluatedUnit {
    let operation_id = submit(db, key, gts_id, content, expected_resource_version)
        .await
        .expect("acceptance");
    let provider = worker(db);
    let conn = provider.conn().expect("conn");
    let item = OperationRepo::find_items(&conn, &allow_all(), operation_id)
        .await
        .expect("items")[0]
        .clone();
    let payload = item.request_payload.clone().expect("payload");
    evaluate(
        &stores(),
        &provider,
        &allow_all(),
        &item.gts_id,
        &payload,
        item.id,
        common::limits().activation_write_set,
    )
    .await
    .expect("evaluation must not fail on infrastructure")
    .expect("the candidate is valid")
}

/// Hand an already-evaluated unit to `commit_revision`, in its own transaction,
/// exactly as `worker::commit_evaluated` does.
async fn commit_the_revision(
    db: &Provider,
    unit: &EvaluatedUnit,
    expected_resource_version: i64,
) -> Result<
    Result<RevisionCommit, types_registry::domain::admission::worker::ItemFailure>,
    WorkerError,
> {
    let provider = worker(db);
    let ports = stores();
    let unit = unit.clone();
    provider
        .transaction_with_config(commit_write(&provider.db()), move |tx| {
            let unit = unit.clone();
            let ports = Arc::clone(&ports);
            Box::pin(async move {
                commit_revision(
                    ports.as_ref(),
                    tx,
                    &allow_all(),
                    &unit,
                    expected_resource_version,
                    common::limits().activation_write_set,
                    LATER,
                )
                .await
            })
        })
        .await
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

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

/// [`BASE`] and the two dependents that carry artifacts, all admitted.
async fn seed_base_and_dependents(db: &Provider) {
    admit(db, "k-base", BASE, base_schema("name"), None).await;
    admit(db, "k-derived", DERIVED, derived_schema(), None).await;
    admit(db, "k-referrer", REFERRER, referencing_schema("note"), None).await;
}

// ---------------------------------------------------------------------------
// The guard, split by hand
// ---------------------------------------------------------------------------

/// The control. Without it every assertion below could be passing because the
/// guard refuses everything.
#[tokio::test]
async fn a_commit_whose_vector_did_not_move_stands() {
    let db = test_db().await;
    seed_base_and_dependents(&db).await;

    let unit = evaluated(&db, "k-ref-2", REFERRER, referencing_schema("tag"), Some(1)).await;
    let committed = commit_the_revision(&db, &unit, 1)
        .await
        .expect("no infrastructure failure")
        .expect("no candidate refusal");

    assert!(matches!(committed, RevisionCommit::Admitted(_)));
    assert_eq!(entity(&db, REFERRER).await.resource_version, 2);
}

/// The criterion: a dependency that moved between evaluation and commit rolls the
/// transaction back. The mutation is a real committed admission of a [`BASE`]
/// revision — not an injected column write — landing while nothing is open.
#[tokio::test]
async fn a_dependency_that_moved_after_evaluation_rolls_the_commit_back() {
    let db = test_db().await;
    seed_base_and_dependents(&db).await;

    let unit = evaluated(&db, "k-ref-2", REFERRER, referencing_schema("tag"), Some(1)).await;
    let before = entity(&db, REFERRER).await;

    // The base moves underneath the evaluation. This also refreshes REFERRER's
    // artifacts, which is what makes the stale resolution the unit is holding
    // genuinely wrong rather than merely old.
    admit(&db, "k-base-2", BASE, base_schema("label"), Some(1)).await;

    let outcome = commit_the_revision(&db, &unit, 1).await;

    let Err(WorkerError::RevalidationRequired(drift)) = outcome else {
        panic!("the guard must refuse a stale evaluation, got {outcome:?}");
    };
    assert_eq!(
        drift,
        VectorDrift::Moved {
            gts_id: BASE.to_owned(),
            role: VectorRole::Dependency,
            recorded: 1,
            found: 2,
        }
    );

    let after = entity(&db, REFERRER).await;
    assert_eq!(
        after.resource_version, before.resource_version,
        "a rolled-back commit moves no version"
    );
    assert!(
        !current(&db, REFERRER).await.resolved_schema.contains("tag"),
        "a rolled-back commit writes no revision"
    );
}

/// The phantom dependent: nothing depended on [`BASE`] when its revision was
/// evaluated, and something does by the time it commits. Detected on **membership**
/// — no column of any recorded entry changed.
#[tokio::test]
async fn a_phantom_dependent_created_after_the_scan_is_detected() {
    let db = test_db().await;
    admit(&db, "k-base", BASE, base_schema("name"), None).await;

    let unit = evaluated(&db, "k-base-2", BASE, base_schema("label"), Some(1)).await;
    assert!(
        unit.vector.entries.is_empty(),
        "the base has no dependencies and, yet, no dependents: {:?}",
        unit.vector
    );

    admit(&db, "k-derived", DERIVED, derived_schema(), None).await;

    let outcome = commit_the_revision(&db, &unit, 1).await;

    let Err(WorkerError::RevalidationRequired(drift)) = outcome else {
        panic!("a phantom dependent must roll the commit back, got {outcome:?}");
    };
    assert_eq!(
        drift,
        VectorDrift::Appeared {
            gts_id: DERIVED.to_owned(),
            role: VectorRole::Dependent,
        }
    );
    assert_eq!(
        entity(&db, BASE).await.resource_version,
        1,
        "a rolled-back commit moves no version"
    );
}

/// The case `resource_version` alone cannot see. A dependent refreshed by someone
/// else's commit gets new artifacts and **no** version move, so only the recorded
/// `resolution_fingerprint` can show it — which is why the vector carries one.
///
/// The refresh is applied through `update_current_schema`, the same port the real
/// reverse-impact refresh writes through, with the dependent's revision pointer
/// carried over unchanged exactly as `refresh::refresh_dependents` does.
#[tokio::test]
async fn a_dependent_refreshed_after_the_scan_is_detected() {
    let db = test_db().await;
    admit(&db, "k-base", BASE, base_schema("name"), None).await;
    admit(&db, "k-derived", DERIVED, derived_schema(), None).await;

    let unit = evaluated(&db, "k-base-2", BASE, base_schema("label"), Some(1)).await;
    let recorded = unit
        .vector
        .entries
        .iter()
        .find(|entry| entry.gts_id == DERIVED)
        .expect("the derived type is a dependent of the base")
        .clone();
    assert_eq!(recorded.role, VectorRole::Dependent);
    assert!(
        recorded.resolution_fingerprint.is_some(),
        "a live Type Schema dependent's effective content is consumed, so its \
         fingerprint is recorded"
    );

    let derived = current(&db, DERIVED).await;
    let derived_id = entity(&db, DERIVED).await.id;
    let before_version = entity(&db, DERIVED).await.resource_version;
    let ports = stores();
    worker(&db)
        .transaction(move |tx| {
            let ports = Arc::clone(&ports);
            Box::pin(async move {
                let moved = ports
                    .update_current_schema(
                        tx,
                        &allow_all(),
                        NewCurrentTypeSchema {
                            entity_id: derived_id,
                            revision_no: derived.revision_no,
                            resolved_schema: derived.resolved_schema.clone(),
                            effective_traits: derived.effective_traits.clone(),
                            effective_traits_schema: derived.effective_traits_schema.clone(),
                            resolution_fingerprint: vec![0xFF; 32],
                            now: LATER,
                        },
                    )
                    .await?;
                assert!(moved, "the refresh must find the dependent's current row");
                Ok(())
            })
        })
        .await
        .expect("the simulated refresh commits");
    assert_eq!(
        entity(&db, DERIVED).await.resource_version,
        before_version,
        "a refresh moves no version, which is the whole reason the fingerprint is in \
         the vector"
    );

    let outcome = commit_the_revision(&db, &unit, 1).await;

    let Err(WorkerError::RevalidationRequired(drift)) = outcome else {
        panic!("a refreshed dependent must roll the commit back, got {outcome:?}");
    };
    assert_eq!(
        drift,
        VectorDrift::Refreshed {
            gts_id: DERIVED.to_owned(),
        }
    );
}

/// The guard is on the creation path too. A creation has no dependents — nothing
/// can point at an identifier that does not resolve — so what it protects is the
/// resolution the candidate was validated against.
#[tokio::test]
async fn a_creation_whose_dependency_moved_after_evaluation_rolls_the_commit_back() {
    let db = test_db().await;
    admit(&db, "k-base", BASE, base_schema("name"), None).await;

    let unit = evaluated(&db, "k-ref", REFERRER, referencing_schema("note"), None).await;
    admit(&db, "k-base-2", BASE, base_schema("label"), Some(1)).await;

    let provider = worker(&db);
    let ports = stores();
    let candidate = unit.clone();
    let outcome = provider
        .transaction_with_config(commit_write(&provider.db()), move |tx| {
            let candidate = candidate.clone();
            let ports = Arc::clone(&ports);
            Box::pin(async move {
                commit_creation(
                    ports.as_ref(),
                    tx,
                    &allow_all(),
                    &candidate,
                    common::limits().activation_write_set,
                    LATER,
                )
                .await
            })
        })
        .await;

    assert!(
        matches!(
            outcome,
            Err(WorkerError::RevalidationRequired(VectorDrift::Moved { .. }))
        ),
        "the guard must refuse a stale creation, got {outcome:?}"
    );
    let conn = db.conn().expect("conn");
    assert!(
        EntityRepo::find_by_gts_id(&conn, &allow_all(), REFERRER)
            .await
            .expect("read")
            .is_none(),
        "a rolled-back creation leaves no entity"
    );
}

/// An `unchanged` outcome is deliberately **not** guarded: it writes nothing, and
/// the one thing it decides is decided from rows read inside its own transaction.
/// Guarding it could only turn a genuine no-op re-submit into a revalidation and,
/// eventually, into a failure.
#[tokio::test]
async fn an_unchanged_resubmission_is_not_refused_by_a_moved_dependency() {
    let db = test_db().await;
    seed_base_and_dependents(&db).await;

    let unit = evaluated(
        &db,
        "k-ref-same",
        REFERRER,
        referencing_schema("note"),
        Some(1),
    )
    .await;
    admit(&db, "k-base-2", BASE, base_schema("label"), Some(1)).await;

    let committed = commit_the_revision(&db, &unit, 1)
        .await
        .expect("no infrastructure failure")
        .expect("no candidate refusal");

    assert!(
        matches!(committed, RevisionCommit::Unchanged { .. }),
        "got {committed:?}"
    );
}

// ---------------------------------------------------------------------------
// The bounded loop, held at the commit boundary
// ---------------------------------------------------------------------------

/// Run one operation with the pass held at the commit boundary, letting `mutate`
/// commit on a second connection in the gap, and return the outcome.
///
/// The pass is held with its commit transaction open and holding nothing — see
/// [`PausePoint::RevisionEntityRead`] — so the mutation is a real concurrent
/// commit rather than an injected write.
async fn admit_with_a_mutation_in_the_gap<F, Fut>(
    db: &Provider,
    settings: WorkerSettings,
    operation_id: Uuid,
    mutate: F,
) -> OperationOutcome
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    let (paused, reached, resume) = PausingStores::new(PausePoint::RevisionEntityRead);
    let ports: Arc<dyn Stores> = paused;
    let provider = worker(db);
    let pass = tokio::spawn(async move {
        run_operation(
            &ports,
            &provider,
            &allow_all(),
            &common::limits(),
            &settings,
            operation_id,
            LATER,
        )
        .await
    });

    reached.await.expect("the pass must reach the commit");
    mutate().await;
    resume.send(()).expect("the pass must still be waiting");

    pass.await
        .expect("the pass task must not panic")
        .expect("the worker must not fail on infrastructure")
}

/// The criterion: a dependency mutated between evaluation and commit causes
/// exactly one rollback and one successful retry.
///
/// "One rollback" is read off the versions: the candidate lands at
/// `resource_version` 2 and revision 2, which a second commit would have made 3.
/// "Successful retry" is read off the artifacts: the committed resolution inlines
/// the base's **new** property, which only a fresh evaluation could have produced —
/// the rolled-back attempt was holding a resolution of the old one.
#[tokio::test]
async fn a_dependency_mutated_between_evaluation_and_commit_costs_one_rollback_and_one_retry() {
    let dir = TestDir::new("types-registry-reval-retry");
    let db = test_db_file(&dir.path().join("registry.db")).await;
    seed_base_and_dependents(&db).await;

    let operation_id = submit(&db, "k-ref-2", REFERRER, referencing_schema("tag"), Some(1))
        .await
        .expect("acceptance");

    let mutating = Arc::clone(&db);
    let outcome = admit_with_a_mutation_in_the_gap(
        &db,
        worker_settings(),
        operation_id,
        move || async move {
            admit(&mutating, "k-base-2", BASE, base_schema("label"), Some(1)).await;
        },
    )
    .await;

    let item = &outcome.items[0];
    assert_eq!(
        item.status,
        OperationItemStatus::Succeeded,
        "the retry must succeed, got {item:?}"
    );
    assert_eq!(item.resource_version, Some(2));
    assert_eq!(
        item.revision_no,
        Some(2),
        "one revision, not two: the drifted attempt wrote nothing"
    );

    let after = current(&db, REFERRER).await;
    assert!(
        after.resolved_schema.contains("tag"),
        "the candidate's own change landed, got {}",
        after.resolved_schema
    );
    assert!(
        after.resolved_schema.contains("label"),
        "the retry re-resolved against the moved base; a committed stale evaluation \
         would still inline `name`, got {}",
        after.resolved_schema
    );
}

/// Exhaustion is terminal. With a budget of one attempt there is no retry to take,
/// so the single drift ends the item as `failed` under a reason an operator and a
/// client can both branch on.
#[tokio::test]
async fn exhausting_the_revalidation_budget_terminalizes_the_item_as_failed() {
    let dir = TestDir::new("types-registry-reval-exhausted");
    let db = test_db_file(&dir.path().join("registry.db")).await;
    seed_base_and_dependents(&db).await;

    let operation_id = submit(&db, "k-ref-2", REFERRER, referencing_schema("tag"), Some(1))
        .await
        .expect("acceptance");

    let mutating = Arc::clone(&db);
    let outcome = admit_with_a_mutation_in_the_gap(
        &db,
        WorkerSettings {
            max_revalidation_attempts: 1,
            ..WorkerSettings::default()
        },
        operation_id,
        move || async move {
            admit(&mutating, "k-base-2", BASE, base_schema("label"), Some(1)).await;
        },
    )
    .await;

    let item = &outcome.items[0];
    assert_eq!(
        item.status,
        OperationItemStatus::Failed,
        "one attempt and one drift is exhaustion, got {item:?}"
    );
    let failure = item.failure.as_ref().expect("a recorded failure");
    assert_eq!(failure.reason, "revalidation_exhausted");
    assert!(
        failure.message.contains(BASE),
        "the message names the last drift, got {}",
        failure.message
    );
    assert_eq!(
        entity(&db, REFERRER).await.resource_version,
        1,
        "an exhausted item wrote nothing"
    );
}

// ---------------------------------------------------------------------------
// Two pods, one database
// ---------------------------------------------------------------------------

fn service(db: &Provider) -> RegistryService {
    let dispatch: Arc<dyn OperationDispatch> = Arc::new(NoDispatch);
    RegistryService::new(
        db.db(),
        stores(),
        RegistrationPolicy::default(),
        TypesRegistryConfig::default(),
        dispatch,
        AdmissionMode::Inline,
    )
}

/// `nfr-multi-pod-correctness`: a commit on one pod is visible to the other's
/// **first** post-commit read.
///
/// Two pods are two `DBProvider`s with their own pools over one database file —
/// the same thing two processes have, minus the process boundary, which is what
/// this criterion is about: neither pod holds any state between calls, so there is
/// nothing to invalidate and nothing that could serve a stale answer. A
/// process-local snapshot is exactly what would fail here, and P0 has no
/// cross-pod invalidation channel to repair one (SPEC §8.2).
#[tokio::test]
async fn a_commit_on_one_pod_is_visible_to_the_others_first_read() -> Result<(), ServiceError> {
    let dir = TestDir::new("types-registry-two-pods");
    let path = dir.path().join("registry.db");
    let pod_a = test_db_file(&path).await;
    let pod_b = test_db_file(&path).await;

    // B looks first, so its miss is a read it actually performed rather than an
    // absence it never asked about.
    let key = EntityKey::parse(BASE);
    assert!(
        service(&pod_b).entity(&key).await?.is_none(),
        "nothing is admitted yet"
    );

    admit(&pod_a, "k-base", BASE, base_schema("name"), None).await;

    let first_read = service(&pod_b)
        .entity(&key)
        .await?
        .expect("B's first read after A's commit must see it");
    assert_eq!(first_read.resource_version, 1);

    // And a revision on A is visible to B just the same: the read is a `SELECT`,
    // not a snapshot, so there is no second thing to invalidate.
    admit(&pod_a, "k-base-2", BASE, base_schema("label"), Some(1)).await;
    let second_read = service(&pod_b)
        .entity(&key)
        .await?
        .expect("the entity is still there");
    assert_eq!(second_read.resource_version, 2);
    assert_eq!(
        second_read.content,
        Some(base_schema("label")),
        "B reads A's newest authored document"
    );
    Ok(())
}
