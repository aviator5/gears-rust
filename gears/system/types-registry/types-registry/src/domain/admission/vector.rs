//! The revision vector: what evaluation's verdict rests on, and the commit-time
//! guard that refuses to write when any of it has moved (SPEC §8.1 steps 3 and
//! 4.3 — D4).
//!
//! # Why a guard rather than a lock
//!
//! `unit::evaluate` deliberately runs with **no transaction open** (§8.2): it
//! builds a transient store, resolves references and meta-compiles the schema
//! against rows it read a moment ago, and any of those rows may have moved since.
//! Nothing prevents that, and nothing should — holding read locks across
//! `gts-rust` validation is what the transient-store design exists to avoid.
//!
//! ponytail: ceiling C9 (SPEC §9) — the commit takes **no** entity or current row
//! locks, which DESIGN §4 step 2 and SPEC §8.1 step 4.2 as originally written both
//! asked for. `SecureSelect` exposes no `FOR UPDATE` and `SQLite` has no row
//! locking, so the only portable primitive is the advisory lock, and one per vector
//! member is up to `activation_write_set` round trips inside the commit
//! transaction. In their place: the compare-and-swap on the candidate's own row,
//! and the write-write conflict a dependency's own refresh creates on the
//! candidate's `type_schema` row — SPEC §8.1 step 4.2 records the argument and the
//! one window it leaves. Upgrade: a `FOR UPDATE` on `SecureSelect`, after which
//! step 4.2 is taken literally on the two backends that have row locks.
//!
//! What makes it *safe* is that the commit re-derives the same question inside its
//! own transaction and compares. Two identical vectors mean the evaluation was
//! computed against the state being committed against. One difference means it was
//! not, and the only sound answer is to throw the evaluation away and redo it —
//! which is why a drift travels as [`WorkerError::RevalidationRequired`] and rolls
//! the transaction back, rather than as an item failure. The candidate is not
//! wrong; this pass is stale.
//!
//! # What the vector carries, and why the fingerprint is not redundant
//!
//! A **dependency** contributes its `resource_version`. That is sufficient: what
//! evaluation consumed from a dependency is its *authored* document, and authored
//! content changes only through a revision, which moves `resource_version` by
//! construction (`unit::commit_revision`).
//!
//! A **dependent** contributes its `resolution_fingerprint` as well, and there the
//! version alone would be blind. A dependent refreshed by someone else's commit
//! gets new effective artifacts and **no** version move at all — that is the whole
//! shape of the reverse-impact refresh (T14, `admission::refresh`). The
//! fingerprint is the only column that moves, so it is the only column that can
//! detect it. This is SPEC's *"plus `resolution_fingerprint` where effective
//! content was consumed"*: the refresh consumes each dependent's effective
//! artifacts to decide whether to rewrite them.
//!
//! # Membership is re-derived, not just re-read
//!
//! Comparing versions of the entities evaluation happened to see would miss a
//! dependency that *appeared* — a transitive one pulled in because some
//! intermediate entity gained an edge — and a dependent that appeared, which is
//! the phantom-dependent case. So the commit re-runs both reads from the recorded
//! **roots** ([`RevisionVector::roots`]), which are a pure function of the
//! candidate's own document and therefore the same question both times, and
//! compares the two answers as sets.
//!
//! # Canonical order everywhere
//!
//! Entries are `gts_id`-sorted (SPEC §8.1 step 4.2's *"canonical identifier
//! order"*), which is what lets the comparison be one merge walk and what makes
//! the reported drift deterministic rather than dependent on row order.

use std::collections::HashMap;

use toolkit_db::DbTx;
use toolkit_db::secure::AccessScope;
use toolkit_macros::domain_model;

use super::errors::{ItemFailure, WorkerError};
use crate::domain::enums::{EntityKind, LifecycleStatus};
use crate::domain::ports::{EntityRow, ReverseImpact, Stores};

/// Which side of the candidate an entry stands on.
///
/// Part of the entry's identity rather than a decoration: the two roles are
/// compared on different columns, and a drift report that did not name the role
/// would leave an operator unable to tell "the base I validated against moved"
/// from "someone else's schema now depends on me".
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum VectorRole {
    /// Evaluation consumed this entity's authored document while building the
    /// transient store.
    Dependency,
    /// This entity depends on the candidate, so the commit's refresh consumes its
    /// effective artifacts.
    Dependent,
}

impl VectorRole {
    /// The word a drift message uses.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Dependency => "dependency",
            Self::Dependent => "dependent",
        }
    }
}

impl std::fmt::Display for VectorRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One entity as of the read that recorded it.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VectorEntry {
    pub gts_id: String,
    pub role: VectorRole,
    pub resource_version: i64,
    /// `Some` exactly where effective content was consumed — a live Type Schema
    /// dependent. `None` for a dependency (only its authored document was read),
    /// for an Instance dependent (`instance` carries no artifacts) and for a
    /// tombstone (the refresh skips both, `admission::refresh`).
    pub resolution_fingerprint: Option<Vec<u8>>,
}

/// The full vector, plus the roots it was derived from.
///
/// `gts_id`-sorted, and the candidate itself is **not** in it: the candidate's own
/// `resource_version` is the caller's optimistic precondition, rechecked by the
/// compare-and-swap that writes it.
#[domain_model]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RevisionVector {
    /// The closure roots — the candidate identifier plus its document's reference
    /// targets — carried so the commit asks the same question evaluation did. A
    /// pure function of the authored document, so re-deriving them would give the
    /// same answer; carried rather than recomputed because the commit holds the
    /// canonical body as text and re-parsing it under the entity lock is work the
    /// commit transaction should not do.
    pub roots: Vec<String>,
    /// One entry per dependency and dependent, `(gts_id, role)`-sorted.
    pub entries: Vec<VectorEntry>,
}

/// The first difference between a recorded vector and a freshly-derived one.
///
/// One difference, not all of them: any difference is already decisive, and the
/// first in canonical order is a stable thing to name in a log line.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VectorDrift {
    /// Not there when evaluation looked. The phantom-dependent case, and the
    /// transitive-dependency case.
    Appeared { gts_id: String, role: VectorRole },
    /// There when evaluation looked, gone now.
    Vanished { gts_id: String, role: VectorRole },
    /// A new revision landed: `resource_version` moved.
    Moved {
        gts_id: String,
        role: VectorRole,
        recorded: i64,
        found: i64,
    },
    /// Someone else's commit re-materialized this dependent's artifacts. The
    /// version stood still — see the module header on why the fingerprint is the
    /// only column that can show this.
    Refreshed { gts_id: String },
}

impl std::fmt::Display for VectorDrift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Appeared { gts_id, role } => {
                write!(f, "{role} '{gts_id}' appeared after evaluation")
            }
            Self::Vanished { gts_id, role } => {
                write!(f, "{role} '{gts_id}' disappeared after evaluation")
            }
            Self::Moved {
                gts_id,
                role,
                recorded,
                found,
            } => write!(
                f,
                "{role} '{gts_id}' moved from resource_version {recorded} to {found} after \
                 evaluation"
            ),
            Self::Refreshed { gts_id } => write!(
                f,
                "dependent '{gts_id}' had its effective artifacts refreshed after evaluation"
            ),
        }
    }
}

impl RevisionVector {
    /// The drift between this vector and a freshly-derived one, or `None` when the
    /// two agree.
    ///
    /// One merge walk over two sorted sequences. The sort key is `(gts_id, role)`,
    /// so an entity that changed role reports as a `Vanished` / `Appeared` pair and
    /// the first of them is returned — which is the honest description: the entity
    /// left one side of the candidate.
    #[must_use]
    pub fn drift(&self, fresh: &Self) -> Option<VectorDrift> {
        let mut recorded = self.entries.iter();
        let mut found = fresh.entries.iter();
        let mut left = recorded.next();
        let mut right = found.next();
        loop {
            match (left, right) {
                (None, None) => return None,
                (Some(a), None) => {
                    return Some(VectorDrift::Vanished {
                        gts_id: a.gts_id.clone(),
                        role: a.role,
                    });
                }
                (None, Some(b)) => {
                    return Some(VectorDrift::Appeared {
                        gts_id: b.gts_id.clone(),
                        role: b.role,
                    });
                }
                (Some(a), Some(b)) => match key(a).cmp(&key(b)) {
                    std::cmp::Ordering::Less => {
                        return Some(VectorDrift::Vanished {
                            gts_id: a.gts_id.clone(),
                            role: a.role,
                        });
                    }
                    std::cmp::Ordering::Greater => {
                        return Some(VectorDrift::Appeared {
                            gts_id: b.gts_id.clone(),
                            role: b.role,
                        });
                    }
                    std::cmp::Ordering::Equal => {
                        if let Some(drift) = moved(a, b) {
                            return Some(drift);
                        }
                        left = recorded.next();
                        right = found.next();
                    }
                },
            }
        }
    }
}

/// The sort and merge key. Borrowed, so the walk allocates nothing until it has a
/// drift to report.
fn key(entry: &VectorEntry) -> (&str, VectorRole) {
    (entry.gts_id.as_str(), entry.role)
}

/// The two column comparisons for one entity present on both sides.
///
/// The version is asked first: a revision moves both columns, and reporting it as
/// `Moved` names the cause rather than one of its effects.
fn moved(recorded: &VectorEntry, found: &VectorEntry) -> Option<VectorDrift> {
    if recorded.resource_version != found.resource_version {
        return Some(VectorDrift::Moved {
            gts_id: found.gts_id.clone(),
            role: found.role,
            recorded: recorded.resource_version,
            found: found.resource_version,
        });
    }
    if recorded.resolution_fingerprint != found.resolution_fingerprint {
        return Some(VectorDrift::Refreshed {
            gts_id: found.gts_id.clone(),
        });
    }
    None
}

/// Derive the vector for one candidate: its dependency closure and its
/// reverse-impact set, as the database holds them right now.
///
/// Reads the closure itself, which is what the commit needs. Evaluation has the
/// same row set already — `load_unit_store` walked it to build the store — and
/// calls [`derive_from`] with it rather than walking it twice in one transaction.
///
/// # Errors
/// As [`derive_from`].
pub async fn derive(
    stores: &dyn Stores,
    tx: &DbTx<'_>,
    scope: &AccessScope,
    candidate_gts_id: &str,
    roots: &[String],
    write_set_bound: usize,
) -> Result<Result<RevisionVector, ItemFailure>, WorkerError> {
    let closure = stores.closure(tx, scope, roots).await?;
    derive_from(
        stores,
        tx,
        scope,
        candidate_gts_id,
        roots,
        &closure.entities,
        write_set_bound,
    )
    .await
}

/// [`derive`] over a dependency closure the caller has already read.
///
/// The vector is built here on both sides — the recording one and the rechecking
/// one — because it *must* be the same function both times, or the comparison
/// would be measuring the difference between two readers rather than between two
/// states. Only who ran the closure walk differs, and that is the same repository
/// call in the same transaction either way.
///
/// The candidate's own entity row is excluded, and its id is taken from the
/// closure rather than read separately: `roots` always contains the candidate
/// identifier, so the closure already resolved it, or reported it missing for a
/// first admission.
///
/// # Errors
/// [`WorkerError`] for an infrastructure failure. A reverse-impact set over
/// `write_set_bound` is a candidate refusal in the `Ok(Err(..))` position under
/// the same `activation_write_set_exceeded` reason the refresh uses — reached
/// earlier here, before anything is written, which is strictly better and changes
/// nothing a client sees.
#[allow(clippy::too_many_arguments)]
pub async fn derive_from(
    stores: &dyn Stores,
    tx: &DbTx<'_>,
    scope: &AccessScope,
    candidate_gts_id: &str,
    roots: &[String],
    closure: &[EntityRow],
    write_set_bound: usize,
) -> Result<Result<RevisionVector, ItemFailure>, WorkerError> {
    let mut entries: Vec<VectorEntry> = Vec::with_capacity(closure.len());
    let mut candidate_id: Option<i64> = None;
    for row in closure {
        if row.gts_id == candidate_gts_id {
            candidate_id = Some(row.id);
            continue;
        }
        entries.push(VectorEntry {
            gts_id: row.gts_id.clone(),
            role: VectorRole::Dependency,
            resource_version: row.resource_version,
            resolution_fingerprint: None,
        });
    }

    // Empty for a creation, which has no row for anything to depend on: a `$ref`
    // to an absent target has no resolved form, so nothing can already point here.
    let dependent_roots: Vec<i64> = candidate_id.into_iter().collect();
    let dependents = match stores
        .reverse_impact(tx, scope, &dependent_roots, write_set_bound)
        .await?
    {
        ReverseImpact::Within(rows) => rows,
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

    // One batched read for every dependent that carries artifacts, rather than one
    // per row: at recheck time this runs inside the commit transaction.
    let artifact_bearing: Vec<i64> = dependents
        .iter()
        .filter(|row| {
            row.entity_kind == EntityKind::TypeSchema
                && row.lifecycle_status != LifecycleStatus::Deleted
        })
        .map(|row| row.id)
        .collect();
    let mut fingerprints: HashMap<i64, Vec<u8>> = stores
        .current_schemas(tx, scope, &artifact_bearing)
        .await?
        .into_iter()
        .map(|row| (row.entity_id, row.resolution_fingerprint))
        .collect();

    for row in &dependents {
        entries.push(VectorEntry {
            gts_id: row.gts_id.clone(),
            role: VectorRole::Dependent,
            resource_version: row.resource_version,
            // Absent for an Instance or a tombstone, which the refresh skips, and
            // absent for a Type Schema whose current row has gone missing — a
            // corruption the refresh reports by name. Recording `None` rather than
            // failing here keeps the guard from being the thing that reports it.
            resolution_fingerprint: fingerprints.remove(&row.id),
        });
    }

    entries.sort_by(|a, b| key(a).cmp(&key(b)));
    Ok(Ok(RevisionVector {
        roots: roots.to_vec(),
        entries,
    }))
}

/// Step 4.3: re-derive the vector inside the commit transaction, compare, and
/// refuse to continue when anything has moved.
///
/// `Ok(Ok(()))` is the only outcome that lets the commit proceed. A drift becomes
/// [`WorkerError::RevalidationRequired`], which rolls the transaction back and
/// sends the worker round for another attempt — the rule has one site here rather
/// than one per commit path.
///
/// # Errors
/// [`WorkerError::RevalidationRequired`] when anything the evaluation rested on
/// has moved; otherwise as [`derive`].
pub async fn guard(
    stores: &dyn Stores,
    tx: &DbTx<'_>,
    scope: &AccessScope,
    candidate_gts_id: &str,
    recorded: &RevisionVector,
    write_set_bound: usize,
) -> Result<Result<(), ItemFailure>, WorkerError> {
    let fresh = match derive(
        stores,
        tx,
        scope,
        candidate_gts_id,
        &recorded.roots,
        write_set_bound,
    )
    .await?
    {
        Ok(fresh) => fresh,
        Err(failure) => return Ok(Err(failure)),
    };
    match recorded.drift(&fresh) {
        None => Ok(Ok(())),
        Some(drift) => Err(WorkerError::RevalidationRequired(drift)),
    }
}

#[cfg(test)]
#[path = "vector_tests.rs"]
mod vector_tests;
