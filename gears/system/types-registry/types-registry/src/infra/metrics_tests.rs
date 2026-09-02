//! The instrument contract: rendered names, label keys, label values and bucket
//! layouts.
//!
//! These are asserted rather than reviewed because they are the *only* part of
//! the adapter a dashboard or an alert rule depends on. A mistyped name, a
//! dropped `_total`, a renamed label value — each of those is invisible in a code
//! review and silently empties a panel. Every test below builds its own
//! [`SdkMeterProvider`] over an in-memory exporter, so nothing here touches the
//! process-global provider the production adapter binds to.
//!
//! The *emission sites* — that the real admission path reaches these instruments
//! with the right labels — are `tests/observability_test.rs`.

use opentelemetry::metrics::MeterProvider;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{
    InMemoryMetricExporter, InMemoryMetricExporterBuilder, PeriodicReader, SdkMeterProvider,
    Temporality,
};

use super::{
    ACTIVATION_WRITE_SET_BUCKETS, AdmissionMetricsMeter, OPERATION_DURATION_BUCKETS_SECONDS, SCOPE,
};
use crate::domain::admission::vector::{VectorDrift, VectorRole};
use crate::domain::ports::metrics::{AdmissionMetrics, RefusalStage, TerminalStatus};

/// The prefix every name below is asserted under — derived from the gear name
/// through the real config path, not spelled as a literal. A change to
/// `MetricsConfig`'s default that moved every series would fail here rather
/// than silently renaming an operator's dashboards.
fn default_prefix() -> String {
    crate::config::MetricsConfig::default().effective_prefix("types-registry")
}

fn recorder() -> (
    SdkMeterProvider,
    InMemoryMetricExporter,
    AdmissionMetricsMeter,
) {
    let exporter = InMemoryMetricExporterBuilder::new()
        .with_temporality(Temporality::Delta)
        .build();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    let metrics = AdmissionMetricsMeter::new(&provider.meter(SCOPE), &default_prefix());
    (provider, exporter, metrics)
}

fn counter_sum(exporter: &InMemoryMetricExporter, name: &str) -> u64 {
    let metrics = exporter.get_finished_metrics().unwrap();
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data()
                {
                    return sum
                        .data_points()
                        .map(opentelemetry_sdk::metrics::data::SumDataPoint::value)
                        .sum();
                }
            }
        }
    }
    0
}

fn counter_sum_where(
    exporter: &InMemoryMetricExporter,
    name: &str,
    labels: &[(&str, &str)],
) -> u64 {
    let metrics = exporter.get_finished_metrics().unwrap();
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data()
                {
                    return sum
                        .data_points()
                        .filter(|dp| {
                            labels.iter().all(|(key, value)| {
                                dp.attributes().any(|kv| {
                                    kv.key.as_str() == *key && kv.value.as_str() == *value
                                })
                            })
                        })
                        .map(opentelemetry_sdk::metrics::data::SumDataPoint::value)
                        .sum();
                }
            }
        }
    }
    0
}

fn histogram_bounds(exporter: &InMemoryMetricExporter, name: &str) -> Option<Vec<f64>> {
    let metrics = exporter.get_finished_metrics().unwrap();
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::F64(MetricData::Histogram(h)) = metric.data()
                {
                    return h.data_points().next().map(|dp| dp.bounds().collect());
                }
            }
        }
    }
    None
}

fn histogram_sum(exporter: &InMemoryMetricExporter, name: &str) -> Option<f64> {
    let metrics = exporter.get_finished_metrics().unwrap();
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::F64(MetricData::Histogram(h)) = metric.data()
                {
                    return h
                        .data_points()
                        .next()
                        .map(opentelemetry_sdk::metrics::data::HistogramDataPoint::sum);
                }
            }
        }
    }
    None
}

fn histogram_count(exporter: &InMemoryMetricExporter, name: &str) -> u64 {
    let metrics = exporter.get_finished_metrics().unwrap();
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::F64(MetricData::Histogram(h)) = metric.data()
                {
                    return h
                        .data_points()
                        .map(opentelemetry_sdk::metrics::data::HistogramDataPoint::count)
                        .sum();
                }
            }
        }
    }
    0
}

// ---------------------------------------------------------------------------
// Candidates by terminal status
// ---------------------------------------------------------------------------

/// The three terminal statuses are separate series under one name, and the label
/// values are the snake-case wire spellings rather than the `Debug` of the enum:
/// `use_debug` is denied precisely so a derive's output cannot become a contract.
#[test]
fn candidates_are_counted_by_their_terminal_status() {
    let (provider, exporter, metrics) = recorder();

    metrics.candidate_terminalized(TerminalStatus::Succeeded);
    metrics.candidate_terminalized(TerminalStatus::Succeeded);
    metrics.candidate_terminalized(TerminalStatus::Unchanged);
    metrics.candidate_terminalized(TerminalStatus::Failed);
    provider.force_flush().unwrap();

    assert_eq!(counter_sum(&exporter, "types_registry_candidates_total"), 4);
    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_candidates_total",
            &[("status", "succeeded")],
        ),
        2,
    );
    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_candidates_total",
            &[("status", "unchanged")],
        ),
        1,
    );
    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_candidates_total",
            &[("status", "failed")],
        ),
        1,
    );
}

/// A non-terminal status is not a candidate outcome, and counting one would put a
/// value in the series that can never be an end state. The port's parameter type
/// makes that unrepresentable: `Pending` and `Running` do not convert to
/// [`TerminalStatus`], so a mis-wired call site fails to compile instead of
/// silently dropping the count.
#[test]
fn a_non_terminal_status_does_not_convert_to_a_terminal_one() {
    use crate::domain::enums::OperationItemStatus;

    assert!(TerminalStatus::try_from(OperationItemStatus::Pending).is_err());
    assert!(TerminalStatus::try_from(OperationItemStatus::Running).is_err());
    for status in [
        OperationItemStatus::Succeeded,
        OperationItemStatus::Unchanged,
        OperationItemStatus::Failed,
    ] {
        assert!(
            TerminalStatus::try_from(status).is_ok(),
            "{status:?} is terminal and must convert"
        );
    }
}

// ---------------------------------------------------------------------------
// Refusals by reason
// ---------------------------------------------------------------------------

/// Both stages count into one name and are told apart by the `stage` label, so
/// "how many submissions were refused at all" is one query and "which stage
/// refused them" is a `by (stage)`.
#[test]
fn refusals_carry_their_stage_and_reason() {
    let (provider, exporter, metrics) = recorder();

    metrics.refused(RefusalStage::Acceptance, "empty_batch");
    metrics.refused(RefusalStage::Admission, "precondition_failed");
    metrics.refused(RefusalStage::Admission, "precondition_failed");
    provider.force_flush().unwrap();

    assert_eq!(counter_sum(&exporter, "types_registry_refusals_total"), 3);
    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_refusals_total",
            &[("stage", "acceptance"), ("reason", "empty_batch")],
        ),
        1,
    );
    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_refusals_total",
            &[("stage", "admission"), ("reason", "precondition_failed")],
        ),
        2,
    );
}

/// Two different reasons at one stage are two series, which is the whole of
/// acceptance criterion "countable **and distinguishable**". A single
/// `refusals_total` with no reason label would satisfy "countable" and none of the
/// rest.
#[test]
fn two_reasons_at_one_stage_are_two_series() {
    let (provider, exporter, metrics) = recorder();

    metrics.refused(RefusalStage::Acceptance, "empty_batch");
    metrics.refused(RefusalStage::Acceptance, "duplicate_candidate");
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_refusals_total",
            &[("reason", "empty_batch")],
        ),
        1,
    );
    assert_eq!(
        counter_sum_where(
            &exporter,
            "types_registry_refusals_total",
            &[("reason", "duplicate_candidate")],
        ),
        1,
    );
}

// ---------------------------------------------------------------------------
// Revalidation retries
// ---------------------------------------------------------------------------

/// The `drift` label is the *shape* of the drift, never the identifier that
/// drifted: `gts_id` is unbounded and would make this series grow with the
/// registry. The four shapes are the four `VectorDrift` variants, so the label
/// vocabulary is closed by the type.
#[test]
fn revalidation_retries_are_counted_by_drift_shape() {
    let (provider, exporter, metrics) = recorder();

    metrics.revalidation_retried(&VectorDrift::Moved {
        gts_id: "x".to_owned(),
        role: VectorRole::Dependency,
        recorded: 1,
        found: 2,
    });
    metrics.revalidation_retried(&VectorDrift::Appeared {
        gts_id: "y".to_owned(),
        role: VectorRole::Dependent,
    });
    metrics.revalidation_retried(&VectorDrift::Vanished {
        gts_id: "z".to_owned(),
        role: VectorRole::Dependency,
    });
    metrics.revalidation_retried(&VectorDrift::Refreshed {
        gts_id: "w".to_owned(),
    });
    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum(&exporter, "types_registry_revalidations_total"),
        4,
    );
    for shape in ["moved", "appeared", "vanished", "refreshed"] {
        assert_eq!(
            counter_sum_where(
                &exporter,
                "types_registry_revalidations_total",
                &[("drift", shape)],
            ),
            1,
            "one retry per drift shape, missing {shape}",
        );
    }
}

// ---------------------------------------------------------------------------
// The two histograms
// ---------------------------------------------------------------------------

/// The bound an operator configures is `limits.activation_write_set`, so the top
/// bucket tracks it: a refresh at the bound falls in the last bucket rather than
/// in `+Inf`, and a refusal for exceeding it is the only thing past the end.
///
/// The top bucket is asserted against `Limits::default()` itself — not against
/// the same literal the constant was defined from, which could never fail.
#[test]
#[allow(clippy::cast_precision_loss)]
fn activation_write_set_buckets_reach_the_configured_default_bound() {
    let (provider, exporter, metrics) = recorder();

    metrics.observe_activation_write_set(3);
    provider.force_flush().unwrap();

    assert_eq!(
        histogram_bounds(&exporter, "types_registry_activation_write_set"),
        Some(ACTIVATION_WRITE_SET_BUCKETS.to_vec()),
    );
    assert_eq!(
        ACTIVATION_WRITE_SET_BUCKETS.last().copied(),
        Some(crate::config::Limits::default().activation_write_set as f64),
        "the top bucket tracks the default limits.activation_write_set",
    );
    assert_eq!(
        histogram_sum(&exporter, "types_registry_activation_write_set"),
        Some(3.0),
    );
}

/// A zero-dependent revision is recorded, not skipped. "How often does a revision
/// refresh nothing" is the question the bottom bucket answers, and omitting the
/// observation would make the histogram's count disagree with the number of
/// revisions.
#[test]
fn an_empty_activation_write_set_is_still_observed() {
    let (provider, exporter, metrics) = recorder();

    metrics.observe_activation_write_set(0);
    provider.force_flush().unwrap();

    assert_eq!(
        histogram_count(&exporter, "types_registry_activation_write_set"),
        1,
    );
    assert_eq!(
        histogram_sum(&exporter, "types_registry_activation_write_set"),
        Some(0.0),
    );
}

/// Seconds, and named `_seconds`, so the rendered series name is identical whether
/// the downstream collector appends unit suffixes or not — the reason no
/// `.with_unit()` hint is set on it.
#[test]
fn operation_duration_is_recorded_in_seconds() {
    let (provider, exporter, metrics) = recorder();

    metrics.observe_operation_duration(std::time::Duration::from_millis(250));
    provider.force_flush().unwrap();

    assert_eq!(
        histogram_bounds(&exporter, "types_registry_operation_duration_seconds"),
        Some(OPERATION_DURATION_BUCKETS_SECONDS.to_vec()),
    );
    let sum = histogram_sum(&exporter, "types_registry_operation_duration_seconds")
        .expect("the duration must be observed");
    assert!(
        (sum - 0.25).abs() < 1e-9,
        "250ms must be recorded as 0.25s, got {sum}",
    );
}

/// The default prefix is what the asserted names above are built from, and it
/// comes from the gear name rather than a literal in the adapter.
#[test]
fn the_default_prefix_is_the_gear_name_in_snake_case() {
    assert_eq!(default_prefix(), "types_registry");
}

/// A configured prefix renames every series, so an operator running two
/// registries against one collector can tell them apart. The suffixes after the
/// prefix stay put — they are this module's contract.
#[test]
fn a_configured_prefix_renames_every_series() {
    let exporter = InMemoryMetricExporterBuilder::new()
        .with_temporality(Temporality::Delta)
        .build();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    let metrics = AdmissionMetricsMeter::new(&provider.meter(SCOPE), "tenant_a_tr");

    metrics.candidate_terminalized(TerminalStatus::Succeeded);
    metrics.refused(RefusalStage::Acceptance, "zero_precondition");
    metrics.observe_activation_write_set(1);
    metrics.observe_operation_duration(std::time::Duration::from_millis(5));
    provider.force_flush().unwrap();

    let names = recorded_names(&exporter);
    for suffix in [
        "candidates_total",
        "refusals_total",
        "activation_write_set",
        "operation_duration_seconds",
    ] {
        assert!(
            names.contains(&format!("tenant_a_tr_{suffix}")),
            "expected tenant_a_tr_{suffix} among {names:?}"
        );
        assert!(
            !names.contains(&format!("types_registry_{suffix}")),
            "the default prefix must not survive a configured one: {names:?}"
        );
    }
}

/// Every instrument name the exporter saw.
fn recorded_names(exporter: &InMemoryMetricExporter) -> Vec<String> {
    let mut names = Vec::new();
    for rm in &exporter.get_finished_metrics().unwrap() {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                names.push(metric.name().to_owned());
            }
        }
    }
    names
}
