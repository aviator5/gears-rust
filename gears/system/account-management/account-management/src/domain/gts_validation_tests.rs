//! Unit tests for [`super::validate_new_user_payload_via_gts`] and
//! [`super::validate_tenant_name_via_gts`].
//!
//! Pin the documented failure-mode arms (mirrors the production
//! gear doc on `gts_validation.rs`):
//!
//! 1. Schema not registered (`CanonicalError::NotFound`):
//!    * `validate_new_user_payload_via_gts` →
//!      `DomainError::ServiceUnavailable` (fail-closed — no
//!      AM-side storage gate for users, so the boundary helper
//!      MUST be authoritative on `format` / `pattern` rules).
//!    * `validate_tenant_name_via_gts` → `Ok(())` (DB CHECK +
//!      bootstrap-before-catalog ordering remain authoritative).
//! 2. Other `CanonicalError` (transport / availability) →
//!    `DomainError::ServiceUnavailable`.
//! 3. Schema returned but `jsonschema::validator_for` rejects it
//!    (catalog drift) → `DomainError::Internal` — schema published
//!    by the deploy bundle is not a valid JSON Schema; operator
//!    action required. Exercised implicitly by the production
//!    `Internal` arm in the helpers.
//! 4. Schema returned + instance fails validation →
//!    `DomainError::Validation` (HTTP 400 — the client can retry
//!    with a corrected payload).
//! 5. Schema returned + valid instance → `Ok(())` (happy path).
//!
//! Schemas are documents in `MockTypesRegistry`, which materializes
//! `resolved_schema` with `gts-rust` as admission does.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    reason = "test helpers"
)]

use std::sync::Arc;

use account_management_sdk::IdpNewUser;
use serde_json::{Value, json};
use toolkit_canonical_errors::CanonicalError;
use toolkit_gts::gts_uri;
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry, Seed};

use crate::domain::error::DomainError;
use crate::infra::types_registry::test_helpers::Tampered;

// ---- helpers -------------------------------------------------------

fn user_payload(username: &str) -> IdpNewUser {
    IdpNewUser::new(username.to_owned())
}

fn schema_doc(type_id: &str, body: &Value) -> Value {
    let mut doc = json!({
        "$id": gts_uri!(type_id),
        "$schema": "http://json-schema.org/draft-07/schema#",
    });
    doc.as_object_mut()
        .expect("object")
        .extend(body.as_object().expect("object").clone());
    doc
}

fn registry_with_user_schema(max_chars: usize) -> MockTypesRegistry {
    let registry = MockTypesRegistry::new();
    registry.seed(
        super::USER_TYPE_ID,
        schema_doc(
            super::USER_TYPE_ID,
            &json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["id", "username"],
                "properties": {
                    "id": { "type": "string", "format": "uuid" },
                    "username": { "type": "string", "minLength": 1, "maxLength": max_chars },
                    "email": { "type": "string", "format": "email" },
                    "display_name": { "type": "string", "minLength": 1, "maxLength": 255 },
                },
            }),
        ),
    );
    registry
}

fn registry_with_tenant_schema(max_chars: usize) -> MockTypesRegistry {
    let registry = MockTypesRegistry::new();
    registry.seed(
        super::TENANT_TYPE_ID,
        schema_doc(
            super::TENANT_TYPE_ID,
            &json!({
                "type": "object",
                "additionalProperties": true,
                "required": ["id", "name"],
                "properties": {
                    "id": { "type": "string", "format": "uuid" },
                    "name": { "type": "string", "minLength": 1, "maxLength": max_chars },
                    "parent_id": { "type": ["string", "null"], "format": "uuid" },
                },
            }),
        ),
    );
    registry
}

/// A registry whose every call fails as temporarily unavailable. The
/// helpers short-circuit only `NotFound`; every other category routes
/// to their `ServiceUnavailable` arm.
fn unavailable_registry() -> MockTypesRegistry {
    let registry = MockTypesRegistry::new();
    registry.inject(
        Fault::on(Call::BatchGet).fail(
            CanonicalError::service_unavailable()
                .with_detail("registry transport down (test stub)")
                .create(),
        ),
    );
    registry
}

// ---- validate_new_user_payload_via_gts ----------------------------

#[tokio::test]
async fn user_payload_schema_not_registered_surfaces_service_unavailable() {
    let registry = MockTypesRegistry::new();
    let payload = user_payload("alice");
    let err = super::validate_new_user_payload_via_gts(&payload, &registry)
        .await
        .expect_err(
            "schema-not-found MUST fail closed: provision_user has no AM-side \
             fallback gate and degrading to length-only checks would silently \
             disable format/pattern rules until the catalog is seeded",
        );
    match err {
        DomainError::ServiceUnavailable { detail, .. } => {
            assert!(
                detail.contains(super::USER_TYPE_ID) && detail.contains("catalog"),
                "ServiceUnavailable.detail must name the missing schema and the \
                 catalog-seed remediation; got: {detail}"
            );
        }
        other => panic!("expected ServiceUnavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn user_payload_valid_username_passes_registered_schema() {
    let registry = registry_with_user_schema(255);
    let payload = user_payload("alice");
    super::validate_new_user_payload_via_gts(&payload, &registry)
        .await
        .expect("valid username passes the registered schema");
}

#[tokio::test]
async fn user_payload_oversized_username_rejects_with_validation() {
    let registry = registry_with_user_schema(255);
    let payload = user_payload(&"x".repeat(256));
    let err = super::validate_new_user_payload_via_gts(&payload, &registry)
        .await
        .expect_err("oversize username must be rejected by the registered schema");
    match err {
        DomainError::Validation { detail } => {
            assert!(
                detail.contains("username"),
                "diagnostic must name the violating field; got: {detail}"
            );
        }
        other => panic!("expected Validation, got {other:?}"),
    }
}

#[tokio::test]
async fn user_payload_registry_transport_error_surfaces_as_service_unavailable() {
    let registry = unavailable_registry();
    let payload = user_payload("alice");
    let err = super::validate_new_user_payload_via_gts(&payload, &registry)
        .await
        .expect_err("registry transport error must surface");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "expected ServiceUnavailable, got {err:?}"
    );
}

// ---- validate_tenant_name_via_gts ---------------------------------

#[tokio::test]
async fn tenant_name_schema_not_registered_short_circuits_to_ok() {
    let registry = MockTypesRegistry::new();
    super::validate_tenant_name_via_gts("acme", &registry)
        .await
        .expect("schema-not-found short-circuits to Ok");
}

#[tokio::test]
async fn tenant_name_valid_passes_registered_schema() {
    let registry = registry_with_tenant_schema(255);
    super::validate_tenant_name_via_gts("acme", &registry)
        .await
        .expect("valid name passes the registered schema");
}

#[tokio::test]
async fn tenant_name_oversized_rejects_with_validation() {
    let registry = registry_with_tenant_schema(255);
    let oversized = "x".repeat(256);
    let err = super::validate_tenant_name_via_gts(&oversized, &registry)
        .await
        .expect_err("oversize name must be rejected");
    match err {
        DomainError::Validation { detail } => {
            assert!(
                detail.contains("name"),
                "diagnostic must name the violating field; got: {detail}"
            );
        }
        other => panic!("expected Validation, got {other:?}"),
    }
}

#[tokio::test]
async fn tenant_name_empty_rejects_with_validation() {
    let registry = registry_with_tenant_schema(255);
    let err = super::validate_tenant_name_via_gts("", &registry)
        .await
        .expect_err("empty name must be rejected");
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[tokio::test]
async fn tenant_name_registry_transport_error_surfaces_as_service_unavailable() {
    let registry = unavailable_registry();
    let err = super::validate_tenant_name_via_gts("acme", &registry)
        .await
        .expect_err("registry transport error must surface");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "expected ServiceUnavailable, got {err:?}"
    );
}

// ---- materialized schema ------------------------------------------

#[tokio::test]
async fn user_payload_honours_properties_inherited_through_a_reference() {
    // The user schema takes `username` from a referenced schema; the
    // registry inlines the reference into `resolved_schema`.
    let fields = "gts.cf.test.am.user_fields.v1~";
    let registry = MockTypesRegistry::new();
    registry.seed(
        fields,
        schema_doc(
            fields,
            &json!({
                "type": "object",
                "properties": {
                    "username": { "type": "string", "minLength": 1, "maxLength": 8 },
                    "email": { "type": "string", "format": "email" },
                    "display_name": { "type": "string" },
                },
            }),
        ),
    );
    registry.seed(
        super::USER_TYPE_ID,
        schema_doc(
            super::USER_TYPE_ID,
            &json!({
                "type": "object",
                "allOf": [{ "$ref": gts_uri!(fields) }],
                "properties": { "id": { "type": "string", "format": "uuid" } },
            }),
        ),
    );

    super::validate_new_user_payload_via_gts(&user_payload("alice"), &registry)
        .await
        .expect("an inherited bound admits a short username");
    let err = super::validate_new_user_payload_via_gts(&user_payload("alexandria"), &registry)
        .await
        .expect_err("the inherited maxLength applies");
    assert!(matches!(err, DomainError::Validation { .. }), "got {err:?}");
}

#[tokio::test]
async fn user_payload_validates_against_the_published_user_schema() {
    let registry = MockTypesRegistry::new();
    registry.seed_inventory();
    let payload = user_payload(&"x".repeat(256));

    let err = super::validate_new_user_payload_via_gts(&payload, &registry)
        .await
        .expect_err("the published schema's username bound applies");
    assert!(matches!(err, DomainError::Validation { .. }), "got {err:?}");
}

#[tokio::test]
async fn a_snapshot_without_the_selected_resolved_schema_is_internal() {
    let registry = Arc::new(registry_with_user_schema(255));
    let tampered = Tampered::without_documents(registry);

    let err = super::validate_new_user_payload_via_gts(&user_payload("alice"), &tampered)
        .await
        .expect_err("an incomplete projection is a contract break");
    assert!(matches!(err, DomainError::Internal { .. }), "got {err:?}");
}

#[tokio::test]
async fn a_deleted_user_schema_fails_closed_as_unregistered() {
    let registry = registry_with_user_schema(255);
    let content = registry.content(super::USER_TYPE_ID).expect("seeded");
    registry.seed_entity(Seed::new(super::USER_TYPE_ID, content).deleted());

    let err = super::validate_new_user_payload_via_gts(&user_payload("alice"), &registry)
        .await
        .expect_err("a tombstone is not a registered schema");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "got {err:?}"
    );
}
