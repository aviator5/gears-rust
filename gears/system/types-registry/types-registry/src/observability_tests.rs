//! The span constructors' contract: fields, their spellings, and what is absent
//! before the facts are known.
//!
//! The instruments' contract lives in `infra::metrics_tests`; the emission sites
//! of both are `tests/observability_test.rs`.

use crate::domain::enums::OperationKind;

/// The operation span carries the three request-level facts an operator filters
/// on. `gts_id` is deliberately absent: an operation is a batch, so one
/// identifier on it would name whichever candidate happened to be first.
///
/// `kind` and `dry_run` are declared empty and filled in by
/// [`super::record_operation_facts`] because the span opens **before** the read
/// that learns them — the alternative is a span that does not cover the read it
/// is describing.
#[test]
#[tracing_test::traced_test]
fn the_operation_span_carries_the_operation_id_kind_and_dry_run_mode() {
    let operation_id = uuid::Uuid::from_u128(0x1234);
    let span = super::operation_span(operation_id);
    super::record_operation_facts(&span, OperationKind::Registration, false);
    let _entered = span.enter();
    tracing::info!("probe");

    assert!(logs_contain(&operation_id.to_string()));
    assert!(logs_contain(r#"kind="registration""#));
    assert!(logs_contain("dry_run=false"));
}

/// Until the facts are recorded the two fields are absent rather than wrong. A
/// span that rendered `kind=""` would put an empty label into a filter an operator
/// writes once and trusts.
#[test]
#[tracing_test::traced_test]
fn the_operation_span_shows_no_kind_before_the_operation_is_read() {
    let span = super::operation_span(uuid::Uuid::from_u128(0x99));
    let _entered = span.enter();
    tracing::info!("probe");

    assert!(!logs_contain("kind="));
}

/// The unit span adds the candidate's identifier and its item id, and restates
/// kind and dry-run mode from the item's own columns rather than relying on
/// inheritance: a unit is what an operator greps for, and a field only reachable
/// by walking up the span tree is not greppable in a flat log line.
#[test]
#[tracing_test::traced_test]
fn the_unit_span_carries_the_candidate_identifier_beside_the_operation_facts() {
    let operation_id = uuid::Uuid::from_u128(0x5678);
    let span = super::unit_span(
        operation_id,
        "cf.core.example.type.v1~",
        OperationKind::Registration,
        true,
        42,
    );
    let _entered = span.enter();
    tracing::info!("probe");

    assert!(logs_contain(r#"gts_id="cf.core.example.type.v1~""#));
    assert!(logs_contain(&operation_id.to_string()));
    assert!(logs_contain(r#"kind="registration""#));
    assert!(logs_contain("dry_run=true"));
    assert!(logs_contain("operation_item_id=42"));
}

/// A deletion is the other kind, and its label is its own word. The vocabulary is
/// closed by the enum, so this pins the spelling rather than the mapping's
/// existence.
#[test]
#[tracing_test::traced_test]
fn a_deletion_operation_is_labelled_deletion() {
    let span = super::operation_span(uuid::Uuid::from_u128(0x7));
    super::record_operation_facts(&span, OperationKind::Deletion, false);
    let _entered = span.enter();
    tracing::info!("probe");

    assert!(logs_contain(r#"kind="deletion""#));
}
