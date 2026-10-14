//! Test helpers shared across crate-internal tests of the GTS Types
//! Registry adapters. Compiled under `#[cfg(test)]` only.

#![cfg(test)]

use std::sync::Arc;

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_gts::{gts_id, gts_uri};
use types_registry_sdk::testing_platform::MockTypesRegistry;
use uuid::Uuid;

/// A registry holding what platform bootstrap reads: the linked inventory
/// (the AM tenant-type envelope among it), the platform root tenant type
/// with no allowed parents, and the AM tenant projection schema pinning
/// `name` to `minLength: 1, maxLength: 255`.
pub fn bootstrap_registry() -> Arc<MockTypesRegistry> {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.seed_inventory();
    registry.seed(
        gts_id!("cf.core.am.tenant_type.v1~cf.core.am.platform.v1~"),
        serde_json::json!({
            "$id": gts_uri!("cf.core.am.tenant_type.v1~cf.core.am.platform.v1~"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [{ "$ref": gts_uri!("cf.core.am.tenant_type.v1~") }],
            "x-gts-traits": { "allowed_parent_types": [] },
        }),
    );
    registry.seed(
        gts_id!("cf.core.am.tenant.v1~"),
        serde_json::json!({
            "$id": gts_uri!("cf.core.am.tenant.v1~"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "required": ["id", "name"],
            "properties": {
                "id": { "type": "string", "format": "uuid" },
                "name": { "type": "string", "minLength": 1, "maxLength": 255 },
                "parent_id": { "type": ["string", "null"], "format": "uuid" },
            },
        }),
    );
    registry
}

/// A platform registry whose batch reads answer snapshots edited after the
/// read: what admission never stores (a trait value its traits schema
/// refuses) or what a broken client might answer (a selected document
/// missing), to exercise adapters' defensive branches.
pub struct Tampered {
    inner: Arc<types_registry_sdk::testing_platform::MockTypesRegistry>,
    edit: Box<SnapshotEdit>,
}

type SnapshotEdit = dyn Fn(&mut types_registry_sdk::Entity) + Send + Sync;

impl Tampered {
    pub fn new(
        inner: Arc<types_registry_sdk::testing_platform::MockTypesRegistry>,
        edit: impl Fn(&mut types_registry_sdk::Entity) + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner,
            edit: Box::new(edit),
        }
    }

    /// Answers without the documents the caller selected — content and every
    /// materialization.
    pub fn without_documents(
        inner: Arc<types_registry_sdk::testing_platform::MockTypesRegistry>,
    ) -> Self {
        Self::new(inner, |entity| {
            entity.content = None;
            entity.resolved_schema = None;
            entity.effective_traits = None;
            entity.effective_traits_schema = None;
        })
    }
}

#[async_trait]
impl types_registry_sdk::PlatformTypesRegistryApi for Tampered {
    async fn batch_get_entities(
        &self,
        ctx: &toolkit_security::PlatformSecurityContext,
        request: types_registry_sdk::BatchGetEntitiesRequest,
    ) -> Result<types_registry_sdk::BatchGetEntitiesResponse, CanonicalError> {
        let mut response = self.inner.batch_get_entities(ctx, request).await?;
        for lookup in response.0.values_mut() {
            if let types_registry_sdk::EntityLookup::Found { entity, .. } = lookup {
                (self.edit)(entity);
            }
        }
        Ok(response)
    }

    async fn list_entities(
        &self,
        ctx: &toolkit_security::PlatformSecurityContext,
        request: types_registry_sdk::ListEntitiesRequest,
    ) -> Result<types_registry_sdk::ListEntitiesResponse, CanonicalError> {
        self.inner.list_entities(ctx, request).await
    }

    async fn register_entities(
        &self,
        ctx: &toolkit_security::PlatformSecurityContext,
        key: types_registry_sdk::IdempotencyKey,
        request: types_registry_sdk::RegisterEntitiesRequest,
    ) -> Result<types_registry_sdk::RegistrationOperation, CanonicalError> {
        self.inner.register_entities(ctx, key, request).await
    }

    async fn delete_entities(
        &self,
        ctx: &toolkit_security::PlatformSecurityContext,
        key: types_registry_sdk::IdempotencyKey,
        request: types_registry_sdk::DeleteEntitiesRequest,
    ) -> Result<types_registry_sdk::DeletionOperation, CanonicalError> {
        self.inner.delete_entities(ctx, key, request).await
    }

    async fn get_operation(
        &self,
        ctx: &toolkit_security::PlatformSecurityContext,
        operation_id: Uuid,
    ) -> Result<types_registry_sdk::Operation, CanonicalError> {
        self.inner.get_operation(ctx, operation_id).await
    }
}

/// A platform registry that answers every read by UUID with the same
/// Type Schema, carrying the requested UUID, and every read by
/// identifier with `NotFound`. For fixtures whose seeded rows carry
/// opaque tenant-type UUIDs no real registry would know: only the
/// typed identifier on the answer is load-bearing. Reads by identifier
/// miss, so AM-owned resource schemas (`gts.cf.core.am.tenant.v1~` /
/// `.user.v1~`) are absent and payload validation short-circuits.
pub struct ConstantTypeRegistry(pub &'static str);

fn not_implemented() -> CanonicalError {
    CanonicalError::internal("not implemented for the constant test registry").create()
}

#[async_trait]
impl types_registry_sdk::PlatformTypesRegistryApi for ConstantTypeRegistry {
    async fn batch_get_entities(
        &self,
        _ctx: &toolkit_security::PlatformSecurityContext,
        request: types_registry_sdk::BatchGetEntitiesRequest,
    ) -> Result<types_registry_sdk::BatchGetEntitiesResponse, CanonicalError> {
        let gts_id = types_registry_sdk::GtsId::try_new(self.0).expect("valid id");
        let answers = request
            .items
            .into_iter()
            .map(|item| {
                let lookup = match item.key {
                    types_registry_sdk::EntityKey::GtsUuid(uuid) => {
                        types_registry_sdk::EntityLookup::Found {
                            entity: Box::new(types_registry_sdk::Entity {
                                gts_id: gts_id.clone(),
                                gts_uuid: uuid,
                                kind: types_registry_sdk::EntityKind::TypeSchema,
                                lifecycle_status: types_registry_sdk::LifecycleStatus::Active,
                                origin: None,
                                content: None,
                                resolved_schema: None,
                                effective_traits: None,
                                effective_traits_schema: None,
                                provenance: None,
                            }),
                            etag: types_registry_sdk::Validator::from_bytes("constant"),
                        }
                    }
                    types_registry_sdk::EntityKey::GtsId(_) => {
                        types_registry_sdk::EntityLookup::NotFound
                    }
                };
                (item.key, lookup)
            })
            .collect();
        Ok(types_registry_sdk::BatchGetEntitiesResponse(answers))
    }

    async fn list_entities(
        &self,
        _ctx: &toolkit_security::PlatformSecurityContext,
        _request: types_registry_sdk::ListEntitiesRequest,
    ) -> Result<types_registry_sdk::ListEntitiesResponse, CanonicalError> {
        Err(not_implemented())
    }

    async fn register_entities(
        &self,
        _ctx: &toolkit_security::PlatformSecurityContext,
        _key: types_registry_sdk::IdempotencyKey,
        _request: types_registry_sdk::RegisterEntitiesRequest,
    ) -> Result<types_registry_sdk::RegistrationOperation, CanonicalError> {
        Err(not_implemented())
    }

    async fn delete_entities(
        &self,
        _ctx: &toolkit_security::PlatformSecurityContext,
        _key: types_registry_sdk::IdempotencyKey,
        _request: types_registry_sdk::DeleteEntitiesRequest,
    ) -> Result<types_registry_sdk::DeletionOperation, CanonicalError> {
        Err(not_implemented())
    }

    async fn get_operation(
        &self,
        _ctx: &toolkit_security::PlatformSecurityContext,
        _operation_id: Uuid,
    ) -> Result<types_registry_sdk::Operation, CanonicalError> {
        Err(not_implemented())
    }
}
