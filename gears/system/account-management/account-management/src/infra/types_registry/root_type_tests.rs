use std::sync::Arc;

use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit_gts::{gts_id, gts_uri};
use types_registry_sdk::AdmissionFailureReason;
use types_registry_sdk::GtsTypeId;
use types_registry_sdk::testing_platform::{Call, MockTypesRegistry};

use super::*;

const ROOT_TYPE: &str = gts_id!("cf.core.am.tenant_type.v1~cf.core.am.platform.v1~");

fn config(idp_provisioning: bool) -> RootTypeConfig {
    RootTypeConfig {
        gts_id: GtsTypeId::new(ROOT_TYPE),
        idp_provisioning,
    }
}

/// The linked inventory, the tenant-type envelope among it.
fn registry() -> Arc<MockTypesRegistry> {
    let registry = Arc::new(MockTypesRegistry::new());
    registry.seed_inventory();
    registry
}

#[test]
fn desired_schema_is_the_am_owned_root_contract() {
    let desired = desired_root_schema(&config(true)).expect("desired schema");

    assert_eq!(
        desired["$id"],
        json!(gts_uri!(
            "cf.core.am.tenant_type.v1~cf.core.am.platform.v1~"
        ))
    );
    assert_eq!(
        desired["$schema"],
        json!("http://json-schema.org/draft-07/schema#")
    );
    assert_eq!(
        desired["description"],
        json!("Platform-root tenant type (no parents).")
    );
    assert_eq!(desired["type"], json!("object"));
    assert_eq!(
        desired.pointer("/allOf/0/$ref"),
        Some(&json!(gts_uri!("cf.core.am.tenant_type.v1~")))
    );
    assert_eq!(
        desired.pointer("/x-gts-traits/allowed_parent_types"),
        Some(&json!([]))
    );
    assert_eq!(
        desired.pointer("/x-gts-traits/idp_provisioning"),
        Some(&json!(true))
    );
}

#[tokio::test]
async fn registers_the_desired_root_and_waits_for_admission() {
    let registry = registry();
    let cfg = config(false);

    register_root_type(registry.as_ref(), &cfg, &CancellationToken::new())
        .await
        .expect("root type registration");

    assert_eq!(
        registry.content(ROOT_TYPE),
        Some(desired_root_schema(&cfg).expect("desired schema"))
    );
}

#[tokio::test]
async fn an_equal_restart_submits_nothing() {
    let registry = registry();
    let cfg = config(false);
    registry.seed(
        ROOT_TYPE,
        desired_root_schema(&cfg).expect("desired schema"),
    );

    register_root_type(registry.as_ref(), &cfg, &CancellationToken::new())
        .await
        .expect("an equal root is up to date");

    assert_eq!(registry.calls(Call::Register), 0);
}

#[tokio::test]
async fn a_permitted_content_change_updates_the_root() {
    let registry = registry();
    registry.seed(
        ROOT_TYPE,
        desired_root_schema(&config(false)).expect("desired schema"),
    );

    register_root_type(registry.as_ref(), &config(true), &CancellationToken::new())
        .await
        .expect("idp_provisioning may change");

    assert_eq!(
        registry.content(ROOT_TYPE),
        Some(desired_root_schema(&config(true)).expect("desired schema"))
    );
}

#[tokio::test]
async fn a_refused_root_is_startup_fatal_and_names_it() {
    let registry = registry();
    registry.reject(ROOT_TYPE, AdmissionFailureReason::IncompatibleWithBaseline);

    let error = register_root_type(registry.as_ref(), &config(false), &CancellationToken::new())
        .await
        .expect_err("a refused root must fail registration");

    assert!(error.to_string().contains(ROOT_TYPE), "{error:#}");
    assert!(
        error.to_string().contains("incompatible_with_baseline"),
        "{error:#}"
    );
}

#[tokio::test]
async fn cancellation_is_startup_fatal() {
    let cancel = CancellationToken::new();
    cancel.cancel();

    register_root_type(registry().as_ref(), &config(false), &cancel)
        .await
        .expect_err("a cancelled registration fails");
}

#[tokio::test]
async fn a_host_without_a_registry_database_reads_nothing_written() {
    // The caller passes a resolved client; resolving fails earlier for a
    // host whose registry has no database. Here the client is present but
    // the root is absent until reconciliation admits it.
    let registry = registry();
    assert_eq!(registry.content(ROOT_TYPE), None);
    register_root_type(registry.as_ref(), &config(false), &CancellationToken::new())
        .await
        .expect("registration");
    assert!(registry.content(ROOT_TYPE).is_some());
}
