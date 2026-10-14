//! Single-tenant resolver plugin gear.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tenant_resolver_sdk::{TenantResolverPluginClient, TenantResolverPluginSpecV1};
use toolkit::Gear;
use toolkit::client_hub::ClientScope;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use toolkit_security::PlatformSecurityContext;
use tracing::info;
use types_registry_sdk::{
    GtsId, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt, PublisherContext,
    ReconcileOptions, ReconcileReport,
};

use crate::config::SingleTenantTrPluginConfig;
use crate::domain::Service;

/// Budget for publishing the plugin instance from `init()`. With several registry instances
/// on one database an admission can wait up to a minute for another instance's outbox.
const PUBLICATION_DEADLINE: Duration = Duration::from_secs(120);

/// Single-tenant resolver plugin gear.
///
/// Zero-configuration plugin for single-tenant deployments.
/// Returns the tenant from security context as the only accessible tenant.
#[toolkit::gear(
    name = "single-tenant-tr-plugin",
    deps = [types_registry]
)]
pub struct SingleTenantTrPlugin;

impl Default for SingleTenantTrPlugin {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl Gear for SingleTenantTrPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: SingleTenantTrPluginConfig = ctx.config_or_default()?;
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            "Loaded single-tenant resolver plugin configuration"
        );

        // Build registration payload and instance id for this plugin.
        let (instance_id, instance_json) =
            PluginV1::<TenantResolverPluginSpecV1>::build_registration(
                "cf.builtin.single_tenant_resolver.plugin.v1",
                &cfg.vendor,
                cfg.priority,
            )?;

        // Publish to types-registry; the backend is registered only once the instance is admitted.
        let registry = ctx.client_hub().get::<dyn PlatformTypesRegistryApi>()?;
        let publisher = PublisherContext {
            name: Self::MODULE_NAME.to_owned(),
            version: env!("CARGO_PKG_VERSION").parse()?,
        };
        let mut options = ReconcileOptions::default();
        options.deadline = PUBLICATION_DEADLINE;
        let gts_id = GtsId::try_new(instance_id.as_ref())?;
        registry
            .reconcile_entities_and_await(
                &PlatformSecurityContext::outbound_marker(),
                &publisher,
                &[(gts_id.clone(), instance_json)],
                &options,
                ctx.cancellation_token(),
            )
            .await
            .map_err(|error| {
                let outcome = ReconcileReport::from_canonical(&error)
                    .and_then(|report| report.outcomes.get(&gts_id).cloned());
                anyhow::anyhow!(
                    "publishing plugin instance {instance_id} failed: {error}; outcome: {outcome:?}"
                )
            })?;

        // Create service and register scoped client in ClientHub
        let service = Arc::new(Service);
        let api: Arc<dyn TenantResolverPluginClient> = service;
        ctx.client_hub()
            .register_scoped::<dyn TenantResolverPluginClient>(
                ClientScope::gts_id(&instance_id),
                api,
            );

        info!(instance_id = %instance_id, vendor = %cfg.vendor, priority = cfg.priority, "registered");
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gear_tests.rs"]
mod gear_tests;
