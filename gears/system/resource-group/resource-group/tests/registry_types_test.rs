#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Resource-group over types-registry type definitions (T38b): which operations
//! read a definition, which lifecycle they accept, and that reads and cleanup
//! never depend on the registry.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use resource_group::domain::error::DomainError;
use resource_group::domain::type_catalog::{RgType, RgTypeCatalog, TypeUse};
use resource_group_sdk::{CreateGroupRequest, UpdateGroupRequest};
use toolkit_odata::ODataQuery;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use common::CreateTypeRequest;

fn code(name: &str) -> String {
    format!(
        "gts.cf.core.rg.type.v1~x.test.{name}.i{}.v1~",
        Uuid::now_v7().as_simple()
    )
}

/// A member type, a root type admitting it, and a child type under the root.
fn publish_family() -> (String, String, String) {
    let member = code("member");
    common::publish_type(CreateTypeRequest {
        code: member.clone(),
        ..CreateTypeRequest::default()
    });
    let root = code("root");
    common::publish_type(CreateTypeRequest {
        code: root.clone(),
        can_be_root: true,
        allowed_parent_types: vec![root.clone()],
        allowed_membership_types: vec![member.clone()],
        metadata_schema: Some(serde_json::json!({
            "type": "object",
            "properties": { "tier": { "type": "string" } }
        })),
    });
    let child = code("child");
    common::publish_type(CreateTypeRequest {
        code: child.clone(),
        allowed_parent_types: vec![root.clone()],
        ..CreateTypeRequest::default()
    });
    (member, root, child)
}

// -------- new links require an Active type --------

#[tokio::test]
async fn creating_a_group_of_an_unknown_or_deleted_type_is_type_not_found() {
    let db = common::test_db().await;
    let groups = common::make_group_service(db);
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);

    let unknown = code("never");
    let err = groups
        .create_group(
            &ctx,
            CreateGroupRequest::new(unknown, "g".to_owned()),
            tenant,
        )
        .await
        .expect_err("unknown type");
    assert!(matches!(err, DomainError::TypeNotFound { .. }), "{err:?}");

    let (_, root, _) = publish_family();
    common::delete_published_type(&root);
    let err = groups
        .create_group(&ctx, CreateGroupRequest::new(root, "g".to_owned()), tenant)
        .await
        .expect_err("deleted type");
    assert!(matches!(err, DomainError::TypeNotFound { .. }), "{err:?}");
}

#[tokio::test]
async fn a_new_membership_needs_an_active_group_type_and_member_type() {
    let db = common::test_db().await;
    let groups = common::make_group_service(db.clone());
    let members = common::make_membership_service(db);
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let (member, root, _) = publish_family();
    let group = common::create_root_group(&groups, &ctx, &root, "g", tenant).await;

    members
        .add_membership(&ctx, group.id, &member, "r1")
        .await
        .expect("both active");

    common::delete_published_type(&member);
    let err = members
        .add_membership(&ctx, group.id, &member, "r2")
        .await
        .expect_err("deleted member type");
    assert!(matches!(err, DomainError::Validation { .. }), "{err:?}");

    let (other_member, other_root, _) = publish_family();
    let other_group = common::create_root_group(&groups, &ctx, &other_root, "g2", tenant).await;
    common::delete_published_type(&other_root);
    let err = members
        .add_membership(&ctx, other_group.id, &other_member, "r1")
        .await
        .expect_err("deleted group type");
    assert!(matches!(err, DomainError::TypeNotFound { .. }), "{err:?}");
}

#[tokio::test]
async fn a_member_type_must_be_a_concrete_type_identifier() {
    let db = common::test_db().await;
    let groups = common::make_group_service(db.clone());
    let members = common::make_membership_service(db);
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let (_, root, _) = publish_family();
    let group = common::create_root_group(&groups, &ctx, &root, "g", tenant).await;

    for invalid in [
        "not-a-gts-id",
        "gts.cf.test.rg.*",
        "gts.x.test.*",
        // An instance identifier (no trailing `~`) is not a type.
        "gts.cf.core.rg.type.v1~x.test._.thing.v1",
        "",
    ] {
        let err = members
            .add_membership(&ctx, group.id, invalid, "r1")
            .await
            .expect_err("not a concrete type identifier");
        assert!(
            matches!(err, DomainError::Validation { .. }),
            "{invalid:?}: {err:?}"
        );
    }
}

// -------- reads and cleanup never consult the registry --------

#[tokio::test]
async fn reads_and_cleanup_work_while_the_registry_is_unavailable() {
    let db = common::test_db().await;
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let (member, root, child) = publish_family();
    let groups = common::make_group_service(db.clone());
    let members = common::make_membership_service(db.clone());
    let r = common::create_root_group(&groups, &ctx, &root, "root", tenant).await;
    let c = common::create_child_group(&groups, &ctx, &child, r.id, "child", tenant).await;
    members
        .add_membership(&ctx, r.id, &member, "m1")
        .await
        .unwrap();

    let offline_groups =
        common::make_group_service_with(db.clone(), common::make_unavailable_types_catalog());
    let offline_members =
        common::make_membership_service_with(db, common::make_unavailable_types_catalog());

    offline_groups.get_group(&ctx, r.id).await.expect("get");
    assert_eq!(
        offline_groups
            .list_groups(&ctx, &ODataQuery::default())
            .await
            .expect("list")
            .items
            .len(),
        2
    );
    offline_groups
        .get_group_descendants(&ctx, r.id, &ODataQuery::default())
        .await
        .expect("hierarchy");
    offline_members
        .list_memberships(&ctx, &ODataQuery::default())
        .await
        .expect("list memberships");
    offline_groups
        .update_group(
            &ctx,
            c.id,
            UpdateGroupRequest {
                name: "renamed".to_owned(),
                parent_id: Some(r.id),
                metadata: None,
            },
        )
        .await
        .expect("a rename reads no definition");
    offline_members
        .remove_membership(&ctx, r.id, &member, "m1")
        .await
        .expect("remove membership");
    offline_groups
        .delete_group(&ctx, r.id, true)
        .await
        .expect("force delete");
}

#[tokio::test]
async fn groups_of_a_deleted_type_stay_readable_updatable_and_deletable() {
    let db = common::test_db().await;
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let (member, root, child) = publish_family();
    let groups = common::make_group_service(db.clone());
    let members = common::make_membership_service(db);
    let r1 = common::create_root_group(&groups, &ctx, &root, "r1", tenant).await;
    let r2 = common::create_child_group(&groups, &ctx, &root, r1.id, "r2", tenant).await;
    let c = common::create_child_group(&groups, &ctx, &child, r1.id, "c", tenant).await;
    members.add_membership(&ctx, c.id, &member, "m1").await.ok();
    members
        .add_membership(&ctx, r1.id, &member, "m2")
        .await
        .unwrap();

    common::delete_published_type(&root);
    common::delete_published_type(&child);

    groups.get_group(&ctx, r1.id).await.expect("read");
    let updated = groups
        .update_group(
            &ctx,
            r1.id,
            UpdateGroupRequest {
                name: "r1".to_owned(),
                parent_id: None,
                metadata: Some(serde_json::json!({ "tier": "gold" })),
            },
        )
        .await
        .expect("an existing group's deleted type still defines its metadata");
    assert_eq!(
        updated.metadata,
        Some(serde_json::json!({ "tier": "gold" }))
    );
    let err = groups
        .update_group(
            &ctx,
            r1.id,
            UpdateGroupRequest {
                name: "r1".to_owned(),
                parent_id: None,
                metadata: Some(serde_json::json!({ "tier": 7 })),
            },
        )
        .await
        .expect_err("the retained schema applies");
    assert!(matches!(err, DomainError::Validation { .. }), "{err:?}");
    groups
        .move_group(&ctx, c.id, Some(r2.id))
        .await
        .expect("a move applies the retained placement traits");

    members
        .remove_membership(&ctx, r1.id, &member, "m2")
        .await
        .expect("cleanup");
    groups
        .delete_group(&ctx, r1.id, true)
        .await
        .expect("cleanup");
}

// -------- the transaction rechecks the type it read --------

/// Answers every lookup with another type's definition, as if the group had
/// been re-created under the same id with a different type between the
/// pre-transaction read and the transaction.
struct WrongType {
    inner: Arc<dyn RgTypeCatalog>,
    other: String,
}

#[async_trait]
impl RgTypeCatalog for WrongType {
    async fn get_type(
        &self,
        ctx: &SecurityContext,
        _code: &str,
        usage: TypeUse,
    ) -> Result<RgType, DomainError> {
        self.inner.get_type(ctx, &self.other, usage).await
    }

    async fn get_membership_rule(
        &self,
        ctx: &SecurityContext,
        _group_code: &str,
        member_code: &str,
    ) -> Result<RgType, DomainError> {
        self.inner
            .get_membership_rule(ctx, &self.other, member_code)
            .await
    }
}

#[tokio::test]
async fn a_definition_read_for_another_type_is_refused_inside_the_transaction() {
    let db = common::test_db().await;
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);
    let (_, root, _) = publish_family();
    let (other_member, other_root, _) = publish_family();
    let groups = common::make_group_service(db.clone());
    let g = common::create_root_group(&groups, &ctx, &root, "g", tenant).await;

    let skewed: Arc<dyn RgTypeCatalog> = Arc::new(WrongType {
        inner: common::make_types_catalog(),
        other: other_root,
    });
    let skewed_groups = common::make_group_service_with(db.clone(), skewed.clone());
    let err = skewed_groups
        .update_group(
            &ctx,
            g.id,
            UpdateGroupRequest {
                name: "g".to_owned(),
                parent_id: None,
                metadata: Some(serde_json::json!({ "tier": "gold" })),
            },
        )
        .await
        .expect_err("the in-transaction row has another type");
    assert!(matches!(err, DomainError::Conflict { .. }), "{err:?}");

    let err = skewed_groups
        .move_group(&ctx, g.id, None)
        .await
        .expect_err("the in-transaction row has another type");
    assert!(matches!(err, DomainError::Conflict { .. }), "{err:?}");

    let skewed_members = common::make_membership_service_with(db, skewed);
    let err = skewed_members
        .add_membership(&ctx, g.id, &other_member, "r1")
        .await
        .expect_err("the in-transaction row has another type");
    assert!(matches!(err, DomainError::Conflict { .. }), "{err:?}");
}
