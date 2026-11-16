-- Node bootstrap (docs/FLEET.md): one-time join tokens, and the nodes that
-- joined with them.
--
-- A token is stored only as its SHA-256. A join claims it for a short lease
-- (claimed_until) while the control plane sets the node up in the panel, then
-- marks it used in the same transaction that records the node. A join that
-- fails gives the claim back, so the machine can retry; a crashed one lets
-- the lease run out.
CREATE TABLE hallow.node_join_tokens (
    id            uuid        PRIMARY KEY,
    token_hash    bytea       NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    role          text        NOT NULL CHECK (role IN ('base', 'burst')),
    region        text        NOT NULL CHECK (char_length(region) BETWEEN 1 AND 64),
    name          text        CHECK (char_length(name) BETWEEN 1 AND 64),
    labels        jsonb       NOT NULL DEFAULT '{}',
    created_at    timestamptz NOT NULL DEFAULT now(),
    expires_at    timestamptz NOT NULL,
    claimed_until timestamptz,
    used_at       timestamptz
);

-- Expired tokens are deleted by age.
CREATE INDEX node_join_tokens_expires_at_idx ON hallow.node_join_tokens (expires_at);

-- Every node that joined. Rows outlive their token.
CREATE TABLE hallow.nodes (
    name                 text        PRIMARY KEY CHECK (char_length(name) BETWEEN 1 AND 64),
    panel_uuid           uuid        NOT NULL UNIQUE,
    role                 text        NOT NULL CHECK (role IN ('base', 'burst')),
    region               text        NOT NULL,
    labels               jsonb       NOT NULL DEFAULT '{}',
    private_ip           inet        NOT NULL,
    public_ip            inet,
    wireguard_public_key text        UNIQUE,
    join_token_id        uuid        REFERENCES hallow.node_join_tokens (id) ON DELETE SET NULL,
    joined_at            timestamptz NOT NULL DEFAULT now()
);
