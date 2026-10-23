//! Who a Types Registry read is made for (types-registry P0 decision P33).
//!
//! A read made while AM serves a request goes through
//! [`TypesRegistryApi`] under that request's own [`SecurityContext`], passed
//! down unchanged. A platform operation with no request behind it —
//! bootstrap, retention, the provisioning reaper — reads through
//! [`PlatformTypesRegistryApi`]. A helper serving both takes a [`TypesRegistryReader`],
//! so each call site names its caller. No variant is built from the other:
//! a tenant-API failure is returned as is, never retried on the platform
//! API, and no context is constructed for a caller that has none.

use std::collections::HashMap;

use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use types_registry_sdk::{
    GtsTypeId, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt, ReadOptions, TypeSchema,
    TypesRegistryApi, TypesRegistryApiExt,
};
use uuid::Uuid;

/// The registry and the caller a read is made for.
#[domain_model]
#[derive(Clone, Copy)]
pub enum TypesRegistryReader<'a> {
    /// A read for the request whose context `ctx` is.
    Caller {
        api: &'a dyn TypesRegistryApi,
        ctx: &'a SecurityContext,
    },
    /// A platform operation's own read: there is no request and no caller.
    Platform(&'a dyn PlatformTypesRegistryApi),
}

impl<'a> TypesRegistryReader<'a> {
    /// A read for `ctx`'s request.
    #[must_use]
    pub fn caller(api: &'a dyn TypesRegistryApi, ctx: &'a SecurityContext) -> Self {
        Self::Caller { api, ctx }
    }

    /// See [`TypesRegistryApiExt::get_type_schema`].
    ///
    /// # Errors
    /// The registry's.
    pub async fn get_type_schema(
        self,
        type_id: &GtsTypeId,
        options: ReadOptions,
    ) -> Result<TypeSchema, CanonicalError> {
        match self {
            Self::Caller { api, ctx } => api.get_type_schema(ctx, type_id, options).await,
            Self::Platform(api) => {
                api.get_type_schema(
                    &PlatformSecurityContext::outbound_marker(),
                    type_id,
                    options,
                )
                .await
            }
        }
    }

    /// See [`TypesRegistryApiExt::resolve_type_id`].
    ///
    /// # Errors
    /// The registry's.
    pub async fn resolve_type_id(self, type_uuid: Uuid) -> Result<GtsTypeId, CanonicalError> {
        match self {
            Self::Caller { api, ctx } => api.resolve_type_id(ctx, type_uuid).await,
            Self::Platform(api) => {
                api.resolve_type_id(&PlatformSecurityContext::outbound_marker(), type_uuid)
                    .await
            }
        }
    }

    /// See [`TypesRegistryApiExt::resolve_type_ids`].
    ///
    /// # Errors
    /// The registry's.
    pub async fn resolve_type_ids(
        self,
        type_uuids: &[Uuid],
    ) -> Result<HashMap<Uuid, Option<GtsTypeId>>, CanonicalError> {
        match self {
            Self::Caller { api, ctx } => api.resolve_type_ids(ctx, type_uuids).await,
            Self::Platform(api) => {
                api.resolve_type_ids(&PlatformSecurityContext::outbound_marker(), type_uuids)
                    .await
            }
        }
    }
}
