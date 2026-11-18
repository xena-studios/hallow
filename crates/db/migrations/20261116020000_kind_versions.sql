-- Versions of each server kind and its live and canary channels
-- (docs/SERVERS.md#versions, #20). A version is an image built from a tag on
-- the kind's repo. `hallow.server_kinds.spec.image` stays the live image, so
-- the planner reads one place; the channels say which version that is and
-- what takes a share of new instances.

CREATE TABLE hallow.kind_versions (
    kind         text        NOT NULL REFERENCES hallow.server_kinds (name) ON DELETE CASCADE,
    version      text        NOT NULL CHECK (version ~ '^[A-Za-z0-9][A-Za-z0-9._+-]{0,63}$'),
    image        text        NOT NULL CHECK (char_length(image) BETWEEN 1 AND 255),
    -- hallow.toml's `base` range, and the base image version it was built on.
    base         text        NOT NULL CHECK (char_length(base) BETWEEN 1 AND 128),
    base_version text        CHECK (char_length(base_version) BETWEEN 1 AND 64),
    git_commit   text        CHECK (char_length(git_commit) BETWEEN 1 AND 64),
    added_at     timestamptz NOT NULL DEFAULT now(),
    added_by     text        NOT NULL CHECK (char_length(added_by) BETWEEN 1 AND 64),
    PRIMARY KEY (kind, version)
);

CREATE INDEX kind_versions_added_idx ON hallow.kind_versions (kind, added_at DESC);

-- One row per kind once a channel was set. `live` is NULL while the kind's
-- image was set directly (PUT /v1/kinds) and isn't a recorded version.
CREATE TABLE hallow.kind_channels (
    kind           text        PRIMARY KEY REFERENCES hallow.server_kinds (name) ON DELETE CASCADE,
    live           text,
    -- What a rollback without a version goes back to.
    previous       text,
    canary         text,
    canary_percent smallint    CHECK (canary_percent BETWEEN 1 AND 100),
    canary_since   timestamptz,
    updated_at     timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (kind, live) REFERENCES hallow.kind_versions (kind, version) ON DELETE CASCADE,
    FOREIGN KEY (kind, previous) REFERENCES hallow.kind_versions (kind, version) ON DELETE CASCADE,
    FOREIGN KEY (kind, canary) REFERENCES hallow.kind_versions (kind, version) ON DELETE CASCADE,
    CHECK ((canary IS NULL) = (canary_percent IS NULL) AND (canary IS NULL) = (canary_since IS NULL))
);

-- Every version action, never updated or deleted, and kept after its kind
-- is removed.
CREATE TABLE hallow.kind_version_audit (
    id           bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind         text        NOT NULL,
    action       text        NOT NULL
                 CHECK (action IN ('add', 'canary', 'stop_canary', 'promote', 'rollback', 'register')),
    version      text,
    from_version text,
    image        text,
    percent      smallint    CHECK (percent BETWEEN 1 AND 100),
    by_name      text        NOT NULL CHECK (char_length(by_name) BETWEEN 1 AND 64),
    panel_user   text        CHECK (char_length(panel_user) BETWEEN 1 AND 64),
    at           timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX kind_version_audit_at_idx ON hallow.kind_version_audit (at DESC, id DESC);
CREATE INDEX kind_version_audit_kind_idx ON hallow.kind_version_audit (kind, at DESC, id DESC);
