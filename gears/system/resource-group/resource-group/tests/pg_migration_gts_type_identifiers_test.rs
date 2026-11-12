#![cfg(feature = "integration")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "migration test: raw SQL against the schema under migration"
)]
//! `m20261111_000001_gts_type_identifiers` on PostgreSQL (testcontainers;
//! `--features integration`, Docker required; `RG_PG_REQUIRE_DOCKER=1` makes a
//! missing Docker fail instead of skip), through the production runner.
//!
//! Also pins the portable 2600 combined UTF-8 byte membership key bound,
//! including pre-upgrade refusal and remediation of legacy oversized keys.

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbErr, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait};
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, Db, connect_db};

use resource_group::infra::storage::migrations::Migrator;

const TENANT_TYPE: &str = "gts.cf.core.rg.type.v1~cf.test.rg.tenant.v1~";
const DEPT_TYPE: &str = "gts.cf.core.rg.type.v1~cf.test.rg.dept.v1~";
const USER_TYPE: &str = "gts.cf.test.rg.user.v1~";

const ROOT: &str = "019a0000-0000-7000-8000-000000000001";
const CHILD: &str = "019a0000-0000-7000-8000-000000000002";
const GRANDCHILD: &str = "019a0000-0000-7000-8000-000000000003";

struct Fixture {
    _container: testcontainers::ContainerAsync<Postgres>,
    db: Db,
    raw: DatabaseConnection,
}

fn require_docker() -> bool {
    std::env::var_os("RG_PG_REQUIRE_DOCKER").is_some_and(|v| v != "0" && !v.is_empty())
}

/// PostgreSQL with only the initial migration applied, and one raw connection.
async fn at_initial_schema() -> Option<Fixture> {
    let request = test_containers::postgres()
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_DB", "app");
    let container = match request.start().await {
        Ok(c) => c,
        Err(e) => {
            assert!(!require_docker(), "PostgreSQL container: {e}");
            eprintln!("skipping -- no Docker: {e}");
            return None;
        }
    };
    let port = container.get_host_port_ipv4(5432).await.expect("port");
    let url = format!("postgres://user:pass@127.0.0.1:{port}/app");
    let db = connect_db(
        &url,
        ConnectOpts {
            max_conns: Some(2),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("connect");
    let initial: Vec<Box<dyn MigrationTrait>> =
        Migrator::migrations().into_iter().take(1).collect();
    run_migrations_for_testing(&db, initial)
        .await
        .expect("initial migration");
    let mut opts = ConnectOptions::new(url);
    opts.max_connections(1).min_connections(1);
    let raw = Database::connect(opts).await.expect("raw connection");
    Some(Fixture {
        _container: container,
        db,
        raw,
    })
}

async fn exec(db: &DatabaseConnection, sql: &str) -> Result<(), DbErr> {
    db.execute_raw(Statement::from_string(
        db.get_database_backend(),
        sql.to_owned(),
    ))
    .await
    .map(drop)
}

async fn run(db: &DatabaseConnection, sql: &str) {
    exec(db, sql)
        .await
        .unwrap_or_else(|e| panic!("`{sql}` failed: {e}"));
}

async fn rows(db: &DatabaseConnection, sql: &str) -> Vec<Vec<String>> {
    db.query_all_raw(Statement::from_string(
        db.get_database_backend(),
        sql.to_owned(),
    ))
    .await
    .expect(sql)
    .into_iter()
    .map(|row| {
        (0..)
            .map_while(|i| row.try_get_by_index::<Option<String>>(i).ok())
            .map(|v| v.unwrap_or_else(|| "NULL".to_owned()))
            .collect()
    })
    .collect()
}

async fn seed_hierarchy(raw: &DatabaseConnection) {
    run(
        raw,
        &format!(
            "INSERT INTO gts_type (schema_id) VALUES ('{TENANT_TYPE}'), ('{DEPT_TYPE}'), ('{USER_TYPE}')"
        ),
    )
    .await;
    let id = |code: &str| format!("(SELECT id FROM gts_type WHERE schema_id = '{code}')");
    run(
        raw,
        &format!(
            "INSERT INTO gts_type_allowed_parent (type_id, parent_type_id) VALUES ({}, {})",
            id(DEPT_TYPE),
            id(TENANT_TYPE)
        ),
    )
    .await;
    run(
        raw,
        &format!(
            "INSERT INTO resource_group (id, parent_id, gts_type_id, name, tenant_id) VALUES \
             ('{ROOT}', NULL, {t}, 'root', '{ROOT}'), \
             ('{CHILD}', '{ROOT}', {d}, 'child', '{ROOT}'), \
             ('{GRANDCHILD}', '{CHILD}', {d}, 'grandchild', '{ROOT}')",
            t = id(TENANT_TYPE),
            d = id(DEPT_TYPE)
        ),
    )
    .await;
    run(
        raw,
        &format!(
            "INSERT INTO resource_group_closure (ancestor_id, descendant_id, depth) VALUES \
             ('{ROOT}', '{ROOT}', 0), ('{CHILD}', '{CHILD}', 0), ('{GRANDCHILD}', '{GRANDCHILD}', 0), \
             ('{ROOT}', '{CHILD}', 1), ('{ROOT}', '{GRANDCHILD}', 2), ('{CHILD}', '{GRANDCHILD}', 1)"
        ),
    )
    .await;
    run(
        raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) VALUES \
             ('{CHILD}', {u}, 'u1'), ('{GRANDCHILD}', {u}, 'u1'), ('{ROOT}', {u}, 'u2')",
            u = id(USER_TYPE)
        ),
    )
    .await;
}

async fn upgrade(db: &Db) -> Result<(), toolkit_db::migration_runner::MigrationError> {
    run_migrations_for_testing(db, Migrator::migrations())
        .await
        .map(drop)
}

#[tokio::test]
async fn pg_oversized_legacy_membership_refuses_upgrade_before_ddl() {
    let Some(f) = at_initial_schema().await else {
        return;
    };
    seed_hierarchy(&f.raw).await;
    // Both values fit the old schema independently, but their combined width
    // cannot fit the new membership indexes. Random hex defeats compression.
    run(
        &f.raw,
        "INSERT INTO gts_type (schema_id) SELECT 'gts.cf.core.rg.type.v1~' || \
         substr(string_agg(md5(random()::text), ''), 1, 1001) FROM generate_series(1, 32)",
    )
    .await;
    run(
        &f.raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
             SELECT '{ROOT}', (SELECT max(id) FROM gts_type), \
             substr(string_agg(md5(random()::text), ''), 1, 2000) FROM generate_series(1, 63)"
        ),
    )
    .await;

    let err = upgrade(&f.db).await.expect_err("oversized legacy key");
    let diagnostic = err.to_string();
    assert!(diagnostic.contains("2600"), "{diagnostic}");
    assert!(diagnostic.contains("UTF-8"), "{diagnostic}");
    assert!(diagnostic.contains("before upgrading"), "{diagnostic}");
    assert_eq!(
        rows(
            &f.raw,
            "SELECT data_type FROM information_schema.columns \
             WHERE table_name = 'resource_group' AND column_name = 'gts_type_id'"
        )
        .await,
        vec![vec!["smallint"]],
        "the preflight refuses before altering groups"
    );
    assert_eq!(
        rows(
            &f.raw,
            "SELECT octet_length(t.schema_id)::text, octet_length(m.resource_id)::text \
             FROM resource_group_membership m JOIN gts_type t ON t.id = m.gts_type_id \
             WHERE length(m.resource_id) = 2000"
        )
        .await,
        vec![vec!["1024", "2000"]],
        "the old catalogue and oversized membership remain intact"
    );

    // The operator can remediate the old key, coordinating the new identifier
    // with its owning gear, then retry without losing the other memberships.
    run(
        &f.raw,
        "UPDATE resource_group_membership SET resource_id = 'remapped-resource' \
         WHERE length(resource_id) = 2000",
    )
    .await;
    upgrade(&f.db).await.expect("upgrade after remediation");
    assert_eq!(
        rows(
            &f.raw,
            "SELECT data_type FROM information_schema.columns \
             WHERE table_name = 'resource_group' AND column_name = 'gts_type_id'"
        )
        .await,
        vec![vec!["text"]],
        "the refused attempt did not record the migration as applied"
    );
    assert_eq!(
        rows(
            &f.raw,
            "SELECT count(*)::text FROM resource_group_membership"
        )
        .await,
        vec![vec!["4"]]
    );
}

#[tokio::test]
async fn pg_an_existing_hierarchy_keeps_every_row_under_its_gts_identifier() {
    let Some(f) = at_initial_schema().await else {
        return;
    };
    seed_hierarchy(&f.raw).await;
    upgrade(&f.db).await.expect("upgrade");

    assert_eq!(
        rows(
            &f.raw,
            "SELECT id::text, COALESCE(parent_id::text, 'NULL'), gts_type_id, tenant_id::text \
             FROM resource_group ORDER BY name"
        )
        .await,
        vec![
            vec![CHILD, ROOT, DEPT_TYPE, ROOT],
            vec![GRANDCHILD, CHILD, DEPT_TYPE, ROOT],
            vec![ROOT, "NULL", TENANT_TYPE, ROOT],
        ]
    );
    assert_eq!(
        rows(&f.raw, "SELECT count(*)::text FROM resource_group_closure").await,
        vec![vec!["6"]]
    );
    assert_eq!(
        rows(
            &f.raw,
            "SELECT group_id::text, gts_type_id, resource_id FROM resource_group_membership \
             ORDER BY resource_id, group_id"
        )
        .await,
        vec![
            vec![CHILD, USER_TYPE, "u1"],
            vec![GRANDCHILD, USER_TYPE, "u1"],
            vec![ROOT, USER_TYPE, "u2"],
        ]
    );
    assert!(
        rows(
            &f.raw,
            "SELECT tablename FROM pg_tables WHERE tablename LIKE 'gts_type%'"
        )
        .await
        .is_empty(),
        "the RG type catalogue is dropped"
    );
    assert!(
        rows(
            &f.raw,
            "SELECT typname FROM pg_type WHERE typname = 'gts_type_path'"
        )
        .await
        .is_empty(),
        "the catalogue's domain is dropped"
    );
    let indexes = rows(
        &f.raw,
        "SELECT indexname, indexdef FROM pg_indexes \
         WHERE tablename IN ('resource_group', 'resource_group_membership') ORDER BY indexname",
    )
    .await;
    let names: Vec<&str> = indexes.iter().map(|r| r[0].as_str()).collect();
    for expected in [
        "idx_rg_gts_type_id",
        "idx_rg_name",
        "idx_rg_parent_id",
        "idx_rg_tenant_id",
        "idx_rgm_gts_type_resource",
        "resource_group_membership_pkey",
        "resource_group_pkey",
    ] {
        assert!(names.contains(&expected), "{expected} missing: {names:?}");
    }
    let def = |name: &str| {
        indexes
            .iter()
            .find(|r| r[0] == name)
            .map(|r| r[1].clone())
            .unwrap()
    };
    assert!(def("idx_rg_gts_type_id").contains("(gts_type_id, id)"));
    assert!(def("idx_rgm_gts_type_resource").contains("(gts_type_id, resource_id)"));
    assert!(def("resource_group_membership_pkey").contains("(group_id, gts_type_id, resource_id)"));
}

#[tokio::test]
async fn pg_constraints_hold_and_the_key_boundary_is_explicit() {
    let Some(f) = at_initial_schema().await else {
        return;
    };
    seed_hierarchy(&f.raw).await;
    upgrade(&f.db).await.expect("upgrade");

    let duplicate = exec(
        &f.raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
             VALUES ('{CHILD}', '{USER_TYPE}', 'u1')"
        ),
    )
    .await
    .expect_err("membership primary key");
    assert!(
        duplicate.to_string().contains("duplicate key"),
        "{duplicate}"
    );

    let dangling = exec(
        &f.raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
             VALUES ('019a0000-0000-7000-8000-000000000099', '{USER_TYPE}', 'u3')"
        ),
    )
    .await
    .expect_err("group foreign key");
    assert!(
        dangling.to_string().contains("fk_rgm_group_id"),
        "{dangling}"
    );

    let parent = exec(
        &f.raw,
        &format!("DELETE FROM resource_group WHERE id = '{CHILD}'"),
    )
    .await
    .expect_err("parent with children");
    assert!(parent.to_string().contains("foreign key"), "{parent}");

    // A maximal (1024-byte) GTS type identifier with UUID resource ids fits
    // the primary key and both indexes.
    let longest = format!("gts.cf.core.rg.type.v1~{}", "a".repeat(1024 - 23));
    for i in 0..3 {
        run(
            &f.raw,
            &format!(
                "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
                 VALUES ('{ROOT}', '{longest}', gen_random_uuid()::text || '-{i}')"
            ),
        )
        .await;
    }
    run(
        &f.raw,
        &format!("UPDATE resource_group SET gts_type_id = '{longest}' WHERE id = '{GRANDCHILD}'"),
    )
    .await;
    let too_long = exec(
        &f.raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
             VALUES ('{ROOT}', '{longest}a', 'x')"
        ),
    )
    .await
    .expect_err("identifier bound");
    assert!(
        too_long.to_string().contains("ck_rgm_gts_type_id"),
        "{too_long}"
    );

    // A maximal, incompressible type identifier (random hex; a repetitive one
    // compresses) leaves 1576 bytes for the resource id under the portable bound.
    let random_hex = |n: usize| {
        format!(
            "(SELECT substr(string_agg(md5(random()::text), ''), 1, {n}) \
             FROM generate_series(1, {}))",
            n.div_ceil(32)
        )
    };
    let incompressible_type = format!("('gts.cf.core.rg.type.v1~' || {})", random_hex(1024 - 23));
    run(
        &f.raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
             VALUES ('{ROOT}', {incompressible_type}, gen_random_uuid()::text)"
        ),
    )
    .await;
    let oversized = exec(
        &f.raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
             VALUES ('{ROOT}', {incompressible_type}, {})",
            random_hex(2000)
        ),
    )
    .await
    .expect_err("combined key byte bound");
    assert!(
        oversized.to_string().contains("ck_rgm_key_bytes"),
        "{oversized}"
    );
    for (resource, accepted) in [
        (random_hex(1576), true),
        (random_hex(1577), false),
        ("repeat('\u{436}', 788)".to_owned(), true),
        ("repeat('\u{436}', 789)".to_owned(), false),
    ] {
        let result = exec(
            &f.raw,
            &format!(
                "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
                 VALUES ('{ROOT}', {incompressible_type}, {resource})"
            ),
        )
        .await;
        if accepted {
            result.expect("combined key is exactly 2600 UTF-8 bytes");
        } else {
            let err = result.expect_err("combined key exceeds 2600 UTF-8 bytes");
            assert!(err.to_string().contains("ck_rgm_key_bytes"), "{err}");
        }
    }
    run(
        &f.raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
             VALUES ('{ROOT}', '{USER_TYPE}', {})",
            random_hex(2000)
        ),
    )
    .await;
}

#[tokio::test]
async fn pg_an_orphaned_type_key_refuses_the_upgrade_and_leaves_the_old_schema() {
    let Some(f) = at_initial_schema().await else {
        return;
    };
    seed_hierarchy(&f.raw).await;
    // Bypass the foreign key on this one connection to stage an orphan.
    run(&f.raw, "SET session_replication_role = replica").await;
    run(
        &f.raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
             VALUES ('{ROOT}', 999, 'orphan')"
        ),
    )
    .await;
    run(&f.raw, "SET session_replication_role = DEFAULT").await;

    upgrade(&f.db)
        .await
        .expect_err("a membership whose type key has no catalogue row");

    assert_eq!(
        rows(
            &f.raw,
            "SELECT data_type FROM information_schema.columns \
             WHERE table_name = 'resource_group' AND column_name = 'gts_type_id'"
        )
        .await,
        vec![vec!["smallint"]],
        "resource_group is untouched"
    );
    assert_eq!(
        rows(&f.raw, "SELECT count(*)::text FROM gts_type").await,
        vec![vec!["3"]]
    );
    assert_eq!(
        rows(
            &f.raw,
            "SELECT count(*)::text FROM resource_group_membership"
        )
        .await,
        vec![vec!["4"]]
    );
}
