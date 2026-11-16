-- Server kinds the control plane starts instances of (docs/INSTANCES.md):
-- each kind's live image and the parts of its hallow.toml that decide how it
-- scales, as JSON (`hallow_proto::instance::KindSpec`). Instances themselves
-- are live state in Redis, never rows.
CREATE TABLE hallow.server_kinds (
    name       text        PRIMARY KEY CHECK (name ~ '^[a-z]([a-z0-9]|-[a-z0-9]){0,39}$'),
    spec       jsonb       NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    updated_by text        CHECK (char_length(updated_by) BETWEEN 1 AND 64)
);
