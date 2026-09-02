//! The OpenTelemetry adapter for the admission path's instruments (T16).
//!
//! The domain side of this contract — method names, label vocabularies,
//! cardinality rules — is [`crate::domain::ports::metrics`]; this module only
//! renders it onto [`opentelemetry`] instruments and is the one place that
//! touches the metrics SDK from this gear. `TypesRegistryGear::init` builds it
//! through [`default_adapter`] and hands the `Arc<dyn AdmissionMetrics>` to the
//! service, the way every other gear wires its meter.
//!
//! # Why the adapter is `Send + Sync` and cheap to hold
//!
//! Every instrument is an `Arc` inside the SDK, so the `Arc<dyn …>` the service
//! holds costs an atomic load per emission, and cloning one into a commit
//! transaction's `'static` closure costs a refcount bump per retry attempt.
//!
//! # Names
//!
//! Full Prometheus names — `{prefix}_…`, `_total` on counters, `_seconds` on the
//! duration histogram — with **no** `.with_unit(..)` hint, so the rendered series
//! name is the same whether the downstream collector runs with
//! `add_metric_suffixes` on or off. The prefix is
//! [`MetricsConfig`](crate::config::MetricsConfig), defaulting to
//! `snake_case(gear name)` = `types_registry`; the suffixes after it are this
//! module's contract and are pinned by `metrics_tests`. This gear declares no
//! exporter and exposes no `/metrics` endpoint; telemetry is OTLP-pushed by
//! `ToolKit`.

use std::sync::Arc;
use std::time::Duration;

use opentelemetry::metrics::{Counter, Histogram, Meter};
use opentelemetry::{InstrumentationScope, KeyValue};

use crate::domain::admission::vector::VectorDrift;
use crate::domain::ports::metrics::{AdmissionMetrics, RefusalStage, TerminalStatus};

/// The instrumentation scope every instrument here is declared on — the crate
/// name, as the other gears' meters use.
///
/// Unrelated to the admission spans, which are `types_registry.admission.*`:
/// the scope labels the *emitting library* for a metrics backend, and the span
/// name is what an operator greps.
pub const SCOPE: &str = "cf-gears-types-registry";

/// Bucket boundaries for `types_registry_activation_write_set`.
///
/// The top bucket is the default `limits.activation_write_set` (512), so a
/// refresh *at* the operator's bound lands in the last bucket rather than in
/// `+Inf`; anything past the end was refused rather than committed. The lower
/// boundaries are dense because the measured fan-out in practice is small
/// (`refresh`'s module notes a maximum of 27) and "did this revision refresh
/// nothing, one thing, or a dozen" is the question worth resolving.
pub const ACTIVATION_WRITE_SET_BUCKETS: [f64; 10] =
    [0.0, 1.0, 2.0, 5.0, 10.0, 25.0, 50.0, 100.0, 256.0, 512.0];

/// Bucket boundaries (seconds) for `types_registry_operation_duration_seconds`.
///
/// One pass is an accept-then-admit round trip: a handful of short transactions
/// plus one CPU-bound validation. The range brackets a sub-100 ms local admission
/// at the bottom and leaves headroom for a batch that revalidates several times
/// at the top.
pub const OPERATION_DURATION_BUCKETS_SECONDS: [f64; 10] =
    [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0];

/// The *shape* of a drift, never the identifier that drifted.
const fn drift_label(drift: &VectorDrift) -> &'static str {
    match drift {
        VectorDrift::Appeared { .. } => "appeared",
        VectorDrift::Vanished { .. } => "vanished",
        VectorDrift::Moved { .. } => "moved",
        VectorDrift::Refreshed { .. } => "refreshed",
    }
}

/// The OpenTelemetry rendering of [`AdmissionMetrics`].
#[derive(Debug)]
pub struct AdmissionMetricsMeter {
    /// `types_registry_candidates_total{status}` — one increment per candidate
    /// **this pass** terminalized. A redelivery that reports an outcome another
    /// pass recorded does not increment: the counter counts decisions, not
    /// reports of them.
    candidates: Counter<u64>,
    /// `types_registry_refusals_total{stage,reason}`.
    refusals: Counter<u64>,
    /// `types_registry_revalidations_total{drift}` — one increment per *retry*
    /// taken, so a candidate that committed on its first attempt contributes
    /// nothing.
    revalidations: Counter<u64>,
    /// `types_registry_activation_write_set` — dependents rewritten by one
    /// revision (SPEC §8.1 step 4.6).
    activation_write_set: Histogram<f64>,
    /// `types_registry_operation_duration_seconds` — one admission pass,
    /// wall-clock.
    operation_duration: Histogram<f64>,
}

impl AdmissionMetricsMeter {
    /// Declare every instrument on `meter`.
    ///
    /// Public so a test can build the adapter against its own in-memory
    /// provider — that is the seam `infra::metrics_tests` asserts the
    /// instrument contract through.
    #[must_use]
    pub fn new(meter: &Meter, prefix: &str) -> Self {
        Self {
            candidates: meter
                .u64_counter(format!("{prefix}_candidates_total"))
                .with_description(
                    "Candidates terminalized by this pass, by terminal status \
                     (succeeded / unchanged / failed)",
                )
                .build(),
            refusals: meter
                .u64_counter(format!("{prefix}_refusals_total"))
                .with_description(
                    "Refusals by the stage that refused (acceptance / admission) and the \
                     machine reason",
                )
                .build(),
            revalidations: meter
                .u64_counter(format!("{prefix}_revalidations_total"))
                .with_description(
                    "Revalidation retries taken after the commit-time revision-vector guard \
                     fired, by drift shape",
                )
                .build(),
            activation_write_set: meter
                .f64_histogram(format!("{prefix}_activation_write_set"))
                .with_description("Dependents whose effective artifacts one revision rewrote")
                .with_boundaries(ACTIVATION_WRITE_SET_BUCKETS.to_vec())
                .build(),
            operation_duration: meter
                .f64_histogram(format!("{prefix}_operation_duration_seconds"))
                .with_description("One admission pass over an operation, wall-clock")
                .with_boundaries(OPERATION_DURATION_BUCKETS_SECONDS.to_vec())
                .build(),
        }
    }
}

impl AdmissionMetrics for AdmissionMetricsMeter {
    fn candidate_terminalized(&self, status: TerminalStatus) {
        self.candidates
            .add(1, &[KeyValue::new("status", status.label())]);
    }

    fn refused(&self, stage: RefusalStage, reason: &'static str) {
        self.refusals.add(
            1,
            &[
                KeyValue::new("stage", stage.label()),
                // `&'static str` straight into the label: the parameter type is
                // the closed-vocabulary guarantee, so no owned copy is needed.
                KeyValue::new("reason", reason),
            ],
        );
    }

    fn revalidation_retried(&self, drift: &VectorDrift) {
        self.revalidations
            .add(1, &[KeyValue::new("drift", drift_label(drift))]);
    }

    fn observe_activation_write_set(&self, refreshed: usize) {
        // The bound is `limits.activation_write_set`, whose default is 512 and
        // whose configured ceiling is far inside f64's exact-integer range, so
        // this conversion cannot lose a digit in practice. `cast_precision_loss`
        // is denied workspace-wide and the histogram API is f64-only.
        #[allow(clippy::cast_precision_loss)]
        self.activation_write_set.record(refreshed as f64, &[]);
    }

    fn observe_operation_duration(&self, elapsed: Duration) {
        self.operation_duration.record(elapsed.as_secs_f64(), &[]);
    }
}

/// Build the adapter on whatever `MeterProvider` is installed globally at the
/// moment this runs, ready for `TypesRegistryGear::init` to inject into the
/// service as an `Arc<dyn AdmissionMetrics>`.
///
/// `ToolKit` installs the real `SdkMeterProvider` before `Gear::init` runs, so
/// a production adapter lands on the OTLP-push pipeline; with no provider
/// installed the instruments are the API's no-ops and cost an atomic load per
/// call.
///
/// `prefix` is the resolved [`MetricsConfig`](crate::config::MetricsConfig)
/// prefix — the gear name in `snake_case` unless an operator set one.
#[must_use]
pub fn default_adapter(prefix: &str) -> Arc<AdmissionMetricsMeter> {
    let scope = InstrumentationScope::builder(SCOPE).build();
    Arc::new(AdmissionMetricsMeter::new(
        &opentelemetry::global::meter_with_scope(scope),
        prefix,
    ))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "metrics_tests.rs"]
mod metrics_tests;
