//! The admission path's spans (T16).
//!
//! # Why spans are a module and metrics are a port
//!
//! Every signal a gear emits splits by what reaching it costs: a `tracing` span
//! is a process-global sink the emitting code does not carry, does not inject
//! and cannot see, so the span constructors below are plain free functions. The
//! *instruments* are different — they are a typed sink behind a trait, so they
//! live behind a domain port ([`crate::domain::ports::metrics`]) with the
//! OpenTelemetry adapter in [`crate::infra::metrics`], injected from
//! `Gear::init`. Hosting the adapter here instead would put an infrastructure
//! SDK dependency into domain call paths where the `de0301_no_infra_in_domain`
//! lint cannot see it.
//!
//! # Label cardinality
//!
//! The one label these spans carry from a vocabulary, `kind`, is closed by
//! `OperationKind`; `dry_run` is a boolean. **No identifier is ever invented
//! here** — `operation_id`, `gts_id` and `operation_item_id` are span fields,
//! per-event rather than per-series, and the metrics port keeps the same rule
//! for its labels.

use tracing::{Span, field};
use uuid::Uuid;

use crate::domain::enums::OperationKind;

/// The label an operation's kind carries.
///
/// Snake case, and deliberately not `EntityKind::as_str`'s prose spelling: that
/// one is a message a caller reads, this one is a series name fragment.
const fn kind_label(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Registration => "registration",
        OperationKind::Deletion => "deletion",
    }
}

/// The span covering one admission pass over one operation.
///
/// `kind` and `dry_run` open **empty** and are filled in by
/// [`record_operation_facts`] once the operation row has been read, because the
/// read is the first thing the pass does and a span created after it would not
/// cover it. An empty field renders as absent rather than as a blank value, so a
/// pass that fails before the read carries no misleading label.
#[must_use]
pub fn operation_span(operation_id: Uuid) -> Span {
    tracing::info_span!(
        "types_registry.admission.operation",
        %operation_id,
        kind = field::Empty,
        dry_run = field::Empty,
    )
}

/// Fill in the two fields [`operation_span`] left empty.
pub fn record_operation_facts(span: &Span, kind: OperationKind, dry_run: bool) {
    span.record("kind", kind_label(kind));
    span.record("dry_run", dry_run);
}

/// The span covering one admission unit — one candidate, one operation item.
///
/// It restates `operation_id`, `kind` and `dry_run` rather than leaving them to
/// be inherited from the operation span. That is deliberate duplication: the unit
/// is what an operator greps for, and with a flat log format a field that lives
/// only on the parent span is not on the line.
#[must_use]
pub fn unit_span(
    operation_id: Uuid,
    gts_id: &str,
    kind: OperationKind,
    dry_run: bool,
    operation_item_id: i64,
) -> Span {
    tracing::info_span!(
        "types_registry.admission.unit",
        %operation_id,
        gts_id,
        kind = kind_label(kind),
        dry_run,
        operation_item_id,
    )
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "observability_tests.rs"]
mod observability_tests;
