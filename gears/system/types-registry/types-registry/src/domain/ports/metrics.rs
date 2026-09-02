//! Output port for the admission path's instruments (T16).
//!
//! Implementations live in [`crate::infra::metrics`] — OpenTelemetry
//! instruments declared on a scoped `Meter` from `ToolKit`'s global
//! `SdkMeterProvider`. Domain code depends only on this trait and the label
//! vocabularies below, never on the SDK, so the layer boundary holds on the
//! admission call path (`acceptance`, `worker`, `unit`) where an
//! infrastructure type would otherwise hide from the `de0301_no_infra_in_domain`
//! lint by living at the crate root.
//!
//! # Reaching the port
//!
//! Injected, like every other gear's meter: `TypesRegistryGear::init` builds the
//! adapter and hands the `Arc<dyn AdmissionMetrics>` to the service, which
//! carries it down the admission call graph — `run_operation` → `process_item`
//! → `commit_evaluated` → the commit transaction's `'static` closure →
//! `commit_creation` / `commit_revision` → the reverse-impact refresh. The
//! closure is why the parameter is an `Arc` rather than a reference: a `'static`
//! closure can borrow nothing, so each retry attempt clones the handle. A
//! caller with no meter passes [`NoopMetrics`], which is the pre-T16 behaviour
//! exactly.
//!
//! # Names
//!
//! The rendered instrument names are the adapter's contract and are pinned by
//! `infra::metrics_tests`: a configurable prefix
//! ([`MetricsConfig`](crate::config::MetricsConfig), default `types_registry`)
//! followed by fixed suffixes — `_total` on counters, `_seconds` on the duration
//! histogram — with no `.with_unit(..)` hint, so the rendered series name is the
//! same whether the downstream collector runs with `add_metric_suffixes` on or
//! off.
//!
//! # Label cardinality
//!
//! Every label value below comes from a closed vocabulary: a Rust enum's
//! variants (`status`, `stage`, `drift`) or a `&'static str` literal at a
//! refusal site (`reason`, mapped to a single `other` when a reason is not
//! provably a literal). **No identifier is ever a label** — `gts_id` is
//! unbounded and belongs on a span, where it is per-event rather than
//! per-series.

use std::time::Duration;

use toolkit_macros::domain_model;

use crate::domain::admission::vector::VectorDrift;
use crate::domain::enums::OperationItemStatus;

/// Which half of SPEC §8.1 refused a submission.
///
/// The two are one counter with this as a label rather than two counters: "how
/// many submissions were refused" is then one query, and "by which stage" a
/// `by (stage)` on it.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalStage {
    /// Steps 1–8: a refusal before the request became a durable operation.
    Acceptance,
    /// Step 3 onwards, per candidate: a refusal recorded on an operation item.
    Admission,
}

impl RefusalStage {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Acceptance => "acceptance",
            Self::Admission => "admission",
        }
    }
}

/// A candidate outcome — the only statuses a terminalized candidate can carry.
///
/// [`OperationItemStatus`] also spells `Pending` and `Running`, which are not
/// outcomes but progress, and counting one would put a value in the
/// `types_registry_candidates_total` series that can never be an end state. The
/// parameter type excludes them by construction; a caller holding a full
/// [`OperationItemStatus`] converts through [`TryFrom`], which refuses the two
/// non-terminal values rather than silently dropping the count.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalStatus {
    Succeeded,
    Unchanged,
    Failed,
}

impl TerminalStatus {
    /// The snake-case label value — deliberately not `Debug`, so a derive's
    /// output cannot become the series contract.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Unchanged => "unchanged",
            Self::Failed => "failed",
        }
    }
}

impl TryFrom<OperationItemStatus> for TerminalStatus {
    type Error = OperationItemStatus;

    fn try_from(status: OperationItemStatus) -> Result<Self, Self::Error> {
        match status {
            OperationItemStatus::Succeeded => Ok(Self::Succeeded),
            OperationItemStatus::Unchanged => Ok(Self::Unchanged),
            OperationItemStatus::Failed => Ok(Self::Failed),
            // The caller learns which status was not terminal rather than a
            // bare "no": the distinction is the whole reason this conversion
            // can fail.
            non_terminal => Err(non_terminal),
        }
    }
}

/// The admission path's instrument set.
///
/// One method per instrument family; the method docs name the series. The
/// adapter in `infra::metrics` owns the rendering.
/// `Debug` is required so the structs that carry the handle — `AcceptanceContext`,
/// `Tuning` — keep their derives; every implementation is a set of instrument
/// handles with nothing sensitive to print.
pub trait AdmissionMetrics: std::fmt::Debug + Send + Sync {
    /// `types_registry_candidates_total{status}` — one increment per candidate
    /// **this pass** terminalized, under its terminal status. A redelivery that
    /// reports an outcome another pass recorded does not increment: the counter
    /// counts decisions, not reports of them.
    fn candidate_terminalized(&self, status: TerminalStatus);

    /// `types_registry_refusals_total{stage,reason}` — one increment per refusal.
    ///
    /// `reason` is the machine reason its site names — `ItemFailure::reason` in
    /// admission, `AcceptanceError::reason` in acceptance — and both are closed
    /// sets of `&'static str` literals. The one reason that is not provably a
    /// literal, a failure read back off a stored `error_payload`, is mapped by
    /// its caller to the single fallback label `other` rather than admitted as
    /// an unbounded series.
    fn refused(&self, stage: RefusalStage, reason: &'static str);

    /// `types_registry_revalidations_total{drift}` — one increment per *retry*
    /// taken, so a candidate that committed on its first attempt contributes
    /// nothing. The label is the *shape* of the drift, never the identifier
    /// that drifted.
    fn revalidation_retried(&self, drift: &VectorDrift);

    /// `types_registry_activation_write_set` — dependents rewritten by one
    /// revision (SPEC §8.1 step 4.6), including zero.
    fn observe_activation_write_set(&self, refreshed: usize);

    /// `types_registry_operation_duration_seconds` — one admission pass,
    /// wall-clock.
    fn observe_operation_duration(&self, elapsed: Duration);
}

/// Instruments that count nothing, for a caller with no meter to inject.
///
/// A `no-db` deployment and most test binaries want the admission path without
/// a metrics pipeline behind it; they pass this rather than paying for — or
/// depending on — a meter that was never installed.
#[domain_model]
#[derive(Debug, Default)]
pub struct NoopMetrics;

impl AdmissionMetrics for NoopMetrics {
    fn candidate_terminalized(&self, _status: TerminalStatus) {}

    fn refused(&self, _stage: RefusalStage, _reason: &'static str) {}

    fn revalidation_retried(&self, _drift: &VectorDrift) {}

    fn observe_activation_write_set(&self, _refreshed: usize) {}

    fn observe_operation_duration(&self, _elapsed: Duration) {}
}
