-- Current Resource Group PostgreSQL schema.
-- Executable migrations live in ../resource-group/src/infra/storage/migrations/.
-- This reference describes the resulting schema; it is not an upgrade script.

CREATE TABLE resource_group (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    parent_id UUID,
    gts_type_id TEXT NOT NULL,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 255),
    metadata JSONB,
    tenant_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ DEFAULT NULL,
    CONSTRAINT ck_rg_gts_type_id
        CHECK (length(gts_type_id) BETWEEN 1 AND 1024),
    CONSTRAINT fk_resource_group_parent
        FOREIGN KEY (parent_id) REFERENCES resource_group(id)
        ON UPDATE CASCADE ON DELETE RESTRICT
);

CREATE INDEX idx_rg_parent_id ON resource_group (parent_id);
CREATE INDEX idx_rg_name ON resource_group (name);
CREATE INDEX idx_rg_gts_type_id ON resource_group (gts_type_id, id);
CREATE INDEX idx_rg_tenant_id ON resource_group (tenant_id);

COMMENT ON COLUMN resource_group.gts_type_id
    IS 'GTS Type Schema identifier; definition and placement traits belong to Types Registry';

CREATE TABLE resource_group_closure (
    ancestor_id UUID NOT NULL,
    descendant_id UUID NOT NULL,
    depth INTEGER NOT NULL CHECK (depth >= 0),
    PRIMARY KEY (ancestor_id, descendant_id),
    CONSTRAINT fk_closure_ancestor
        FOREIGN KEY (ancestor_id) REFERENCES resource_group(id)
        ON UPDATE CASCADE ON DELETE RESTRICT,
    CONSTRAINT fk_closure_descendant
        FOREIGN KEY (descendant_id) REFERENCES resource_group(id)
        ON UPDATE CASCADE ON DELETE RESTRICT
);

CREATE INDEX idx_rgc_descendant_id ON resource_group_closure (descendant_id);
CREATE INDEX idx_rgc_ancestor_depth ON resource_group_closure (ancestor_id, depth);

CREATE TABLE resource_group_membership (
    group_id UUID NOT NULL,
    gts_type_id TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT resource_group_membership_pkey
        PRIMARY KEY (group_id, gts_type_id, resource_id),
    CONSTRAINT ck_rgm_gts_type_id
        CHECK (length(gts_type_id) BETWEEN 1 AND 1024),
    CONSTRAINT fk_rgm_group_id
        FOREIGN KEY (group_id) REFERENCES resource_group(id)
        ON UPDATE CASCADE ON DELETE RESTRICT
);

CREATE INDEX idx_rgm_gts_type_resource
    ON resource_group_membership (gts_type_id, resource_id);

COMMENT ON COLUMN resource_group_membership.gts_type_id
    IS 'Member resource GTS type identifier; the resource itself is owned by its gear';
