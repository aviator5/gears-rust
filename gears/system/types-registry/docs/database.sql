-- Types Registry managed-state schema (P1).
--
-- This is a PostgreSQL reference schema, not a migration. Backend migrations
-- map identity, UUID, binary, timestamp, and binary-collation types to the
-- corresponding SQLite / PostgreSQL / MySQL representation.
--
-- JSON documents are stored as canonical UTF-8 text. Types Registry stores no
-- externally managed entity identifiers, content, revisions, mappings, or
-- tenant state.
--
-- Every column holding a GTS Identifier or GTS pattern is varchar(1024) with a
-- binary collation, and MUST be declared with an ASCII character set on a
-- backend whose default is multi-byte. Both halves are load-bearing.
--
-- The binary collation makes prefix ranges exact and identical on all three
-- backends. It is what lets a pattern compile to explicit bounds rather than a
-- LIKE, and what makes the derivation reverse lookup a range scan: every base
-- is a literal string prefix of the identifiers derived from it, and `~`
-- (0x7E) sorts after every character a segment may contain, while `.` (0x2E)
-- sorts before them, so a prefix range is clean in both directions.
--
-- The ASCII declaration is a portability requirement, not an optimization.
-- InnoDB caps an index key at 3072 bytes; varchar(1024) in utf8mb4 reserves
-- 4096, so the unique index on entity.gts_id and every composite index that
-- ends in an identifier would be rejected outright on MySQL. The GTS grammar
-- admits only lowercase ASCII - segments are `[a-z_][a-z0-9_]*`, separators are
-- `.` and `~`, versions are digits, an anonymous tail is hex - so one byte per
-- character is exact rather than a truncation. This applies to entity.gts_id,
-- version_family.family_key, operation_item.gts_id, dependency_pattern.pattern
-- and its upper bound, and source_claim.pattern and its upper bound.
--
-- Durable dispatch uses toolkit-db outbox with the `types_registry_outbox`
-- table prefix. Those ToolKit-owned tables are created by outbox migrations
-- and are intentionally not duplicated here. Outbox messages contain only an
-- operation UUID.
--
-- Every actual mutation is asynchronous. An accepted POST always creates one
-- operation row, which carries both the scoped Idempotency-Key and the
-- client-visible workflow state, one operation_item per candidate GTS
-- Identifier, and one outbox message. There is no synchronous acceptance path
-- and no separate request-receipt table.
--
-- Table names are not prefixed with `registry_` a second time: the
-- `types_registry__` prefix already namespaces them.
--
-- Enumerations are stored as smallint, with the meaning of every value written
-- beside the column. MySQL ENUM and PostgreSQL CREATE TYPE are both unavailable
-- as a common representation and SQLite has neither, so an enumeration was
-- always going to be emulated; a fixed 2-byte integer emulates it more cheaply
-- than a varchar, carries no collation or character-set question, and keeps the
-- indexes that lead with a status or a scope narrow.
--
-- Three rules govern those values.
--
-- They are storage encoding only. The SDK and REST contracts keep the string
-- vocabulary - a response says `"status": "completed"`, never `3` - and the
-- mapping lives in the storage layer. A numeric value must never reach a public
-- payload.
--
-- Numbering is append-only. Renumbering is a data migration, unlike renaming a
-- string that no row stores. Values are assigned in the order the governing ADR
-- lists them, a new value takes the next free number, and a retired value's
-- number is never reused.
--
-- Numbering is per column and deliberately not aligned between columns. Giving
-- `pending` the same number in operation.status and operation_item.status would
-- imply a relationship between two distinct vocabularies that does not exist,
-- and it would force gaps wherever they diverge - and a gap reads as a mistake.
--
-- CHECK constraints list the admissible values rather than bounding a range, so
-- that a number outside the vocabulary is rejected instead of being accepted as
-- a value nothing has defined yet.


-- A version family binds a family key to one ownership scope, and holds nothing
-- else. Under ADR-0008 the registry names no newest member and keeps no
-- current-member pointer, so there is no member count, no highest major, and no
-- family-scoped compare-and-swap. The single job of the row is to make
-- `owner_scope(version_successor) == owner_scope(version_family_root)`
-- enforceable by a uniqueness constraint plus an ordinary read, and to keep
-- concurrent first registration from creating one family under two owners.
--
-- `family_key` is the canonical GTS Identifier with the major version of its
-- LAST segment removed, every preceding segment held exactly as written, and the
-- trailing `~` of a type identifier normalized away (ADR-0004):
--
--   gts.acme.crm.customer.v1~                 -> gts.acme.crm.customer
--   gts.x.core.events.type.v1~acme.order.v1~  -> gts.x.core.events.type.v1~acme.order
--   gts.x.core.events.type.v2~acme.order.v1~  -> gts.x.core.events.type.v2~acme.order
--   gts.x.core.events.topic.v1~acme.orders.v1 -> gts.x.core.events.topic.v1~acme.orders
--
-- Dropping the kind marker is deliberate: it makes a name either a type family
-- or an Instance family and never both. A derived type
-- `gts.A~acme.orders.v1~` and a well-known Instance `gts.A~acme.orders.v1`
-- differ by one character and denote entirely unrelated things, and nothing
-- needs both - an Instance of that derived type is
-- `gts.A~acme.orders.v1~<segment>`, not the colliding form. A registry that
-- exists partly to catch naming accidents should catch this one.
--
-- Enforcement needs no kind column here. Both spellings map to one key, so the
-- second registrant finds this row, and admission - under the family lock it
-- already holds for the ownership check - reads any member and rejects a
-- candidate whose kind differs. Every member's identifier already carries its
-- kind, so a column would duplicate it and need an invariant to stay true.
--
-- Ownership is different, and the asymmetry is the reason it does get columns:
-- it must be fixed BEFORE any member exists, so that two concurrent first
-- registrations cannot assign one family to two owners. The kind constraint only
-- bites once a member exists, and the loser of that race blocks on this row
-- until the winner's member is visible, since family row and entity row commit
-- together.
--
-- With no members there is no constraint, which is the correct release after the
-- purge of ADR-0013. Ordinary deletion leaves member rows in place, so
-- it does not free the name.
--
-- A family key is NOT a GTS Identifier - it carries no version and no kind - so
-- it must not be parsed as one. The encoding is total: every managed identifier's
-- last segment carries a major version, because ADR-0004 forbids minor versions
-- and ADR-0001 forbids an explicit UUID tail.
--
-- There is no index on the owner columns. No P1 flow asks for the families a
-- tenant owns: discovery and search filter on entity, which carries its own
-- owner copy for exactly that reason. Ownership is also write-once - there is no
-- correction operation, since ADR-0013's purge lets a mis-assigned owner be
-- repaired by delete, purge, re-register - so nothing ever updates these columns
-- after the family is created.
CREATE TABLE types_registry__version_family (
    id               bigint        GENERATED BY DEFAULT AS IDENTITY,
    family_key       varchar(1024) COLLATE "C" NOT NULL,
    ownership_scope  smallint      NOT NULL, -- 1 global, 2 tenant
    owner_tenant_id  uuid          NULL,
    created_at       timestamptz   NOT NULL,

    CONSTRAINT pk_tr_version_family PRIMARY KEY (id),
    CONSTRAINT uq_tr_version_family_key UNIQUE (family_key),
    CONSTRAINT ck_tr_version_family_owner CHECK (
        (ownership_scope = 1 AND owner_tenant_id IS NULL)
        OR
        (ownership_scope = 2 AND owner_tenant_id IS NOT NULL)
    )
);


-- Request identity and client-visible workflow state are one row. The relation
-- was strictly one-to-one on the operation side and one-to-zero-or-one on the
-- request side, the optional half existing only for a synchronous `unchanged`
-- acceptance; with that path removed the two are the same record.
--
-- No synchronous acceptance path exists because an all-equal batch is
-- unreachable for a caller that honours its own preconditions:
-- `must_not_exist` fails once the entity exists, and `match_resource_version`
-- fails once the content the caller read has moved. A caller that reconciled
-- before writing simply sends no POST. `outcome = 'unchanged'` therefore
-- survives only as the guarantee that a redundant submission creates no
-- revision and does not advance resource_version - not as a hot path.
--
-- `idempotency_scope_hash` is a digest over (plane, tenant_id, principal_id).
-- The principal participates so that one
-- subject's key cannot return another subject's response, and with it another
-- subject's Registry References and resource versions, inside one tenant.
--
-- The digest is a correctness device, not a way to narrow the unique index. The
-- direct alternative, UNIQUE over the three scope columns plus the key, would
-- not enforce anything on the platform plane: tenant_id is NULL there, and
-- all three backends treat NULLs in a unique index as distinct, so two platform
-- operations with the same key and principal would both be admitted. Folding
-- the scope into a digest removes the NULL from the constraint.
--
-- `kind` names the mutation family. Every value comes from an accepted decision:
-- registration and its revisions (ADR-0012), deletion (ADR-0008), and purge
-- (ADR-0013). Additions extend the CHECK rather than bypassing it.
--
-- There is no ownership correction. A mis-assigned owner is repaired by delete,
-- purge, re-register: purge releases the identifier, so the repair no longer
-- strands it, which was the only reason a dedicated operation existed
-- (ADR-0009). Ownership is therefore immutable for the life of an entity.
--
-- Worker leases, attempts, retries, and dead letters belong to the ToolKit
-- outbox processor tables and are not duplicated here.
CREATE TABLE types_registry__operation (
    id                       uuid         NOT NULL,
    kind                     smallint     NOT NULL, -- 1 registration, 2 deletion, 3 purge
    plane                    smallint     NOT NULL, -- 1 platform, 2 tenant
    tenant_id                uuid         NULL,
    principal_id             varchar(255) NOT NULL,
    idempotency_key          varchar(255) NOT NULL,
    idempotency_scope_hash   bytea        NOT NULL,
    request_fingerprint      bytea        NOT NULL,
    -- 1 pending, 2 running, 3 succeeded, 4 unchanged, 5 partially_succeeded,
    -- 6 failed, 7 cancelled, 8 expired
    status                   smallint     NOT NULL,
    created_at               timestamptz  NOT NULL,
    started_at               timestamptz  NULL,
    completed_at             timestamptz  NULL,

    CONSTRAINT pk_tr_operation PRIMARY KEY (id),
    CONSTRAINT uq_tr_operation_idem
        UNIQUE (idempotency_scope_hash, idempotency_key),
    CONSTRAINT ck_tr_operation_kind CHECK (kind IN (1, 2, 3)),
    CONSTRAINT ck_tr_operation_plane CHECK (
        (plane = 1 AND tenant_id IS NULL)            -- platform
        OR
        (plane = 2 AND tenant_id IS NOT NULL)        -- tenant
    ),
    CONSTRAINT ck_tr_operation_status CHECK (
        status IN (1, 2, 3, 4, 5, 6, 7, 8)
    ),
    CONSTRAINT ck_tr_operation_state CHECK (
        (status = 1                                  -- pending
            AND started_at IS NULL
            AND completed_at IS NULL)
        OR
        (status = 2                                  -- running
            AND started_at IS NOT NULL
            AND completed_at IS NULL)
        OR
        (status IN (3, 4, 5, 6)                      -- terminal outcomes
            AND started_at IS NOT NULL
            AND completed_at IS NOT NULL)
        OR
        (status IN (7, 8)                            -- cancelled, expired
            AND completed_at IS NOT NULL)
    )
);

-- Finds non-terminal operations whose worker died, so they can be expired. It
-- covers terminal rows too, which will be the overwhelming majority over time; a
-- partial index would be far smaller but is not portable to MySQL, so the full
-- index is the deliberate choice.
CREATE INDEX idx_tr_operation_status
    ON types_registry__operation (status, created_at, id);


-- One durable candidate and public result per exact GTS Identifier.
--
-- The entity kind and the resulting Registry Reference are both absent because
-- both follow from `gts_id` in this same row - the kind from its trailing `~`,
-- the reference from its deterministic derivation. `entity` stores each of them
-- for a reason that does not apply here: there they lead an index or back a
-- uniqueness constraint, while these rows are only ever read by operation_id.
--
-- Each transition carries its own timestamp, so there is no `updated_at`. That
-- differs from `entity`, where transitions are not individually dated and only
-- resource_version orders them.
--
-- The optimistic precondition is one column, not a kind plus a value. Zero means
-- `must_not_exist`; any other value is the entity resource version to match. The
-- sentinel is injective because ck_tr_entity_resource_version requires a real
-- version to be at least 1 - permitting version 0 there would break this
-- encoding silently, which is why the two constraints belong together in the
-- reader's mind. Splitting it in two would make `must_not_exist` with a version,
-- and a match with none, representable states that a constraint then has to
-- forbid. The vocabulary is closed at two by ADR-0012, and closed is what makes
-- a sentinel safe: "must exist at any version" is meaningless for a caller that
-- has just read the entity, and upsert is refused deliberately. etcd encodes
-- absence the same way, as `mod_revision == 0`.
--
-- The contract keeps the explicit form - `must_not_exist` or
-- `match_resource_version { resource_version }` - as the header rule requires:
-- numbers are storage encoding and never reach a public payload.
--
-- `request_payload` is dropped when an item reaches a terminal state. For a
-- successful item the content has moved into a revision; for a failed one it is
-- genuinely lost, which is the case where seeing the submission would help most.
-- That is accepted rather than overlooked: operations are retained for as long
-- as the revisions referencing them, so keeping rejected content would keep it
-- forever, the structured reason survives in `error_payload`, and the submitter
-- holds its own copy.
CREATE TABLE types_registry__operation_item (
    id                        bigint        GENERATED BY DEFAULT AS IDENTITY,
    operation_id              uuid          NOT NULL,
    item_no                   integer       NOT NULL,
    gts_id                    varchar(1024) COLLATE "C" NOT NULL,
    -- 0 means the candidate must not exist; otherwise the version to match
    expected_resource_version bigint        NOT NULL,
    -- 1 pending, 2 running, 3 succeeded, 4 unchanged, 5 failed, 6 blocked,
    -- 7 cancelled
    status                    smallint      NOT NULL,
    request_payload           text          NULL,
    result_revision_no        integer       NULL,
    result_resource_version   bigint        NULL,
    error_payload             text          NULL,
    created_at                timestamptz   NOT NULL,
    started_at                timestamptz   NULL,
    completed_at              timestamptz   NULL,

    CONSTRAINT pk_tr_operation_item PRIMARY KEY (id),
    CONSTRAINT uq_tr_operation_item_no
        UNIQUE (operation_id, item_no),
    CONSTRAINT uq_tr_operation_item_gts
        UNIQUE (operation_id, gts_id),
    CONSTRAINT fk_tr_operation_item_operation
        FOREIGN KEY (operation_id)
        REFERENCES types_registry__operation (id) ON DELETE CASCADE,
    CONSTRAINT ck_tr_operation_item_no CHECK (item_no >= 0),
    CONSTRAINT ck_tr_operation_item_precondition
        CHECK (expected_resource_version >= 0),
    CONSTRAINT ck_tr_operation_item_revision
        CHECK (result_revision_no IS NULL OR result_revision_no >= 1),
    CONSTRAINT ck_tr_operation_item_resource_version
        CHECK (
            result_resource_version IS NULL
            OR result_resource_version >= 1
        ),
    CONSTRAINT ck_tr_operation_item_status CHECK (
        status IN (1, 2, 3, 4, 5, 6, 7)
    ),
    CONSTRAINT ck_tr_operation_item_state CHECK (
        (status = 1                                  -- pending
            AND request_payload IS NOT NULL
            AND result_revision_no IS NULL
            AND result_resource_version IS NULL
            AND error_payload IS NULL
            AND started_at IS NULL
            AND completed_at IS NULL)
        OR
        (status = 2                                  -- running
            AND request_payload IS NOT NULL
            AND result_revision_no IS NULL
            AND result_resource_version IS NULL
            AND error_payload IS NULL
            AND started_at IS NOT NULL
            AND completed_at IS NULL)
        OR
        (status IN (3, 4)                            -- succeeded, unchanged
            AND request_payload IS NULL
            AND result_revision_no IS NOT NULL
            AND result_resource_version IS NOT NULL
            AND error_payload IS NULL
            AND started_at IS NOT NULL
            AND completed_at IS NOT NULL)
        OR
        (status IN (5, 6)                            -- failed, blocked
            AND request_payload IS NULL
            AND result_revision_no IS NULL
            AND result_resource_version IS NULL
            AND error_payload IS NOT NULL
            AND completed_at IS NOT NULL)
        OR
        (status = 7                                  -- cancelled
            AND request_payload IS NULL
            AND result_revision_no IS NULL
            AND result_resource_version IS NULL
            AND completed_at IS NOT NULL)
    )
);

-- No index by status. Polling reads every item of one operation ordered by
-- item_no, which uq_tr_operation_item_no serves, and a batch is bounded, so
-- scanning its items costs less than maintaining a second index.


-- The logical registry entity: one row per admitted managed GTS Identifier,
-- of either kind. It is also the tombstone of a deleted identifier, which is
-- what keeps a previously issued Registry Reference reverse-resolvable.
--
-- Two columns look derivable and are stored deliberately, both to serve an
-- index rather than to record a fact:
--
--   * `registry_reference` is UUIDv5 over `gts_id`, but the hash is not
--     invertible, so reverse resolution needs an index over the stored value.
--     Its uniqueness is also the ADR-0001 collision detector: a derivation that
--     did collide is rejected at admission rather than silently rebinding a
--     stored domain reference. An expression index is not an option - UUIDv5
--     over a namespace is not portable across the three backends.
--   * `entity_kind` follows from the trailing `~`, but a suffix predicate is
--     not portably indexable, and the column also carries the kind-conditional
--     constraints.
--
-- `lifecycle_status` has two values and no third. Under ADR-0008 no managed
-- entity ever carries `deprecated` in P1, and externally managed entities are
-- never stored here, so adding a value to this enumeration would be changing a
-- decision rather than extending a vocabulary.
--
-- Ownership is copied from version_family so that SecureORM can scope on this
-- row and every visibility filter avoids a join. The invariant that the copy
-- equals the family's owner is enforced by admission under the family row lock,
-- not by a constraint: a composite foreign key would be skipped entirely for
-- global entities, where owner_tenant_id is NULL and MATCH SIMPLE does not
-- check, so it would cover half the cases while looking complete.
CREATE TABLE types_registry__entity (
    id                       bigint        GENERATED BY DEFAULT AS IDENTITY,
    registry_reference       uuid          NOT NULL,
    gts_id                   varchar(1024) COLLATE "C" NOT NULL,
    -- 1 type_schema, 2 instance
    entity_kind              smallint      NOT NULL,
    family_id                bigint        NOT NULL,
    ownership_scope          smallint      NOT NULL, -- 1 global, 2 tenant
    owner_tenant_id          uuid          NULL,
    lifecycle_status         smallint      NOT NULL, -- 1 active, 2 deleted
    resource_version         bigint        NOT NULL,
    deleted_at               timestamptz   NULL,
    created_at               timestamptz   NOT NULL,
    updated_at               timestamptz   NOT NULL,

    CONSTRAINT pk_tr_entity PRIMARY KEY (id),
    CONSTRAINT uq_tr_entity_gts_id UNIQUE (gts_id),
    CONSTRAINT uq_tr_entity_reference UNIQUE (registry_reference),
    CONSTRAINT fk_tr_entity_family
        FOREIGN KEY (family_id)
        REFERENCES types_registry__version_family (id) ON DELETE RESTRICT,
    CONSTRAINT ck_tr_entity_kind
        CHECK (entity_kind IN (1, 2)),
    CONSTRAINT ck_tr_entity_owner CHECK (
        (ownership_scope = 1 AND owner_tenant_id IS NULL)
        OR
        (ownership_scope = 2 AND owner_tenant_id IS NOT NULL)
    ),
    CONSTRAINT ck_tr_entity_lifecycle CHECK (
        (lifecycle_status = 1 AND deleted_at IS NULL)     -- active
        OR
        (lifecycle_status = 2 AND deleted_at IS NOT NULL) -- deleted
    ),
    CONSTRAINT ck_tr_entity_resource_version
        CHECK (resource_version >= 1)
);

-- Exact family membership for discovery, which a GTS wildcard cannot express on
-- its own because it is greedy across the chain separator and so also captures
-- types derived from a member. `gts_id` is included to make the enumeration
-- covering, since what the caller wants back is the identifiers.
CREATE INDEX idx_tr_entity_family
    ON types_registry__entity (family_id, lifecycle_status, gts_id);

-- The workhorse of every read. A tenant-scoped query is `ownership_scope = 1`
-- (global) `OR (ownership_scope = 2 AND owner_tenant_id IN <ancestor chain>)`,
-- so it resolves to one index range per ancestor plus one for global, each
-- carrying its own `gts_id` range for a pattern scan. Tenant hierarchies are
-- shallow, so the fan-out is small.
--
-- There is deliberately no second index leading with `entity_kind`: every read
-- must filter visibility anyway, so a kind-led scan cannot avoid this index.
-- If filtering by kind ever becomes hot, `entity_kind` belongs inside this
-- index rather than in a competing one.
CREATE INDEX idx_tr_entity_visibility
    ON types_registry__entity (
        ownership_scope,
        owner_tenant_id,
        lifecycle_status,
        gts_id
    );


-- Immutable admission snapshot: the authored document exactly as admitted, its
-- hash, and the provenance needed to explain the verdict it was admitted under.
--
-- Neither the effective artifacts nor the dependency revisions they were
-- resolved against are kept here, and the reason is the same for both: nothing
-- reads the admission-time resolution.
--
-- Compatibility compares a candidate against the current revision, never against
-- history, and the per-level evolvability of ADR-0003 is reported to the caller
-- in the operation result rather than read back later. The one operation that
-- does look backwards is the repair after the compatibility relation changes
-- meaning, and it does not want the historical resolution either. What the
-- registry promises a consumer is that the current revision accepts everything
-- an earlier one accepted, so the repair check is `Valid(rev_k) ⊆
-- Valid(current)` with both sides resolved against the dependencies that are
-- current now. That is sufficient rather than approximate: each dependency
-- evolved backward compatibly on its own chain, so
-- `Effective(rev_k)@D_then ⊆ Effective(rev_k)@D_now` by monotonicity of
-- conjunction, and composing gives exactly the promised guarantee. Resolving
-- rev_k against its historical dependencies would reconstruct a form no consumer
-- ever validated against.
--
-- What survives is therefore the authored document, its hash, and enough
-- provenance to scope that repair: `gts_spec_version` and `gts_impl_version`
-- identify the revisions admitted under superseded rules, so the repair runs
-- over those chains instead of the whole registry.
--
-- The vector still exists at admission time as concurrency control - validation
-- happens outside a transaction and the commit re-checks that no dependency
-- moved - but it lives in the worker for the duration of one attempt. It need
-- not survive: a redelivered outbox message revalidates from scratch.
--
-- The admitting principal is not duplicated: `operation_item_id` is NOT NULL and
-- its foreign key is RESTRICT, so the operation and its principal are always
-- reachable. That also settles a retention question by construction - revisions
-- are retained until purge, so the operations that produced them are too. This
-- is affordable because an operation row is narrow and `request_payload` is
-- already dropped when an item reaches a terminal state.
CREATE TABLE types_registry__type_schema_revision (
    id                         bigint       GENERATED BY DEFAULT AS IDENTITY,
    entity_id                  bigint       NOT NULL,
    revision_no                integer      NOT NULL,
    raw_schema                 text         NOT NULL,
    content_hash               bytea        NOT NULL,
    gts_spec_version           varchar(32)  NOT NULL,
    gts_impl_version           varchar(32)  NOT NULL,
    operation_item_id          bigint       NOT NULL,
    created_at                 timestamptz  NOT NULL,
    updated_at                 timestamptz  NOT NULL,

    CONSTRAINT pk_tr_type_schema_revision PRIMARY KEY (id),
    CONSTRAINT uq_tr_type_schema_revision_no
        UNIQUE (entity_id, revision_no),
    CONSTRAINT uq_tr_type_schema_revision_item UNIQUE (operation_item_id),
    CONSTRAINT fk_tr_type_schema_revision_entity
        FOREIGN KEY (entity_id)
        REFERENCES types_registry__entity (id) ON DELETE CASCADE,
    CONSTRAINT fk_tr_type_schema_revision_item
        FOREIGN KEY (operation_item_id)
        REFERENCES types_registry__operation_item (id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_tr_type_schema_revision_no CHECK (revision_no >= 1)
);

-- There is no index on (entity_id, content_hash). The no-op equality proof
-- compares a candidate against the current revision, which is one row reached
-- through type_schema, and re-submitting content equal to an older non-current
-- revision is admitted as a new revision under ADR-0005 rather than looked up.


-- Immutable admission snapshot of one registered Instance value, with the exact
-- Type Schema revision that validated it (ADR-0006). It keeps no dependency
-- revision vector and no admitting principal, for the same reasons as
-- type_schema_revision: nothing reads the admission-time resolution, and the
-- principal is reachable through operation_item_id.
--
-- The Type Schema revision that validated this value is referenced as an
-- (entity, revision_no) pair,
-- the same shape the current-state table uses. A single pointer to
-- type_schema_revision.id would fit this table's access pattern slightly better,
-- since the reference here is only ever traversed forwards, but it would save one
-- integer per row on a table holding one row per admitted Instance value while
-- costing a shape difference between two neighbouring tables. `instance`
-- genuinely needs the pair, so uniformity is worth more than the column.
--
-- `type_schema_entity_id` is also derivable from the Instance identifier -
-- the chain up to and including the last `~`, normative per GTS spec 11.1 - and
-- is materialized here to carry the composite foreign key.
CREATE TABLE types_registry__instance_revision (
    id                            bigint       GENERATED BY DEFAULT AS IDENTITY,
    entity_id                     bigint       NOT NULL,
    revision_no                   integer      NOT NULL,
    canonical_value               text         NOT NULL,
    content_hash                  bytea        NOT NULL,
    type_schema_entity_id         bigint       NOT NULL,
    type_schema_revision_no       integer      NOT NULL,
    gts_spec_version              varchar(32)  NOT NULL,
    gts_impl_version              varchar(32)  NOT NULL,
    operation_item_id             bigint       NOT NULL,
    created_at                    timestamptz  NOT NULL,
    updated_at                    timestamptz  NOT NULL,

    CONSTRAINT pk_tr_instance_revision PRIMARY KEY (id),
    CONSTRAINT uq_tr_instance_revision_no
        UNIQUE (entity_id, revision_no),
    CONSTRAINT uq_tr_instance_revision_item UNIQUE (operation_item_id),
    CONSTRAINT fk_tr_instance_revision_entity
        FOREIGN KEY (entity_id)
        REFERENCES types_registry__entity (id) ON DELETE CASCADE,
    CONSTRAINT fk_tr_instance_revision_schema
        FOREIGN KEY (
            type_schema_entity_id,
            type_schema_revision_no
        )
        REFERENCES types_registry__type_schema_revision (
            entity_id,
            revision_no
        )
        ON DELETE RESTRICT,
    CONSTRAINT fk_tr_instance_revision_item
        FOREIGN KEY (operation_item_id)
        REFERENCES types_registry__operation_item (id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_tr_instance_revision_no CHECK (revision_no >= 1)
);

-- No index by content hash, for the same reason as on type_schema_revision, and
-- none by Type Schema revision: revalidation when a schema advances runs
-- over current Instances, which idx_tr_instance_schema serves.


-- Current state of a Type Schema, as opposed to type_schema_revision, which is
-- its history. It holds only what actually differs from the revision it points
-- at, so nothing is duplicated between the two: the authored document, its hash,
-- and the checker versions are reached by joining on (entity_id, revision_no),
-- the foreign key already declared below.
--
-- What differs is the resolution: these artifacts are resolved against the
-- dependencies current NOW, and are recomputed when a floating dependency
-- advances without producing a new authored revision here. That divergence is
-- why this is a distinct fact rather than a cache of the revision.
--
-- Per-level content-model classification is not stored. It is a pure function of
-- `resolved_schema` in this same row, it is wanted only off the hot path by an
-- owner or a CI check asking whether a level can still gain an optional
-- property, and a compatibility check returns it as a by-product anyway.
--
-- `resolution_fingerprint` is a digest over the canonical bytes of the three
-- artifact columns, rewritten whenever they are recomputed. It is a content
-- digest and deliberately NOT a counter: a dependency-driven recompute very
-- often reproduces byte-identical artifacts, and a counter would invalidate
-- every consumer's cache for a change that did not reach them. Equality is the
-- only operation defined on it - it carries no order and must never be read as
-- newer or older.
--
-- Two reasons it exists, and both are needed to justify the column. Read
-- freshness and write concurrency are different axes: `entity.resource_version`
-- guards writes, and bumping it when a base advances would reject writers whose
-- authored content is unaffected, yet a consumer holding a resolved schema still
-- has to learn that the registry would now answer differently. And a conditional
-- read must be able to answer "is your validator current" without fetching and
-- hashing a large document, which is what storing the digest buys. It
-- participates in the resolution validator alongside `entity.resource_version`
-- and the tenant ancestor-chain version, and never in optimistic concurrency.
--
-- The digest input must be canonical and independent of the serializer's map
-- iteration order, or the value flaps without the artifacts changing.
CREATE TABLE types_registry__type_schema (
    entity_id                  bigint      NOT NULL,
    revision_no                integer     NOT NULL,
    resolved_schema            text        NOT NULL,
    effective_traits           text        NOT NULL,
    effective_traits_schema    text        NOT NULL,
    resolution_fingerprint     bytea       NOT NULL,
    created_at                 timestamptz NOT NULL,
    updated_at                 timestamptz NOT NULL,

    CONSTRAINT pk_tr_type_schema PRIMARY KEY (entity_id),
    CONSTRAINT fk_tr_type_schema_revision_ptr
        FOREIGN KEY (entity_id, revision_no)
        REFERENCES types_registry__type_schema_revision (
            entity_id,
            revision_no
        )
        ON DELETE CASCADE
);


-- Current state of a registered Instance, as opposed to instance_revision,
-- which is its history. Everything the revision already holds - the value, its
-- hash, the schema revision it was admitted against, the checker versions - is
-- reached by joining on (entity_id, revision_no).
--
-- Only one thing is genuinely current state: `validated_type_schema_revision_no`,
-- which advances when a newer schema revision revalidates this unchanged value.
-- There is no counterpart to type_schema's resolution fingerprint, because an
-- Instance has no derived form to drift. Its value is authored and immutable per
-- revision, so a read result cannot go stale without the entity itself changing,
-- and `entity.resource_version` with the tenant ancestor-chain version is a
-- complete validator for it.
CREATE TABLE types_registry__instance (
    entity_id                         bigint      NOT NULL,
    revision_no                       integer     NOT NULL,
    type_schema_entity_id             bigint      NOT NULL,
    validated_type_schema_revision_no integer     NOT NULL,
    created_at                        timestamptz NOT NULL,
    updated_at                        timestamptz NOT NULL,

    CONSTRAINT pk_tr_instance PRIMARY KEY (entity_id),
    CONSTRAINT fk_tr_instance_revision_ptr
        FOREIGN KEY (entity_id, revision_no)
        REFERENCES types_registry__instance_revision (
            entity_id,
            revision_no
        )
        ON DELETE CASCADE,
    CONSTRAINT fk_tr_instance_validated_schema
        FOREIGN KEY (
            type_schema_entity_id,
            validated_type_schema_revision_no
        )
        REFERENCES types_registry__type_schema_revision (
            entity_id,
            revision_no
        )
        ON DELETE RESTRICT
);

CREATE INDEX idx_tr_instance_schema
    ON types_registry__instance (
        type_schema_entity_id,
        validated_type_schema_revision_no
    );


-- Stored facts: the direct, exact managed-to-managed references that a GTS
-- Identifier does not already carry. Extracted from the admitted document and
-- replaced wholesale for one entity on each admission.
--
-- Two relations are deliberately absent because they ARE identifier-derivable
-- and storing them would duplicate a fact the identifier carries, creating a
-- class of defect where the row disagrees with the identifier:
--
--   * derivation - every base is a literal string prefix of the derived
--     identifier, so `chain_ids()` yields them forward and a prefix range scan
--     over entity.gts_id yields them in reverse;
--   * an Instance's conforming Type Schema - the chain up to and including the
--     last `~`, normative per GTS spec 11.1.
--
-- `edge_kind` distinguishes the two extraction mechanisms rather than decorating
-- the graph: `schema_ref` comes from the strict `$ref` extractor, which also
-- walks `$defs`, combinators, and `x-gts-traits-schema`; `gts_ref` comes from
-- `x-gts-ref`, which that extractor deliberately excludes as an instance-value
-- constraint rather than a schema dependency to inline. Both are needed, from
-- different sources. A target may be of either entity kind: a trait value
-- carrying `x-gts-ref` commonly names a well-known Instance, so a Type Schema
-- can depend on an Instance.
--
-- This set alone decides deletion admissibility. It is read with the two
-- identifier-derived relations above and never with the closure, because a
-- transitive-only dependent must not block: it would disappear the moment the
-- intermediate entity did.
CREATE TABLE types_registry__dependency_edge (
    from_entity_id bigint      NOT NULL,
    edge_kind      smallint    NOT NULL, -- 1 schema_ref, 2 gts_ref
    to_entity_id   bigint      NOT NULL,

    CONSTRAINT pk_tr_dependency_edge
        PRIMARY KEY (from_entity_id, edge_kind, to_entity_id),
    CONSTRAINT fk_tr_dependency_edge_from
        FOREIGN KEY (from_entity_id)
        REFERENCES types_registry__entity (id) ON DELETE CASCADE,
    CONSTRAINT fk_tr_dependency_edge_to
        FOREIGN KEY (to_entity_id)
        REFERENCES types_registry__entity (id) ON DELETE CASCADE,
    CONSTRAINT ck_tr_dependency_edge_kind
        CHECK (edge_kind IN (1, 2))
);

CREATE INDEX idx_tr_dependency_edge_to
    ON types_registry__dependency_edge (to_entity_id, from_entity_id);


-- Stored facts, like dependency_edge, for the `x-gts-ref` values that are a
-- prefix pattern rather than an exact identifier and therefore cannot be one
-- exact edge. `pattern_upper_bound` narrows candidates to an index range before
-- the GTS matcher confirms them.
--
-- Two things are unsettled here. Membership is expanded against the managed
-- identifiers that exist at expansion time, and nothing yet defines what
-- re-expands it when a NEW entity is admitted that falls under an already stored
-- pattern. And GTS spec 9.6 admits `x-gts-ref: "gts.*"`, meaning any valid GTS
-- identifier, which as a dependency would expand to every managed entity; which
-- `x-gts-ref` values become dependencies at all, rather than remaining pure
-- format constraints on an instance value, has to be decided before this table
-- can be relied on.
CREATE TABLE types_registry__dependency_pattern (
    from_entity_id      bigint        NOT NULL,
    pattern             varchar(1024) COLLATE "C" NOT NULL,
    pattern_upper_bound varchar(1024) COLLATE "C" NOT NULL,

    CONSTRAINT pk_tr_dependency_pattern
        PRIMARY KEY (from_entity_id, pattern),
    CONSTRAINT fk_tr_dependency_pattern_from
        FOREIGN KEY (from_entity_id)
        REFERENCES types_registry__entity (id) ON DELETE CASCADE
);

CREATE INDEX idx_tr_dependency_pattern_range
    ON types_registry__dependency_pattern (
        pattern,
        pattern_upper_bound
    );


-- A derived index, not a fact: the transitive reachability relation computed
-- over the union of dependency_edge, dependency_pattern membership, and the two
-- identifier-derived relations named above - derivation along the `$id` chain
-- and an Instance's conformance to its Type Schema. Every pair here is
-- recomputable from those inputs, which is what makes a full rebuild a valid
-- repair.
--
-- Including derivation is load-bearing rather than tidy, because a composed path
-- is not identifier-derivable even though each derivation step is. If T1 holds a
-- `$ref` to T2 and T2 derives from T3, then T3 belongs to T1's semantic contract
-- - T1's effective schema resolves through T2 into T3 - yet T1's identifier says
-- nothing about T2, let alone T3. Only the stored T1 -> T2 edge combined with
-- T2's identifier chain yields T1 -> T3.
--
-- Deliberately unqualified: there is no dependency-kind column, because the kind
-- of a transitive path is not a single value. The T1 -> T3 pair above is neither
-- ref-transitive nor derivation-transitive; the path is mixed, and several paths
-- with different kind sequences may join one pair. Carrying a kind would force it
-- into the key, store one reachable pair many times, and turn availability from
-- an existence check into a de-duplicating query. Its only consumer would be
-- diagnostics, which needs the whole path anyway and can traverse for it at
-- report time. Correctness needs the direct-versus-transitive axis, which the
-- table split already expresses.
--
-- Read forward for availability propagation (ADR-0010) and in reverse for the
-- revalidation set when a target admits a revision: the reverse index joins
-- dependent_entity_id to the current-state tables to fetch every current schema
-- whose effective form may change, without scanning revision history. Never read
-- for deletion admissibility.
--
-- Whether P1 needs this materialized at all is unsettled. Its availability
-- reader is unreachable in valid P1 managed state - deletion is blocked while a
-- direct dependent lives, and admission keeps a target visible wherever its
-- dependent is - unless a tenant moves within the hierarchy, which is PRD open
-- question 17. Its revalidation reader runs on the write path, where a BFS
-- alternating reverse edge lookups with prefix range scans is affordable and
-- costs no O(ancestors x descendants) row churn per admission.
CREATE TABLE types_registry__dependency_closure (
    dependent_entity_id  bigint NOT NULL,
    dependency_entity_id bigint NOT NULL,

    CONSTRAINT pk_tr_dependency_closure
        PRIMARY KEY (dependent_entity_id, dependency_entity_id),
    CONSTRAINT fk_tr_dependency_closure_dependent
        FOREIGN KEY (dependent_entity_id)
        REFERENCES types_registry__entity (id) ON DELETE CASCADE,
    CONSTRAINT fk_tr_dependency_closure_dependency
        FOREIGN KEY (dependency_entity_id)
        REFERENCES types_registry__entity (id) ON DELETE CASCADE,
    CONSTRAINT ck_tr_dependency_closure_self
        CHECK (dependent_entity_id <> dependency_entity_id)
);

CREATE INDEX idx_tr_dependency_closure_dependency
    ON types_registry__dependency_closure (
        dependency_entity_id,
        dependent_entity_id
    );


-- Active claims and permanent retired reservations share one identity. Exact
-- uniqueness is structural; GTS overlap is enforced by the built-in validator.
CREATE TABLE types_registry__source_claim (
    id                         bigint        GENERATED BY DEFAULT AS IDENTITY,
    pattern                    varchar(1024) COLLATE "C" NOT NULL,
    pattern_upper_bound        varchar(1024) COLLATE "C" NOT NULL,
    status                     smallint      NOT NULL, -- 1 active, 2 retired
    plugin_config_entity_id    bigint        NULL,
    plugin_config_revision_no  integer       NULL,
    plugin_instance_gts_id     varchar(1024) COLLATE "C" NULL,
    -- Bitmask over entity_kind: bit `1 << (entity_kind - 1)`, so 1 type_schema,
    -- 2 instance, 3 both. A set rather than a single value, kept numeric so it
    -- agrees with the entity_kind enumeration instead of restating its names.
    entity_kinds               smallint      NULL,
    priority                   integer       NULL,
    capabilities               text          NULL,
    created_at                 timestamptz   NOT NULL,
    updated_at                 timestamptz   NOT NULL,
    retired_at                 timestamptz   NULL,

    CONSTRAINT pk_tr_source_claim PRIMARY KEY (id),
    CONSTRAINT uq_tr_source_claim_pattern UNIQUE (pattern),
    CONSTRAINT fk_tr_source_claim_config
        FOREIGN KEY (
            plugin_config_entity_id,
            plugin_config_revision_no
        )
        REFERENCES types_registry__instance_revision (
            entity_id,
            revision_no
        )
        ON DELETE RESTRICT,
    CONSTRAINT ck_tr_source_claim_kinds
        CHECK (entity_kinds IS NULL OR entity_kinds BETWEEN 1 AND 3),
    CONSTRAINT ck_tr_source_claim_state CHECK (
        (status = 1                                  -- active
            AND plugin_config_entity_id IS NOT NULL
            AND plugin_config_revision_no IS NOT NULL
            AND plugin_instance_gts_id IS NOT NULL
            AND entity_kinds IS NOT NULL
            AND priority IS NOT NULL
            AND capabilities IS NOT NULL
            AND retired_at IS NULL)
        OR
        (status = 2                                  -- retired
            AND plugin_config_entity_id IS NULL
            AND plugin_config_revision_no IS NULL
            AND plugin_instance_gts_id IS NULL
            AND entity_kinds IS NULL
            AND priority IS NULL
            AND capabilities IS NULL
            AND retired_at IS NOT NULL)
    )
);

CREATE INDEX idx_tr_source_claim_order
    ON types_registry__source_claim (
        status,
        priority,
        plugin_instance_gts_id
    );

CREATE INDEX idx_tr_source_claim_range
    ON types_registry__source_claim (
        status,
        pattern,
        pattern_upper_bound
    );
