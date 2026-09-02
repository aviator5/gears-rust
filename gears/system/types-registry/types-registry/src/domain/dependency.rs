//! Dependency-edge extraction: the three direct edges one candidate implies, from
//! its authored content and its identifier (DESIGN §3.2, SPEC §3.2).
//!
//! Pure — a document and an identifier in, an edge set out. No database, no clock,
//! no state. That is not a testing convenience: the same function runs on both
//! sides of an admission, and the two sides have nothing else in common.
//!
//! * **Write side.** The commit resolves these targets to entity rows and replaces
//!   the candidate's outgoing rows (`DependencyRepo::replace_outgoing`).
//! * **Read side.** [`reference_targets`] seeds the closure a store build reads
//!   (`domain::gts_store::load_unit_store`), because the rows above are written at
//!   *commit* and so do not exist during the read that validates the candidate
//!   authoring them.
//!
//! # Two kinds are derivable from the identifier and are stored anyway
//!
//! `Derivation` and `InstanceOf` are pure functions of the name — `base~derived~`
//! consumes `base~` by being spelled so. They are materialized because T14's
//! **reverse** walk has no identifier to walk backwards from: finding everything
//! derived from a revised base means reading rows, and a prefix range over
//! `entity.gts_id` cannot be mixed into the same recursive traversal on all three
//! backends (DESIGN §3.2).
//!
//! # `x-gts-ref` is not an edge
//!
//! It constrains what a value may *name*, and `gts-rust` enforces that by matching
//! the value string against a pattern — `XGtsRefValidator` never asks whether the
//! named entity exists. So the constraint is satisfiable with nothing in the
//! registry, the target is not inlined into any effective artifact (DESIGN §3.1), and
//! an edge for it would have exactly one effect: blocking a deletion that harms
//! nobody. Policies that need to classify the identifier named by this keyword do
//! so directly from candidate content; the dependency graph has no use for that
//! classification.
//!
//! # Why `$ref` comes from `gts-rust`
//!
//! `$ref` has one canonical interpretation, `gts::extract_gts_refs`, shared with
//! the resolution that validates the candidate. Re-deriving it locally would be a
//! second implementation of a GTS rule (`constraint-gts-implementation`) and the
//! two would drift.
//!
//! # An Instance carries exactly one edge
//!
//! `$ref` and `x-gts-ref` are schema keywords, so the same strings inside a value
//! are data. Extracting them would invent an edge from a coincidence — and a
//! malformed one would refuse a perfectly valid value.

use gts::{ExtractRefsError, GtsId, extract_gts_refs};
use serde_json::Value;
use thiserror::Error;
use toolkit_macros::domain_model;

use crate::domain::enums::DependencyKind;

/// One outgoing edge of a candidate, named by the target's identifier.
///
/// Identifiers rather than entity ids: extraction is pure, and an entity id is a
/// fact about the database. The commit resolves every target and fails retryably if
/// a row that evaluation observed is no longer present.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DependencyEdge {
    pub kind: DependencyKind,
    pub target: String,
}

/// The one way extraction can fail: the strict `$ref` extractor refused a
/// reference — a malformed or bare-id `$ref`, or a document nested past its scan
/// cap.
///
/// A struct rather than an enum with one arm, because there is one cause and naming
/// it twice would suggest otherwise. The caller reports it as `invalid_schema`: it
/// is the same refusal `validate_schema` would reach, arrived at earlier.
#[domain_model]
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("the references of '{gts_id}' cannot be extracted: {source}")]
pub struct EdgeExtractionError {
    pub gts_id: String,
    pub source: ExtractRefsError,
}

/// Extract every outgoing edge of one candidate, sorted and deduplicated.
///
/// Sorted because the result is compared in tests and written as a set: two
/// extractions of one document must produce the same sequence, and `dependency`'s
/// primary key already treats repeats as one row.
///
/// # Errors
/// [`EdgeExtractionError`] when a `$ref` in a **schema** is not a valid GTS
/// reference. An Instance value cannot fail: nothing in it is read as a reference.
pub fn extract_edges(
    id: &GtsId,
    content: &Value,
) -> Result<Vec<DependencyEdge>, EdgeExtractionError> {
    let mut edges = Vec::new();

    if id.is_type() {
        // The immediate base only. The chain above it is reached by walking that
        // base's own edge — storing the whole chain from here would make every
        // ancestor a direct dependent and defeat the deletion-safety rule, which
        // asks about *direct* rows and only those.
        let chain = id.chain_ids();
        if chain.len() >= 2 {
            edges.push(DependencyEdge {
                kind: DependencyKind::Derivation,
                target: chain[chain.len() - 2].clone(),
            });
        }

        let refs = extract_gts_refs(content).map_err(|source| EdgeExtractionError {
            gts_id: id.id().to_owned(),
            source,
        })?;
        edges.extend(refs.into_iter().map(|target| DependencyEdge {
            kind: DependencyKind::SchemaRef,
            target,
        }));
    } else if let Some(type_id) = id.get_type_id() {
        // `None` only for a single-segment identifier, which `GtsId::try_new`
        // refuses for an Instance before this function is ever reached.
        edges.push(DependencyEdge {
            kind: DependencyKind::InstanceOf,
            target: type_id,
        });
    }

    edges.sort();
    edges.dedup();
    Ok(edges)
}

/// The targets a store build must seed its dependency closure with: the `$ref`
/// targets, and only those.
///
/// `Derivation` and `InstanceOf` targets are members of the candidate's own
/// `~`-chain, which the closure already seeds from the identifier
/// (`DependencyRepo::closure`). Passing them again would say the same thing twice
/// and charge the closure bound for it. An `x-gts-ref` target is not seeded either,
/// and for a stronger reason: validating the keyword never reads the target
/// document, so loading it would charge the bound for a document nothing asks a
/// question of.
#[must_use]
pub fn reference_targets(edges: &[DependencyEdge]) -> Vec<String> {
    let mut targets: Vec<String> = edges
        .iter()
        .filter(|e| e.kind == DependencyKind::SchemaRef)
        .map(|e| e.target.clone())
        .collect();
    targets.sort();
    targets.dedup();
    targets
}

#[cfg(test)]
#[path = "dependency_tests.rs"]
mod dependency_tests;
