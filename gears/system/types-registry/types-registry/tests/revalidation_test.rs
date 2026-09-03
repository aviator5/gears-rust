//! Commit-time revision-vector guard and bounded revalidation loop.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]
#![recursion_limit = "256"]

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
use types_registry::domain::admission::worker::{
    OperationOutcome, Tuning, WorkerError, run_operation,
};
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

const BASE: &str = gts_id!("cf.core.reval.thing.v1~");
const DERIVED: &str = gts_id!("cf.core.reval.thing.v1~cf.core.reval.leaf.v1~");
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
            metrics: &common::metrics(),
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
        Tuning {
            limits: &common::limits(),
            worker: &worker_settings(),
            metrics: &common::metrics(),
        },
        operation_id,
        LATER,
    )
    .await
    .expect("the worker must not fail on infrastructure")
}

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
                    &common::metrics(),
                )
                .await
            })
        })
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

async fn seed_base_and_dependents(db: &Provider) {
    admit(db, "k-base", BASE, base_schema("name"), None).await;
    admit(db, "k-derived", DERIVED, derived_schema(), None).await;
    admit(db, "k-referrer", REFERRER, referencing_schema("note"), None).await;
}

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

#[tokio::test]
async fn a_dependency_that_moved_after_evaluation_rolls_the_commit_back() {
    let db = test_db().await;
    seed_base_and_dependents(&db).await;

    let unit = evaluated(&db, "k-ref-2", REFERRER, referencing_schema("tag"), Some(1)).await;
    let before = entity(&db, REFERRER).await;

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

#[tokio::test]
async fn a_phantom_dependent_created_after_the_scan_is_detected() {
    let db = test_db().await;
    admit(&db, "k-base", BASE, base_schema("name"), None).await;

    let unit = evaluated(&db, "k-base-2", BASE, base_schema("label"), Some(1)).await;
    assert!(
        unit.vector.entries().is_empty(),
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

#[tokio::test]
async fn a_dependent_refreshed_after_the_scan_is_detected() {
    let db = test_db().await;
    admit(&db, "k-base", BASE, base_schema("name"), None).await;
    admit(&db, "k-derived", DERIVED, derived_schema(), None).await;

    let unit = evaluated(&db, "k-base-2", BASE, base_schema("label"), Some(1)).await;
    let recorded = unit
        .vector
        .entries()
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
            Tuning {
                limits: &common::limits(),
                worker: &settings,
                metrics: &common::metrics(),
            },
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

fn service(db: &Provider) -> RegistryService {
    let dispatch: Arc<dyn OperationDispatch> = Arc::new(NoDispatch);
    RegistryService::new(
        db.db(),
        stores(),
        RegistrationPolicy::default(),
        TypesRegistryConfig::default(),
        dispatch,
        AdmissionMode::Inline,
        common::metrics(),
    )
}

#[tokio::test]
async fn a_commit_on_one_pod_is_visible_to_the_others_first_read() -> Result<(), ServiceError> {
    let dir = TestDir::new("types-registry-two-pods");
    let path = dir.path().join("registry.db");
    let pod_a = test_db_file(&path).await;
    let pod_b = test_db_file(&path).await;

    // B looks first, so its miss is a read it actually performed rather than an absence it never
    // asked about.
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

    // And a revision on A is visible to B just the same: the read is a `SELECT`, not a snapshot, so
    // there is no second thing to invalidate.
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
