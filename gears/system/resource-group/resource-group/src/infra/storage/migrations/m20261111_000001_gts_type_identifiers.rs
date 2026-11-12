//! Groups and memberships reference their GTS type by identifier; RG's type
//! catalogue is dropped (T38b: the types registry is the only catalogue).
//!
//! `resource_group.gts_type_id` and `resource_group_membership.gts_type_id`
//! change from a `SMALLINT` surrogate key into `gts_type` to the GTS type
//! identifier itself (`gts_type.schema_id`, at most 1024 characters), backfilled
//! row by row before the catalogue tables are dropped. A row whose type key has
//! no catalogue row aborts the migration (the new column is `NOT NULL`) rather
//! than being dropped. Closure rows, tenants, the membership primary key and the
//! indexes keep their coverage under the same names.
//!
//! * PostgreSQL alters the tables in place.
//! * SQLite cannot drop a column that carries a foreign key or a primary-key
//!   part, so both tables are rebuilt (create-copy-drop-rename). Memberships and
//!   closure rows are evacuated to backup tables first: dropping
//!   `resource_group` with foreign keys enforced would otherwise refuse (or,
//!   unenforced, orphan them), and `PRAGMA foreign_keys` cannot change inside
//!   the migration's transaction. The old table's self references are cleared
//!   before it is dropped: its `ON DELETE RESTRICT` parent key is checked per
//!   row during the drop's implicit delete.
//! * The runtime applies the migration and its history record in one
//!   transaction (DDL is transactional on both backends), so a refusal such as
//!   an orphaned type key leaves the previous schema untouched.
//! * MySQL: resource-group has never supported it (the initial migration
//!   refuses); this migration refuses it too.
//!
//! Both backends bound membership keys to 2600 combined UTF-8 bytes for the
//! type and resource identifiers, leaving PostgreSQL's standard 8 KiB B-tree
//! pages room for the group UUID and tuple overhead. A preflight refuses old
//! oversized keys before any DDL; shorten/remap those identifiers with the
//! owning gear before upgrading. The old schema and rows remain intact.
//!
//! The catalogue's type definitions (`can_be_root`, allowed parent/membership
//! types, metadata schemas) are not carried anywhere: the migration cannot
//! publish them to the types registry, and dropping the catalogue removes the
//! only copy. Types defined only in an old RG database must be exported (or the
//! database backed up) before upgrading and published by their owners or
//! through the platform API -- before the upgrade, or after it from that saved
//! copy. Until then their groups and memberships stay readable and deletable,
//! and refuse new links, moves and metadata updates (T38b). There is no
//! runtime fallback.
//!
//! `down()` is irreversible: the dropped definitions cannot be rebuilt from the
//! identifiers that remain.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

#[derive(DeriveMigrationName)]
pub struct Migration;

// Frozen with this migration; a future SDK limit change needs a new migration.
const MAX_MEMBERSHIP_KEY_BYTES: usize = 2600;

const POSTGRES_UP: &str = r"
ALTER TABLE resource_group ADD COLUMN gts_type_code TEXT;
UPDATE resource_group g SET gts_type_code = t.schema_id FROM gts_type t WHERE t.id = g.gts_type_id;
ALTER TABLE resource_group ALTER COLUMN gts_type_code SET NOT NULL;
ALTER TABLE resource_group DROP CONSTRAINT fk_rg_gts_type;
DROP INDEX IF EXISTS idx_rg_gts_type_id;
ALTER TABLE resource_group DROP COLUMN gts_type_id;
ALTER TABLE resource_group RENAME COLUMN gts_type_code TO gts_type_id;
ALTER TABLE resource_group
    ADD CONSTRAINT ck_rg_gts_type_id CHECK (length(gts_type_id) BETWEEN 1 AND 1024);
CREATE INDEX idx_rg_gts_type_id ON resource_group (gts_type_id, id);

ALTER TABLE resource_group_membership ADD COLUMN gts_type_code TEXT;
UPDATE resource_group_membership m SET gts_type_code = t.schema_id
    FROM gts_type t WHERE t.id = m.gts_type_id;
ALTER TABLE resource_group_membership ALTER COLUMN gts_type_code SET NOT NULL;
ALTER TABLE resource_group_membership DROP CONSTRAINT resource_group_membership_pkey;
ALTER TABLE resource_group_membership DROP CONSTRAINT fk_rgm_gts_type;
DROP INDEX IF EXISTS idx_rgm_gts_type_resource;
ALTER TABLE resource_group_membership DROP COLUMN gts_type_id;
ALTER TABLE resource_group_membership RENAME COLUMN gts_type_code TO gts_type_id;
ALTER TABLE resource_group_membership
    ADD CONSTRAINT ck_rgm_gts_type_id CHECK (length(gts_type_id) BETWEEN 1 AND 1024);
ALTER TABLE resource_group_membership
    ADD CONSTRAINT ck_rgm_key_bytes
        CHECK (octet_length(gts_type_id) + octet_length(resource_id) <= 2600);
ALTER TABLE resource_group_membership
    ADD CONSTRAINT resource_group_membership_pkey PRIMARY KEY (group_id, gts_type_id, resource_id);
CREATE INDEX idx_rgm_gts_type_resource ON resource_group_membership (gts_type_id, resource_id);

DROP TABLE gts_type_allowed_membership;
DROP TABLE gts_type_allowed_parent;
DROP TABLE gts_type;
DROP DOMAIN IF EXISTS gts_type_path;
";

const SQLITE_UP: &str = r"
CREATE TABLE resource_group_closure_backup AS SELECT * FROM resource_group_closure;
CREATE TABLE resource_group_membership_backup AS
SELECT m.group_id AS group_id, t.schema_id AS gts_type_id, m.resource_id AS resource_id,
       m.created_at AS created_at
FROM resource_group_membership m
LEFT JOIN gts_type t ON t.id = m.gts_type_id;
DROP TABLE resource_group_membership;
DELETE FROM resource_group_closure;

CREATE TABLE resource_group_new (
    id TEXT PRIMARY KEY,
    parent_id TEXT,
    gts_type_id TEXT NOT NULL CHECK (length(gts_type_id) BETWEEN 1 AND 1024),
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 255),
    metadata TEXT,
    tenant_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT NULL,
    CONSTRAINT fk_resource_group_parent
        FOREIGN KEY (parent_id) REFERENCES resource_group_new(id)
        ON UPDATE CASCADE ON DELETE RESTRICT
);
INSERT INTO resource_group_new
    (id, parent_id, gts_type_id, name, metadata, tenant_id, created_at, updated_at)
SELECT g.id, g.parent_id, t.schema_id, g.name, g.metadata, g.tenant_id, g.created_at, g.updated_at
FROM resource_group g
LEFT JOIN gts_type t ON t.id = g.gts_type_id;
-- `DROP TABLE` deletes the old rows first, and the self reference is
-- `ON DELETE RESTRICT`, checked per row: unlink them (the copy keeps the links).
UPDATE resource_group SET parent_id = NULL;
DROP TABLE resource_group;
ALTER TABLE resource_group_new RENAME TO resource_group;

CREATE INDEX IF NOT EXISTS idx_rg_parent_id ON resource_group (parent_id);
CREATE INDEX IF NOT EXISTS idx_rg_name ON resource_group (name);
CREATE INDEX IF NOT EXISTS idx_rg_gts_type_id ON resource_group (gts_type_id, id);
CREATE INDEX IF NOT EXISTS idx_rg_tenant_id ON resource_group (tenant_id);

CREATE TABLE resource_group_membership (
    group_id TEXT NOT NULL,
    gts_type_id TEXT NOT NULL CHECK (length(gts_type_id) BETWEEN 1 AND 1024),
    resource_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    CONSTRAINT ck_rgm_key_bytes
        CHECK (length(CAST(gts_type_id AS BLOB)) + length(CAST(resource_id AS BLOB)) <= 2600),
    CONSTRAINT fk_rgm_group_id
        FOREIGN KEY (group_id) REFERENCES resource_group(id)
        ON UPDATE CASCADE ON DELETE RESTRICT,
    PRIMARY KEY (group_id, gts_type_id, resource_id)
);
INSERT INTO resource_group_membership (group_id, gts_type_id, resource_id, created_at)
SELECT group_id, gts_type_id, resource_id, created_at FROM resource_group_membership_backup;
DROP TABLE resource_group_membership_backup;
CREATE INDEX IF NOT EXISTS idx_rgm_gts_type_resource
    ON resource_group_membership (gts_type_id, resource_id);

INSERT INTO resource_group_closure (ancestor_id, descendant_id, depth)
SELECT ancestor_id, descendant_id, depth FROM resource_group_closure_backup;
DROP TABLE resource_group_closure_backup;

DROP TABLE gts_type_allowed_membership;
DROP TABLE gts_type_allowed_parent;
DROP TABLE gts_type;
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let (sql, key_bytes) = match backend {
            sea_orm::DatabaseBackend::Postgres => (
                POSTGRES_UP,
                "octet_length(t.schema_id) + octet_length(m.resource_id)",
            ),
            sea_orm::DatabaseBackend::Sqlite => (
                SQLITE_UP,
                "length(CAST(t.schema_id AS BLOB)) + length(CAST(m.resource_id AS BLOB))",
            ),
            _ => {
                return Err(DbErr::Migration(format!(
                    "{backend:?} is not a supported backend for resource-group"
                )));
            }
        };
        let conn = manager.get_connection();
        let preflight = format!(
            "SELECT 1 FROM resource_group_membership m \
             JOIN gts_type t ON t.id = m.gts_type_id \
             WHERE {key_bytes} > {MAX_MEMBERSHIP_KEY_BYTES} LIMIT 1"
        );
        if conn
            .query_one_raw(Statement::from_string(backend, preflight))
            .await?
            .is_some()
        {
            return Err(DbErr::Migration(format!(
                "{}: membership resource_type and resource_id exceed \
                 {MAX_MEMBERSHIP_KEY_BYTES} combined UTF-8 bytes; \
                 back up the database and remap oversized resource identifiers \
                 with their owning gear or remove the affected memberships \
                 before upgrading; the old schema and rows are unchanged",
                self.name()
            )));
        }
        conn.execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(format!(
            "{}: irreversible \u{2014} the dropped RG type catalogue cannot be rebuilt \
             from the GTS identifiers groups and memberships keep",
            self.name()
        )))
    }
}
