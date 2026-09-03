//! The revision vector: what evaluation's verdict rests on, and the commit-time guard that refuses
//! to write when any of it has moved (SPEC §8.1 steps 3 and 4.3 — D4).

use std::collections::HashMap;

use toolkit_db::DbTx;
use toolkit_db::secure::AccessScope;
use toolkit_macros::domain_model;

use super::errors::{ItemFailure, WorkerError};
use crate::domain::enums::{EntityKind, LifecycleStatus};
use crate::domain::ports::{EntityRow, ReverseImpact, Stores};

/// Which side of the candidate an entry stands on.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum VectorRole {
    /// Evaluation consumed this entity's authored document while building the transient store.
    Dependency,
    /// This entity depends on the candidate, so the commit's refresh consumes its effective
    /// artifacts.
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
    /// `Some` exactly where effective content was consumed — a live Type Schema dependent.
    pub resolution_fingerprint: Option<Vec<u8>>,
}

/// The full vector, plus the roots it was derived from.
#[domain_model]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RevisionVector {
    /// The closure roots — the candidate identifier plus its document's reference targets — carried
    /// so the commit asks the same question evaluation did.
    pub roots: Vec<String>,
    /// One entry per dependency and dependent, `(gts_id, role)`-sorted.
    entries: Vec<VectorEntry>,
}

/// The first difference between a recorded vector and a freshly-derived one.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VectorDrift {
    /// Not there when evaluation looked.
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
    /// Someone else's commit re-materialized this dependent's artifacts.
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
    /// The only constructor: takes the entries in whatever order they were collected and sorts them
    /// into the canonical `(gts_id, role)` order the comparison below is a merge walk over.
    #[must_use]
    pub fn new(roots: Vec<String>, mut entries: Vec<VectorEntry>) -> Self {
        entries.sort_by(|a, b| key(a).cmp(&key(b)));
        Self { roots, entries }
    }

    /// One entry per dependency and dependent, `(gts_id, role)`-sorted.
    #[must_use]
    pub fn entries(&self) -> &[VectorEntry] {
        &self.entries
    }

    /// The drift between this vector and a freshly-derived one, or `None` when the two agree.
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

/// The sort and merge key.
fn key(entry: &VectorEntry) -> (&str, VectorRole) {
    (entry.gts_id.as_str(), entry.role)
}

/// The two column comparisons for one entity present on both sides.
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

/// Derive the vector for one candidate: its dependency closure and its reverse-impact set, as the
/// database holds them right now.
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

    // Empty for a creation, which has no row for anything to depend on: a `$ref` to an absent
    // target has no resolved form, so nothing can already point here.
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

    // One batched read for every dependent that carries artifacts, rather than one per row: at
    // recheck time this runs inside the commit transaction.
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
            // Absent for an Instance or a tombstone, which the refresh skips, and absent for a Type
            // Schema whose current row has gone missing — a corruption the refresh reports by name.
            resolution_fingerprint: fingerprints.remove(&row.id),
        });
    }

    Ok(Ok(RevisionVector::new(roots.to_vec(), entries)))
}

/// Step 4.3: re-derive the vector inside the commit transaction, compare, and refuse to continue
/// when anything has moved.
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
