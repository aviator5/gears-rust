//! The admission path's emission sites (T16).
//!
//! `infra::metrics_tests` pins the *instrument contract* — names, labels,
//! buckets — against instruments a test built itself. This file asks the other
//! half of the question: does the real path actually reach those instruments,
//! with the labels the criteria name, on the outcomes the criteria name. So
//! nothing here builds an `AdmissionMetricsMeter`; every assertion is downstream
//! of a real `accept` or `run_operation`.
//!
//! # Why one global recorder and a serial lock
//!
//! Production binds the adapter to the process-global `MeterProvider` from
//! `Gear::init`; a test binary never runs gear init, so [`recorder`] installs an
//! in-memory provider **and binds the adapter to it**, once for this binary,
//! before any admission runs. The domain reaches the instruments through its
//! port's process-wide binding — see `domain::ports::metrics` for why that is a
//! binding rather than a threaded parameter.
//!
//! Two consequences, both handled rather than hoped away:
//!
//! * The exporter is shared, so every metric assertion holds [`SERIAL`] across
//!   reset → drive → flush → assert. That makes each figure exactly this test's
//!   own, with no `>=` hedging.
//! * `Temporality::Delta` is what makes the reset meaningful: under the default
//!   cumulative temporality a flush re-exports every count since process start,
//!   and `reset()` would clear the stored batches without clearing the sums.
//!
//! The span assertions filter the captured log by an identifier unique to their
//! own test, so the shared buffer costs them nothing — but they hold [`SERIAL`]
//! all the same, because their admissions increment the very counters the tests
//! above are measuring a delta of. Letting them run alongside made
//! `a_creation_observes_no_activation_write_set` see three successes instead of
//! one, intermittently.
//!
//! No `sleep`, no timer, no polling (SPEC §13): `force_flush` is synchronous and
//! the worker is a plain function.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

mod common;

use std::io;
use std::sync::{Arc, Mutex, OnceLock};

use opentelemetry::metrics::MeterProvider;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{
    InMemoryMetricExporter, InMemoryMetricExporterBuilder, PeriodicReader, SdkMeterProvider,
    Temporality,
};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::{DBProvider, DbError, DbTx};
use toolkit_gts::gts_id;
use tracing_subscriber::fmt::MakeWriter;
use uuid::Uuid;

use common::{
    PausePoint, PausingStores, TestDir, allow_all, stores, test_db, test_db_file, worker_settings,
};
use types_registry::config::{MetricsConfig, TypesRegistryConfig};
use types_registry::domain::admission::acceptance::{AcceptanceContext, AcceptanceError, accept};
use types_registry::domain::admission::worker::{
    ItemFailure, OperationOutcome, Tuning, WorkerError, reason_label, run_operation,
};
use types_registry::domain::admission::{Candidate, OperationDispatch, SubmitRequest};
use types_registry::domain::enums as domain_enums;
use types_registry::domain::enums::OperationItemStatus;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::ports::Stores;
use types_registry::domain::ports::metrics::{AdmissionMetrics, RefusalStage};
use types_registry::infra::metrics::{AdmissionMetricsMeter, SCOPE};

const NOW: OffsetDateTime = datetime!(2026-08-21 09:15:30 UTC);
const LATER: OffsetDateTime = datetime!(2026-08-21 10:20:40 UTC);

/// Its own family prefix, for the reason `dependency_test.rs` records: the
/// `SQLite` family lock is keyed per DSN across processes, so two binaries
/// admitting one family key contend even with isolated databases.
const SUBJECT: &str = gts_id!("cf.core.obsv.subject.v1~");
/// Reaches [`SUBJECT`] through a `$ref`, so a revision of [`SUBJECT`] refreshes it.
const REFERRER: &str = gts_id!("cf.core.obsv.referrer.v1~");
/// Never registered. Naming a version for it is the cheapest admission-stage
/// refusal there is: `commit_revision` refuses an absent identifier.
const ABSENT: &str = gts_id!("cf.core.obsv.absent.v1~");

type Provider = Arc<DBProvider<DbError>>;

/// Serializes the metric assertions — see the module header.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ---------------------------------------------------------------------------
// The recorder: one meter provider and one subscriber for the whole binary
// ---------------------------------------------------------------------------

/// The captured `types_registry` log, shared by every test in this binary.
static LOG: Mutex<Vec<u8>> = Mutex::new(Vec::new());

struct LogWriter;

impl io::Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        LOG.lock().expect("log buffer").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl MakeWriter<'_> for LogWriter {
    type Writer = Self;

    fn make_writer(&self) -> Self::Writer {
        Self
    }
}

/// Install the in-memory meter provider and the log-capturing subscriber, once.
///
/// Called at the top of every test. [`metrics`] is the adapter over this
/// provider, and every `accept` / `run_operation` below is handed it — the
/// test-binary stand-in for what `Gear::init` injects in production, and what
/// makes the domain's instruments land on this exporter rather than on a no-op.
fn recorder() -> &'static (SdkMeterProvider, InMemoryMetricExporter) {
    static RECORDER: OnceLock<(SdkMeterProvider, InMemoryMetricExporter)> = OnceLock::new();
    RECORDER.get_or_init(|| {
        let exporter = InMemoryMetricExporterBuilder::new()
            .with_temporality(Temporality::Delta)
            .build();
        let provider = SdkMeterProvider::builder()
            .with_reader(PeriodicReader::builder(exporter.clone()).build())
            .build();
        opentelemetry::global::set_meter_provider(provider.clone());

        // Only this gear, and only down to DEBUG: the point is to read the
        // admission path's own spans, and a `trace`-level catch-all would bury
        // them under every statement `sea-orm` logs.
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter("types_registry=debug")
            .with_writer(LogWriter)
            .with_ansi(false)
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("this binary installs exactly one subscriber");

        (provider, exporter)
    })
}

/// Everything captured so far. Read whole and filtered by the caller, because the
/// buffer is shared with every other test in this binary.
fn captured_log() -> String {
    String::from_utf8_lossy(&LOG.lock().expect("log buffer")).into_owned()
}

/// The captured lines mentioning `needle` — an identifier unique to one test, so
/// the result is that test's own output even under parallel execution.
fn lines_mentioning(needle: &str) -> Vec<String> {
    captured_log()
        .lines()
        .filter(|line| line.contains(needle))
        .map(str::to_owned)
        .collect()
}

// ---------------------------------------------------------------------------
// Reading the exporter
// ---------------------------------------------------------------------------

/// The adapter every admission in this file is handed.
///
/// Built once over [`recorder`]'s provider, under the default prefix, so the
/// series names asserted below are the ones production renders.
fn metrics() -> &'static Arc<dyn AdmissionMetrics> {
    static METRICS: OnceLock<Arc<dyn AdmissionMetrics>> = OnceLock::new();
    METRICS.get_or_init(|| {
        let (provider, _) = recorder();
        Arc::new(AdmissionMetricsMeter::new(
            &provider.meter(SCOPE),
            &MetricsConfig::default().effective_prefix("types-registry"),
        ))
    })
}

fn counter_sum_where(name: &str, labels: &[(&str, &str)]) -> u64 {
    let metrics = recorder().1.get_finished_metrics().unwrap();
    let mut total = 0;
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data()
                {
                    total += sum
                        .data_points()
                        .filter(|dp| {
                            labels.iter().all(|(key, value)| {
                                dp.attributes().any(|kv| {
                                    kv.key.as_str() == *key && kv.value.as_str() == *value
                                })
                            })
                        })
                        .map(opentelemetry_sdk::metrics::data::SumDataPoint::value)
                        .sum::<u64>();
                }
            }
        }
    }
    total
}

fn histogram_count(name: &str) -> u64 {
    let metrics = recorder().1.get_finished_metrics().unwrap();
    let mut total = 0;
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::F64(MetricData::Histogram(h)) = metric.data()
                {
                    total += h
                        .data_points()
                        .map(opentelemetry_sdk::metrics::data::HistogramDataPoint::count)
                        .sum::<u64>();
                }
            }
        }
    }
    total
}

fn histogram_sum(name: &str) -> f64 {
    let metrics = recorder().1.get_finished_metrics().unwrap();
    let mut total = 0.0;
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::F64(MetricData::Histogram(h)) = metric.data()
                {
                    total += h
                        .data_points()
                        .map(opentelemetry_sdk::metrics::data::HistogramDataPoint::sum)
                        .sum::<f64>();
                }
            }
        }
    }
    total
}

/// Discard everything recorded before the test's own work.
fn reset_metrics() {
    let (provider, exporter) = recorder();
    provider.force_flush().expect("flush");
    exporter.reset();
}

fn flush() {
    recorder().0.force_flush().expect("flush");
}

// ---------------------------------------------------------------------------
// Driving one admission
// ---------------------------------------------------------------------------

struct NoDispatch;

#[async_trait::async_trait]
impl OperationDispatch for NoDispatch {
    async fn enqueue(&self, _tx: &DbTx<'_>, _operation_id: Uuid) -> anyhow::Result<()> {
        Ok(())
    }
}

fn worker(db: &Provider) -> DBProvider<WorkerError> {
    DBProvider::new(db.db())
}

fn subject_schema(property: &str) -> Value {
    json!({
        "$id": format!("gts://{SUBJECT}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { property: { "type": "string" } },
    })
}

/// `marker` is a second property, so a *revision* of this schema differs from
/// the admitted one — identical content commits as `Unchanged`, which writes
/// nothing and therefore never reaches the revision-vector guard.
fn referencing_schema(marker: &str) -> Value {
    json!({
        "$id": format!("gts://{REFERRER}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {
            "subject": { "$ref": format!("gts://{SUBJECT}") },
            marker: { "type": "string" },
        },
    })
}

fn absent_schema() -> Value {
    json!({
        "$id": format!("gts://{ABSENT}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
    })
}

async fn submit(
    db: &Provider,
    key: &str,
    candidates: Vec<Candidate>,
) -> Result<Uuid, AcceptanceError> {
    submit_via(db, key, Arc::new(NoDispatch), candidates).await
}

/// [`submit`] with the dispatcher chosen by the caller — the seam the
/// infrastructure-arm test needs, since a failing dispatch is the one
/// `AcceptanceError` arm that is drivable end to end without breaking the
/// database.
async fn submit_via(
    db: &Provider,
    key: &str,
    dispatch: Arc<dyn OperationDispatch>,
    candidates: Vec<Candidate>,
) -> Result<Uuid, AcceptanceError> {
    let provider: DBProvider<AcceptanceError> = DBProvider::new(db.db());
    let policy = RegistrationPolicy::default();
    let config = TypesRegistryConfig::default();
    accept(
        &stores(),
        &provider,
        &allow_all(),
        &AcceptanceContext {
            policy: &policy,
            config: &config,
            metrics: metrics(),
        },
        &dispatch,
        &SubmitRequest {
            idempotency_key: key.to_owned(),
            kind: domain_enums::OperationKind::Registration,
            dry_run: false,
            candidates,
        },
        NOW,
    )
    .await
    .map(|accepted| accepted.operation_id)
}

fn candidate(gts_id: &str, content: Value, expected_resource_version: Option<i64>) -> Candidate {
    Candidate {
        gts_id: gts_id.to_owned(),
        content: Some(content),
        expected_resource_version,
        force: false,
    }
}

async fn admit(
    db: &Provider,
    key: &str,
    gts_id: &str,
    content: Value,
    expected_resource_version: Option<i64>,
) -> OperationOutcome {
    let operation_id = submit(
        db,
        key,
        vec![candidate(gts_id, content, expected_resource_version)],
    )
    .await
    .expect("acceptance");
    run_operation(
        &stores(),
        &worker(db),
        &allow_all(),
        Tuning {
            limits: &common::limits(),
            worker: &worker_settings(),
            metrics: metrics(),
        },
        operation_id,
        LATER,
    )
    .await
    .expect("the worker must not fail on infrastructure")
}

// ---------------------------------------------------------------------------
// Candidates by terminal status
// ---------------------------------------------------------------------------

/// A first admission counts one `succeeded` candidate and observes one pass
/// duration — the two signals that answer "is the registry admitting anything,
/// and how long is it taking".
#[tokio::test]
async fn a_successful_admission_counts_one_succeeded_candidate_and_one_pass_duration() {
    let _serial = SERIAL.lock().await;
    recorder();
    let db = test_db().await;
    reset_metrics();

    let outcome = admit(&db, "k-ok", SUBJECT, subject_schema("name"), None).await;
    flush();

    assert_eq!(outcome.items[0].status, OperationItemStatus::Succeeded);
    assert_eq!(
        counter_sum_where(
            "types_registry_candidates_total",
            &[("status", "succeeded")],
        ),
        1,
    );
    assert_eq!(
        histogram_count("types_registry_operation_duration_seconds"),
        1,
        "one pass, one observation",
    );
}

/// Re-submitting identical content is `Unchanged`, and it must be countable
/// *apart* from a success: the two look the same to a client polling for
/// completion and are entirely different to an operator watching write volume.
#[tokio::test]
async fn a_redundant_resubmission_counts_an_unchanged_candidate() {
    let _serial = SERIAL.lock().await;
    recorder();
    let db = test_db().await;
    admit(&db, "k-first", SUBJECT, subject_schema("name"), None).await;
    reset_metrics();

    let outcome = admit(&db, "k-again", SUBJECT, subject_schema("name"), Some(1)).await;
    flush();

    assert_eq!(outcome.items[0].status, OperationItemStatus::Unchanged);
    assert_eq!(
        counter_sum_where(
            "types_registry_candidates_total",
            &[("status", "unchanged")],
        ),
        1,
    );
    assert_eq!(
        counter_sum_where(
            "types_registry_candidates_total",
            &[("status", "succeeded")],
        ),
        0,
        "an unchanged re-submission is not a success",
    );
}

// ---------------------------------------------------------------------------
// Refusals by reason, at both stages
// ---------------------------------------------------------------------------

/// An admission-stage refusal is two facts: the candidate ended `failed`, and it
/// ended that way for a named reason. Both are counted, and the `stage` label is
/// what tells this refusal apart from one the acceptance path made.
#[tokio::test]
async fn an_admission_refusal_counts_a_failed_candidate_and_its_reason() {
    let _serial = SERIAL.lock().await;
    recorder();
    let db = test_db().await;
    reset_metrics();

    // A revision naming version 1 of an identifier the registry does not hold.
    let outcome = admit(&db, "k-absent", ABSENT, absent_schema(), Some(1)).await;
    flush();

    assert_eq!(outcome.items[0].status, OperationItemStatus::Failed);
    assert_eq!(
        counter_sum_where("types_registry_candidates_total", &[("status", "failed")]),
        1,
    );
    assert_eq!(
        counter_sum_where(
            "types_registry_refusals_total",
            &[("stage", "admission"), ("reason", "precondition_failed")],
        ),
        1,
    );
    assert_eq!(
        counter_sum_where("types_registry_refusals_total", &[("stage", "acceptance")],),
        0,
        "the request was accepted; only the candidate was refused",
    );
}

/// An acceptance-stage refusal never becomes an operation, so no item status can
/// carry it. The counter is the only place it is visible.
#[tokio::test]
async fn an_acceptance_refusal_counts_under_its_own_stage_and_reason() {
    let _serial = SERIAL.lock().await;
    recorder();
    let db = test_db().await;
    reset_metrics();

    let refused = submit(&db, "k-empty", Vec::new()).await;
    flush();

    assert!(matches!(refused, Err(AcceptanceError::EmptyBatch)));
    assert_eq!(
        counter_sum_where(
            "types_registry_refusals_total",
            &[("stage", "acceptance"), ("reason", "empty_batch")],
        ),
        1,
    );
    assert_eq!(
        counter_sum_where("types_registry_candidates_total", &[]),
        0,
        "nothing was admitted, so no candidate reached a terminal status",
    );
}

/// Two different acceptance refusals are two series. This is the criterion
/// "countable **and distinguishable**" read against the real path rather than
/// against instruments a test built: it is `AcceptanceError::reason`'s
/// per-variant mapping that is under test here.
#[tokio::test]
async fn two_acceptance_refusals_are_told_apart_by_their_reason() {
    let _serial = SERIAL.lock().await;
    recorder();
    let db = test_db().await;
    reset_metrics();

    submit(
        &db,
        "k-dup",
        vec![
            candidate(SUBJECT, subject_schema("name"), None),
            candidate(SUBJECT, subject_schema("name"), None),
        ],
    )
    .await
    .expect_err("a duplicate candidate is refused");
    submit(
        &db,
        "k-zero",
        vec![candidate(SUBJECT, subject_schema("name"), Some(0))],
    )
    .await
    .expect_err("expected_resource_version 0 is refused");
    flush();

    assert_eq!(
        counter_sum_where(
            "types_registry_refusals_total",
            &[("reason", "duplicate_candidate")],
        ),
        1,
    );
    assert_eq!(
        counter_sum_where(
            "types_registry_refusals_total",
            &[("reason", "zero_precondition")],
        ),
        1,
    );
}

/// A refusal reason that is not provably a literal counts under the single closed
/// `other` label rather than becoming a series of its own — the cardinality rule
/// the `reason` parameter type enforces and `worker::reason_label` implements.
///
/// Driven through the bound adapter with the exact value `record_failure` passes
/// for the one owned-reason producer there is: a failure read back off a stored
/// `error_payload` (`ItemFailure::from_payload`). The live worker always refuses
/// with a fresh literal (pinned by the test above), so this is as close to that
/// site as the current call graph reaches.
#[tokio::test]
async fn a_read_back_failure_reason_counts_under_other_and_creates_no_new_series() {
    let _serial = SERIAL.lock().await;
    recorder();
    reset_metrics();

    let failure = ItemFailure::from_payload(
        r#"{"reason":"precondition_failed","message":"read back off a stored row"}"#,
    );
    assert!(
        matches!(failure.reason, std::borrow::Cow::Owned(_)),
        "from_payload is the owned-reason producer the mapping exists for"
    );
    metrics().refused(RefusalStage::Admission, reason_label(&failure.reason));
    flush();

    assert_eq!(
        counter_sum_where(
            "types_registry_refusals_total",
            &[("stage", "admission"), ("reason", "other")],
        ),
        1,
    );
    assert_eq!(
        counter_sum_where(
            "types_registry_refusals_total",
            &[("reason", "precondition_failed")],
        ),
        0,
        "the owned reason must count under `other`, not as its own series",
    );
}

/// An infrastructure arm of the acceptance path is a fault, not a refusal: it must
/// not emit the "refused a submission" `warn` (the REST boundary's
/// `opaque_internal` already logs it once, at `error`), while the counter still
/// counts it — the counter is the one place both kinds of outcome are meant to be
/// visible side by side, so the counting half is exactly what a careless edit to
/// the `warn`'s `if` would silently break.
///
/// `Dispatch` is the drivable infrastructure arm: the dispatcher is a port the
/// test owns, unlike the database.
#[tokio::test]
async fn an_infrastructure_failure_is_counted_but_not_warned_as_a_refusal() {
    // The infrastructure arm: validation passes, the dispatch fails. Declared
    // before the first statement, because an item after one reads as if it were
    // scoped to that point when it is in fact live for the whole body.
    struct FailingDispatch;
    #[async_trait::async_trait]
    impl OperationDispatch for FailingDispatch {
        async fn enqueue(&self, _tx: &DbTx<'_>, _operation_id: Uuid) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("observability-dispatch-outage"))
        }
    }

    let _serial = SERIAL.lock().await;
    recorder();
    let db = test_db().await;
    reset_metrics();

    // A client refusal, with an identifier unique to this test so its warn line
    // is filterable out of the shared log: the `warn` carries the error `Display`,
    // which names it.
    let client = gts_id!("cf.core.obsv.warnclient.v1~");
    let refused = submit(
        &db,
        "k-warn-client",
        vec![candidate(client, subject_schema("name"), Some(0))],
    )
    .await;
    assert!(matches!(
        refused,
        Err(AcceptanceError::ZeroPrecondition { .. })
    ));

    // Validation passes; the dispatch is what fails.
    let infra = gts_id!("cf.core.obsv.warninfra.v1~");
    let content = json!({
        "$id": format!("gts://{infra}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
    });
    let failed = submit_via(
        &db,
        "k-warn-infra",
        Arc::new(FailingDispatch),
        vec![candidate(infra, content, None)],
    )
    .await;
    assert!(
        matches!(failed, Err(AcceptanceError::Dispatch(_))),
        "the dispatch failure is the arm under test, got {failed:?}"
    );
    flush();

    assert!(
        lines_mentioning(client)
            .iter()
            .any(|line| line.contains("types_registry refused a submission")),
        "a client refusal is warned; captured:\n{}",
        captured_log()
    );
    // The suppressed warn was the only place the dispatch error's `Display`
    // would reach the log from `accept`, so its absence proves the suppression.
    assert!(
        !captured_log().contains("observability-dispatch-outage"),
        "an infrastructure arm must not be logged as a refusal; captured:\n{}",
        captured_log()
    );

    assert_eq!(
        counter_sum_where(
            "types_registry_refusals_total",
            &[("stage", "acceptance"), ("reason", "zero_precondition")],
        ),
        1,
        "the client refusal is counted",
    );
    assert_eq!(
        counter_sum_where(
            "types_registry_refusals_total",
            &[("stage", "acceptance"), ("reason", "dispatch_failure")],
        ),
        1,
        "the infrastructure fault is counted too; the counter has no `if`",
    );
}

// ---------------------------------------------------------------------------
// The activation write set
// ---------------------------------------------------------------------------

/// A revision of a schema something `$ref`s refreshes that dependent, and the
/// size of what it rewrote is the bound `limits.activation_write_set` governs —
/// so it is the figure an operator needs before raising or lowering that bound.
#[tokio::test]
async fn a_revision_observes_the_activation_write_set_it_rewrote() {
    let _serial = SERIAL.lock().await;
    recorder();
    let db = test_db().await;
    admit(&db, "k-subject", SUBJECT, subject_schema("name"), None).await;
    admit(
        &db,
        "k-referrer",
        REFERRER,
        referencing_schema("note"),
        None,
    )
    .await;
    reset_metrics();

    let outcome = admit(
        &db,
        "k-subject-2",
        SUBJECT,
        subject_schema("label"),
        Some(1),
    )
    .await;
    flush();

    assert_eq!(outcome.items[0].status, OperationItemStatus::Succeeded);
    assert_eq!(
        histogram_count("types_registry_activation_write_set"),
        1,
        "one revision, one observation",
    );
    let refreshed = histogram_sum("types_registry_activation_write_set");
    assert!(
        (refreshed - 1.0).abs() < f64::EPSILON,
        "exactly the one dependent was rewritten, got {refreshed}",
    );
}

/// A creation observes **nothing**, and that is the histogram's scope rather than
/// a gap: nothing can depend on an identifier the registry did not hold a moment
/// ago, so `commit_creation` runs no reverse-impact refresh at all. The series
/// therefore counts *revisions*, and reading its count as "admissions" would
/// overstate the denominator.
///
/// A zero **is** recorded for a revision whose dependents all recomputed to
/// identical bytes — `observability_tests.rs` pins that half.
#[tokio::test]
async fn a_creation_observes_no_activation_write_set() {
    let _serial = SERIAL.lock().await;
    recorder();
    let db = test_db().await;
    reset_metrics();

    let outcome = admit(&db, "k-lonely", SUBJECT, subject_schema("name"), None).await;
    flush();

    assert_eq!(outcome.items[0].status, OperationItemStatus::Succeeded);
    assert_eq!(
        histogram_count("types_registry_activation_write_set"),
        0,
        "a creation has no dependents to refresh, so it observes no write set",
    );
    assert_eq!(
        counter_sum_where(
            "types_registry_candidates_total",
            &[("status", "succeeded")],
        ),
        1,
        "the control: the pass did run, so the zero above is scope and not silence",
    );
}

// ---------------------------------------------------------------------------
// Revalidation retries
// ---------------------------------------------------------------------------

/// The retry counter is incremented by the guard's own arm, so it counts
/// *rollbacks*, not attempts: a candidate that commits first time contributes
/// nothing, and this one — whose dependency moves while its commit transaction is
/// held open at the boundary — contributes exactly one, under the shape of the
/// drift that caused it.
#[tokio::test]
async fn a_revalidation_retry_is_counted_by_its_drift_shape() {
    let _serial = SERIAL.lock().await;
    recorder();
    let dir = TestDir::new("types-registry-obsv-retry");
    let db = test_db_file(&dir.path().join("registry.db")).await;
    admit(&db, "k-subject", SUBJECT, subject_schema("name"), None).await;
    admit(
        &db,
        "k-referrer",
        REFERRER,
        referencing_schema("note"),
        None,
    )
    .await;

    let operation_id = submit(
        &db,
        "k-referrer-2",
        vec![candidate(REFERRER, referencing_schema("tag"), Some(1))],
    )
    .await
    .expect("acceptance");
    reset_metrics();

    let (paused, reached, resume) = PausingStores::new(PausePoint::RevisionEntityRead);
    let ports: Arc<dyn Stores> = paused;
    let provider = worker(&db);
    let pass = tokio::spawn(async move {
        run_operation(
            &ports,
            &provider,
            &allow_all(),
            Tuning {
                limits: &common::limits(),
                worker: &worker_settings(),
                metrics: metrics(),
            },
            operation_id,
            LATER,
        )
        .await
    });

    reached.await.expect("the pass must reach the commit");
    // A real committed revision of the dependency, landing in the gap.
    let mutating = Arc::clone(&db);
    admit(
        &mutating,
        "k-subject-2",
        SUBJECT,
        subject_schema("label"),
        Some(1),
    )
    .await;
    resume.send(()).expect("the pass must still be waiting");
    let outcome = pass
        .await
        .expect("the pass task must not panic")
        .expect("the worker must not fail on infrastructure");
    flush();

    assert_eq!(
        outcome.items[0].status,
        OperationItemStatus::Succeeded,
        "the retry must succeed, got {:?}",
        outcome.items[0],
    );
    assert_eq!(
        counter_sum_where("types_registry_revalidations_total", &[("drift", "moved")]),
        1,
        "one rollback, counted under the drift that caused it",
    );
}

// ---------------------------------------------------------------------------
// Spans
// ---------------------------------------------------------------------------

/// Every line the pass logs carries both spans, and between them the four fields
/// the criterion names: `operation_id`, `gts_id`, kind and dry-run mode.
///
/// Asserted on the admission log line rather than on a probe event, because what
/// is under test is that the *real* path opens the spans — a probe would only
/// prove the span constructors still work, which `observability_tests.rs` already
/// does.
#[tokio::test]
async fn the_real_pass_wraps_its_work_in_an_operation_span_and_a_unit_span() {
    let _serial = SERIAL.lock().await;
    recorder();
    let db = test_db().await;
    // Unique to this test, which is what makes the shared log filterable.
    let unique = gts_id!("cf.core.obsv.spanned.v1~");
    let content = json!({
        "$id": format!("gts://{unique}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
    });

    let outcome = admit(&db, "k-span", unique, content, None).await;
    let operation_id = outcome.operation_id;

    let lines = lines_mentioning(unique);
    assert!(
        !lines.is_empty(),
        "the admission must log something naming the candidate; captured:\n{}",
        captured_log()
    );
    let admitted = lines
        .iter()
        .find(|line| line.contains("candidate admitted"))
        .unwrap_or_else(|| panic!("no admission line among:\n{}", lines.join("\n")));

    assert!(
        admitted.contains("types_registry.admission.operation"),
        "the operation span must be on the line: {admitted}"
    );
    assert!(
        admitted.contains("types_registry.admission.unit"),
        "the unit span must be on the line: {admitted}"
    );
    assert!(
        admitted.contains(&operation_id.to_string()),
        "operation_id must be on the line: {admitted}"
    );
    assert!(
        admitted.contains(r#"kind="registration""#),
        "the operation kind must be on the line: {admitted}"
    );
    assert!(
        admitted.contains("dry_run=false"),
        "the dry-run mode must be on the line: {admitted}"
    );
}

/// The operation span's `kind` and `dry_run` are recorded from the row the pass
/// reads, not from the caller's request — so a pass driven by a redelivery, which
/// has no request in hand, carries them too.
///
/// The assertion is on the lines **the second pass alone** added: the first pass
/// also logs inside the span, so a whole-buffer assertion would pass even if the
/// redelivered pass recorded nothing.
#[tokio::test]
async fn a_redelivered_pass_still_carries_the_operation_facts() {
    let _serial = SERIAL.lock().await;
    recorder();
    let db = test_db().await;
    let unique = gts_id!("cf.core.obsv.redelivered.v1~");
    let content = json!({
        "$id": format!("gts://{unique}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
    });
    let operation_id = submit(&db, "k-redeliver", vec![candidate(unique, content, None)])
        .await
        .expect("acceptance");

    // The first pass completes the operation.
    run_operation(
        &stores(),
        &worker(&db),
        &allow_all(),
        Tuning {
            limits: &common::limits(),
            worker: &worker_settings(),
            metrics: metrics(),
        },
        operation_id,
        LATER,
    )
    .await
    .expect("the worker must not fail on infrastructure");
    let first_pass_lines = lines_mentioning(&operation_id.to_string()).len();

    // Discard the first pass's counts, so what the exporter holds below is the
    // redelivered pass alone.
    reset_metrics();

    // The second pass finds the operation terminal.
    run_operation(
        &stores(),
        &worker(&db),
        &allow_all(),
        Tuning {
            limits: &common::limits(),
            worker: &worker_settings(),
            metrics: metrics(),
        },
        operation_id,
        LATER,
    )
    .await
    .expect("the worker must not fail on infrastructure");
    flush();

    // Lines are appended to a shared buffer and the filter is by needle, so the
    // second pass's lines are exactly the ones past the first pass's count.
    let all_lines = lines_mentioning(&operation_id.to_string());
    let redelivered_lines = &all_lines[first_pass_lines..];
    assert!(
        !redelivered_lines.is_empty(),
        "the redelivered pass must emit its own log line; captured:\n{}",
        all_lines.join("\n")
    );
    assert!(
        redelivered_lines
            .iter()
            .any(|line| line.contains(r#"kind="registration""#)),
        "the operation facts must be recorded on the redelivered pass's span; \
         captured:\n{}",
        redelivered_lines.join("\n")
    );

    // And that pass is observationally a no-op beyond the duration it spent: no
    // candidate is terminalized a second time, because the counter counts
    // decisions and the redelivery reported one another pass decided.
    assert_eq!(
        counter_sum_where("types_registry_candidates_total", &[]),
        0,
        "a redelivered pass must not re-count the candidate it reported",
    );
    assert_eq!(
        histogram_count("types_registry_operation_duration_seconds"),
        1,
        "the redelivered pass still observes the one duration it spent",
    );
}
