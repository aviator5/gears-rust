//! Dependency edges written by a real admission (T13).
//!
//! The extractor's own rules are pinned beside `domain/dependency.rs`, without a
//! database. What only an admission can show is here: that the rows land, that they
//! carry the entity ids the identifiers name, that a re-admission **replaces** the
//! set rather than adding to it, and that a target no entity carries writes no row.
//!
//! Every test calls the worker directly, as `admission_worker_test.rs` does: no
//! `sleep`, no timer, no polling (SPEC §13).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::secure::SecureEntityExt;
use toolkit_db::{DBProvider, DbError, DbTx};
use toolkit_gts::gts_id;
use uuid::Uuid;

use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::acceptance::{AcceptanceContext, AcceptanceError, accept};
use types_registry::domain::admission::worker::{OperationOutcome, WorkerError};
use types_registry::domain::admission::{Candidate, OperationDispatch, SubmitRequest};
use types_registry::domain::enums as domain_enums;
use types_registry::domain::enums::DependencyKind;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::infra::storage::entity::dependency;
use types_registry::infra::storage::repo::EntityRepo;

mod common;
use common::{allow_all, run_operation, stores, test_db};

/// Every identifier here lives in its own `dep` module, and deliberately so: the
/// family advisory lock's `SQLite` scope is keyed on the DSN, which every test binding
/// `sqlite::memory:` shares through a cross-process marker directory. Two binaries
/// admitting **one family key** therefore contend across processes, and a test
/// probing that lock with no retry budget (`family_test`) sees it held by a
/// neighbour. Distinct family keys cost nothing and remove the interaction.
const NOW: OffsetDateTime = datetime!(2026-08-18 09:15:30 UTC);
const LATER: OffsetDateTime = datetime!(2026-08-18 10:20:40 UTC);

/// The base of the derivation chain, and the conforming type of the Instance.
const BASE: &str = gts_id!("cf.core.dep.thing.v1~");
/// Derived from [`BASE`] — so its derivation edge is a fact about its identifier.
const DERIVED: &str = gts_id!("cf.core.dep.thing.v1~cf.core.dep.leaf.v1~");
/// A schema **outside** every chain here: reachable only through a `$ref`.
const SHAPE: &str = gts_id!("cf.core.dep.shape.v1~");
/// A second out-of-chain schema, so a revision can swap one reference for another.
const INVOICE: &str = gts_id!("cf.core.dep.invoice.v1~");
/// The target of an `x-gts-ref` constraint.
const ROLE: &str = gts_id!("cf.core.dep.role.v1~");
/// A major-0 identifier used only as an `x-gts-ref` instance-value constraint.
const UNSTABLE: &str = gts_id!("cf.core.dep.unstable.v0~");
/// An Instance of [`BASE`].
const INSTANCE: &str = gts_id!("cf.core.dep.thing.v1~cf.core.dep.first.v1");
/// An identifier no test here ever admits.
const ABSENT: &str = gts_id!("cf.core.dep.ghost.v1~");

struct NoDispatch;

#[async_trait::async_trait]
impl OperationDispatch for NoDispatch {
    async fn enqueue(&self, _tx: &DbTx<'_>, _operation_id: Uuid) -> anyhow::Result<()> {
        Ok(())
    }
}

fn worker(db: &Arc<DBProvider<DbError>>) -> DBProvider<WorkerError> {
    DBProvider::new(db.db())
}

/// A plain schema with one string property.
fn schema(id: &str) -> Value {
    json!({
        "$id": format!("gts://{id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "name": { "type": "string" } },
    })
}

/// A schema whose body carries whatever a case needs beside its identity.
fn schema_with(id: &str, properties: &Value) -> Value {
    json!({
        "$id": format!("gts://{id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": properties.clone(),
    })
}

async fn submit(
    db: &Arc<DBProvider<DbError>>,
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

/// Submit one candidate and run its admission to terminality.
async fn admit(
    db: &Arc<DBProvider<DbError>>,
    key: &str,
    gts_id: &str,
    content: Value,
    expected_resource_version: Option<i64>,
) -> OperationOutcome {
    let op = submit(db, key, gts_id, content, expected_resource_version)
        .await
        .expect("accepted");
    let outcome = run_operation(
        &stores(),
        &worker(db),
        &allow_all(),
        &common::limits(),
        op,
        LATER,
    )
    .await
    .expect("the worker itself must not fail");
    assert_eq!(
        outcome.items[0].status,
        domain_enums::OperationItemStatus::Succeeded,
        "{gts_id} must be admitted: {:?}",
        outcome.items[0].failure,
    );
    outcome
}

/// Submit and admit without asserting the outcome — for the cases whose claim *is*
/// the refusal. `admit` asserts success and cannot express one.
async fn try_admit(
    db: &Arc<DBProvider<DbError>>,
    key: &str,
    gts_id: &str,
    content: Value,
    expected_resource_version: Option<i64>,
) -> OperationOutcome {
    let op = submit(db, key, gts_id, content, expected_resource_version)
        .await
        .expect("accepted");
    run_operation(
        &stores(),
        &worker(db),
        &allow_all(),
        &common::limits(),
        op,
        LATER,
    )
    .await
    .expect("the worker itself must not fail")
}

async fn entity_id_of(db: &Arc<DBProvider<DbError>>, gts_id: &str) -> i64 {
    let provider = worker(db);
    let conn = provider.conn().expect("conn");
    EntityRepo::find_by_gts_id(&conn, &allow_all(), gts_id)
        .await
        .expect("read")
        .unwrap_or_else(|| panic!("{gts_id} has no entity row"))
        .id
}

/// One entity's outgoing edges as `(kind, target identifier)`, sorted — the shape a
/// test can state without knowing any row id.
async fn outgoing(db: &Arc<DBProvider<DbError>>, gts_id: &str) -> Vec<(DependencyKind, String)> {
    let provider = worker(db);
    let conn = provider.conn().expect("conn");
    let scope = allow_all();
    let from = entity_id_of(db, gts_id).await;

    let rows = dependency::Entity::find()
        .filter(dependency::Column::FromEntityId.eq(from))
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("edges");

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let target = EntityRepo::find_by_ids(&conn, &scope, &[row.to_entity_id])
            .await
            .expect("read")
            .pop()
            .expect("an edge target row exists: the foreign key says so");
        out.push((row.kind.into(), target.gts_id));
    }
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// The edge kinds, written by one admission each
// ---------------------------------------------------------------------------

/// A derived schema that also references out of its chain carries both kinds a
/// schema can have — and the `$ref` to its base and the derivation from it point at
/// the same entity by different relations, which the primary key `(from, kind, to)`
/// keeps apart. The `x-gts-ref` beside them adds nothing because it is an
/// instance-value constraint rather than a dependency.
#[tokio::test]
async fn one_admission_writes_a_row_per_edge_kind() {
    let db = test_db().await;
    admit(&db, "base", BASE, schema(BASE), None).await;
    admit(&db, "shape", SHAPE, schema(SHAPE), None).await;
    admit(&db, "role", ROLE, schema(ROLE), None).await;

    let derived = json!({
        "$id": format!("gts://{DERIVED}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "allOf": [
            { "$ref": format!("gts://{BASE}") },
            {
                "type": "object",
                "properties": {
                    "shape": { "$ref": format!("gts://{SHAPE}") },
                    "role": { "type": "string", "x-gts-ref": ROLE },
                },
            },
        ],
    });
    admit(&db, "derived", DERIVED, derived, None).await;

    assert_eq!(
        outgoing(&db, DERIVED).await,
        vec![
            (DependencyKind::SchemaRef, BASE.to_owned()),
            (DependencyKind::Derivation, BASE.to_owned()),
            (DependencyKind::SchemaRef, SHAPE.to_owned()),
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>(),
        "the `$ref` to the base and the derivation from it are two relations, not one; \
         the `x-gts-ref` to ROLE is neither",
    );
}

/// Conformance is materialized even though the identifier implies it: T14's reverse
/// walk has no identifier to walk backwards from.
#[tokio::test]
async fn an_instance_admission_writes_its_conformance_edge() {
    let db = test_db().await;
    admit(&db, "base", BASE, schema(BASE), None).await;
    admit(&db, "instance", INSTANCE, json!({ "name": "first" }), None).await;

    assert_eq!(
        outgoing(&db, INSTANCE).await,
        vec![(DependencyKind::InstanceOf, BASE.to_owned())],
    );
}

/// A schema with no references and no ancestor has no outgoing rows at all — the
/// case a "write something for every admission" implementation would get wrong.
#[tokio::test]
async fn a_reference_free_first_generation_schema_writes_no_edge() {
    let db = test_db().await;
    admit(&db, "base", BASE, schema(BASE), None).await;

    assert!(outgoing(&db, BASE).await.is_empty());
}

// ---------------------------------------------------------------------------
// A revision replaces the set
// ---------------------------------------------------------------------------

/// The reason the write is a *replace*: a reference the new revision dropped must
/// stop being an edge, or a deletion the graph should permit would stay blocked
/// forever.
#[tokio::test]
async fn a_revision_removes_the_edge_it_dropped_and_adds_the_one_it_gained() {
    let db = test_db().await;
    admit(&db, "shape", SHAPE, schema(SHAPE), None).await;
    admit(&db, "invoice", INVOICE, schema(INVOICE), None).await;
    admit(
        &db,
        "first",
        BASE,
        schema_with(
            BASE,
            &json!({ "shape": { "$ref": format!("gts://{SHAPE}") } }),
        ),
        None,
    )
    .await;
    assert_eq!(
        outgoing(&db, BASE).await,
        vec![(DependencyKind::SchemaRef, SHAPE.to_owned())],
    );

    admit(
        &db,
        "second",
        BASE,
        schema_with(
            BASE,
            &json!({ "invoice": { "$ref": format!("gts://{INVOICE}") } }),
        ),
        Some(1),
    )
    .await;

    assert_eq!(
        outgoing(&db, BASE).await,
        vec![(DependencyKind::SchemaRef, INVOICE.to_owned())],
        "the dropped reference must be gone, not merely joined by the new one",
    );
}

/// A revision whose references did not change writes the same set again. Stated
/// because "replace" could otherwise be read as "clear and forget".
#[tokio::test]
async fn a_revision_that_keeps_its_reference_keeps_the_edge() {
    let db = test_db().await;
    admit(&db, "shape", SHAPE, schema(SHAPE), None).await;
    let referencing = |title: &str| {
        json!({
            "$id": format!("gts://{BASE}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": title,
            "type": "object",
            "properties": { "shape": { "$ref": format!("gts://{SHAPE}") } },
        })
    };
    admit(&db, "first", BASE, referencing("first"), None).await;
    admit(&db, "second", BASE, referencing("second"), Some(1)).await;

    assert_eq!(
        outgoing(&db, BASE).await,
        vec![(DependencyKind::SchemaRef, SHAPE.to_owned())],
    );
}

// ---------------------------------------------------------------------------
// Targets that name nothing
// ---------------------------------------------------------------------------

/// An `x-gts-ref` writes no row whether or not its target exists, and admits either
/// way: `gts-rust` enforces the keyword by matching the value string against the
/// pattern and never consults the registry. Both halves are asserted
/// together, because "no row" would otherwise be indistinguishable from "the target
/// happened to be missing".
#[tokio::test]
async fn an_x_gts_ref_writes_no_row_whether_its_target_exists_or_not() {
    let db = test_db().await;
    admit(&db, "role", ROLE, schema(ROLE), None).await;

    for (key, id, target) in [("present", SHAPE, ROLE), ("absent", INVOICE, ABSENT)] {
        admit(
            &db,
            key,
            id,
            schema_with(
                id,
                &json!({ "role": { "type": "string", "x-gts-ref": target } }),
            ),
            None,
        )
        .await;
        assert!(
            outgoing(&db, id).await.is_empty(),
            "{id} constrains a value to name {target}; that is not a dependency",
        );
    }

    // And the constrained target keeps no incoming row either, so a deletion of it
    // has nothing to be blocked by — the point of the decision.
    let provider = worker(&db);
    let conn = provider.conn().expect("conn");
    let rows = dependency::Entity::find()
        .secure()
        .scope_with(&allow_all())
        .all(&conn)
        .await
        .expect("edges");
    assert!(rows.is_empty(), "no edge of any kind was written: {rows:?}");
}

/// Major-0 quarantine applies to resolution-bearing references and derivation, not
/// to an `x-gts-ref` constraint that only matches an identifier string in a payload.
#[tokio::test]
async fn a_stable_schema_may_use_an_x_gts_ref_that_names_major_zero() {
    let db = test_db().await;
    for (key, id, pattern) in [
        ("stable-x-gts-ref-v0-exact", INVOICE, UNSTABLE.to_owned()),
        ("stable-x-gts-ref-v0-pattern", SHAPE, format!("{UNSTABLE}*")),
    ] {
        admit(
            &db,
            key,
            id,
            schema_with(
                id,
                &json!({ "role": { "type": "string", "x-gts-ref": pattern } }),
            ),
            None,
        )
        .await;

        assert!(
            outgoing(&db, id).await.is_empty(),
            "an x-gts-ref naming major zero is not a dependency",
        );
    }
}

/// The mirror of the case above: an unresolvable `$ref` is a content failure, and it
/// writes nothing at all — no entity, so no edges either.
#[tokio::test]
async fn a_ref_naming_no_entity_fails_the_candidate() {
    let db = test_db().await;
    let op = submit(
        &db,
        "base",
        BASE,
        schema_with(
            BASE,
            &json!({ "ghost": { "$ref": format!("gts://{ABSENT}") } }),
        ),
        None,
    )
    .await
    .expect("accepted");
    let outcome = run_operation(
        &stores(),
        &worker(&db),
        &allow_all(),
        &common::limits(),
        op,
        LATER,
    )
    .await
    .expect("the worker itself must not fail");

    let item = &outcome.items[0];
    assert_eq!(item.status, domain_enums::OperationItemStatus::Failed);
    assert_eq!(
        item.failure.as_ref().map(|f| f.reason.as_ref()),
        Some("invalid_schema"),
    );

    let provider = worker(&db);
    let conn = provider.conn().expect("conn");
    let rows = dependency::Entity::find()
        .secure()
        .scope_with(&allow_all())
        .all(&conn)
        .await
        .expect("edges");
    assert!(rows.is_empty(), "a refused candidate writes no edge");
}

// ---------------------------------------------------------------------------
// the relation is acyclic, and this is where that is enforced
// ---------------------------------------------------------------------------

/// **The acyclicity invariant (ADR-0012).** A revision that would close a `$ref`
/// cycle is refused, so no cyclic edge is ever written.
///
/// The reason is not a registry policy but the resolution semantics: an effective
/// artifact is materialized by *inlining* the referenced schema, and inlining has no
/// fixpoint over a cycle — there is no resolved form to store or serve. `gts-rust`
/// reaches that conclusion during validation and reports it as `invalid_schema`.
///
/// Asserted here rather than assumed anywhere else: every traversal over
/// `dependency` is written to survive a cyclic row (`repo_test.rs`,
/// `dependency_repo_test.rs`), and this is the test that says such a row cannot come
/// from admission. The in-batch half of the claim — two candidates referencing each
/// other in one operation, where the overlay makes both visible — arrives with batch
/// admission (T19).
#[tokio::test]
async fn a_revision_that_would_close_a_ref_cycle_is_refused() {
    let db = test_db().await;
    // SHAPE stands alone; INVOICE references it. Both admit.
    admit(&db, "k-shape", SHAPE, schema(SHAPE), None).await;
    admit(
        &db,
        "k-invoice",
        INVOICE,
        schema_with(
            INVOICE,
            &json!({ "shape": { "$ref": format!("gts://{SHAPE}") } }),
        ),
        None,
    )
    .await;

    // Now revise SHAPE to reference INVOICE, which would close SHAPE -> INVOICE ->
    // SHAPE.
    let outcome = try_admit(
        &db,
        "k-shape-2",
        SHAPE,
        schema_with(
            SHAPE,
            &json!({ "invoice": { "$ref": format!("gts://{INVOICE}") } }),
        ),
        Some(1),
    )
    .await;

    let item = &outcome.items[0];
    assert_eq!(
        item.status,
        domain_enums::OperationItemStatus::Failed,
        "closing a reference cycle must be refused, got {item:?}"
    );
    let failure = item
        .failure
        .as_ref()
        .expect("a failed item names its reason");
    assert_eq!(failure.reason, "invalid_schema");
    assert!(
        failure.message.to_lowercase().contains("circular"),
        "the refusal must name the cycle, got {}",
        failure.message
    );

    // And the relation is unchanged: SHAPE still has no outgoing edge.
    assert!(
        outgoing(&db, SHAPE).await.is_empty(),
        "a refused revision writes no edge"
    );
    assert_eq!(
        outgoing(&db, INVOICE).await,
        vec![(DependencyKind::SchemaRef, SHAPE.to_owned())],
        "the one direction that does resolve is still the only edge"
    );
}
