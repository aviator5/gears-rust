//! Database-backed domain service for all transports (SPEC §8.4). REST only maps
//! domain values, allowing a future `api/grpc` adapter without new domain methods.
//! `Idempotency-Key` is a field; no `StatusCode`, `HeaderMap` or `Json` crosses here.
//!
//! API traffic uses [`AdmissionMode::Outbox`]: T21 starts the worker in `init()`
//! and enqueues in the acceptance transaction. Seeding permanently uses
//! [`AdmissionMode::Inline`], accepting and admitting in one call (SPEC §8.1).
//!
//! ponytail C6: no PDP or `SecurityContext` here; managed entities are
//! `#[secure(unrestricted)]`. `allow_all` authorizes without row filtering;
//! `AccessScope::default()` denies all. Every repository already takes a scope
//! for P1's request-scoped `AccessScope` derived from `SecurityContext`.

use std::collections::BTreeMap;
use std::sync::Arc;

use gts::GtsIdPattern;
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, ScopeError};
use toolkit_db::{DBProvider, Db, DbError};
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::config::TypesRegistryConfig;
use crate::domain::admission::acceptance::{AcceptanceContext, AcceptanceError, accept};
use crate::domain::admission::worker::{Tuning, WorkerError, run_operation};
use crate::domain::admission::{
    Accepted, AdmissionFailureReason, Candidate, OperationDispatch, SubmitRequest,
};
use crate::domain::enums::{
    EntityKind, LifecycleStatus, OperationItemStatus, OperationKind, OperationStatus,
};
use crate::domain::policy::RegistrationPolicy;
use crate::domain::ports::metrics::{AdmissionMetrics, PassLabels, RefusalStage};
use crate::domain::ports::{
    CurrentDocument, CurrentInstanceValue, CurrentTypeSchemaRow, EntityRow, PageRequest,
    RecoveryCursor, Stores, snapshot_read,
};

/// Identifier or deterministic Registry Reference (`GtsId::to_uuid()`) for the
/// same row. [`EntityKey::parse`] keeps classification in the domain.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EntityKey {
    GtsId(String),
    Uuid(Uuid),
}

impl EntityKey {
    /// Parse UUIDs as Registry References; otherwise validate identifiers on read.
    /// Unambiguous: GTS identifier segments contain dots and a version, never a bare UUID.
    #[must_use]
    pub fn parse(key: &str) -> Self {
        match Uuid::parse_str(key) {
            Ok(uuid) => Self::Uuid(uuid),
            Err(_) => Self::GtsId(key.to_owned()),
        }
    }
}

/// Deletion target shared by single and batch requests.
#[domain_model]
#[derive(Clone, Debug)]
pub struct DeleteTarget {
    pub key: EntityKey,
    /// Acceptance rejects missing, zero and negative versions with
    /// `deletion_requires_version`, `zero_precondition` and `negative_precondition`.
    pub expected_resource_version: Option<i64>,
}

/// A submitted deletion, before its keys are resolved to identifiers.
#[domain_model]
#[derive(Clone, Debug)]
pub struct DeleteRequest {
    pub idempotency_key: String,
    pub dry_run: bool,
    pub targets: Vec<DeleteTarget>,
}

/// One operation and its per-candidate outcomes, as a caller polls it.
#[domain_model]
#[derive(Clone, Debug)]
pub struct OperationRecord {
    pub operation_id: Uuid,
    pub kind: OperationKind,
    pub dry_run: bool,
    pub status: OperationStatus,
    pub created_at: OffsetDateTime,
    pub started_at: Option<OffsetDateTime>,
    pub completed_at: Option<OffsetDateTime>,
    pub items: Vec<OperationItemRecord>,
}

/// One candidate's durable outcome.
#[domain_model]
#[derive(Clone, Debug)]
pub struct OperationItemRecord {
    pub gts_id: String,
    pub status: OperationItemStatus,
    pub resource_version: Option<i64>,
    /// The stored structured reason, verbatim.
    pub error: Option<String>,
}

/// One entity with its content and D3's materialized artifacts.
#[domain_model]
#[derive(Clone, Debug)]
pub struct EntityRecord {
    pub gts_id: String,
    pub gts_uuid: Uuid,
    pub kind: EntityKind,
    pub lifecycle_status: LifecycleStatus,
    pub resource_version: i64,
    pub owning_gear: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    /// The authored document under the public resource name. Absent only if the
    /// current-state row is missing, which is a corrupt row rather than a state a
    /// reader should expect.
    pub content: Option<Value>,
    pub resolved_schema: Option<Value>,
    pub effective_traits: Option<Value>,
    pub effective_traits_schema: Option<Value>,
}

/// One key's answer in a batch read.
///
/// Absence is a result, not a failure: a caller reconciling a set needs to know
/// which of its keys is missing, and one absent key must not fail the others.
/// P0 has the two states DESIGN's four reduce to — `unchanged` needs T29's
/// validators and `failed` needs federation, which is out of scope (SPEC §10.1).
#[domain_model]
#[derive(Clone, Debug)]
// `Found` already owns heap JSON up to the 1 MB document bound, so the 240 bytes
// `NotFound` pays for the shared discriminant are not what makes a batch large.
// Boxing would buy that back and charge an allocation and an indirection per found
// record — the wrong trade for the variant that carries every successful read.
#[allow(clippy::large_enum_variant)]
pub enum EntityLookup {
    Found(EntityRecord),
    NotFound,
}

/// A content-free discovery query over **active** entities (D12).
///
/// No kind, origin, availability or scope filter: each is either out of P0 scope
/// (SPEC §2) or a tenant-plane input, and the pattern already narrows by
/// identifier, which is what a GTS caller filters on.
#[domain_model]
#[derive(Clone, Debug, Default)]
pub struct DiscoveryQuery {
    /// A GTS wildcard pattern, decided by `gts-rust` and never by SQL.
    pub pattern: Option<String>,
    /// Exclusive keyset lower bound: the position the previous page stopped at.
    pub after: Option<String>,
    /// `None` takes `limits.page_size_default`; above `limits.page_size_max` is refused.
    pub limit: Option<u32>,
}

/// One entity as a discovery page names it: identity and metadata, no authored
/// content, none of D3's artifacts and no validator (§8.5, §10.2).
///
/// A separate type rather than an `EntityRecord` with the documents left `None`:
/// `None` already means "the current-state row is missing" there, so reusing it
/// would make a corrupt row and a content-free projection the same value.
#[domain_model]
#[derive(Clone, Debug)]
pub struct EntitySummary {
    pub gts_id: String,
    pub gts_uuid: Uuid,
    pub kind: EntityKind,
    pub lifecycle_status: LifecycleStatus,
    pub resource_version: i64,
    pub owning_gear: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// One bounded discovery page and the position a caller resumes from.
#[domain_model]
#[derive(Clone, Debug)]
pub struct DiscoveryPage {
    pub items: Vec<EntitySummary>,
    /// The page size actually applied: the caller's `limit`, or
    /// `limits.page_size_default` where it named none. Returned rather than left
    /// for a transport adapter to restate, so REST and a future gRPC adapter
    /// cannot report different defaults for the same read.
    pub limit: u32,
    /// The identifier the scan stopped at. Meaningful only with [`Self::has_more`]:
    /// rows the pattern rejected were consumed too, so this is not necessarily the
    /// last item returned.
    pub next_after: Option<String>,
    /// `true` when the scan stopped on the page limit or its scan budget rather than
    /// on exhausting the range. May over-report, which costs the caller one more
    /// empty page rather than a lost row.
    pub has_more: bool,
}

/// How many keys one batch read may name.
///
/// ponytail: ceiling C10 — **100, not DESIGN §3.3's 500.** DESIGN picked the
/// higher number so a reconciliation could read every identifier it might write
/// before selecting the at most `limits.batch_candidates` (100) it actually
/// submits. P0 gives that headroom up deliberately: one `found` result carries the
/// authored document plus D3's three materialized artifacts, and §3.2 bounds a
/// resolved document at 1 MB, so the ceiling is what bounds a single response —
/// 500 keys is a response this gear should never be asked to build. A
/// reconciliation that wants to inspect more identifiers than it writes pages its
/// reads instead, which the T23 helper owns. The upgrade path is that helper
/// plus a bound on response *bytes* rather than on keys; until then the key count
/// is the only bound there is.
///
/// A constant rather than a configuration key because §10.3's configuration is
/// fixed for P0. Equal to `limits.batch_candidates` today and still not the same
/// bound: a deployment that raises the write ceiling must not silently widen read
/// fan-out with it.
pub const MAX_BATCH_GET_KEYS: usize = 100;

/// What the service can fail with. One layer above the two admission halves, so a
/// transport adapter maps one type.
#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(transparent)]
    Acceptance(#[from] AcceptanceError),
    #[error(transparent)]
    Worker(#[from] WorkerError),
    #[error("storage failure: {0}")]
    Storage(#[from] ScopeError),
    #[error("database failure: {0}")]
    Db(#[from] DbError),
    #[error("a stored document could not be read as JSON: {0}")]
    CorruptDocument(String),
    /// Unresolvable Registry Reference: no identifier exists to record an item outcome.
    /// An absent GTS identifier instead fails asynchronously during admission.
    #[error("no entity has Registry Reference {gts_uuid}")]
    UnresolvedReference { gts_uuid: Uuid },
    /// A batch read named no key at all, or more than [`MAX_BATCH_GET_KEYS`].
    #[error(
        "a batch read must name between 1 and {MAX_BATCH_GET_KEYS} keys; this one named {count}"
    )]
    BatchReadOutOfRange { count: usize },
    /// `limit` outside `1..=limits.page_size_max` (D12).
    #[error("a page size must be between 1 and {max}; this request asked for {limit}")]
    PageSizeOutOfRange { limit: u32, max: u32 },
    /// The discovery pattern is not a GTS identifier pattern.
    #[error("the discovery pattern is not a GTS pattern: {message}")]
    InvalidPattern { message: String },
}

/// How accepted operations are driven after their acceptance transaction commits.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionMode {
    /// Drive the worker before returning. Used by P0 API traffic and seeding.
    Inline,
    /// Leave the durable operation for the outbox worker.
    Outbox,
}

/// The database-backed registry service.
#[domain_model]
pub struct RegistryService {
    db: Db,
    /// Injected persistence ports keep `SeaORM` out; the gear supplies `infra::storage::Repos`.
    stores: Arc<dyn Stores>,
    policy: RegistrationPolicy,
    config: TypesRegistryConfig,
    dispatch: Arc<dyn OperationDispatch>,
    admission_mode: AdmissionMode,
    /// The admission instruments (T16).
    metrics: Arc<dyn AdmissionMetrics>,
}

impl RegistryService {
    /// `admission_mode` selects the inline/outbox driver described in the module docs.
    #[must_use]
    pub fn new(
        db: Db,
        stores: Arc<dyn Stores>,
        policy: RegistrationPolicy,
        config: TypesRegistryConfig,
        dispatch: Arc<dyn OperationDispatch>,
        admission_mode: AdmissionMode,
        metrics: Arc<dyn AdmissionMetrics>,
    ) -> Self {
        Self {
            db,
            stores,
            policy,
            config,
            dispatch,
            admission_mode,
            metrics,
        }
    }

    /// Admission budget, also used by the outbox leased handler.
    pub(crate) fn operation_timeout(&self) -> std::time::Duration {
        self.config.worker.operation_timeout
    }

    /// Classify admission failures against the database engine that produced them.
    pub(crate) fn retryable(&self, error: &WorkerError) -> bool {
        error.transient(self.db.backend())
    }

    /// Delivery attempts the outbox handler may spend on one operation.
    pub(crate) fn max_delivery_attempts(&self) -> u32 {
        self.config.worker.max_delivery_attempts
    }

    /// Instruments for outcomes outside the service, such as outbox delivery.
    pub(crate) fn metrics(&self) -> &dyn AdmissionMetrics {
        self.metrics.as_ref()
    }

    /// Page the startup recovery backlog without reading it all at once.
    /// Start with `None`, then use the last cursor; fewer than `limit` rows means EOF.
    pub async fn nonterminal_operation_page(
        &self,
        after: Option<RecoveryCursor>,
        limit: u64,
    ) -> Result<Vec<RecoveryCursor>, ServiceError> {
        let provider: DBProvider<ServiceError> = DBProvider::new(self.db.clone());
        let stores = Arc::clone(&self.stores);
        let scope = Self::scope();
        provider
            .transaction_with_config(snapshot_read(&self.db), move |tx| {
                Box::pin(async move {
                    Ok(stores
                        .find_nonterminal_ids(tx, &scope, after, limit)
                        .await?)
                })
            })
            .await
    }

    /// Terminalize abandoned delivery, recording `reason` only on undecided items;
    /// terminal items retain their outcomes. This exposes abandonment through
    /// `GET /operations/{id}` and removes the operation from boot recovery.
    ///
    /// # Errors
    /// [`ServiceError::Storage`] or [`ServiceError::Db`] if the write fails.
    pub(crate) async fn abandon(
        &self,
        operation_id: Uuid,
        now: OffsetDateTime,
        error_code: &'static str,
    ) -> Result<(), ServiceError> {
        let provider: DBProvider<ServiceError> = DBProvider::new(self.db.clone());
        let stores = Arc::clone(&self.stores);
        let scope = Self::scope();
        // Safe diagnostic codes correlate the operation and dead letter with logs.
        // Never expose an infrastructure error's Display (SQL/credentials/content).
        let payload = serde_json::json!({
            "reason": AdmissionFailureReason::AdmissionAbandoned.as_str(),
            "message": "admission could not complete because of a system failure",
            "error_code": error_code,
            "operation_id": operation_id,
        })
        .to_string();
        provider
            .transaction(move |tx| {
                Box::pin(async move {
                    let items = stores.find_items(tx, &scope, operation_id).await?;
                    for item in items {
                        if item.status == OperationItemStatus::Pending
                            || item.status == OperationItemStatus::Running
                        {
                            stores
                                .mark_item_failed(tx, &scope, item.id, payload.clone(), now)
                                .await?;
                        }
                    }
                    // One statement from either non-terminal status: an
                    // operation abandoned before its pass reached `mark_running`
                    // is still `pending`, and `mark_completed` would not move it —
                    // leaving a non-terminal operation with terminal items that
                    // every boot's recovery scan picks up again. `false` means
                    // another writer terminalized it first, which is fine.
                    stores.mark_abandoned(tx, &scope, operation_id, now).await?;
                    Ok(())
                })
            })
            .await
    }

    /// See the module docs for the P0 `allow_all` scope.
    fn scope() -> AccessScope {
        AccessScope::allow_all()
    }

    /// Accept a submission, and — while admission is inline — admit it.
    ///
    /// **NOT cancel-safe.** A disconnect/timeout after acceptance commits but
    /// before inline admission leaves durable `pending` work and its idempotency
    /// key. Replay under that key resumes non-terminal work; before T21's outbox,
    /// this was the only recovery path. The acceptance record must not be undone.
    ///
    /// # Errors
    /// [`ServiceError::Acceptance`] for every synchronous refusal, including the
    /// fingerprint conflict; [`ServiceError::Worker`] only for an infrastructure
    /// failure during inline admission, never for a candidate that was refused on
    /// its merits.
    pub async fn submit(
        &self,
        request: &SubmitRequest,
        now: OffsetDateTime,
    ) -> Result<Accepted, ServiceError> {
        let provider: DBProvider<AcceptanceError> = DBProvider::new(self.db.clone());
        let accepted = accept(
            &self.stores,
            &provider,
            &Self::scope(),
            &AcceptanceContext {
                policy: &self.policy,
                config: &self.config,
                metrics: &self.metrics,
            },
            &self.dispatch,
            request,
            now,
        )
        .await?;

        // Gate on `!terminal`, not `!replayed`, to resume interrupted inline work.
        // `run_operation` is idempotent: completed operations and terminal items are skipped.
        let mut accepted = accepted;
        if self.admission_mode == AdmissionMode::Inline && !accepted.terminal() {
            // After the acceptance transaction committed, never inside it: the
            // worker reads the operation it is admitting.
            self.admit(accepted.operation_id, now).await?;
            // `Ok` means completed or already terminal; no receipt status reread is needed.
            accepted.status = OperationStatus::Completed;
        }
        Ok(accepted)
    }

    /// Admit an accepted operation with shared inline/outbox tuning.
    /// Completed operations and terminal items are skipped, making redelivery safe.
    ///
    /// # Errors
    /// [`ServiceError::Worker`] for infrastructure failures. Candidate refusals are
    /// recorded on items and return `Ok`.
    pub async fn admit(&self, operation_id: Uuid, now: OffsetDateTime) -> Result<(), ServiceError> {
        let worker: DBProvider<WorkerError> = DBProvider::new(self.db.clone());
        run_operation(
            &self.stores,
            &worker,
            &Self::scope(),
            Tuning {
                limits: &self.config.limits,
                worker: &self.config.worker,
                metrics: &self.metrics,
                allow_compatibility_force: self.config.allow_compatibility_force,
            },
            operation_id,
            now,
        )
        .await?;
        Ok(())
    }

    /// Submit a single or batch deletion through the shared admission path (SPEC §8.4).
    ///
    /// # Errors
    /// [`ServiceError::UnresolvedReference`] for an unknown Registry Reference,
    /// plus errors from [`Self::submit`].
    pub async fn delete(
        &self,
        request: &DeleteRequest,
        now: OffsetDateTime,
    ) -> Result<Accepted, ServiceError> {
        // Enforce `limits.batch_candidates` before `resolve_targets` can perform
        // unbounded reads; acceptance's own check runs after resolution.
        let limit = self.config.limits.batch_candidates;
        if request.targets.len() > limit {
            let error = AcceptanceError::BatchTooLarge {
                count: request.targets.len(),
                limit,
            };
            // Counted here because `accept` counts at its own exit and this refusal
            // never reaches it; the series must not depend on which check fired.
            self.metrics.refused(
                RefusalStage::Acceptance,
                error.reason(),
                PassLabels::new(OperationKind::Deletion, request.dry_run),
            );
            return Err(ServiceError::Acceptance(error));
        }
        let candidates = self.resolve_targets(&request.targets).await?;
        self.submit(
            &SubmitRequest {
                idempotency_key: request.idempotency_key.clone(),
                kind: OperationKind::Deletion,
                dry_run: request.dry_run,
                candidates,
            },
            now,
        )
        .await
    }

    /// Resolve deletion keys to candidate identifiers before acceptance, which reads
    /// no entity state (SPEC §8.1). UUID-to-identifier mappings are immutable;
    /// admission rechecks lifecycle and preconditions under its locks.
    async fn resolve_targets(
        &self,
        targets: &[DeleteTarget],
    ) -> Result<Vec<Candidate>, ServiceError> {
        let references: Vec<Uuid> = targets
            .iter()
            .filter_map(|target| match &target.key {
                EntityKey::Uuid(gts_uuid) => Some(*gts_uuid),
                EntityKey::GtsId(_) => None,
            })
            .collect();
        // Identifier-only batches need no lookup.
        let resolved = if references.is_empty() {
            BTreeMap::new()
        } else {
            self.reverse_resolve(references).await?
        };

        targets
            .iter()
            .map(|target| {
                let gts_id = match &target.key {
                    EntityKey::GtsId(gts_id) => gts_id.clone(),
                    EntityKey::Uuid(gts_uuid) => resolved.get(gts_uuid).cloned().ok_or(
                        ServiceError::UnresolvedReference {
                            gts_uuid: *gts_uuid,
                        },
                    )?,
                };
                Ok(Candidate {
                    gts_id,
                    // Deletion has no content or compatibility check to waive (ADR-0004).
                    content: None,
                    expected_resource_version: target.expected_resource_version,
                    force: false,
                })
            })
            .collect()
    }

    /// Resolve Registry References under one snapshot, in chunked batch reads
    /// rather than one query per reference. The caller has already bounded the
    /// batch by `limits.batch_candidates`. Omit missing rows so the caller reports
    /// the first unresolved target in request order.
    async fn reverse_resolve(
        &self,
        references: Vec<Uuid>,
    ) -> Result<BTreeMap<Uuid, String>, ServiceError> {
        let provider: DBProvider<ServiceError> = DBProvider::new(self.db.clone());
        let scope = Self::scope();
        let stores = Arc::clone(&self.stores);
        provider
            .transaction_with_config(snapshot_read(&self.db), move |tx| {
                Box::pin(async move {
                    // Resolve tombstones too, preserving the identifier path's `not_active` outcome.
                    let rows = stores.find_by_gts_uuids(tx, &scope, &references).await?;
                    Ok(rows
                        .into_iter()
                        .map(|row| (row.gts_uuid, row.gts_id))
                        .collect())
                })
            })
            .await
    }

    /// Read one operation and its per-candidate outcomes.
    ///
    /// # Errors
    /// [`ServiceError::Storage`] for a read failure. An absent operation is
    /// `Ok(None)`, because "not found" is an answer rather than a fault.
    pub async fn operation(&self, id: Uuid) -> Result<Option<OperationRecord>, ServiceError> {
        let provider: DBProvider<ServiceError> = DBProvider::new(self.db.clone());
        let scope = Self::scope();
        let stores = Arc::clone(&self.stores);
        // Worker transactions write status and item outcomes separately; one
        // snapshot prevents combining states that never coexisted.
        let Some((operation, items)) = provider
            .transaction_with_config(snapshot_read(&self.db), move |tx| {
                Box::pin(async move {
                    let Some(operation) = stores.find_by_id(tx, &scope, id).await? else {
                        return Ok(None);
                    };
                    let items = stores.find_items(tx, &scope, id).await?;
                    Ok(Some((operation, items)))
                })
            })
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(OperationRecord {
            operation_id: operation.id,
            kind: operation.kind,
            dry_run: operation.dry_run,
            status: operation.status,
            created_at: operation.created_at,
            started_at: operation.started_at,
            completed_at: operation.completed_at,
            items: items
                .into_iter()
                .map(|item| OperationItemRecord {
                    gts_id: item.gts_id,
                    status: item.status,
                    resource_version: item.result_resource_version,
                    error: item.error_payload,
                })
                .collect(),
        }))
    }

    /// Read one entity by identifier or Registry Reference, with its authored
    /// content and D3's materialized artifacts.
    ///
    /// One key's [`Self::batch_get`], as `delete_entity` is one target's `delete`.
    /// Sharing the implementation is what makes the two surfaces agree: the key is
    /// classified once, an absent row is an absence on both, and an identifier that
    /// cannot exist is not validated by one surface and looked up by the other.
    /// DELETED tombstones stay exact-readable and only leave discovery.
    ///
    /// # Errors
    /// [`ServiceError::Storage`] for a read failure, or
    /// [`ServiceError::CorruptDocument`] if a stored document is not JSON.
    pub async fn entity(&self, key: &EntityKey) -> Result<Option<EntityRecord>, ServiceError> {
        let results = self.batch_get(std::slice::from_ref(key)).await?;
        Ok(results.into_iter().find_map(|(_, lookup)| match lookup {
            EntityLookup::Found(record) => Some(record),
            EntityLookup::NotFound => None,
        }))
    }

    /// Read a bounded set of keys, answering every one of them (DESIGN §3.3).
    ///
    /// Results are returned in request order with the key each was asked by, so a
    /// caller that mixed identifiers and Registry References matches answers to
    /// questions without re-deriving either. A key named twice collapses onto its
    /// first mention: the answer is per key, not per mention. The two spellings of
    /// one row are **not** duplicates of each other — each is a key a caller asked
    /// about and each is echoed.
    ///
    /// Constant in round trips rather than linear in keys: two identity reads and
    /// three current-state reads, all under one snapshot, whatever the batch size.
    ///
    /// # Errors
    /// [`ServiceError::BatchReadOutOfRange`] for an empty or over-long batch,
    /// [`ServiceError::Storage`] for a read failure, or
    /// [`ServiceError::CorruptDocument`] if a stored document is not JSON.
    pub async fn batch_get(
        &self,
        keys: &[EntityKey],
    ) -> Result<Vec<(EntityKey, EntityLookup)>, ServiceError> {
        // Bounded before any read, as deletion bounds its batch: the ceiling exists
        // to keep one request's work finite, so it cannot be checked after the work.
        if keys.is_empty() || keys.len() > MAX_BATCH_GET_KEYS {
            return Err(ServiceError::BatchReadOutOfRange { count: keys.len() });
        }
        let mut requested: Vec<EntityKey> = Vec::with_capacity(keys.len());
        let mut seen: std::collections::HashSet<&EntityKey> =
            std::collections::HashSet::with_capacity(keys.len());
        for key in keys {
            if seen.insert(key) {
                requested.push(key.clone());
            }
        }

        let gts_ids: Vec<String> = requested
            .iter()
            .filter_map(|key| match key {
                EntityKey::GtsId(gts_id) => Some(gts_id.clone()),
                EntityKey::Uuid(_) => None,
            })
            .collect();
        let gts_uuids: Vec<Uuid> = requested
            .iter()
            .filter_map(|key| match key {
                EntityKey::Uuid(gts_uuid) => Some(*gts_uuid),
                EntityKey::GtsId(_) => None,
            })
            .collect();

        let provider: DBProvider<ServiceError> = DBProvider::new(self.db.clone());
        let scope = Self::scope();
        let stores = Arc::clone(&self.stores);
        // One snapshot keeps each row's atomically written `resource_version`,
        // artifacts and authored document together. T11 revisions could otherwise
        // pair N with N + 1 artifacts, breaking T29's version/body promise for
        // conditional reads.
        let state = provider
            .transaction_with_config(snapshot_read(&self.db), move |tx| {
                Box::pin(async move {
                    let mut rows = stores.find_by_gts_ids(tx, &scope, &gts_ids).await?;
                    rows.extend(stores.find_by_gts_uuids(tx, &scope, &gts_uuids).await?);
                    // The two identity reads are independent, so a row named by both
                    // spellings arrives twice; the current-state reads below must
                    // name it once.
                    rows.sort_by_key(|row| row.id);
                    rows.dedup_by_key(|row| row.id);

                    // Branch on row kind, not on key: Type Schemas have a document
                    // and D3's three artifacts, Instances only an authored value.
                    let type_ids = ids_of(&rows, EntityKind::TypeSchema);
                    let instance_ids = ids_of(&rows, EntityKind::Instance);
                    let schemas = stores.current_schemas(tx, &scope, &type_ids).await?;
                    let documents = stores.current_documents(tx, &scope, &type_ids).await?;
                    let values = stores.current_values(tx, &scope, &instance_ids).await?;
                    Ok(BatchState {
                        rows,
                        schemas,
                        documents,
                        values,
                    })
                })
            })
            .await?;

        let records = state.into_records()?;
        let by_gts_id: BTreeMap<&str, &EntityRecord> = records
            .values()
            .map(|record| (record.gts_id.as_str(), record))
            .collect();
        let by_gts_uuid: BTreeMap<Uuid, &EntityRecord> = records
            .values()
            .map(|record| (record.gts_uuid, record))
            .collect();

        Ok(requested
            .into_iter()
            .map(|key| {
                let found = match &key {
                    EntityKey::GtsId(gts_id) => by_gts_id.get(gts_id.as_str()).copied(),
                    EntityKey::Uuid(gts_uuid) => by_gts_uuid.get(gts_uuid).copied(),
                };
                let lookup = match found {
                    Some(record) => EntityLookup::Found(record.clone()),
                    None => EntityLookup::NotFound,
                };
                (key, lookup)
            })
            .collect())
    }

    /// One bounded, content-free page of active entities, ordered by canonical
    /// identifier (D12).
    ///
    /// The page size and its ceiling are deployment configuration, which is why the
    /// default and the refusal live here rather than in a transport adapter: a gRPC
    /// adapter must not be able to page differently from REST.
    ///
    /// # Errors
    /// [`ServiceError::PageSizeOutOfRange`] for a refused `limit`,
    /// [`ServiceError::InvalidPattern`] for a pattern `gts-rust` will not compile,
    /// or [`ServiceError::Storage`] for a read failure.
    pub async fn discover(&self, query: &DiscoveryQuery) -> Result<DiscoveryPage, ServiceError> {
        let max = self.config.limits.page_size_max;
        let limit = query.limit.unwrap_or(self.config.limits.page_size_default);
        if limit == 0 || limit > max {
            return Err(ServiceError::PageSizeOutOfRange { limit, max });
        }
        // Compiled by `gts-rust`, the sole authority on the pattern grammar
        // (`constraint-gts-implementation`). A string it refuses is a refused
        // request, not an empty page: the two are indistinguishable to a caller
        // that mistyped a wildcard.
        let pattern = query
            .pattern
            .as_deref()
            .map(|raw| {
                GtsIdPattern::try_new(raw).map_err(|e| ServiceError::InvalidPattern {
                    message: e.to_string(),
                })
            })
            .transpose()?;
        let request = PageRequest {
            after: query.after.clone(),
            limit,
        };

        let provider: DBProvider<ServiceError> = DBProvider::new(self.db.clone());
        let scope = Self::scope();
        let stores = Arc::clone(&self.stores);
        let page = provider
            .transaction_with_config(snapshot_read(&self.db), move |tx| {
                Box::pin(async move {
                    Ok(stores
                        .list_page(tx, &scope, pattern.as_ref(), request)
                        .await?)
                })
            })
            .await?;

        Ok(DiscoveryPage {
            items: page
                .items
                .into_iter()
                .map(|row| EntitySummary {
                    gts_id: row.gts_id,
                    gts_uuid: row.gts_uuid,
                    kind: row.entity_kind,
                    lifecycle_status: row.lifecycle_status,
                    resource_version: row.resource_version,
                    owning_gear: row.owning_gear,
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                })
                .collect(),
            limit,
            next_after: page.next_after,
            has_more: page.has_more,
        })
    }
}

/// The entity ids of one kind, for the kind-specific current-state reads.
fn ids_of(rows: &[EntityRow], kind: EntityKind) -> Vec<i64> {
    rows.iter()
        .filter(|row| row.entity_kind == kind)
        .map(|row| row.id)
        .collect()
}

/// One snapshot's worth of rows and current state, before it becomes records.
///
/// A struct rather than a tuple because the three current-state reads are keyed by
/// `entity_id` and pairing the wrong one with `rows` is exactly the mistake that
/// returns an Instance's value as a resolved schema.
struct BatchState {
    rows: Vec<EntityRow>,
    schemas: Vec<CurrentTypeSchemaRow>,
    documents: Vec<CurrentDocument>,
    values: Vec<CurrentInstanceValue>,
}

impl BatchState {
    /// Assemble one [`EntityRecord`] per row, keyed by entity id.
    ///
    /// A row with no current state is a corrupt row rather than a state a reader
    /// should expect, so it is an error and not an absence — the same answer the
    /// single read has always given, which is what keeps one key's batch identical
    /// to the exact read.
    fn into_records(self) -> Result<BTreeMap<i64, EntityRecord>, ServiceError> {
        let schemas: BTreeMap<i64, CurrentTypeSchemaRow> = self
            .schemas
            .into_iter()
            .map(|row| (row.entity_id, row))
            .collect();
        let documents: BTreeMap<i64, CurrentDocument> = self
            .documents
            .into_iter()
            .map(|row| (row.entity_id, row))
            .collect();
        let values: BTreeMap<i64, CurrentInstanceValue> = self
            .values
            .into_iter()
            .map(|row| (row.entity_id, row))
            .collect();

        let mut out = BTreeMap::new();
        for row in self.rows {
            let (content, resolved_schema, effective_traits, effective_traits_schema) =
                match row.entity_kind {
                    EntityKind::TypeSchema => {
                        let current = schemas.get(&row.id).ok_or_else(|| {
                            missing_state(&row.gts_id, "current Type Schema state")
                        })?;
                        let document = documents.get(&row.id).ok_or_else(|| {
                            missing_state(&row.gts_id, "current Type Schema document")
                        })?;
                        (
                            Some(parse_stored(&document.raw_schema, &row.gts_id)?),
                            Some(parse_stored(&current.resolved_schema, &row.gts_id)?),
                            Some(parse_stored(&current.effective_traits, &row.gts_id)?),
                            Some(parse_stored(&current.effective_traits_schema, &row.gts_id)?),
                        )
                    }
                    // Instances have no derived artifacts; all three are intentionally `None`.
                    EntityKind::Instance => {
                        let value = values
                            .get(&row.id)
                            .ok_or_else(|| missing_state(&row.gts_id, "current Instance state"))?;
                        (
                            Some(parse_stored(&value.canonical_value, &row.gts_id)?),
                            None,
                            None,
                            None,
                        )
                    }
                };
            out.insert(
                row.id,
                EntityRecord {
                    gts_id: row.gts_id,
                    gts_uuid: row.gts_uuid,
                    kind: row.entity_kind,
                    lifecycle_status: row.lifecycle_status,
                    resource_version: row.resource_version,
                    owning_gear: row.owning_gear,
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                    content,
                    resolved_schema,
                    effective_traits,
                    effective_traits_schema,
                },
            );
        }
        Ok(out)
    }
}

fn missing_state(gts_id: &str, what: &str) -> ServiceError {
    ServiceError::CorruptDocument(format!("entity '{gts_id}' has no {what}"))
}

fn parse_stored(text: &str, gts_id: &str) -> Result<Value, ServiceError> {
    serde_json::from_str(text)
        .map_err(|e| ServiceError::CorruptDocument(format!("'{gts_id}': {e}")))
}
