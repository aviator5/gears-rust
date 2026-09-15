//! Transport-neutral, database-backed registry service (SPEC §8.4).
//! Submissions are accepted here and admitted by the outbox.
//! P0 managed entities are unrestricted, but ports already accept an access scope.

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
    CurrentDocument, CurrentInstanceValue, CurrentTypeSchemaRow, EntityRow, PageRequest, Stores,
    snapshot_read,
};

/// GTS identifier or deterministic Registry Reference for the same row.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EntityKey {
    GtsId(String),
    Uuid(Uuid),
}

impl EntityKey {
    /// Parse a UUID as a Registry Reference; otherwise keep the GTS identifier.
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
    /// Required positive version, validated during acceptance.
    pub expected_resource_version: Option<i64>,
}

/// A submitted deletion, before its keys are resolved to identifiers.
#[domain_model]
#[derive(Clone, Debug)]
pub struct DeleteRequest {
    /// Required; optional only to share acceptance validation.
    pub idempotency_key: Option<String>,
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
#[allow(
    clippy::large_enum_variant,
    reason = "boxing Found penalises every successful read with an allocation and an indirection; NotFound's discriminant overhead is acceptable"
)]
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
    /// Registry Reference with no identifier for an asynchronous item outcome.
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

impl ServiceError {
    /// Return an exhaustive, log-safe cause kind without formatting sensitive data.
    #[must_use]
    pub const fn cause_kind(&self) -> &'static str {
        match self {
            Self::Acceptance(_) => "acceptance",
            Self::Worker(_) => "worker",
            Self::Storage(_) => "storage",
            Self::Db(_) => "database",
            Self::CorruptDocument(_) => "corrupt_document",
            Self::UnresolvedReference { .. } => "unresolved_reference",
            Self::BatchReadOutOfRange { .. } => "batch_read_out_of_range",
            Self::PageSizeOutOfRange { .. } => "page_size_out_of_range",
            Self::InvalidPattern { .. } => "invalid_pattern",
        }
    }
}

/// The database-backed registry service.
#[domain_model]
pub struct RegistryService {
    db: Db,
    /// Injected ports keep `SeaORM` out of the domain.
    stores: Arc<dyn Stores>,
    policy: RegistrationPolicy,
    config: TypesRegistryConfig,
    dispatch: Arc<dyn OperationDispatch>,
    /// The admission instruments (T16).
    metrics: Arc<dyn AdmissionMetrics>,
}

impl RegistryService {
    #[must_use]
    pub fn new(
        db: Db,
        stores: Arc<dyn Stores>,
        policy: RegistrationPolicy,
        config: TypesRegistryConfig,
        dispatch: Arc<dyn OperationDispatch>,
        metrics: Arc<dyn AdmissionMetrics>,
    ) -> Self {
        Self {
            db,
            stores,
            policy,
            config,
            dispatch,
            metrics,
        }
    }

    /// Admission budget, also used by the outbox leased handler.
    pub(crate) fn operation_timeout(&self) -> std::time::Duration {
        self.config.worker.operation_timeout
    }

    /// Delivery attempts the outbox handler may spend on one operation.
    pub(crate) fn max_delivery_attempts(&self) -> u32 {
        self.config.worker.max_delivery_attempts
    }

    /// Instruments for outcomes outside the service, such as outbox delivery.
    pub(crate) fn metrics(&self) -> &dyn AdmissionMetrics {
        self.metrics.as_ref()
    }

    /// Fail undecided items and terminalize the operation as a system failure.
    ///
    /// # Errors
    /// [`ServiceError::Storage`] or [`ServiceError::Db`] if the write fails.
    pub(crate) async fn record_system_failure(
        &self,
        operation_id: Uuid,
        now: OffsetDateTime,
        error_code: &'static str,
    ) -> Result<(), ServiceError> {
        let provider: DBProvider<ServiceError> = DBProvider::new(self.db.clone());
        let stores = Arc::clone(&self.stores);
        let scope = Self::scope();
        // Expose stable codes, never infrastructure error text.
        let payload = serde_json::json!({
            "reason": AdmissionFailureReason::SystemFailure.as_str(),
            "message": "admission could not complete because of a system failure",
            "error_code": error_code,
            "operation_id": operation_id,
        })
        .to_string();
        provider
            .transaction(move |tx| {
                Box::pin(async move {
                    // One guarded statement fits the remaining lease and preserves outcomes.
                    stores
                        .fail_nonterminal_items(tx, &scope, operation_id, payload, now)
                        .await?;
                    // A system failure can move either pending or running operations.
                    stores
                        .mark_system_failed(tx, &scope, operation_id, now)
                        .await?;
                    Ok(())
                })
            })
            .await
    }

    /// See the module docs for the P0 `allow_all` scope.
    fn scope() -> AccessScope {
        AccessScope::allow_all()
    }

    /// Accept a submission and dispatch it durably; the outbox admits it.
    /// Acceptance and dispatch share one transaction, so a committed operation
    /// always has a driver.
    ///
    /// # Errors
    /// [`ServiceError::Acceptance`] for every synchronous refusal, including the
    /// fingerprint conflict.
    pub async fn submit(
        &self,
        request: &SubmitRequest,
        now: OffsetDateTime,
    ) -> Result<Accepted, ServiceError> {
        let provider: DBProvider<AcceptanceError> = DBProvider::new(self.db.clone());
        Ok(accept(
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
        .await?)
    }

    /// Admit an operation, skipping completed work on redelivery.
    /// Cancellation is recoverable because candidate outcomes commit independently.
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
        // Bound Registry Reference lookups before resolving targets.
        let limit = self.config.limits.batch_candidates;
        if request.targets.len() > limit {
            let error = AcceptanceError::BatchTooLarge {
                count: request.targets.len(),
                limit,
            };
            // This refusal never reaches acceptance's metric.
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

    /// Resolve immutable Registry References; admission rechecks mutable state.
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
