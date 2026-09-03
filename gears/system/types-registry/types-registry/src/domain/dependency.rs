//! Dependency-edge extraction: the three direct edges one candidate implies, from its authored
//! content and its identifier (DESIGN §3.2, SPEC §3.2).

use gts::{ExtractRefsError, GtsId, extract_gts_refs};
use serde_json::Value;
use thiserror::Error;
use toolkit_macros::domain_model;

use crate::domain::enums::DependencyKind;

/// One outgoing edge of a candidate, named by the target's identifier.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DependencyEdge {
    pub kind: DependencyKind,
    pub target: String,
}

/// The one way extraction can fail: the strict `$ref` extractor refused a reference — a malformed
/// or bare-id `$ref`, or a document nested past its scan cap.
#[domain_model]
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("the references of '{gts_id}' cannot be extracted: {source}")]
pub struct EdgeExtractionError {
    pub gts_id: String,
    pub source: ExtractRefsError,
}

/// Extract every outgoing edge of one candidate, sorted and deduplicated.
pub fn extract_edges(
    id: &GtsId,
    content: &Value,
) -> Result<Vec<DependencyEdge>, EdgeExtractionError> {
    let mut edges = Vec::new();

    if id.is_type() {
        // The immediate base only.
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
        // `None` only for a single-segment identifier, which `GtsId::try_new` refuses for an
        // Instance before this function is ever reached.
        edges.push(DependencyEdge {
            kind: DependencyKind::InstanceOf,
            target: type_id,
        });
    }

    edges.sort();
    edges.dedup();
    Ok(edges)
}

/// The targets a store build must seed its dependency closure with: the `$ref` targets, and only
/// those.
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
