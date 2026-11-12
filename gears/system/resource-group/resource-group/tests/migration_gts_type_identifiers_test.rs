#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "migration test: raw SQL against the schema under migration"
)]
//! `m20261111_000001_gts_type_identifiers` on SQLite with foreign keys enforced,
//! through the production migration runner: an existing hierarchy keeps every
//! group, closure row and membership with its type key replaced by the GTS
//! identifier, constraints and indexes survive, and a refused backfill rolls the
//! whole migration back.

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbErr, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, Db, connect_db};

use resource_group::infra::storage::migrations::Migrator;

const TENANT_TYPE: &str = "gts.cf.core.rg.type.v1~cf.test.rg.tenant.v1~";
const DEPT_TYPE: &str = "gts.cf.core.rg.type.v1~cf.test.rg.dept.v1~";
const USER_TYPE: &str = "gts.cf.test.rg.user.v1~";

// UUIDs as SeaORM stores them on SQLite (16-byte BLOB), in hex.
const ROOT: &str = "019A0000000070008000000000000001";
const CHILD: &str = "019A0000000070008000000000000002";
const GRANDCHILD: &str = "019A0000000070008000000000000003";
const OTHER_ROOT: &str = "019A0000000070008000000000000004";

struct Fixture {
    db: Db,
    raw: DatabaseConnection,
    _dir: tempdir::Dir,
}

mod tempdir {
    pub struct Dir(pub std::path::PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            if let Err(e) = std::fs::remove_dir_all(&self.0) {
                eprintln!("leaving {}: {e}", self.0.display());
            }
        }
    }
    pub fn new() -> Dir {
        let path = std::env::temp_dir().join(format!("rg-migration-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path).expect("temp dir");
        Dir(path)
    }
}

/// A file database with only the initial migration applied, and a second raw
/// connection to it with foreign keys enforced.
async fn at_initial_schema() -> Fixture {
    let dir = tempdir::new();
    let dsn = format!("sqlite://{}?mode=rwc", dir.0.join("rg.db").display());
    let db = connect_db(
        &dsn,
        ConnectOpts {
            max_conns: Some(1),
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
    let raw = Database::connect(&dsn).await.expect("raw connection");
    run(&raw, "PRAGMA foreign_keys = ON").await;
    Fixture { db, raw, _dir: dir }
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

/// Two roots, a three-level chain, its closure rows and memberships, all under
/// the old surrogate type keys.
async fn seed_hierarchy(raw: &DatabaseConnection) {
    run(
        raw,
        &format!(
            "INSERT INTO gts_type (id, schema_id) VALUES \
             (1, '{TENANT_TYPE}'), (2, '{DEPT_TYPE}'), (3, '{USER_TYPE}')"
        ),
    )
    .await;
    run(
        raw,
        "INSERT INTO gts_type_allowed_parent (type_id, parent_type_id) VALUES (2, 1), (2, 2)",
    )
    .await;
    run(
        raw,
        "INSERT INTO gts_type_allowed_membership (type_id, membership_type_id) VALUES (2, 3)",
    )
    .await;
    run(
        raw,
        &format!(
            "INSERT INTO resource_group (id, parent_id, gts_type_id, name, metadata, tenant_id) VALUES \
             (X'{ROOT}', NULL, 1, 'root', NULL, X'{ROOT}'), \
             (X'{CHILD}', X'{ROOT}', 2, 'child', '{{\"k\":1}}', X'{ROOT}'), \
             (X'{GRANDCHILD}', X'{CHILD}', 2, 'grandchild', NULL, X'{ROOT}'), \
             (X'{OTHER_ROOT}', NULL, 1, 'other', NULL, X'{OTHER_ROOT}')"
        ),
    )
    .await;
    run(
        raw,
        &format!(
            "INSERT INTO resource_group_closure (ancestor_id, descendant_id, depth) VALUES \
             (X'{ROOT}', X'{ROOT}', 0), (X'{CHILD}', X'{CHILD}', 0), \
             (X'{GRANDCHILD}', X'{GRANDCHILD}', 0), (X'{OTHER_ROOT}', X'{OTHER_ROOT}', 0), \
             (X'{ROOT}', X'{CHILD}', 1), (X'{ROOT}', X'{GRANDCHILD}', 2), \
             (X'{CHILD}', X'{GRANDCHILD}', 1)"
        ),
    )
    .await;
    run(
        raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) VALUES \
             (X'{CHILD}', 3, 'u1'), (X'{GRANDCHILD}', 3, 'u1'), (X'{ROOT}', 3, 'u2')"
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
async fn oversized_legacy_membership_refuses_upgrade_before_rebuilding_tables() {
    let f = at_initial_schema().await;
    seed_hierarchy(&f.raw).await;
    let longest = format!("gts.cf.core.rg.type.v1~{}", "a".repeat(1001));
    // 1000 characters, but 2000 UTF-8 bytes: the portable contract counts
    // bytes rather than SQLite's character length.
    let resource = "\u{436}".repeat(1000);
    run(
        &f.raw,
        &format!("INSERT INTO gts_type (id, schema_id) VALUES (4, '{longest}')"),
    )
    .await;
    run(
        &f.raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
             VALUES (X'{ROOT}', 4, '{resource}')"
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
            "SELECT typeof(gts_type_id) FROM resource_group LIMIT 1"
        )
        .await,
        vec![vec!["integer"]]
    );
    assert_eq!(
        rows(
            &f.raw,
            "SELECT CAST(length(CAST(t.schema_id AS BLOB)) AS TEXT), \
             CAST(length(CAST(m.resource_id AS BLOB)) AS TEXT) \
             FROM resource_group_membership m JOIN gts_type t ON t.id = m.gts_type_id \
             WHERE m.gts_type_id = 4"
        )
        .await,
        vec![vec!["1024", "2000"]]
    );
    assert!(
        rows(
            &f.raw,
            "SELECT name FROM sqlite_master WHERE name LIKE '%_backup' OR name LIKE '%_new'"
        )
        .await
        .is_empty(),
        "the preflight runs before any rebuild"
    );
    run(
        &f.raw,
        "UPDATE resource_group_membership SET resource_id = 'remapped-resource' WHERE gts_type_id = 4",
    )
    .await;
    upgrade(&f.db).await.expect("upgrade after remediation");
    assert_eq!(
        rows(
            &f.raw,
            "SELECT typeof(gts_type_id) FROM resource_group LIMIT 1"
        )
        .await,
        vec![vec!["text"]],
        "the refused attempt did not record the migration as applied"
    );
    assert_eq!(
        rows(
            &f.raw,
            "SELECT CAST(count(*) AS TEXT) FROM resource_group_membership"
        )
        .await,
        vec![vec!["4"]]
    );
}

#[tokio::test]
async fn an_existing_hierarchy_keeps_every_row_under_its_gts_identifier() {
    let f = at_initial_schema().await;
    seed_hierarchy(&f.raw).await;
    upgrade(&f.db).await.expect("upgrade");

    assert_eq!(
        rows(
            &f.raw,
            "SELECT hex(id), CASE WHEN parent_id IS NULL THEN NULL ELSE hex(parent_id) END, \
             gts_type_id, name, metadata, hex(tenant_id) \
             FROM resource_group ORDER BY name"
        )
        .await,
        vec![
            vec![CHILD, ROOT, DEPT_TYPE, "child", "{\"k\":1}", ROOT],
            vec![GRANDCHILD, CHILD, DEPT_TYPE, "grandchild", "NULL", ROOT],
            vec![OTHER_ROOT, "NULL", TENANT_TYPE, "other", "NULL", OTHER_ROOT],
            vec![ROOT, "NULL", TENANT_TYPE, "root", "NULL", ROOT],
        ]
    );
    assert_eq!(
        rows(
            &f.raw,
            "SELECT hex(ancestor_id), hex(descendant_id), CAST(depth AS TEXT) \
             FROM resource_group_closure ORDER BY 1, 2"
        )
        .await
        .len(),
        7,
        "every closure row survives"
    );
    assert_eq!(
        rows(
            &f.raw,
            &format!(
                "SELECT CAST(depth AS TEXT) FROM resource_group_closure \
                 WHERE ancestor_id = X'{ROOT}' AND descendant_id = X'{GRANDCHILD}'"
            )
        )
        .await,
        vec![vec!["2"]]
    );
    assert_eq!(
        rows(
            &f.raw,
            "SELECT hex(group_id), gts_type_id, resource_id FROM resource_group_membership \
             ORDER BY resource_id, 1"
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
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'gts_type%'"
        )
        .await
        .is_empty(),
        "the RG type catalogue is dropped"
    );
    assert!(
        rows(
            &f.raw,
            "SELECT name FROM sqlite_master WHERE name LIKE '%_backup' OR name LIKE '%_new'"
        )
        .await
        .is_empty(),
        "no rebuild leftovers"
    );
    let mut indexes: Vec<String> = rows(
        &f.raw,
        "SELECT name FROM sqlite_master WHERE type = 'index' AND name LIKE 'idx_%'",
    )
    .await
    .into_iter()
    .map(|r| r[0].clone())
    .collect();
    indexes.sort();
    assert_eq!(
        indexes,
        [
            "idx_rg_gts_type_id",
            "idx_rg_name",
            "idx_rg_parent_id",
            "idx_rg_tenant_id",
            "idx_rgc_ancestor_depth",
            "idx_rgc_descendant_id",
            "idx_rgm_gts_type_resource",
        ]
    );
}

#[tokio::test]
async fn constraints_hold_after_the_upgrade() {
    let f = at_initial_schema().await;
    seed_hierarchy(&f.raw).await;
    upgrade(&f.db).await.expect("upgrade");

    let duplicate = exec(
        &f.raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
             VALUES (X'{CHILD}', '{USER_TYPE}', 'u1')"
        ),
    )
    .await
    .expect_err("membership primary key");
    assert!(duplicate.to_string().contains("UNIQUE"), "{duplicate}");

    let dangling = exec(
        &f.raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
             VALUES (X'00000000000000000000000000000099', '{USER_TYPE}', 'u3')"
        ),
    )
    .await
    .expect_err("membership group foreign key");
    assert!(dangling.to_string().contains("FOREIGN KEY"), "{dangling}");

    let parent = exec(
        &f.raw,
        &format!("DELETE FROM resource_group WHERE id = X'{ROOT}'"),
    )
    .await
    .expect_err("a parent with children (and closure rows) is restricted");
    assert!(parent.to_string().contains("FOREIGN KEY"), "{parent}");

    let longest = format!("gts.cf.core.rg.type.v1~{}", "a".repeat(1024 - 23));
    assert_eq!(longest.len(), 1024);
    run(
        &f.raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
             VALUES (X'{ROOT}', '{longest}', '019a0000-0000-7000-8000-000000000042')"
        ),
    )
    .await;
    let too_long = exec(
        &f.raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
             VALUES (X'{ROOT}', '{longest}a', 'u9')"
        ),
    )
    .await
    .expect_err("type identifier bound");
    assert!(too_long.to_string().contains("CHECK"), "{too_long}");

    for (resource, accepted) in [
        ("a".repeat(1576), true),
        ("a".repeat(1577), false),
        ("\u{436}".repeat(788), true),
        ("\u{436}".repeat(789), false),
    ] {
        let result = exec(
            &f.raw,
            &format!(
                "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
                 VALUES (X'{ROOT}', '{longest}', '{resource}')"
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
             VALUES (X'{ROOT}', '{USER_TYPE}', '{}')",
            "b".repeat(2000)
        ),
    )
    .await;
}

#[tokio::test]
async fn an_orphaned_type_key_refuses_the_upgrade_and_leaves_the_old_schema() {
    let f = at_initial_schema().await;
    seed_hierarchy(&f.raw).await;
    run(&f.raw, "PRAGMA foreign_keys = OFF").await;
    run(
        &f.raw,
        &format!(
            "INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id) \
             VALUES (X'{ROOT}', 99, 'orphan')"
        ),
    )
    .await;
    run(&f.raw, "PRAGMA foreign_keys = ON").await;

    upgrade(&f.db)
        .await
        .expect_err("a membership whose type key has no catalogue row");

    assert_eq!(
        rows(
            &f.raw,
            "SELECT typeof(gts_type_id) FROM resource_group LIMIT 1"
        )
        .await,
        vec![vec!["integer"]],
        "resource_group is not rebuilt"
    );
    assert_eq!(
        rows(&f.raw, "SELECT CAST(count(*) AS TEXT) FROM gts_type").await,
        vec![vec!["3"]],
        "the catalogue is not dropped"
    );
    assert_eq!(
        rows(
            &f.raw,
            "SELECT CAST(count(*) AS TEXT) FROM resource_group_closure"
        )
        .await,
        vec![vec!["7"]],
        "closure rows are not evacuated"
    );
    assert_eq!(
        rows(
            &f.raw,
            "SELECT CAST(count(*) AS TEXT) FROM resource_group_membership"
        )
        .await,
        vec![vec!["4"]]
    );
    assert!(
        rows(
            &f.raw,
            "SELECT name FROM sqlite_master WHERE name LIKE '%_backup' OR name LIKE '%_new'"
        )
        .await
        .is_empty(),
        "no partial rebuild"
    );
}

#[tokio::test]
async fn down_is_refused_as_irreversible() {
    let f = at_initial_schema().await;
    upgrade(&f.db).await.expect("upgrade");
    let migration = Migrator::migrations()
        .pop()
        .expect("the GTS identifier migration");
    let err = migration
        .down(&SchemaManager::new(&f.raw))
        .await
        .expect_err("irreversible");
    assert!(err.to_string().contains("irreversible"), "{err}");
}
