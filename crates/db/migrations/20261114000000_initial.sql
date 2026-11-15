-- Hallow's kept data: accounts, ranks, sessions and punishments.
--
-- Everything lives in the `hallow` schema, so it never collides with the
-- Calagopus panel's tables in `public` when both share one database.
-- Enumerations are `text` with CHECK constraints rather than Postgres enum
-- types: adding a value is a one-line migration, and the panel's TypeScript
-- (Drizzle) side reads them as plain strings.

CREATE SCHEMA IF NOT EXISTS hallow;

-- ---------------------------------------------------------------------------
-- Accounts
-- ---------------------------------------------------------------------------

-- One row per player who has ever joined. The primary key is the player's
-- UUID as the proxy sees it (Mojang's for Java, Floodgate's for Bedrock).
CREATE TABLE hallow.accounts (
    uuid        uuid        PRIMARY KEY,
    -- Current username. Not unique: if a player renames and someone else
    -- takes the old name before the first player returns, both rows carry it
    -- until the first player logs in again.
    username    text        NOT NULL CHECK (char_length(username) BETWEEN 1 AND 32),
    edition     text        NOT NULL DEFAULT 'java' CHECK (edition IN ('java', 'bedrock')),
    first_seen  timestamptz NOT NULL DEFAULT now(),
    last_seen   timestamptz NOT NULL DEFAULT now(),
    -- Personal data: see the privacy note in crates/db/README.md.
    last_ip     inet,
    CHECK (last_seen >= first_seen)
);

-- Find an account by its current name, ignoring case.
CREATE INDEX accounts_username_lower_idx ON hallow.accounts (lower(username));

-- Every username an account has used, including the current one.
CREATE TABLE hallow.account_usernames (
    account_uuid uuid        NOT NULL REFERENCES hallow.accounts (uuid) ON DELETE CASCADE,
    username     text        NOT NULL CHECK (char_length(username) BETWEEN 1 AND 32),
    first_used   timestamptz NOT NULL DEFAULT now(),
    last_used    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (account_uuid, username)
);

-- Find who used a name in the past, ignoring case.
CREATE INDEX account_usernames_username_lower_idx ON hallow.account_usernames (lower(username));

-- ---------------------------------------------------------------------------
-- Ranks
-- ---------------------------------------------------------------------------

-- Permission nodes use Vine's format (upstream/Vine/src/permissions.rs):
-- dot-separated lowercase segments (`hallow.staff.find`), `*` for
-- everything, a trailing `.*` for a subtree (`hallow.staff.*`), and a
-- leading `-` to deny (`-hallow.staff.ban`). Vine matches case-insensitively;
-- storing nodes lowercase keeps that true in SQL too.
CREATE FUNCTION hallow.permission_nodes_valid(nodes text[]) RETURNS boolean
    LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
    RETURN (
        SELECT coalesce(bool_and(n ~ '^-?(\*|[a-z0-9_-]+(\.[a-z0-9_-]+)*(\.\*)?)$'), true)
        FROM unnest(nodes) AS n
    );

CREATE TABLE hallow.ranks (
    name           text        PRIMARY KEY CHECK (name ~ '^[a-z0-9_-]{1,32}$'),
    -- Shown before the player's name, with legacy `&` colour codes.
    display_prefix text        NOT NULL DEFAULT '',
    -- Higher weight wins: it orders ranks for display and for picking the
    -- player's primary rank.
    weight         integer     NOT NULL DEFAULT 0,
    permissions    text[]      NOT NULL DEFAULT '{}'
                               CHECK (hallow.permission_nodes_valid(permissions)),
    -- The rank every account holds without a grant (Vine's `default_group`).
    is_default     boolean     NOT NULL DEFAULT false,
    created_at     timestamptz NOT NULL DEFAULT now()
);

-- At most one default rank.
CREATE UNIQUE INDEX ranks_single_default_idx ON hallow.ranks (is_default) WHERE is_default;

INSERT INTO hallow.ranks (name, weight, is_default) VALUES ('default', 0, true);

-- Ranks an account holds. A NULL expiry is permanent.
CREATE TABLE hallow.account_ranks (
    account_uuid uuid        NOT NULL REFERENCES hallow.accounts (uuid) ON DELETE CASCADE,
    rank_name    text        NOT NULL REFERENCES hallow.ranks (name) ON UPDATE CASCADE ON DELETE CASCADE,
    granted_at   timestamptz NOT NULL DEFAULT now(),
    -- The staff member who granted it; NULL for console, system or a store
    -- purchase.
    granted_by   uuid        REFERENCES hallow.accounts (uuid) ON DELETE SET NULL,
    expires_at   timestamptz,
    PRIMARY KEY (account_uuid, rank_name),
    CHECK (expires_at IS NULL OR expires_at > granted_at)
);

-- ---------------------------------------------------------------------------
-- Sessions
-- ---------------------------------------------------------------------------

-- One row per connection to the network, from login to logout.
CREATE TABLE hallow.sessions (
    id               uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    account_uuid     uuid        NOT NULL REFERENCES hallow.accounts (uuid) ON DELETE CASCADE,
    -- The proxy's id, as it registered with the control plane.
    proxy            text        NOT NULL,
    -- The server the player was on most recently.
    last_server      text,
    -- Java protocol version; NULL for Bedrock players through Geyser.
    protocol_version integer,
    ip               inet,
    login_at         timestamptz NOT NULL DEFAULT now(),
    -- NULL while the session is open.
    logout_at        timestamptz,
    CHECK (logout_at IS NULL OR logout_at >= login_at)
);

-- A player's sessions, newest first.
CREATE INDEX sessions_account_idx ON hallow.sessions (account_uuid, login_at DESC);
-- Open sessions on a proxy, to close them all when the proxy goes away.
CREATE INDEX sessions_open_by_proxy_idx ON hallow.sessions (proxy) WHERE logout_at IS NULL;
-- Accounts that connected from an address (alt lookup in the panel).
CREATE INDEX sessions_ip_idx ON hallow.sessions (ip);

-- ---------------------------------------------------------------------------
-- Punishments
-- ---------------------------------------------------------------------------

-- Who issued or revoked a punishment: a staff account, or the console,
-- automation (`system`) or the anticheat verdict service.
CREATE TABLE hallow.punishments (
    id              bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind            text        NOT NULL CHECK (kind IN ('ban', 'mute', 'kick', 'warn')),
    -- Punishment history is kept: an account that received, issued or
    -- revoked punishments can't be deleted until those rows are.
    target_uuid     uuid        NOT NULL REFERENCES hallow.accounts (uuid) ON DELETE RESTRICT,
    issuer_kind     text        NOT NULL CHECK (issuer_kind IN ('account', 'console', 'system', 'anticheat')),
    issuer_uuid     uuid        REFERENCES hallow.accounts (uuid) ON DELETE RESTRICT,
    reason          text        NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    -- NULL is permanent. Kicks take effect once and never carry one.
    expires_at      timestamptz,
    revoked_at      timestamptz,
    revoked_by_kind text        CHECK (revoked_by_kind IN ('account', 'console', 'system', 'anticheat')),
    revoked_by_uuid uuid        REFERENCES hallow.accounts (uuid) ON DELETE RESTRICT,
    revoked_reason  text,
    -- An account issuer names the account; the others never do.
    CHECK ((issuer_kind = 'account') = (issuer_uuid IS NOT NULL)),
    CHECK (revoked_by_kind IS NULL OR (revoked_by_kind = 'account') = (revoked_by_uuid IS NOT NULL)),
    CHECK (expires_at IS NULL OR expires_at > created_at),
    CHECK (kind <> 'kick' OR expires_at IS NULL),
    -- Revocation fields are set together.
    CHECK ((revoked_at IS NULL) = (revoked_by_kind IS NULL)),
    CHECK (revoked_at IS NOT NULL OR (revoked_by_uuid IS NULL AND revoked_reason IS NULL))
);

-- "Is this account banned now" and "this account's active mute": unrevoked
-- bans and mutes per account. Expiry can't go in the predicate (now() isn't
-- immutable), so it is the last key column and filtered at query time.
CREATE INDEX punishments_active_idx ON hallow.punishments (target_uuid, kind, expires_at)
    WHERE revoked_at IS NULL AND kind IN ('ban', 'mute');
-- A player's punishment history, newest first.
CREATE INDEX punishments_target_history_idx ON hallow.punishments (target_uuid, created_at DESC);
-- What a staff member has issued, for the panel's audit view.
CREATE INDEX punishments_issuer_idx ON hallow.punishments (issuer_uuid, created_at DESC)
    WHERE issuer_uuid IS NOT NULL;
