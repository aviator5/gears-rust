//! Startup seeding: admit the linked inventory and configured entities into the
//! database before the new APIs are published (T31, SPEC D6/D11/§8.4).
//!
//! One pass reads the current state, compares, and submits every differing
//! document in **one** operation with a compare-and-set precondition, so the
//! worker admits the whole dependency graph together. Equal active documents are
//! not submitted and keep their revisions; omitted documents are never deleted.
//! A pass that loses to a concurrent seeder re-reads and submits again under a new
//! key. One monotonic deadline bounds every read, submission, poll and pause.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde_json::Value;
use time::OffsetDateTime;
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;
use toolkit_security::PlatformSecurityContext;
use uuid::Uuid;

use crate::domain::admission::fingerprint::canonical_text;
use crate::domain::admission::{AdmissionFailureReason, Candidate, SubmitRequest};
use crate::domain::caller::CallerContext;
use crate::domain::enums::{LifecycleStatus, OperationItemStatus, OperationStatus};
use crate::domain::key::EntityKey;
use crate::domain::registry_service::{
    BatchGetItem, EntityLookup, MAX_BATCH_GET_KEYS, OperationRecord, RegistryService, ServiceError,
};
use crate::domain::selection::{EntityField, FieldSelection};

/// The `gts://` prefix of a Type Schema's `$id`.
const GTS_ID_URI_PREFIX: &str = "gts://";

/// First pause between two passes, and between two polls; each later one doubles.
const BACKOFF_INITIAL: Duration = Duration::from_millis(50);
const BACKOFF_MAX: Duration = Duration::from_secs(1);

/// The registry's own seed documents, by canonical identifier.
#[domain_model]
#[derive(Clone, Debug, Default)]
pub struct SeedSet {
    documents: BTreeMap<String, Seed>,
}

/// One seed and its canonical text, the form acceptance stores and compares: it
/// keeps every byte-level distinction, `-0.0` against `0.0` included.
#[domain_model]
#[derive(Clone, Debug)]
struct Seed {
    document: Value,
    canonical: String,
}

impl SeedSet {
    /// Identify each document by the first of `id_fields` it carries, as the
    /// legacy catalogue does. Identical duplicates collapse; differing ones refuse.
    ///
    /// # Errors
    /// [`SeedError::MissingIdentifier`] or [`SeedError::ConflictingDeclarations`].
    pub fn collect(
        documents: impl IntoIterator<Item = Value>,
        id_fields: &[String],
    ) -> Result<Self, SeedError> {
        let mut set = BTreeMap::new();
        for (index, document) in documents.into_iter().enumerate() {
            let gts_id =
                identifier(&document, id_fields).ok_or_else(|| SeedError::MissingIdentifier {
                    index,
                    fields: id_fields.to_vec(),
                })?;
            let canonical = canonical_text(&document);
            match set.get(&gts_id) {
                Some(Seed {
                    canonical: existing,
                    ..
                }) if *existing != canonical => {
                    return Err(SeedError::ConflictingDeclarations { gts_id });
                }
                Some(_) => {}
                None => {
                    set.insert(
                        gts_id,
                        Seed {
                            document,
                            canonical,
                        },
                    );
                }
            }
        }
        Ok(Self { documents: set })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.documents.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }
}

fn identifier(document: &Value, id_fields: &[String]) -> Option<String> {
    id_fields.iter().find_map(|field| {
        let id = document.get(field)?.as_str()?;
        let id = if field == "$id" {
            id.strip_prefix(GTS_ID_URI_PREFIX).unwrap_or(id)
        } else {
            id
        };
        Some(id.to_owned())
    })
}

/// One seed that did not reach the database, and why.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeedFailure {
    pub gts_id: String,
    /// The admission failure reason, as its wire code.
    pub reason: String,
    pub message: String,
}

impl std::fmt::Display for SeedFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}): {}", self.gts_id, self.reason, self.message)
    }
}

fn list(failures: &[SeedFailure]) -> String {
    failures
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Why seeding did not admit the whole set. Every variant fails boot.
#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum SeedError {
    #[error("seed document #{index} names no identifier in any of {fields:?}")]
    MissingIdentifier { index: usize, fields: Vec<String> },
    #[error("seed '{gts_id}' is declared twice with different documents")]
    ConflictingDeclarations { gts_id: String },
    #[error(
        "the seed set has {count} documents, but limits.batch_candidates admits at most \
         {limit} in the one operation seeding uses"
    )]
    TooLarge { count: usize, limit: usize },
    #[error("the registry refused the seed submission: {0}")]
    Refused(#[source] ServiceError),
    #[error("the registry failed while seeding: {0}")]
    Service(#[source] ServiceError),
    #[error("{} seed(s) were rejected: {}", .0.len(), list(.0))]
    Rejected(Vec<SeedFailure>),
    #[error("seeding did not finish within {budget:?}; undecided: {}", list(.undecided))]
    Deadline {
        budget: Duration,
        undecided: Vec<SeedFailure>,
    },
    #[error("seeding was cancelled")]
    Cancelled,
    #[error("seed operation {operation_id} completed inconsistently: {detail}")]
    Protocol { operation_id: Uuid, detail: String },
}

/// What a successful seed did.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeedSummary {
    pub documents: usize,
    /// Already present, active and equal on the first read.
    pub unchanged: usize,
    /// Created or revised by this call (or by a concurrent seeder it converged with).
    pub admitted: usize,
    pub passes: u32,
}

/// The single budget of one seed call.
struct Budget<'c> {
    deadline: Instant,
    cancel: &'c CancellationToken,
}

#[derive(Clone, Copy)]
enum Stop {
    Deadline,
    Cancelled,
}

impl Budget<'_> {
    async fn bounded<F: Future>(&self, future: F) -> Result<F::Output, Stop> {
        if self.cancel.is_cancelled() {
            return Err(Stop::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(Stop::Deadline);
        }
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(Stop::Cancelled),
            () = sleep_until(self.deadline) => Err(Stop::Deadline),
            output = future => Ok(output),
        }
    }

    /// Whether the budget still holds: checked before every success, because the
    /// synchronous work after the last await is not bounded by `select!`.
    fn check(&self) -> Result<(), Stop> {
        if self.cancel.is_cancelled() {
            Err(Stop::Cancelled)
        } else if Instant::now() >= self.deadline {
            Err(Stop::Deadline)
        } else {
            Ok(())
        }
    }

    async fn pause(&self, backoff: Duration) -> Result<(), Stop> {
        let wake = Instant::now()
            .checked_add(backoff)
            .map_or(self.deadline, |wake| wake.min(self.deadline));
        self.bounded(sleep_until(wake)).await
    }
}

/// The current state of one stored seed.
struct Current {
    /// Canonical text of the stored authored document.
    content: String,
    resource_version: i64,
    lifecycle: LifecycleStatus,
}

/// How one pass ended.
enum PassEnd {
    /// Nothing differed: every seed is stored, active and equal.
    UpToDate,
    /// Every submitted candidate was admitted.
    Admitted,
    /// Lost to a concurrent writer and nothing else: read again.
    Contended(Vec<SeedFailure>),
    Failed(SeedError),
}

/// Admit `set` through the registry and wait for every outcome under `budget`.
///
/// The budget starts before the first read and is never reset. Only equal active
/// documents or successful outcomes count; any refusal, deadline or cancellation
/// fails with the identifiers involved.
///
/// # Errors
/// See [`SeedError`].
pub async fn seed(
    registry: &RegistryService,
    set: &SeedSet,
    budget: Duration,
    cancel: &CancellationToken,
) -> Result<SeedSummary, SeedError> {
    let started = Instant::now();
    let window = Budget {
        deadline: started.checked_add(budget).unwrap_or(started),
        cancel,
    };
    // Checked on the whole set, equal documents included: a populated database
    // must not let a set grow past the limit a cold start would refuse.
    let limit = registry.limits().batch_candidates;
    if set.len() > limit {
        return Err(SeedError::TooLarge {
            count: set.len(),
            limit,
        });
    }
    let mut summary = SeedSummary {
        documents: set.len(),
        unchanged: 0,
        admitted: 0,
        passes: 0,
    };
    if set.is_empty() {
        return window
            .check()
            .map(|()| summary)
            .map_err(|stop| stopped(stop, budget, set, Vec::new()));
    }
    let ctx = PlatformSecurityContext::outbound_marker();
    let seeder = Seeder {
        registry,
        caller: CallerContext::Platform(&ctx),
        window: &window,
    };
    let mut backoff = BACKOFF_INITIAL;
    let mut contention = Vec::new();
    loop {
        summary.passes += 1;
        let end = match seeder.pass(set, &mut summary).await {
            Ok(end) => end,
            Err(stop) => return Err(stopped(stop, budget, set, contention)),
        };
        match end {
            PassEnd::UpToDate | PassEnd::Admitted => {
                if let Err(stop) = window.check() {
                    return Err(stopped(stop, budget, set, contention));
                }
                summary.admitted = summary.documents - summary.unchanged;
                return Ok(summary);
            }
            PassEnd::Failed(error) => return Err(error),
            PassEnd::Contended(failures) => {
                tracing::info!(
                    pass = summary.passes,
                    contended = failures.len(),
                    "types_registry seed lost to a concurrent writer; reading again"
                );
                contention = failures;
            }
        }
        if let Err(stop) = window.pause(backoff).await {
            return Err(stopped(stop, budget, set, contention));
        }
        backoff = backoff.saturating_mul(2).min(BACKOFF_MAX);
    }
}

/// The error for a stop, naming what was still undecided and the latest cause known.
fn stopped(stop: Stop, budget: Duration, set: &SeedSet, contention: Vec<SeedFailure>) -> SeedError {
    match stop {
        Stop::Cancelled => SeedError::Cancelled,
        Stop::Deadline => {
            let undecided = if contention.is_empty() {
                set.documents
                    .keys()
                    .map(|gts_id| SeedFailure {
                        gts_id: gts_id.clone(),
                        reason: "deadline_exceeded".to_owned(),
                        message: "not confirmed before the seed deadline".to_owned(),
                    })
                    .collect()
            } else {
                contention
            };
            SeedError::Deadline { budget, undecided }
        }
    }
}

struct Seeder<'s> {
    registry: &'s RegistryService,
    caller: CallerContext<'s>,
    window: &'s Budget<'s>,
}

impl Seeder<'_> {
    /// One read → compare → submit → await pass.
    async fn pass(&self, set: &SeedSet, summary: &mut SeedSummary) -> Result<PassEnd, Stop> {
        let current = match self.read(set).await? {
            Ok(current) => current,
            Err(error) => return Ok(PassEnd::Failed(error)),
        };
        let candidates = compare(set, &current);
        if summary.passes == 1 {
            summary.unchanged = set.len() - candidates.len();
        }
        if candidates.is_empty() {
            return Ok(PassEnd::UpToDate);
        }
        // A fresh key per submission: a pass that changed nothing is never
        // resubmitted, and a failed operation must not be replayed on restart.
        let request = SubmitRequest {
            idempotency_key: Some(format!("types-registry-seed-{}", Uuid::new_v4())),
            dry_run: false,
            candidates,
        };
        let submitted = self
            .window
            .bounded(
                self.registry
                    .submit(self.caller, &request, OffsetDateTime::now_utc()),
            )
            .await?;
        let accepted = match submitted {
            Ok(accepted) => accepted,
            Err(error) => return Ok(PassEnd::Failed(SeedError::Refused(error))),
        };
        tracing::info!(
            operation_id = %accepted.operation_id,
            candidates = request.candidates.len(),
            "types_registry seed submitted"
        );
        let operation = match self.await_completion(accepted.operation_id).await? {
            Ok(operation) => operation,
            Err(error) => return Ok(PassEnd::Failed(error)),
        };
        Ok(judge(&request.candidates, &operation))
    }

    /// Read the stored state of every seed, in pages the batch read admits.
    async fn read(
        &self,
        set: &SeedSet,
    ) -> Result<Result<BTreeMap<String, Current>, SeedError>, Stop> {
        let selection = FieldSelection::from_fields([EntityField::Content, EntityField::Origin]);
        let ids: Vec<&String> = set.documents.keys().collect();
        let mut current = BTreeMap::new();
        for page in ids.chunks(MAX_BATCH_GET_KEYS) {
            let items: Vec<BatchGetItem> = page
                .iter()
                .map(|id| BatchGetItem::from(EntityKey::GtsId((*id).clone())))
                .collect();
            let answers = match self
                .window
                .bounded(self.registry.batch_get(self.caller, &items, selection))
                .await?
            {
                Ok(answers) => answers,
                Err(error) => return Ok(Err(SeedError::Service(error))),
            };
            for (key, lookup) in answers {
                let EntityKey::GtsId(gts_id) = key else {
                    continue;
                };
                match lookup {
                    EntityLookup::NotFound => {}
                    EntityLookup::Found { record, .. } => {
                        let (Some(origin), Some(content)) = (record.origin, record.content) else {
                            return Ok(Err(read_fault(&gts_id, "lacks a selected field")));
                        };
                        let Ok(content) = serde_json::from_str::<Value>(content.get()) else {
                            return Ok(Err(read_fault(&gts_id, "stored content is not JSON")));
                        };
                        let content = canonical_text(&content);
                        current.insert(
                            gts_id,
                            Current {
                                content,
                                resource_version: origin.resource_version,
                                lifecycle: record.lifecycle_status,
                            },
                        );
                    }
                    EntityLookup::Unchanged { .. } => {
                        return Ok(Err(read_fault(
                            &gts_id,
                            "answered unchanged unconditionally",
                        )));
                    }
                }
            }
        }
        Ok(Ok(current))
    }

    /// Poll the operation until it completes; outcomes commit per candidate.
    async fn await_completion(
        &self,
        operation_id: Uuid,
    ) -> Result<Result<OperationRecord, SeedError>, Stop> {
        let mut interval = BACKOFF_INITIAL;
        loop {
            let polled = self
                .window
                .bounded(self.registry.operation_record(operation_id))
                .await?;
            match polled {
                Ok(Some(operation)) if operation.status == OperationStatus::Completed => {
                    return Ok(Ok(operation));
                }
                Ok(Some(_)) => {}
                Ok(None) => {
                    return Ok(Err(SeedError::Protocol {
                        operation_id,
                        detail: "the accepted operation cannot be read".to_owned(),
                    }));
                }
                Err(error) => return Ok(Err(SeedError::Service(error))),
            }
            self.window.pause(interval).await?;
            interval = interval.saturating_mul(2).min(BACKOFF_MAX);
        }
    }
}

fn read_fault(gts_id: &str, why: &str) -> SeedError {
    SeedError::Service(ServiceError::CorruptDocument(format!(
        "the seed read of '{gts_id}' {why}"
    )))
}

/// The seeds to submit: absent ones as creations, differing or inactive ones as
/// revisions of the version read. An equal active seed is left alone.
fn compare(set: &SeedSet, current: &BTreeMap<String, Current>) -> Vec<Candidate> {
    set.documents
        .iter()
        .filter_map(|(gts_id, seed)| {
            let found = current.get(gts_id);
            if let Some(found) = found
                && found.lifecycle == LifecycleStatus::Active
                && found.content == seed.canonical
            {
                return None;
            }
            Some(Candidate {
                gts_id: gts_id.clone(),
                content: Some(seed.document.clone()),
                expected_resource_version: found.map(|f| f.resource_version),
                force: false,
            })
        })
        .collect()
}

/// Classify a completed operation: all admitted, contention only, or a refusal.
fn judge(candidates: &[Candidate], operation: &OperationRecord) -> PassEnd {
    let mut outcomes: BTreeMap<&str, Vec<&crate::domain::registry_service::OperationItemRecord>> =
        BTreeMap::new();
    for item in &operation.items {
        if let EntityKey::GtsId(gts_id) = &item.key {
            outcomes.entry(gts_id.as_str()).or_default().push(item);
        }
    }
    let mut rejected = Vec::new();
    let mut contended = Vec::new();
    let mut unreported = BTreeSet::new();
    for candidate in candidates {
        let Some([item]) = outcomes.get(candidate.gts_id.as_str()).map(Vec::as_slice) else {
            unreported.insert(candidate.gts_id.clone());
            continue;
        };
        match item.status {
            OperationItemStatus::Succeeded | OperationItemStatus::Unchanged => {}
            OperationItemStatus::Pending | OperationItemStatus::Running => {
                unreported.insert(candidate.gts_id.clone());
            }
            OperationItemStatus::Failed => {
                let failure = failure_of(&candidate.gts_id, item);
                if contention(&failure.reason) {
                    contended.push(failure);
                } else {
                    rejected.push(failure);
                }
            }
        }
    }
    if !unreported.is_empty() {
        return PassEnd::Failed(SeedError::Protocol {
            operation_id: operation.operation_id,
            detail: format!(
                "the completed operation did not decide {}",
                unreported.into_iter().collect::<Vec<_>>().join(", ")
            ),
        });
    }
    if !rejected.is_empty() {
        // A blocked descendant of a refused seed is reported with it.
        rejected.extend(contended);
        return PassEnd::Failed(SeedError::Rejected(rejected));
    }
    if contended.is_empty() {
        PassEnd::Admitted
    } else {
        PassEnd::Contended(contended)
    }
}

fn failure_of(
    gts_id: &str,
    item: &crate::domain::registry_service::OperationItemRecord,
) -> SeedFailure {
    let (reason, message) = match &item.error {
        Some(Ok(stored)) => (stored.reason.clone(), stored.message.clone()),
        Some(Err(unreadable)) => (
            unreadable.reason.as_wire().to_owned(),
            unreadable.cause.clone(),
        ),
        None => (
            "unknown".to_owned(),
            "the item failed without a stored reason".to_owned(),
        ),
    };
    SeedFailure {
        gts_id: gts_id.to_owned(),
        reason,
        message,
    }
}

/// Outcomes a concurrent writer causes: the next pass's read settles them. A
/// descendant blocked behind a contended seed is retried with it.
fn contention(reason: &str) -> bool {
    matches!(
        AdmissionFailureReason::from_wire(reason),
        AdmissionFailureReason::AlreadyExists
            | AdmissionFailureReason::PreconditionFailed
            | AdmissionFailureReason::RevalidationExhausted
            | AdmissionFailureReason::BlockedByDependency
            | AdmissionFailureReason::BlockedByPredecessor
    )
}

#[cfg(test)]
#[path = "seeding_tests.rs"]
mod seeding_tests;
