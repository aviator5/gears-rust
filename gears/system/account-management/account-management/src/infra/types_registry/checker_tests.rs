//! Tests for [`super::GtsTenantTypeChecker`].
//!
//! Extracted into a companion file per `cargo gears lint` rule `DE1101` (inline test
//! blocks > 100 lines must move out of the production source file).
//! Schemas are real documents in `MockTypesRegistry`, which materializes
//! `effective_traits` with `gts-rust` as admission does; the AM envelope is the
//! linked inventory declaration.

use super::*;
use serde_json::json;
use toolkit_canonical_errors::CanonicalError;
use toolkit_gts::{GTS_ID_PREFIX, gts_id, gts_uri};
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry};

use crate::infra::types_registry::test_helpers::Tampered;

const PLATFORM: &str = gts_id!("cf.core.am.tenant_type.v1~acme.am.tenant_type.platform.v1~");
const CUSTOMER: &str = gts_id!("cf.core.am.tenant_type.v1~acme.am.tenant_type.customer.v1~");
const ALIEN_ROOT: &str = gts_id!("acme.core.events.type.v1~");
const ALIEN_LEAF: &str = gts_id!("acme.core.events.type.v1~acme.commerce.orders.order.v1~");

fn uuid(type_id: &str) -> Uuid {
    GtsId::try_new(type_id).expect("valid id").to_uuid()
}

/// A registry holding the linked inventory, the AM envelope among it.
fn registry() -> Arc<MockTypesRegistry> {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.seed_inventory();
    registry
}

/// A tenant type derived from the envelope, declaring `allowed_parent_types`
/// when given.
fn seed_tenant_type(registry: &MockTypesRegistry, type_id: &str, allowed: Option<Value>) {
    let mut doc = json!({
        "$id": gts_uri!(type_id),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "allOf": [{ "$ref": gts_uri!(TENANT_TYPE_BASE_GTS_ID) }],
    });
    if let Some(allowed) = allowed {
        doc["x-gts-traits"] = json!({ ALLOWED_PARENT_TYPES_TRAIT: allowed });
    }
    registry.seed(type_id, doc);
}

fn seed_alien(registry: &MockTypesRegistry) {
    registry.seed(
        ALIEN_ROOT,
        json!({
            "$id": gts_uri!(ALIEN_ROOT),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
        }),
    );
    registry.seed(
        ALIEN_LEAF,
        json!({
            "$id": gts_uri!(ALIEN_LEAF),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [{ "$ref": gts_uri!(ALIEN_ROOT) }],
        }),
    );
}

#[tokio::test]
async fn admits_when_parent_in_child_allowed_parent_types() {
    let registry = registry();
    seed_tenant_type(&registry, PLATFORM, None);
    seed_tenant_type(&registry, CUSTOMER, Some(json!([PLATFORM])));
    let checker = GtsTenantTypeChecker::new(registry.clone());

    checker
        .check_parent_child(uuid(PLATFORM), uuid(CUSTOMER))
        .await
        .expect("admit when parent listed");
    // Single batch call — checker MUST NOT issue per-uuid round-trips.
    assert_eq!(registry.calls(Call::BatchGet), 1);
}

// -------- same-type nesting --------

#[tokio::test]
async fn admits_same_type_nesting_when_self_listed() {
    let registry = registry();
    seed_tenant_type(&registry, CUSTOMER, Some(json!([CUSTOMER])));
    let checker = GtsTenantTypeChecker::new(registry);

    checker
        .check_parent_child(uuid(CUSTOMER), uuid(CUSTOMER))
        .await
        .expect("admit when self-nesting allowed");
}

#[tokio::test]
async fn rejects_same_type_nesting_when_self_not_listed() {
    let registry = registry();
    seed_tenant_type(&registry, CUSTOMER, Some(json!([PLATFORM])));
    let checker = GtsTenantTypeChecker::new(registry);

    let err = checker
        .check_parent_child(uuid(CUSTOMER), uuid(CUSTOMER))
        .await
        .expect_err("self-nesting must reject");
    assert!(matches!(err, DomainError::TypeNotAllowed { .. }));
    assert!(err.to_string().contains("same-type nesting"), "got: {err}");
}

// -------- rejection paths --------

#[tokio::test]
async fn rejects_when_parent_not_in_allowed_list() {
    let stranger = gts_id!("cf.core.am.tenant_type.v1~acme.am.tenant_type.stranger.v1~");
    let registry = registry();
    seed_tenant_type(&registry, stranger, None);
    seed_tenant_type(&registry, CUSTOMER, Some(json!([PLATFORM])));
    let checker = GtsTenantTypeChecker::new(registry);

    let err = checker
        .check_parent_child(uuid(stranger), uuid(CUSTOMER))
        .await
        .expect_err("parent not listed must reject");
    assert!(matches!(err, DomainError::TypeNotAllowed { .. }));
    assert_eq!(err.code(), "type_not_allowed");
}

#[tokio::test]
async fn a_parent_without_its_own_traits_gets_the_envelope_default() {
    // The envelope defaults `allowed_parent_types` to `[]`: a type that
    // declares nothing admits no parent, itself included.
    let registry = registry();
    seed_tenant_type(&registry, PLATFORM, None);
    let checker = GtsTenantTypeChecker::new(registry);

    let err = checker
        .check_parent_child(uuid(PLATFORM), uuid(PLATFORM))
        .await
        .expect_err("the default list is empty");
    assert!(
        matches!(err, DomainError::TypeNotAllowed { .. }),
        "got: {err}"
    );
}

#[tokio::test]
async fn rejects_child_not_under_envelope_as_invalid_tenant_type() {
    // Registered, but its chain root is not the AM tenant_type envelope.
    let registry = registry();
    seed_alien(&registry);
    let checker = GtsTenantTypeChecker::new(registry);

    let err = checker
        .check_parent_child(uuid(ALIEN_ROOT), uuid(ALIEN_LEAF))
        .await
        .expect_err("alien child must reject");
    assert!(matches!(err, DomainError::InvalidTenantType { .. }));
    assert!(err.to_string().contains("does not descend"), "got: {err}");
}

#[tokio::test]
async fn rejects_parent_not_under_envelope_as_invalid_tenant_type() {
    // The child lists the alien parent literally — a value its traits
    // schema (`x-gts-ref` to the envelope) keeps admission from storing —
    // and only the envelope re-check on the parent refuses it.
    let registry = registry();
    seed_alien(&registry);
    seed_tenant_type(&registry, CUSTOMER, Some(json!([PLATFORM])));
    let checker = GtsTenantTypeChecker::new(Arc::new(Tampered::new(registry, |entity| {
        if entity.gts_id.as_ref() == CUSTOMER {
            entity.effective_traits = Some(json!({ ALLOWED_PARENT_TYPES_TRAIT: [ALIEN_LEAF] }));
        }
    })));

    let err = checker
        .check_parent_child(uuid(ALIEN_LEAF), uuid(CUSTOMER))
        .await
        .expect_err("alien parent must reject");
    assert!(matches!(err, DomainError::InvalidTenantType { .. }));
    assert!(
        err.to_string().contains("parent tenant type")
            && err.to_string().contains("does not descend"),
        "got: {err}"
    );
}

#[tokio::test]
async fn rejects_child_not_registered_as_invalid_tenant_type() {
    let checker = GtsTenantTypeChecker::new(registry());

    let err = checker
        .check_parent_child(Uuid::from_u128(0x1), Uuid::from_u128(0x2))
        .await
        .expect_err("unregistered child must reject");
    assert!(matches!(err, DomainError::InvalidTenantType { .. }));
    assert!(err.to_string().contains("child tenant type"), "got: {err}");
}

#[tokio::test]
async fn rejects_parent_not_registered_as_invalid_tenant_type() {
    let registry = registry();
    seed_tenant_type(&registry, CUSTOMER, Some(json!([PLATFORM])));
    let checker = GtsTenantTypeChecker::new(registry);

    let err = checker
        .check_parent_child(Uuid::from_u128(0xCAFE), uuid(CUSTOMER))
        .await
        .expect_err("missing parent must reject");
    assert!(matches!(err, DomainError::InvalidTenantType { .. }));
    assert!(err.to_string().contains("parent tenant type"), "got: {err}");
}

// -------- malformed trait values --------

#[tokio::test]
async fn rejects_when_allowed_parent_types_contains_an_instance_identifier() {
    // One malformed entry collapses the whole list, so a sibling
    // well-formed entry cannot admit through it.
    // Admission refuses such a document by the envelope's `x-gts-ref`.
    let instance =
        format!("{GTS_ID_PREFIX}cf.core.am.tenant_type.v1~acme.am.tenant.customer.v1~prod_acct_42");
    let registry = registry();
    seed_tenant_type(&registry, CUSTOMER, Some(json!([CUSTOMER])));
    let checker = GtsTenantTypeChecker::new(Arc::new(Tampered::new(registry, move |entity| {
        entity.effective_traits = Some(json!({ ALLOWED_PARENT_TYPES_TRAIT: [CUSTOMER, instance] }));
    })));

    let err = checker
        .check_parent_child(uuid(CUSTOMER), uuid(CUSTOMER))
        .await
        .expect_err("malformed entry must collapse the list to InvalidTenantType");
    assert!(
        matches!(err, DomainError::InvalidTenantType { .. }),
        "got: {err}"
    );
}

#[tokio::test]
async fn rejects_when_allowed_parent_types_is_not_an_array() {
    let registry = registry();
    seed_tenant_type(&registry, CUSTOMER, Some(json!([CUSTOMER])));
    let checker = GtsTenantTypeChecker::new(Arc::new(Tampered::new(registry, |entity| {
        entity.effective_traits = Some(json!({ ALLOWED_PARENT_TYPES_TRAIT: "not an array" }));
    })));

    let err = checker
        .check_parent_child(uuid(CUSTOMER), uuid(CUSTOMER))
        .await
        .expect_err("malformed trait must reject");
    assert!(
        matches!(err, DomainError::InvalidTenantType { .. }),
        "got: {err}"
    );
}

// -------- registry faults and contract breaks --------

#[tokio::test]
async fn a_failed_batch_read_is_service_unavailable() {
    let registry = registry();
    seed_tenant_type(&registry, CUSTOMER, Some(json!([CUSTOMER])));
    registry.inject(
        Fault::on(Call::BatchGet).fail(CanonicalError::internal("registry exploded").create()),
    );
    let checker = GtsTenantTypeChecker::new(registry);

    let err = checker
        .check_parent_child(uuid(CUSTOMER), uuid(CUSTOMER))
        .await
        .expect_err("registry error must propagate");
    assert!(matches!(err, DomainError::ServiceUnavailable { .. }));
    assert_eq!(err.code(), "service_unavailable");
    assert_eq!(err.http_status(), 503);
}

#[tokio::test]
async fn a_snapshot_without_the_selected_effective_traits_is_not_an_empty_list() {
    let registry = registry();
    seed_tenant_type(&registry, CUSTOMER, Some(json!([CUSTOMER])));
    let checker = GtsTenantTypeChecker::new(Arc::new(Tampered::without_documents(registry)));

    let err = checker
        .check_parent_child(uuid(CUSTOMER), uuid(CUSTOMER))
        .await
        .expect_err("an incomplete projection is a contract break");
    assert!(matches!(err, DomainError::Internal { .. }), "got: {err}");
}

#[tokio::test(start_paused = true)]
async fn rejects_when_registry_probe_times_out() {
    let registry = registry();
    registry.inject(Fault::on(Call::BatchGet).delay(Duration::from_millis(50)));
    let checker = GtsTenantTypeChecker::with_timeout(registry, 10);

    let err = checker
        .check_parent_child(Uuid::from_u128(0x1), Uuid::from_u128(0x1))
        .await
        .expect_err("slow registry must time out");
    assert!(matches!(err, DomainError::ServiceUnavailable { .. }));
    assert!(
        err.to_string().contains("types-registry: timeout exceeded"),
        "got: {err}"
    );
}
