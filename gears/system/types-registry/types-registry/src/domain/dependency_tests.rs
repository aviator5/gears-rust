//! Unit tests for the pure edge extractor. No database, no clock: every case is a
//! document and an identifier in, an edge set out — which is the whole reason
//! [`extract_edges`](super::extract_edges) takes content rather than a row.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use gts::GtsId;
use serde_json::{Value, json};
use toolkit_gts::gts_id;

use super::{DependencyEdge, extract_edges, reference_targets};
use crate::domain::enums::DependencyKind;

const ROOT: &str = gts_id!("cf.core.example.root.v1~");
const DERIVED: &str = gts_id!("cf.core.example.root.v1~cf.core.example.leaf.v1~");
const INSTANCE: &str = gts_id!("cf.core.example.root.v1~cf.core.example.first.v1");
const OTHER: &str = gts_id!("cf.core.other.shape.v1~");
const THIRD: &str = gts_id!("cf.core.other.third.v1~");

fn id(s: &str) -> GtsId {
    GtsId::try_new(s).expect("the fixture identifier parses")
}

/// A schema carrying whatever body a case needs, with the `$id` and dialect every
/// admitted schema has.
fn schema(gts_id: &str, body: Value) -> Value {
    let mut doc = json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
    });
    let Value::Object(extra) = body else {
        panic!("a schema body fixture must be an object");
    };
    for (key, value) in extra {
        doc[key] = value;
    }
    doc
}

fn edges(gts_id: &str, content: &Value) -> Vec<DependencyEdge> {
    extract_edges(&id(gts_id), content).expect("the fixture extracts")
}

fn targets_of(edges: &[DependencyEdge], kind: DependencyKind) -> Vec<String> {
    edges
        .iter()
        .filter(|e| e.kind == kind)
        .map(|e| e.target.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// `$ref`
// ---------------------------------------------------------------------------

#[test]
fn a_ref_outside_the_identifier_chain_is_a_schema_ref_edge() {
    let doc = schema(
        ROOT,
        json!({ "properties": { "shape": { "$ref": format!("gts://{OTHER}") } } }),
    );
    assert_eq!(
        targets_of(&edges(ROOT, &doc), DependencyKind::SchemaRef),
        vec![OTHER.to_owned()],
        "a `$ref` is the one edge kind no identifier implies",
    );
}

#[test]
fn refs_are_deduplicated_and_the_traits_schema_is_covered() {
    let doc = schema(
        ROOT,
        json!({
            "properties": {
                "a": { "$ref": format!("gts://{OTHER}") },
                "b": { "$ref": format!("gts://{OTHER}#/properties/inner") },
                "local": { "$ref": "#/$defs/Local" },
            },
            "$defs": { "Local": { "type": "string" } },
            "x-gts-traits-schema": {
                "type": "object",
                "properties": { "t": { "$ref": format!("gts://{THIRD}") } },
            },
        }),
    );
    let mut found = targets_of(&edges(ROOT, &doc), DependencyKind::SchemaRef);
    found.sort();
    assert_eq!(
        found,
        vec![OTHER.to_owned(), THIRD.to_owned()],
        "two references to one target are one edge; a local pointer is none, and \
         `x-gts-traits-schema` is part of the document",
    );
}

#[test]
fn a_malformed_ref_is_reported_rather_than_silently_dropped() {
    // A bare id with no `gts://` scheme: what `validate_schema` also refuses.
    let doc = schema(ROOT, json!({ "properties": { "a": { "$ref": OTHER } } }));
    let err = extract_edges(&id(ROOT), &doc).expect_err("a bare-id ref is not extractable");
    assert_eq!(err.gts_id, ROOT);
}

// ---------------------------------------------------------------------------
// `x-gts-ref`
// ---------------------------------------------------------------------------

#[test]
fn an_exact_x_gts_ref_targets_that_identifier() {
    let doc = schema(
        ROOT,
        json!({ "properties": { "role": { "type": "string", "x-gts-ref": OTHER } } }),
    );
    assert_eq!(
        targets_of(&edges(ROOT, &doc), DependencyKind::GtsRef),
        vec![OTHER.to_owned()],
    );
}

#[test]
fn a_wildcard_x_gts_ref_targets_the_patterns_longest_valid_prefix() {
    let doc = schema(
        ROOT,
        json!({
            "properties": {
                "role": { "type": "string", "x-gts-ref": format!("{OTHER}*") },
            },
        }),
    );
    assert_eq!(
        targets_of(&edges(ROOT, &doc), DependencyKind::GtsRef),
        vec![OTHER.to_owned()],
        "the edge protects the entity the pattern names, never its open match set",
    );
}

#[test]
fn an_x_gts_ref_that_names_nothing_valid_creates_no_edge() {
    let doc = schema(
        ROOT,
        json!({ "properties": { "any": { "type": "string", "x-gts-ref": "gts.*" } } }),
    );
    assert!(
        targets_of(&edges(ROOT, &doc), DependencyKind::GtsRef).is_empty(),
        "`gts.*` has no valid identifier prefix, so there is no entity to protect",
    );
}

#[test]
fn a_relative_x_gts_ref_pointer_creates_no_edge() {
    for pointer in ["/$id", "./properties/id", "../$id"] {
        let doc = schema(
            ROOT,
            json!({ "properties": { "self": { "type": "string", "x-gts-ref": pointer } } }),
        );
        assert!(
            targets_of(&edges(ROOT, &doc), DependencyKind::GtsRef).is_empty(),
            "{pointer} resolves inside this document (GTS §9.6), naming no other entity",
        );
    }
}

// ---------------------------------------------------------------------------
// Derivation and conformance
// ---------------------------------------------------------------------------

#[test]
fn a_derived_schema_edges_only_to_its_immediate_base() {
    let doc = schema(DERIVED, json!({}));
    assert_eq!(
        targets_of(&edges(DERIVED, &doc), DependencyKind::Derivation),
        vec![ROOT.to_owned()],
        "the chain above the base is reached by walking the base's own edge, not by \
         a second row from here",
    );
}

#[test]
fn a_first_generation_schema_has_no_derivation_edge() {
    let doc = schema(ROOT, json!({}));
    assert!(
        edges(ROOT, &doc).is_empty(),
        "nothing above a single segment"
    );
}

#[test]
fn an_instance_conforms_to_its_type_and_carries_nothing_else() {
    let value = json!({ "name": "first" });
    assert_eq!(
        edges(INSTANCE, &value),
        vec![DependencyEdge {
            kind: DependencyKind::InstanceOf,
            target: ROOT.to_owned(),
        }],
    );
}

#[test]
fn an_instance_values_ref_shaped_data_is_data_and_not_an_edge() {
    // An Instance is a *value*: `$ref` and `x-gts-ref` are schema keywords, so the
    // same strings inside a value name nothing. Extracting them would invent an
    // edge — and, worse, a malformed one would refuse a perfectly valid value.
    let value = json!({
        "$ref": OTHER,
        "x-gts-ref": "not-an-identifier",
        "nested": { "$ref": format!("gts://{THIRD}") },
    });
    assert_eq!(
        edges(INSTANCE, &value),
        vec![DependencyEdge {
            kind: DependencyKind::InstanceOf,
            target: ROOT.to_owned(),
        }],
    );
}

// ---------------------------------------------------------------------------
// The four kinds, and the cases that produce none, over one table
// ---------------------------------------------------------------------------

#[test]
fn the_edge_kinds_over_their_fixtures() {
    struct Case {
        what: &'static str,
        gts_id: &'static str,
        content: Value,
        expected: Vec<(DependencyKind, &'static str)>,
    }

    let cases = vec![
        Case {
            what: "a `$ref` and an `x-gts-ref` in one derived schema: three kinds at once",
            gts_id: DERIVED,
            content: schema(
                DERIVED,
                json!({
                    "properties": {
                        "shape": { "$ref": format!("gts://{OTHER}") },
                        "role": { "type": "string", "x-gts-ref": THIRD },
                    },
                }),
            ),
            expected: vec![
                (DependencyKind::SchemaRef, OTHER),
                (DependencyKind::GtsRef, THIRD),
                (DependencyKind::Derivation, ROOT),
            ],
        },
        Case {
            what: "a reference-free root schema has no edges of any kind",
            gts_id: ROOT,
            content: schema(
                ROOT,
                json!({ "properties": { "name": { "type": "string" } } }),
            ),
            expected: vec![],
        },
        Case {
            what: "an `x-gts-ref` inside a data-valued keyword is data, not a constraint",
            gts_id: ROOT,
            content: schema(
                ROOT,
                json!({ "properties": { "a": { "const": { "x-gts-ref": OTHER } } } }),
            ),
            expected: vec![],
        },
        Case {
            what: "an Instance of a derived type conforms to that type, not to the root",
            gts_id: gts_id!("cf.core.example.root.v1~cf.core.example.leaf.v1~cf.core.example.i.v1"),
            content: json!({ "name": "i" }),
            expected: vec![(DependencyKind::InstanceOf, DERIVED)],
        },
    ];

    for case in cases {
        let mut found: Vec<(DependencyKind, String)> = edges(case.gts_id, &case.content)
            .into_iter()
            .map(|e| (e.kind, e.target))
            .collect();
        found.sort();
        let mut expected: Vec<(DependencyKind, String)> = case
            .expected
            .into_iter()
            .map(|(kind, target)| (kind, target.to_owned()))
            .collect();
        expected.sort();
        assert_eq!(found, expected, "{}", case.what);
    }
}

// ---------------------------------------------------------------------------
// The read-side seed
// ---------------------------------------------------------------------------

#[test]
fn only_the_reference_kinds_seed_the_closure() {
    // Derivation and conformance targets are chain members, which the closure
    // already seeds from the identifier (T10). Seeding them again would say the
    // same thing twice; a `$ref` or `x-gts-ref` target is the half no identifier
    // implies.
    let doc = schema(
        DERIVED,
        json!({
            "properties": {
                "shape": { "$ref": format!("gts://{OTHER}") },
                "role": { "type": "string", "x-gts-ref": THIRD },
            },
        }),
    );
    let mut seeds = reference_targets(&edges(DERIVED, &doc));
    seeds.sort();
    assert_eq!(seeds, vec![OTHER.to_owned(), THIRD.to_owned()]);
}
