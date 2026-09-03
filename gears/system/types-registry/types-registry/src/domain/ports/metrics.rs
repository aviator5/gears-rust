//! Output port for admission metrics.

use std::time::Duration;

use toolkit_macros::domain_model;

use crate::domain::admission::vector::VectorDrift;
use crate::domain::enums::OperationItemStatus;

/// Which half of SPEC §8.1 refused a submission.
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
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalStatus {
    Succeeded,
    Unchanged,
    Failed,
}

impl TerminalStatus {
    /// The snake-case label value — deliberately not `Debug`, so a derive's output cannot become
    /// the series contract.
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
            // The caller learns which status was not terminal rather than a bare "no": the
            // distinction is the whole reason this conversion can fail.
            non_terminal => Err(non_terminal),
        }
    }
}

/// The admission path's instrument set.
pub trait AdmissionMetrics: std::fmt::Debug + Send + Sync {
    /// `types_registry_candidates_total{status}` — one increment per candidate **this pass**
    /// terminalized, under its terminal status.
    fn candidate_terminalized(&self, status: TerminalStatus);

    /// `types_registry_refusals_total{stage,reason}` — one increment per refusal.
    fn refused(&self, stage: RefusalStage, reason: &'static str);

    /// `types_registry_revalidations_total{drift}` — one increment per *retry* taken, so a
    /// candidate that committed on its first attempt contributes nothing.
    fn revalidation_retried(&self, drift: &VectorDrift);

    /// `types_registry_activation_write_set` — dependents rewritten by one revision (SPEC §8.1 step
    /// 4.6), including zero.
    fn observe_activation_write_set(&self, refreshed: usize);

    /// `types_registry_operation_duration_seconds` — one admission pass, wall-clock.
    fn observe_operation_duration(&self, elapsed: Duration);
}

/// Instruments that count nothing, for a caller with no meter to inject.
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
