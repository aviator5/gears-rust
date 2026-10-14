//! Registration of Account Management's configured platform-root tenant type.
//!
//! AM reconciles the root type into the database-backed Types Registry
//! through the in-process platform API during its own `init()`, and waits
//! for admission before bootstrap reads it. An equal document submits
//! nothing; a permitted content change (`idp_provisioning`) is an update
//! under the registry's ordinary compatibility rules, and a refused one
//! fails startup naming the identifier.
//!
//! The caller validates the stored root binding before calling this, so a
//! configured-type mismatch never reaches the registry.

use std::time::Duration;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use toolkit_gts::GTS_ID_URI_PREFIX;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    GtsId, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt, PublisherContext,
    ReconcileOptions, ReconcileReport,
};

use crate::domain::root_type::{RootTypeConfig, TENANT_TYPE_BASE};

const JSON_SCHEMA_DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";

/// Budget for registering the root type from `init()`. With several registry
/// instances on one database an admission can wait up to a minute for another
/// instance's outbox.
const REGISTRATION_DEADLINE: Duration = Duration::from_secs(120);

/// Reconcile the configured root-type definition and wait for its admission.
///
/// # Errors
/// An invalid configuration, a refused or unsettled admission (the error
/// names the identifier and its outcome), the deadline or cancellation.
pub async fn register_root_type(
    registry: &dyn PlatformTypesRegistryApi,
    cfg: &RootTypeConfig,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    let id = GtsId::try_new(cfg.validated_id().map_err(anyhow::Error::msg)?)?;
    let desired = desired_root_schema(cfg)?;
    let publisher = PublisherContext {
        name: "account-management".to_owned(),
        version: env!("CARGO_PKG_VERSION").parse()?,
    };
    let mut options = ReconcileOptions::default();
    options.deadline = REGISTRATION_DEADLINE;
    registry
        .reconcile_entities_and_await(
            &PlatformSecurityContext::outbound_marker(),
            &publisher,
            &[(id.clone(), desired)],
            &options,
            cancel,
        )
        .await
        .map_err(|error| {
            let outcome = ReconcileReport::from_canonical(&error)
                .and_then(|report| report.outcomes.get(&id).cloned());
            anyhow::anyhow!("root tenant type {id} was not admitted: {error}; outcome: {outcome:?}")
        })?;
    Ok(())
}

/// Build the single AM-owned concrete root schema.
pub fn desired_root_schema(cfg: &RootTypeConfig) -> anyhow::Result<Value> {
    // The configuration guarantees a type directly derived from the envelope.
    let type_id = cfg.validated_id().map_err(anyhow::Error::msg)?;
    Ok(json!({
        "$id": format!("{GTS_ID_URI_PREFIX}{type_id}"),
        "$schema": JSON_SCHEMA_DRAFT_07,
        "description": "Platform-root tenant type (no parents).",
        "type": "object",
        "allOf": [{ "$ref": format!("{GTS_ID_URI_PREFIX}{TENANT_TYPE_BASE}") }],
        "x-gts-traits": {
            "allowed_parent_types": [],
            "idp_provisioning": cfg.idp_provisioning,
        }
    }))
}

#[cfg(test)]
#[path = "root_type_tests.rs"]
mod tests;
