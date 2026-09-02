//! Dependency-edge extraction: the four direct edges one candidate implies, from
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
//! # Why `$ref` comes from `gts-rust` and `x-gts-ref` is walked here
//!
//! `$ref` has one canonical interpretation, `gts::extract_gts_refs`, shared with
//! the resolution that validates the candidate. Re-deriving it locally would be a
//! second implementation of a GTS rule (`constraint-gts-implementation`) and the
//! two would drift.
//!
//! `x-gts-ref` has no such extractor in `gts` 0.12.0: `XGtsRefValidator` validates
//! patterns and instance values but never reports the sites it visited, and
//! `GtsEntity::gts_refs` — the lenient walker whose own documentation offers it "for
//! the dependency graph" — collects *every* identifier-shaped string, so it would
//! both invent edges out of `const` data and miss every wildcard pattern, which is
//! not a valid identifier at all. So the keyword sites are walked here, while what a
//! site *means* still comes from `gts`: [`GtsId::try_new`] is the only judge of
//! whether a target names anything.
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

/// The JSON Schema keywords whose contents are instance *data* rather than a
/// subschema. Mirrored from `gts::schema_refs`, which skips the same four, so the
/// two halves of one document are read under one rule.
const DATA_VALUED_KEYWORDS: [&str; 4] = ["const", "default", "examples", "enum"];

/// One outgoing edge of a candidate, named by the target's identifier.
///
/// Identifiers rather than entity ids: extraction is pure, and an entity id is a
/// fact about the database. The commit resolves them, and a target that resolves to
/// nothing writes no row.
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

        // Runs only once the strict extractor has accepted the document, which
        // bounds its nesting at that extractor's scan cap — so this walk needs no
        // depth guard of its own.
        edges.extend(
            x_gts_ref_targets(content)
                .into_iter()
                .map(|target| DependencyEdge {
                    kind: DependencyKind::GtsRef,
                    target,
                }),
        );
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

/// The targets a store build must seed its dependency closure with: the reference
/// kinds, and only those.
///
/// `Derivation` and `InstanceOf` targets are members of the candidate's own
/// `~`-chain, which the closure already seeds from the identifier
/// (`DependencyRepo::closure`). Passing them again would say the same thing twice
/// and charge the closure bound for it.
#[must_use]
pub fn reference_targets(edges: &[DependencyEdge]) -> Vec<String> {
    let mut targets: Vec<String> = edges
        .iter()
        .filter(|e| matches!(e.kind, DependencyKind::SchemaRef | DependencyKind::GtsRef))
        .map(|e| e.target.clone())
        .collect();
    targets.sort();
    targets.dedup();
    targets
}

/// Every entity one document's `x-gts-ref` constraints name.
fn x_gts_ref_targets(content: &Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_x_gts_refs(content, &mut out);
    out
}

fn collect_x_gts_refs(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(pattern)) = map.get("x-gts-ref")
                && let Some(target) = reference_target(pattern)
            {
                out.push(target);
            }
            for (key, nested) in map {
                if key == "x-gts-ref" || DATA_VALUED_KEYWORDS.contains(&key.as_str()) {
                    continue;
                }
                collect_x_gts_refs(nested, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_x_gts_refs(item, out);
            }
        }
        _ => {}
    }
}

/// The entity one `x-gts-ref` pattern protects: the exact identifier, or the
/// pattern's longest valid identifier prefix — never its open match set (DESIGN
/// §3.2). A new entity that starts matching the pattern therefore needs no
/// re-expansion of anyone's edges.
///
/// Three answers, and two of them are `None`:
///
/// * a GTS §9.6 relative pointer (`/$id`, `./properties/id`) resolves inside the
///   document itself, naming no other entity;
/// * a pattern with no valid identifier prefix (`gts.*`) names no entity to
///   protect;
/// * anything else contributes its longest `~`-terminated valid prefix, which for
///   an exact identifier is the identifier.
fn reference_target(pattern: &str) -> Option<String> {
    if pattern.starts_with('/') || pattern.starts_with('.') {
        return None;
    }
    if GtsId::try_new(pattern).is_ok() {
        return Some(pattern.to_owned());
    }
    // Ascending byte offsets, so the last accepted prefix is the longest. Only
    // `~`-terminated prefixes are candidates: a type identifier ends in `~`, and a
    // truncated final segment would name a type that does not exist.
    pattern
        .char_indices()
        .filter(|(_, c)| *c == '~')
        .filter_map(|(idx, c)| pattern.get(..idx + c.len_utf8()))
        .rfind(|prefix| GtsId::try_new(prefix).is_ok())
        .map(str::to_owned)
}

#[cfg(test)]
#[path = "dependency_tests.rs"]
mod dependency_tests;
