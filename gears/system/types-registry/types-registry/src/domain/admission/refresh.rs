//! Reverse-impact refresh: after a revision lands, every dependent's effective
//! artifacts are re-materialized in the **same** transaction (SPEC §8.1 step 4.6,
//! DESIGN §4 — D5).
//!
//! A dependent's authored document did not change, so it gets no revision and its
//! `entity.resource_version` does not move — that column is reserved for
//! optimistic writes. What moves is `type_schema`'s three artifacts and their
//! `resolution_fingerprint`, which is why the fingerprint supports equality and
//! never ordering (`database.sql`).
//!
//! # Why the traversal is a recursive CTE and the refresh is a loop
//!
//! `DependencyRepo::reverse_impact` answers *who depends on this* in one
//! statement — a pure reachability question SQL is good at, and one round trip
//! matters because this runs while the commit holds the candidate's row.
//!
//! The refresh itself cannot be pushed down with it. Which dependents are
//! **written** is decided by recomputing each one and comparing digests, so the
//! set the loop produces is a function of the recomputation, not of the graph.
//! SPEC D5 originally read the two together and concluded "no CTE"; the CTE half
//! of that has been reversed (ADR-0001 makes a scoped `WITH RECURSIVE`
//! expressible through the typed builder), and the loop half stands for its own
//! reason, which was never portability.
//!
//! # The fingerprint-stability stop
//!
//! A dependent that recomputes to identical bytes is **not written**, and its
//! branch stops with it. Stopping the branch loses nothing: a dependent's
//! resolution consumes its own dependencies' *authored* documents, so if this one
//! did not move, nothing behind it can have moved either. What the loop does
//! differently from the pre-CTE worklist is *when* it learns that — the walk has
//! already returned the whole set, so a stable node costs one recomputation
//! instead of pruning the read. That is the price of the single round trip, and
//! the write set is identical either way.
//!
//! # This validates inside the commit transaction, deliberately
//!
//! `unit::evaluate` goes to great lengths to keep `gts-rust` validation *outside*
//! any transaction. This step cannot: "the dependents are current in the same
//! transaction as the new revision" is the criterion, and a refresh computed
//! before the commit would be computed against a base that the commit may not
//! write. The bound (`limits.activation_write_set`) is what keeps the work inside
//! the transaction finite.
//!
//! Inside is not *on*: the meta-compilation itself runs on the blocking pool via
//! `spawn_blocking` — the same offload `unit::evaluate` uses — with the owned
//! store and the whole subject list in one task, so the sum of all dependents'
//! compilations blocks no Tokio worker while the transaction stays open across
//! the await. Only the fingerprint reads and the writes remain on the async
//! path.

use std::collections::HashMap;

use time::OffsetDateTime;
use toolkit_db::DbTx;
use toolkit_db::secure::AccessScope;
use toolkit_macros::domain_model;

use super::errors::{ItemFailure, WorkerError};
use crate::domain::artifacts::{MaterializedArtifacts, materialize};
use crate::domain::enums::{EntityKind, LifecycleStatus};
use crate::domain::gts_store::{UnitDocument, load_unit_store};
use crate::domain::ports::{
    CurrentTypeSchemaRow, EntityRow, NewCurrentTypeSchema, ReverseImpact, Stores,
};

/// What one refresh wrote.
#[domain_model]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefreshOutcome {
    /// The dependents whose artifacts were rewritten, `gts_id`-sorted. Its length
    /// is the activation write set the bound governs.
    pub refreshed: Vec<String>,
    /// How many dependents were recomputed, including the ones that came out
    /// identical and were skipped. Reported so an operator can tell a large graph
    /// from a large *change*.
    pub examined: usize,
}

/// Re-materialize the effective artifacts of everything that transitively depends
/// on `roots`.
///
/// `roots` are the entity ids the commit just revised, and they are refreshed by
/// the commit itself — the reverse read excludes them.
///
/// # Errors
/// [`WorkerError`] for an infrastructure failure. A candidate-level refusal — the
/// write set over its bound, or a dependent that no longer validates — is an
/// [`ItemFailure`] in the `Ok(Err(..))` position, and the caller's transaction
/// rolls the whole commit back on it.
pub async fn refresh_dependents(
    stores: &dyn Stores,
    tx: &DbTx<'_>,
    scope: &AccessScope,
    roots: &[i64],
    write_set_bound: usize,
    now: OffsetDateTime,
) -> Result<Result<RefreshOutcome, ItemFailure>, WorkerError> {
    let dependents = match stores
        .reverse_impact(tx, scope, roots, write_set_bound)
        .await?
    {
        ReverseImpact::Within(rows) => rows,
        // The read logged the roots and the size it reached; the refusal carries
        // the operator-facing numbers and the machine reason a client branches on.
        ReverseImpact::OverBound { at_least, bound } => {
            return Ok(Err(ItemFailure::new(
                "activation_write_set_exceeded",
                format!(
                    "this revision would refresh at least {at_least} dependents, over the \
                     configured activation write set bound of {bound}; nothing was committed"
                ),
            )));
        }
    };

    // Instances carry no materialized artifacts — `instance` holds a revision
    // pointer and nothing else — so the walk reaches them and there is nothing to
    // recompute. Tombstones are skipped for a different reason: a withdrawn entity
    // has no current artifacts anyone reads, and rewriting them would resurrect a
    // read the deletion retired.
    let subjects: Vec<EntityRow> = dependents
        .into_iter()
        .filter(|row| {
            row.entity_kind == EntityKind::TypeSchema
                && row.lifecycle_status != LifecycleStatus::Deleted
        })
        .collect();
    if subjects.is_empty() {
        return Ok(Ok(RefreshOutcome::default()));
    }

    // One store for the whole write set rather than one per dependent: they share
    // most of their closure, and each build is a closure read plus two document
    // reads. Their own stored documents go in as the overlay, so what the store
    // resolves is exactly what is committed — including the revision this commit
    // just wrote for the root, which is read from the database and is *not*
    // overlaid.
    let mut documents = Vec::with_capacity(subjects.len());
    let subject_ids: Vec<i64> = subjects.iter().map(|row| row.id).collect();
    let mut raw: HashMap<i64, String> = stores
        .current_documents(tx, scope, &subject_ids)
        .await?
        .into_iter()
        .map(|doc| (doc.entity_id, doc.raw_schema))
        .collect();
    for row in &subjects {
        let Some(text) = raw.remove(&row.id) else {
            return Err(WorkerError::CurrentStateMissing {
                gts_id: row.gts_id.clone(),
                entity_id: row.id,
            });
        };
        let content = serde_json::from_str(&text).map_err(|source| {
            WorkerError::StoreBuild(crate::domain::gts_store::StoreBuildError::Content {
                gts_id: row.gts_id.clone(),
                source,
            })
        })?;
        documents.push(UnitDocument {
            gts_id: row.gts_id.clone(),
            content,
        });
    }

    let mut store = load_unit_store(stores, tx, scope, documents)
        .await
        .map_err(WorkerError::StoreBuild)?;

    // One blocking task recomputes every dependent, not one await per subject:
    // the CPU-bound meta-compilation of up to `write_set_bound` dependents must
    // block no Tokio worker while the commit transaction is open across the
    // await. The store is owned and closed — all database reads finished above
    // — so the task needs nothing from this future but the subjects themselves.
    let blocking_subjects: Vec<(i64, String)> = subjects
        .iter()
        .map(|row| (row.id, row.gts_id.clone()))
        .collect();
    let recomputed = tokio::task::spawn_blocking(move || {
        let mut artifacts = Vec::with_capacity(blocking_subjects.len());
        for (entity_id, gts_id) in blocking_subjects {
            let resolved = store.store_mut().validate_schema(&gts_id);
            let resolved = match resolved {
                Ok(resolved) => resolved,
                Err(error) => {
                    return Err(DependentInvalid {
                        entity_id,
                        gts_id,
                        error: error.to_string(),
                    });
                }
            };
            artifacts.push((entity_id, gts_id, materialize(&resolved)));
        }
        Ok(artifacts)
    })
    .await
    .map_err(WorkerError::EvaluationTask)?;
    let recomputed: Vec<(i64, String, MaterializedArtifacts)> = match recomputed {
        Ok(recomputed) => recomputed,
        // Committed content that no longer resolves against the new revision.
        // A candidate-level refusal, not infrastructure: the *candidate* is what
        // broke it, and retrying would break it identically. The compatibility
        // gate that refuses such a revision before it is written is T17–T19;
        // until then this is the backstop that keeps the registry consistent.
        //
        // The dependent's identifier and the raw validation error are operator
        // knowledge, logged here — the refusal the caller reads names only the
        // candidate, because the dependent is a third party that depends on it
        // and its identifier is not the submitter's to learn (the
        // non-disclosure rule `WorkerError::DependencyTargetAbsent` states at
        // the REST boundary).
        Err(refusal) => {
            tracing::warn!(
                gts_id = %refusal.gts_id,
                entity_id = refusal.entity_id,
                error = %refusal.error,
                "types_registry dependent no longer validates against a new revision"
            );
            return Ok(Err(ItemFailure::new(
                "dependent_invalid",
                "a dependent of this candidate no longer validates against this \
                 revision; nothing was committed"
                    .to_owned(),
            )));
        }
    };

    // Every fingerprint in one batched read — one round trip, not one per
    // dependent, because each extra statement is time the transaction holds the
    // candidate's row and the version-family lock, which is the window sibling
    // admissions of the same family block in (`Stores::current_schemas` exists
    // for exactly this).
    //
    // **After the recomputation, not before it.** A commit transaction runs
    // `READ COMMITTED` (`ports::commit_write`) and nothing here locks a
    // *dependent* — the family lock covers the candidate's family, and step
    // 4.3's vector guard has already run — so a dependent's own revision can
    // commit while this pass computes. `update_current` is keyed on `entity_id`
    // alone, so a row read before a long meta-compilation and written after it
    // would restore that row's stale `revision_no` over the newer revision, or
    // skip the refresh on a fingerprint that is no longer current. Reading here
    // leaves only the write loop between the read and the write, with no
    // CPU-bound work in between.
    let mut current: HashMap<i64, CurrentTypeSchemaRow> = stores
        .current_schemas(tx, scope, &subject_ids)
        .await?
        .into_iter()
        .map(|row| (row.entity_id, row))
        .collect();

    let mut refreshed = Vec::new();
    for (entity_id, gts_id, artifacts) in recomputed {
        let current =
            current
                .remove(&entity_id)
                .ok_or_else(|| WorkerError::CurrentStateMissing {
                    gts_id: gts_id.clone(),
                    entity_id,
                })?;
        if current.resolution_fingerprint == artifacts.resolution_fingerprint {
            continue;
        }

        // The dependent's own revision pointer is carried over unchanged: this is a
        // read-side change to a document nobody re-authored.
        if !stores
            .update_current_schema(
                tx,
                scope,
                NewCurrentTypeSchema {
                    entity_id,
                    revision_no: current.revision_no,
                    resolved_schema: artifacts.resolved_schema,
                    effective_traits: artifacts.effective_traits,
                    effective_traits_schema: artifacts.effective_traits_schema,
                    resolution_fingerprint: artifacts.resolution_fingerprint,
                    now,
                },
            )
            .await?
        {
            return Err(WorkerError::CurrentStateMissing {
                gts_id: gts_id.clone(),
                entity_id,
            });
        }
        refreshed.push(gts_id);
    }

    Ok(Ok(RefreshOutcome {
        refreshed,
        examined: subjects.len(),
    }))
}

/// The blocking task's refusal: which dependent stopped validating, and why.
///
/// Operator-side only — see the refusal site for why none of it reaches the
/// caller.
struct DependentInvalid {
    entity_id: i64,
    gts_id: String,
    error: String,
}
