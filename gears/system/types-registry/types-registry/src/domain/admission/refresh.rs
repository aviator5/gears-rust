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

use std::collections::HashMap;

use time::OffsetDateTime;
use toolkit_db::DbTx;
use toolkit_db::secure::AccessScope;
use toolkit_macros::domain_model;

use super::errors::{ItemFailure, WorkerError};
use crate::domain::artifacts::materialize;
use crate::domain::enums::{EntityKind, LifecycleStatus};
use crate::domain::gts_store::{UnitDocument, load_unit_store};
use crate::domain::ports::{EntityRow, NewCurrentTypeSchema, ReverseImpact, Stores};

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

    let mut refreshed = Vec::new();
    for row in &subjects {
        let resolved = match store.store_mut().validate_schema(&row.gts_id) {
            Ok(resolved) => resolved,
            // Committed content that no longer resolves against the new revision.
            // A candidate-level refusal, not infrastructure: the *candidate* is what
            // broke it, and retrying would break it identically. The compatibility
            // gate that refuses such a revision before it is written is T17–T19;
            // until then this is the backstop that keeps the registry consistent.
            Err(e) => {
                return Ok(Err(ItemFailure::new(
                    "dependent_invalid",
                    format!(
                        "dependent '{}' no longer validates against this revision: {e}",
                        row.gts_id
                    ),
                )));
            }
        };
        let artifacts = materialize(&resolved);

        // Read per dependent rather than batched, still: the write set is bounded
        // and small (measured max fan-out 27), and this read is interleaved with the
        // recomputation that decides whether to write. The batched port the earlier
        // note here called for now exists — `Stores::current_schemas`, which the
        // revision vector (T15) needed twice — so the change is available if the
        // bound is ever raised; it is not made here, where it would reorder the
        // transaction's reads and writes for nothing.
        let current = stores
            .find_current_schema(tx, scope, row.id)
            .await?
            .ok_or_else(|| WorkerError::CurrentStateMissing {
                gts_id: row.gts_id.clone(),
                entity_id: row.id,
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
                    entity_id: row.id,
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
                gts_id: row.gts_id.clone(),
                entity_id: row.id,
            });
        }
        refreshed.push(row.gts_id.clone());
    }

    Ok(Ok(RefreshOutcome {
        refreshed,
        examined: subjects.len(),
    }))
}
