//! Wire DTOs of the platform REST surface and their codecs (SPEC §10.1, §8.5).
//!
//! Each DTO mirrors the registry's handler DTO field for field; the models never refer
//! to one. Encoding refuses what the wire cannot carry, in the shape the local client
//! refuses it. Decoding refuses a malformed payload rather than defaulting it, and keeps
//! an omitted document apart from a selected JSON `null`.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use gts::GtsId;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use uuid::Uuid;

use crate::field;
use crate::gts::TypeResource;
use crate::item_failure::{AdmissionFailure, AdmissionFailureReason};
use crate::models::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, Cursor, DeleteEntitiesRequest,
    DeletionItemResult, DeletionOperation, DeletionOutcome, Entity, EntityField, EntityKey,
    EntityKind, EntityLookup, JsonDocument, LifecycleFilter, LifecycleStatus, ListEntitiesRequest,
    ListEntitiesResponse, Operation, OperationStatus, Origin, Projection, Provenance,
    RegisterEntitiesRequest, RegistrationItemResult, RegistrationOperation, RegistrationOutcome,
    Validator,
};

// ---- failures ----------------------------------------------------------------

/// A response the registry could not have sent; never read as some answer.
fn malformed(what: &str, detail: &dyn std::fmt::Display) -> CanonicalError {
    tracing::error!(what, %detail, "types_registry REST client received a malformed response");
    CanonicalError::internal(format!("the registry sent a malformed {what}")).create()
}

/// A success body as `T`; a body that does not parse is malformed, never defaulted.
pub fn parse<T: serde::de::DeserializeOwned>(what: &str, body: &[u8]) -> Result<T, CanonicalError> {
    serde_json::from_slice(body).map_err(|e| malformed(what, &e))
}

/// The local client's refusal of a precondition no stored version can match.
fn version_out_of_range(key: &str, version: u64) -> CanonicalError {
    TypeResource::invalid_argument()
        .with_resource(key.to_owned())
        .with_field_violation(
            field::EXPECTED_RESOURCE_VERSION_FIELD,
            format!("{version} is not a resource version this registry issues"),
            field::VALIDATION_FAILED,
        )
        .create()
}

fn wire_version(key: &str, version: u64) -> Result<i64, CanonicalError> {
    i64::try_from(version).map_err(|_| version_out_of_range(key, version))
}

/// The registry's bound on a validator, the same as on a key.
const MAX_VALIDATOR_LEN: usize = 1024;

/// The local client's refusal of a validator over the registry's bound.
fn validator_too_long(len: usize) -> CanonicalError {
    TypeResource::invalid_argument()
        .with_field_violation(
            field::IF_NONE_MATCH_FIELD,
            format!(
                "each {} entity-tag must be at most {MAX_VALIDATOR_LEN} bytes; this one is {len}",
                field::IF_NONE_MATCH_FIELD
            ),
            field::VALIDATION_FAILED,
        )
        .create()
}

// ---- keys, validators, projection -------------------------------------------

/// Classified as the registry classifies a key: a UUID first, otherwise an identifier.
fn entity_key(text: &str) -> Result<EntityKey, CanonicalError> {
    if let Ok(uuid) = Uuid::parse_str(text) {
        return Ok(EntityKey::GtsUuid(uuid));
    }
    GtsId::try_new(text)
        .map(EntityKey::GtsId)
        .map_err(|e| malformed("entity key", &e))
}

fn gts_id(text: &str) -> Result<GtsId, CanonicalError> {
    GtsId::try_new(text).map_err(|e| malformed("identifier", &e))
}

/// RFC 9110 `etagc`, the bytes a strong entity-tag may quote.
fn is_etagc(byte: u8) -> bool {
    byte == b'!' || (b'#'..=b'~').contains(&byte)
}

/// A strong entity-tag: the validator, quoted. One over the registry's bound is refused,
/// as the local client refuses it, whatever its bytes. Otherwise a validator no registry
/// issues (bytes no entity-tag can quote) is sent as no condition, so the key reads as
/// `found` — what the local client answers for a token that matches nothing.
fn entity_tag(validator: &Validator) -> Result<Option<String>, CanonicalError> {
    let bytes = validator.as_bytes();
    if bytes.len() > MAX_VALIDATOR_LEN {
        return Err(validator_too_long(bytes.len()));
    }
    if bytes.is_empty() || !bytes.iter().copied().all(is_etagc) {
        return Ok(None);
    }
    // `etagc` is ASCII, so the bytes are UTF-8.
    Ok(std::str::from_utf8(bytes)
        .ok()
        .map(|token| format!("\"{token}\"")))
}

/// A received strong entity-tag as the validator token, without its quotes.
pub fn validator(tag: &str) -> Result<Validator, CanonicalError> {
    tag.strip_prefix('"')
        .and_then(|tag| tag.strip_suffix('"'))
        .filter(|token| !token.is_empty() && token.bytes().all(is_etagc))
        .map(|token| Validator::from_bytes(token.as_bytes()))
        .ok_or_else(|| malformed("entity-tag", &tag.escape_debug()))
}

/// The `$select` value; `Default` sends none, so the registry applies its own default.
fn select(projection: &Projection) -> Option<String> {
    match projection {
        Projection::Default => None,
        Projection::Select(selection) => Some(
            selection
                .fields()
                .map(EntityField::name)
                .collect::<Vec<_>>()
                .join(","),
        ),
    }
}

// ---- requests -----------------------------------------------------------------

/// `POST …/entities`. The publisher is not on the wire (Phase 9), as on the local client.
#[derive(Debug, Serialize)]
pub struct SubmitBody {
    items: Vec<SubmitItem>,
    dry_run: bool,
}

#[derive(Debug, Serialize)]
struct SubmitItem {
    gts_id: String,
    content: JsonDocument,
    /// Omitted means "must not exist".
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_resource_version: Option<i64>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    skip_compatibility_check: bool,
}

impl SubmitBody {
    pub fn encode(request: RegisterEntitiesRequest) -> Result<Self, CanonicalError> {
        let items = request
            .items
            .into_iter()
            .map(|item| {
                let gts_id = item.gts_id.to_string();
                let expected_resource_version = item
                    .expected_resource_version
                    .map(|version| wire_version(&gts_id, version))
                    .transpose()?;
                Ok(SubmitItem {
                    gts_id,
                    content: item.content,
                    expected_resource_version,
                    skip_compatibility_check: item.skip_compatibility_check,
                })
            })
            .collect::<Result<_, CanonicalError>>()?;
        Ok(Self {
            items,
            dry_run: request.dry_run,
        })
    }
}

/// `POST …/entities:batchDelete`.
#[derive(Debug, Serialize)]
pub struct DeleteBody {
    items: Vec<DeleteItem>,
    dry_run: bool,
}

#[derive(Debug, Serialize)]
struct DeleteItem {
    entity_key: String,
    expected_resource_version: i64,
}

impl DeleteBody {
    pub fn encode(request: DeleteEntitiesRequest) -> Result<Self, CanonicalError> {
        let items = request
            .items
            .into_iter()
            .map(|item| {
                let entity_key = item.key.to_string();
                let expected_resource_version =
                    wire_version(&entity_key, item.expected_resource_version)?;
                Ok(DeleteItem {
                    entity_key,
                    expected_resource_version,
                })
            })
            .collect::<Result<_, CanonicalError>>()?;
        Ok(Self {
            items,
            dry_run: request.dry_run,
        })
    }
}

/// `POST …/entities:batchGet`. `fresh` is a client-cache flag and is not sent.
#[derive(Debug, Serialize)]
pub struct BatchGetBody {
    items: Vec<BatchGetItem>,
    #[serde(rename = "$select", skip_serializing_if = "Option::is_none")]
    select: Option<String>,
}

#[derive(Debug, Serialize)]
struct BatchGetItem {
    entity_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    if_none_match: Option<String>,
}

impl BatchGetBody {
    pub fn encode(request: &BatchGetEntitiesRequest) -> Result<Self, CanonicalError> {
        Ok(Self {
            items: request
                .items
                .iter()
                .map(|item| {
                    Ok(BatchGetItem {
                        entity_key: item.key.to_string(),
                        if_none_match: item
                            .if_none_match
                            .as_ref()
                            .map(entity_tag)
                            .transpose()?
                            .flatten(),
                    })
                })
                .collect::<Result<_, CanonicalError>>()?,
            select: select(&request.projection),
        })
    }
}

/// `GET …/entities` query pairs, in a fixed order. The lifecycle filter is always sent.
pub fn discovery_query(request: &ListEntitiesRequest) -> Vec<(&'static str, String)> {
    let filter = &request.filter;
    let mut pairs = Vec::new();
    if let Some(pattern) = &filter.pattern {
        pairs.push((field::PATTERN_FIELD, pattern.to_string()));
    }
    if let Some(depth) = filter.max_chain_depth {
        pairs.push((field::DEPTH_FIELD, depth.to_string()));
    }
    if let Some(kind) = filter.kind {
        pairs.push((field::KIND_FIELD, kind_name(kind).to_owned()));
    }
    pairs.push((
        field::LIFECYCLE_STATUS_FIELD,
        lifecycle_name(filter.lifecycle).to_owned(),
    ));
    if let Some(limit) = request.page.limit {
        pairs.push((field::LIMIT_FIELD, limit.to_string()));
    }
    if let Some(cursor) = &request.page.cursor {
        pairs.push((field::CURSOR_FIELD, cursor.as_str().to_owned()));
    }
    if let Some(select) = select(&request.projection) {
        pairs.push((field::SELECT_FIELD, select));
    }
    pairs
}

const fn kind_name(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::TypeSchema => "type_schema",
        EntityKind::Instance => "instance",
    }
}

const fn lifecycle_name(filter: LifecycleFilter) -> &'static str {
    match filter {
        LifecycleFilter::Active => "active",
        LifecycleFilter::Deleted => "deleted",
        LifecycleFilter::All => "all",
    }
}

// ---- responses: vocabularies ------------------------------------------------------

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OperationStatusDto {
    Pending,
    Running,
    Completed,
}

impl From<OperationStatusDto> for OperationStatus {
    fn from(status: OperationStatusDto) -> Self {
        match status {
            OperationStatusDto::Pending => Self::Pending,
            OperationStatusDto::Running => Self::Running,
            OperationStatusDto::Completed => Self::Completed,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ItemStatusDto {
    Pending,
    Running,
    Succeeded,
    Unchanged,
    Failed,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum EntityKindDto {
    TypeSchema,
    Instance,
}

impl From<EntityKindDto> for EntityKind {
    fn from(kind: EntityKindDto) -> Self {
        match kind {
            EntityKindDto::TypeSchema => Self::TypeSchema,
            EntityKindDto::Instance => Self::Instance,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LifecycleStatusDto {
    Active,
    Deleted,
}

impl From<LifecycleStatusDto> for LifecycleStatus {
    fn from(status: LifecycleStatusDto) -> Self {
        match status {
            LifecycleStatusDto::Active => Self::Active,
            LifecycleStatusDto::Deleted => Self::Deleted,
        }
    }
}

// ---- responses: the submission receipt ------------------------------------------

/// The `200`/`202` body of a submission. The outcome is read through the operation.
#[derive(Debug, Deserialize)]
pub struct AcceptedDto {
    pub operation_id: Uuid,
    status: OperationStatusDto,
    pub replayed: bool,
}

impl AcceptedDto {
    pub fn status(&self) -> OperationStatus {
        self.status.into()
    }
}

// ---- responses: operations ---------------------------------------------------------

/// An operation as polled; `kind` selects the item shape. Timestamps are not modelled.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperationDto {
    Registration(OperationBody<RegistrationItemDto>),
    Deletion(OperationBody<DeletionItemDto>),
}

#[derive(Debug, Deserialize)]
pub struct OperationBody<I> {
    operation_id: Uuid,
    dry_run: bool,
    status: OperationStatusDto,
    items: Vec<I>,
}

#[derive(Debug, Deserialize)]
pub struct RegistrationItemDto {
    gts_id: String,
    #[serde(flatten)]
    state: ItemStateDto,
}

#[derive(Debug, Deserialize)]
pub struct DeletionItemDto {
    entity_key: String,
    #[serde(flatten)]
    state: ItemStateDto,
}

/// The registry always sends both members, `null` when they do not apply; an omitted
/// one is malformed rather than read as `null`.
#[derive(Debug, Deserialize)]
struct ItemStateDto {
    status: ItemStatusDto,
    #[serde(deserialize_with = "nullable")]
    resource_version: Option<u64>,
    #[serde(deserialize_with = "nullable")]
    error: Option<ItemErrorDto>,
}

/// A required member that may be `null`: without `default`, an absent one is an error.
fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}

#[derive(Debug, Deserialize)]
struct ItemErrorDto {
    reason: String,
    message: String,
    context: Map<String, Value>,
}

/// An item state with exactly the data the registry gives it.
enum ItemState {
    Pending,
    Running,
    Succeeded(u64),
    WouldSucceed,
    Unchanged(u64),
    Failed(CanonicalError),
}

impl ItemStateDto {
    /// The state these fields describe; the combinations the registry never sends are
    /// refused. `key` is the item's canonical key, naming a failure's resource.
    fn decode(self, dry_run: bool, key: &str) -> Result<ItemState, CanonicalError> {
        use ItemStatusDto as S;
        Ok(match (self.status, self.resource_version, self.error) {
            (S::Pending, None, None) => ItemState::Pending,
            (S::Running, None, None) => ItemState::Running,
            (S::Succeeded, Some(version @ 1..), None) if !dry_run => ItemState::Succeeded(version),
            (S::Succeeded, None, None) if dry_run => ItemState::WouldSucceed,
            (S::Unchanged, Some(version @ 1..), None) => ItemState::Unchanged(version),
            (S::Failed, None, Some(error)) => ItemState::Failed(error.into_canonical(key)),
            (status, version, error) => {
                return Err(malformed(
                    "operation item",
                    &format_args!(
                        "{key}: {status:?} with version {version:?} and {} error in a {} \
                         operation",
                        if error.is_some() { "an" } else { "no" },
                        if dry_run { "dry-run" } else { "committing" },
                    ),
                ));
            }
        })
    }
}

impl ItemErrorDto {
    /// The local client's failure for the same record: the reason, verbatim when unknown,
    /// and every context entry. A non-string value keeps its JSON text.
    fn into_canonical(self, key: &str) -> CanonicalError {
        self.context
            .into_iter()
            .fold(
                AdmissionFailure::new(
                    AdmissionFailureReason::from_wire(&self.reason),
                    self.message,
                ),
                |failure, (name, value)| {
                    let value = match value {
                        Value::String(text) => text,
                        other => other.to_string(),
                    };
                    failure.with_context(name, value)
                },
            )
            .into_canonical(key)
    }
}

impl OperationDto {
    pub fn decode(self) -> Result<Operation, CanonicalError> {
        Ok(match self {
            Self::Registration(body) => Operation::Registration(body.decode_registration()?),
            Self::Deletion(body) => Operation::Deletion(body.decode_deletion()?),
        })
    }
}

impl OperationBody<RegistrationItemDto> {
    fn decode_registration(self) -> Result<RegistrationOperation, CanonicalError> {
        let dry_run = self.dry_run;
        let items = self
            .items
            .into_iter()
            .map(|item| {
                let gts_id = gts_id(&item.gts_id)?;
                let outcome = match item.state.decode(dry_run, &item.gts_id)? {
                    ItemState::Pending => RegistrationOutcome::Pending,
                    ItemState::Running => RegistrationOutcome::Running,
                    ItemState::Succeeded(resource_version) => {
                        RegistrationOutcome::Succeeded { resource_version }
                    }
                    ItemState::WouldSucceed => RegistrationOutcome::WouldSucceed,
                    ItemState::Unchanged(resource_version) => {
                        RegistrationOutcome::Unchanged { resource_version }
                    }
                    ItemState::Failed(error) => RegistrationOutcome::Failed { error },
                };
                Ok(RegistrationItemResult { gts_id, outcome })
            })
            .collect::<Result<_, CanonicalError>>()?;
        Ok(RegistrationOperation {
            operation_id: self.operation_id,
            status: self.status.into(),
            items,
        })
    }
}

impl OperationBody<DeletionItemDto> {
    fn decode_deletion(self) -> Result<DeletionOperation, CanonicalError> {
        let dry_run = self.dry_run;
        let items = self
            .items
            .into_iter()
            .map(|item| {
                let entity_key = entity_key(&item.entity_key)?;
                let outcome = match item.state.decode(dry_run, &item.entity_key)? {
                    ItemState::Pending => DeletionOutcome::Pending,
                    ItemState::Running => DeletionOutcome::Running,
                    ItemState::Succeeded(resource_version) => {
                        DeletionOutcome::Succeeded { resource_version }
                    }
                    ItemState::WouldSucceed => DeletionOutcome::WouldSucceed,
                    ItemState::Unchanged(_) => {
                        return Err(malformed(
                            "operation item",
                            &format_args!("{}: a deletion is never unchanged", item.entity_key),
                        ));
                    }
                    ItemState::Failed(error) => DeletionOutcome::Failed { error },
                };
                Ok(DeletionItemResult {
                    entity_key,
                    outcome,
                })
            })
            .collect::<Result<_, CanonicalError>>()?;
        Ok(DeletionOperation {
            operation_id: self.operation_id,
            status: self.status.into(),
            items,
        })
    }
}

// ---- responses: entities -------------------------------------------------------------

/// A present member, `null` included; with `#[serde(default)]` an absent one is `None`.
fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

/// One projected entity. An unselected document is absent; a selected JSON `null` is
/// present as `null`, and the two stay apart.
#[derive(Debug, Deserialize)]
pub struct EntityDto {
    gts_id: String,
    gts_uuid: Uuid,
    kind: EntityKindDto,
    lifecycle_status: LifecycleStatusDto,
    #[serde(default)]
    origin: Option<OriginDto>,
    #[serde(default, deserialize_with = "present")]
    content: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    resolved_schema: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    effective_traits: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    effective_traits_schema: Option<Value>,
    #[serde(default)]
    provenance: Option<ProvenanceDto>,
}

/// Federation's `external` origin is outside P0 and is refused, not guessed.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OriginDto {
    Managed {
        resource_version: u64,
        #[serde(with = "time::serde::rfc3339")]
        created_at: OffsetDateTime,
        #[serde(with = "time::serde::rfc3339")]
        updated_at: OffsetDateTime,
    },
}

#[derive(Debug, Deserialize)]
struct ProvenanceDto {
    compatibility_check_skipped: bool,
}

impl EntityDto {
    pub fn decode(self) -> Result<Entity, CanonicalError> {
        Ok(Entity {
            gts_id: gts_id(&self.gts_id)?,
            gts_uuid: self.gts_uuid,
            kind: self.kind.into(),
            lifecycle_status: self.lifecycle_status.into(),
            origin: self.origin.map(|origin| match origin {
                OriginDto::Managed {
                    resource_version,
                    created_at,
                    updated_at,
                } => Origin::Managed {
                    resource_version,
                    created_at,
                    updated_at,
                },
            }),
            content: self.content,
            resolved_schema: self.resolved_schema,
            effective_traits: self.effective_traits,
            effective_traits_schema: self.effective_traits_schema,
            provenance: self.provenance.map(|p| Provenance {
                compatibility_check_skipped: p.compatibility_check_skipped,
            }),
        })
    }
}

// ---- responses: batch read and discovery ---------------------------------------------------

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LookupStatusDto {
    Found,
    Unchanged,
    NotFound,
}

#[derive(Debug, Deserialize)]
struct LookupDto {
    entity_key: String,
    status: LookupStatusDto,
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    entity: Option<EntityDto>,
}

/// The `:batchGet` answer: one lookup per distinct key.
#[derive(Debug, Deserialize)]
pub struct LookupsDto {
    items: Vec<LookupDto>,
}

impl LookupDto {
    fn decode(self) -> Result<(EntityKey, EntityLookup), CanonicalError> {
        let key = entity_key(&self.entity_key)?;
        let lookup = match (self.status, self.etag, self.entity) {
            (LookupStatusDto::Found, Some(etag), Some(entity)) => EntityLookup::Found {
                entity: Box::new(entity.decode()?),
                etag: validator(&etag)?,
            },
            (LookupStatusDto::Unchanged, Some(etag), None) => EntityLookup::Unchanged {
                etag: validator(&etag)?,
            },
            (LookupStatusDto::NotFound, None, None) => EntityLookup::NotFound,
            (status, etag, entity) => {
                return Err(malformed(
                    "lookup",
                    &format_args!(
                        "{}: {status:?} with{} etag and with{} entity",
                        self.entity_key,
                        if etag.is_some() { "" } else { "out" },
                        if entity.is_some() { "" } else { "out" },
                    ),
                ));
            }
        };
        Ok((key, lookup))
    }
}

impl LookupsDto {
    pub fn decode(self) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        let mut lookups = HashMap::with_capacity(self.items.len());
        for item in self.items {
            let (key, lookup) = item.decode()?;
            match lookups.entry(key) {
                Entry::Vacant(slot) => {
                    slot.insert(lookup);
                }
                Entry::Occupied(slot) => {
                    return Err(malformed("lookup", &format_args!("{} twice", slot.key())));
                }
            }
        }
        Ok(BatchGetEntitiesResponse(lookups))
    }
}

/// One discovery page; the applied `limit` is not modelled.
#[derive(Debug, Deserialize)]
pub struct PageDto {
    items: Vec<EntityDto>,
    page_info: PageInfoDto,
}

#[derive(Debug, Deserialize)]
struct PageInfoDto {
    #[serde(default)]
    next_cursor: Option<String>,
}

impl PageDto {
    pub fn decode(self) -> Result<ListEntitiesResponse, CanonicalError> {
        Ok(ListEntitiesResponse {
            items: self
                .items
                .into_iter()
                .map(EntityDto::decode)
                .collect::<Result<_, _>>()?,
            next: self.page_info.next_cursor.map(Cursor::from_token),
        })
    }
}

#[cfg(test)]
#[path = "dto_tests.rs"]
mod dto_tests;
