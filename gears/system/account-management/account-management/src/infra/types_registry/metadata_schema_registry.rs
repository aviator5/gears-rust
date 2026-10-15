//! `GtsMetadataSchemaRegistry` — production [`MetadataSchemaRegistry`]
//! wired against `types_registry_sdk::PlatformTypesRegistryApi` resolved
//! from `ClientHub`.
//!
//! Responsibilities (see
//! [`crate::domain::metadata::registry::MetadataSchemaRegistry`]):
//!
//! 1. *Existence* — surface unknown schemas as
//!    [`DomainError::MetadataEntryNotFound`] BEFORE any downstream
//!    DB read or write. AM does not distinguish "schema unknown" from
//!    "entry missing" on the wire — both collapse into the unified
//!    metadata 404.
//! 2. *Inheritance policy* — read the schema's
//!    `x-gts-traits.inheritance_policy` (effective, chain-merged) and
//!    map onto [`InheritancePolicy::Inherit`] / [`InheritancePolicy::OverrideOnly`].
//!    Default per FEATURE §3.1 is `override_only`, so a missing /
//!    null / non-`"inherit"` value collapses to `OverrideOnly`.
//! 3. *Reverse hydration* — `schema_uuid → String` via the registry's
//!    reads by UUID; the `type_id` on the schema is returned verbatim
//!    as the chained `gts.cf.core.am.tenant_metadata.v1~vendor.app.foo.v1~`
//!    string.
//!
//! A deleted schema is as absent as an unknown one: exact reads answer
//! tombstones, which the legacy catalogue never held, and a row whose
//! chain was deleted is an orphan. A snapshot without a selected
//! document is an internal contract break, never an empty document.
//!
//! Determinism contract (per the trait): the adapter MUST NOT cache
//! across calls. Every invocation re-resolves through the SDK so trait
//! updates take effect immediately. Any short-lived caching is the SDK
//! client's to do.

use async_trait::async_trait;
use gts::{GtsId, GtsTypeId};
use serde_json::Value;
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    EntityField, FieldSelection, LifecycleStatus, PlatformTypesRegistryApi,
    PlatformTypesRegistryApiExt, Projection, ReadOptions, TypeSchema,
};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::metadata::registry::{InheritancePolicy, MetadataSchemaRegistry};

/// Top-level key in the effective trait map carrying the inheritance
/// policy enum value. Defined on the `gts.cf.core.am.tenant_metadata.v1~`
/// envelope's `x-gts-traits-schema` (see
/// `account_management_sdk::gts_envelopes::TenantMetadataTraits`, the
/// Rust source of truth the macro emits the trait schema from).
const INHERITANCE_POLICY_TRAIT: &str = "inheritance_policy";

/// Wire token for the `Inherit` policy. Anything else (missing key,
/// `null`, non-string value, unknown string) collapses to the
/// documented default ([`InheritancePolicy::OverrideOnly`]) per
/// FEATURE §3.1.
const INHERIT_POLICY_TOKEN: &str = "inherit";
const OVERRIDE_POLICY_TOKEN: &str = "override_only";

/// Production [`MetadataSchemaRegistry`] backed by the GTS Types
/// Registry.
pub struct GtsMetadataSchemaRegistry {
    registry: Arc<dyn PlatformTypesRegistryApi>,
}

impl GtsMetadataSchemaRegistry {
    /// Construct a new registry adapter around the platform registry
    /// client resolved from `ClientHub`.
    #[must_use]
    pub fn new(registry: Arc<dyn PlatformTypesRegistryApi>) -> Self {
        Self { registry }
    }
}

/// Map a `CanonicalError` onto the appropriate `DomainError` for
/// the schema-registry seam:
///
/// * `CanonicalError::NotFound` → [`DomainError::MetadataEntryNotFound`]
///   (HTTP 404 with the unified `resource_type =
///   gts.cf.core.am.tenant_metadata.v1~`).
/// * any other transport / registry error → `ServiceUnavailable`.
fn map_registry_err(err: CanonicalError, schema_token: &str) -> DomainError {
    match err {
        CanonicalError::NotFound { .. } => not_registered(schema_token),
        other => DomainError::service_unavailable(format!("types-registry: {other}")),
    }
}

/// One Type Schema by identifier under `projection`; a deleted schema is
/// as absent as an unknown one.
async fn read_schema(
    registry: &dyn PlatformTypesRegistryApi,
    type_id: &GtsTypeId,
    projection: Projection,
) -> Result<TypeSchema, DomainError> {
    let schema_str = type_id.as_ref();
    let schema = registry
        .get_type_schema(
            &PlatformSecurityContext::outbound_marker(),
            type_id,
            ReadOptions {
                projection,
                ..ReadOptions::default()
            },
        )
        .await
        .map_err(|err| map_registry_err(err, schema_str))?;
    if schema.lifecycle_status != LifecycleStatus::Active {
        return Err(not_registered(schema_str));
    }
    Ok(schema)
}

fn not_registered(schema_token: &str) -> DomainError {
    DomainError::MetadataEntryNotFound {
        detail: format!("schema {schema_token} is not registered in the types registry"),
        entry: schema_token.to_owned(),
    }
}

fn missing_document(type_id: &GtsTypeId, field: &str) -> DomainError {
    DomainError::internal(format!(
        "types-registry answered metadata schema `{type_id}` without the selected {field}"
    ))
}

/// The UUID round-trip guard: re-derive the UUID from the answered
/// `type_id` and confirm it matches the requested one.
fn round_tripped(schema: &TypeSchema, schema_uuid: Uuid) -> Result<GtsTypeId, DomainError> {
    // The registry has already validated `schema.type_id`.
    // Defense-in-depth: a client bug that mapped an arbitrary schema to
    // a `schema_uuid` would otherwise let a List flow re-hydrate the
    // wrong public id alongside a tenant's stored row. Surface as
    // `Internal` so the bug is loud.
    //
    // We do NOT re-validate the chain-prefix shape here: the only
    // callers of this registry are the metadata flows, and a
    // non-tenant-metadata schema in the storage row could only have
    // landed there via a manual write that bypassed
    // `MetadataService::upsert_metadata` (which validates via
    // `ParsedTypeId::parse`). The UUID round-trip check catches the
    // same class of bug end-to-end without coupling the registry
    // adapter to the AM root-segment constant.
    let raw = schema.type_id.as_ref();
    if uuid_for_registered_schema(raw) != schema_uuid {
        return Err(DomainError::Internal {
            diagnostic: format!(
                "types-registry returned schema {raw} for uuid {schema_uuid} but the AM-side \
                 UUIDv5 derivation does not round-trip; possible SDK bug or schema renaming \
                 mid-flight"
            ),
            cause: None,
        });
    }
    Ok(GtsTypeId::new(raw))
}

/// Compute the deterministic `schema_uuid` for an already-validated
/// `type_id` string. Mirrors the helper in
/// [`crate::domain::metadata::registry`] — both rely on
/// `gts::GtsId::to_uuid()` for the canonical namespace.
///
/// # Panics
///
/// Panics if `type_id` is not parseable as a GTS id. Callers MUST
/// pass strings already validated by
/// [`crate::domain::metadata::type_id::ParsedTypeId::parse`] (the
/// service-layer guard runs before the registry sees the value).
#[allow(
    clippy::expect_used,
    reason = "callers validate via ParsedTypeId before invoking registry; \
              an unparseable input here is a service-layer contract break"
)]
fn uuid_for_registered_schema(type_id: &str) -> Uuid {
    GtsId::try_new(type_id)
        .expect(
            "types-registry returned a type_id that does not parse as a GTS id - \
             upstream SDK contract break",
        )
        .to_uuid()
}

#[async_trait]
impl MetadataSchemaRegistry for GtsMetadataSchemaRegistry {
    async fn resolve_inheritance_policy(
        &self,
        type_id: &GtsTypeId,
    ) -> Result<InheritancePolicy, DomainError> {
        let schema = read_schema(
            self.registry.as_ref(),
            type_id,
            Projection::Select(FieldSelection::with(&[EntityField::EffectiveTraits])),
        )
        .await?;

        // The registry materializes `effective_traits` walking self →
        // ancestors with rightmost-wins semantics, then layering
        // schema-declared `default` values from every
        // `x-gts-traits-schema` block in the chain. The base
        // envelope (`gts.cf.core.am.tenant_metadata.v1~`) declares
        // `inheritance_policy: { default: "override_only" }` so a
        // properly-derived schema always carries a string value here.
        let effective = schema
            .effective_traits
            .as_ref()
            .ok_or_else(|| missing_document(type_id, "effective_traits"))?;
        let raw = effective.get(INHERITANCE_POLICY_TRAIT);
        let policy = match raw {
            // Explicit `"inherit"` opt-in.
            Some(v) if v.as_str() == Some(INHERIT_POLICY_TOKEN) => InheritancePolicy::Inherit,
            // Explicit `"override_only"` OR an absent / null trait
            // (FEATURE §3.1 defaults to override-only when the trait
            // is unspecified). Both are valid wire shapes; treat as
            // the documented default silently.
            Some(v) if v.as_str() == Some(OVERRIDE_POLICY_TOKEN) => InheritancePolicy::OverrideOnly,
            None => InheritancePolicy::OverrideOnly,
            Some(v) if v.is_null() => InheritancePolicy::OverrideOnly,
            // Truly unknown shape: typo, future variant, wrong wire
            // type. Warn so the operator sees the drift before users
            // observe unexpected walk-up behaviour, then default to
            // override-only (the safe-default in the walk-up
            // algorithm -- an unrecognised value cannot accidentally
            // promote a schema to inheriting from ancestors).
            Some(other) => {
                tracing::warn!(
                    target: "am.metadata",
                    type_id = %type_id,
                    raw_value = ?other,
                    "unknown inheritance_policy value; defaulting to override_only"
                );
                InheritancePolicy::OverrideOnly
            }
        };
        Ok(policy)
    }

    async fn resolve_ids_by_uuid(
        &self,
        schema_uuids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, GtsTypeId>, DomainError> {
        // One batch read per page. A failed read fails the whole lookup;
        // unknown and deleted schemas are omitted (page-poisoning guard)
        // and the service layer decides per-row how to surface the miss.
        let found = self
            .registry
            .batch_get_type_schemas_by_uuid(
                &PlatformSecurityContext::outbound_marker(),
                schema_uuids,
                ReadOptions::default(),
            )
            .await
            .map_err(|err| DomainError::service_unavailable(format!("types-registry: {err}")))?;
        let mut out = std::collections::HashMap::with_capacity(found.len());
        for (uuid, schema) in found {
            if let Some(schema) = schema
                && schema.lifecycle_status == LifecycleStatus::Active
            {
                out.insert(uuid, round_tripped(&schema, uuid)?);
            }
        }
        Ok(out)
    }

    async fn resolve_id_by_uuid(&self, schema_uuid: Uuid) -> Result<GtsTypeId, DomainError> {
        let token = schema_uuid.to_string();
        let schema = self
            .registry
            .get_type_schema_by_uuid(
                &PlatformSecurityContext::outbound_marker(),
                schema_uuid,
                ReadOptions::default(),
            )
            .await
            .map_err(|err| map_registry_err(err, &token))?;
        if schema.lifecycle_status != LifecycleStatus::Active {
            return Err(not_registered(&token));
        }
        round_tripped(&schema, schema_uuid)
    }

    async fn validate_value(&self, type_id: &GtsTypeId, value: &Value) -> Result<(), DomainError> {
        // Mirrors the `validate_provision_input_metadata_via_gts` pattern
        // from `domain::gts_validation` (the canonical AM seam for GTS
        // body validation) — single round-trip to the registry, then
        // `jsonschema` on the registry-resolved schema, then
        // `iter_errors` to collect every violation into one
        // `DomainError::MetadataValidation` detail.
        //
        // The error-shape differs by one variant vs the IdP helper:
        // a missing schema here surfaces as
        // [`DomainError::MetadataEntryNotFound`] (HTTP 404 with the
        // unified metadata `resource_type`), not `Internal` — metadata
        // schemas are caller-named so an unregistered chain is a
        // public 404, not a deploy-prerequisite failure.
        let schema = read_schema(
            self.registry.as_ref(),
            type_id,
            Projection::Select(FieldSelection::with(&[EntityField::ResolvedSchema])),
        )
        .await?;
        let resolved = schema
            .resolved_schema
            .ok_or_else(|| missing_document(type_id, "resolved_schema"))?;
        // The base AM metadata envelope declares `$schema:
        // "http://json-schema.org/draft-07/schema#"`, and derived metadata
        // schemas carry `$schema: gts_uri!("cf.core.am.tenant_metadata.v1~")`
        // -- the GTS chain identifier that AM uses for inheritance. The
        // generic `jsonschema::validator_for` auto-detects the draft from
        // `$schema` and fails closed when the URI is not a registered
        // meta-schema (which custom GTS chain ids never are). Pin the
        // draft to 7 explicitly so the chain id is treated as opaque
        // metadata and validation runs against the well-known draft the
        // envelope was authored for.
        let validator = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft7)
            .build(&resolved)
            .map_err(|err| {
                // Catalog drift: the schema body in the registry is not a
                // valid JSON Schema. Operator action required; surface as
                // `Internal` so the public envelope does not pretend the
                // caller's payload is the problem.
                DomainError::Internal {
                    diagnostic: format!(
                        "GTS metadata schema `{type_id}` is not a valid JSON Schema \
                         (catalog drift): {err}"
                    ),
                    cause: None,
                }
            })?;
        // An instance of the metadata type is `{"payload": {…}}`, not the bare
        // object (§4.4.1, `TenantMetadataEnvelopeV1`). AM stores the payload content,
        // so the envelope is composed here rather than demanded of callers — the only
        // seam that validates a metadata value as a whole document.
        let enveloped = serde_json::json!({ "payload": value });
        let errors: Vec<String> = validator
            .iter_errors(&enveloped)
            .map(|e| e.to_string())
            .collect();
        if !errors.is_empty() {
            return Err(DomainError::MetadataValidation {
                detail: format!(
                    "metadata value violates registered schema `{type_id}`: {}",
                    errors.join("; ")
                ),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "metadata_schema_registry_tests.rs"]
mod metadata_schema_registry_tests;
