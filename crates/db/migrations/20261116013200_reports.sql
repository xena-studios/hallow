-- Player reports (#32): `/report <player> <reason>` in game, worked through
-- by staff in the panel. A report is `open` until a staff member claims it,
-- and ends `resolved` (optionally with the punishment it led to) or
-- `dismissed`, with an optional note.
--
-- Who claimed or closed a report is an actor, stored like a punishment's
-- issuer: `account` (in-game staff, by UUID), `staff` (panel staff, by panel
-- username and user id), `console`, `system` or `anticheat`.
CREATE TABLE hallow.reports (
    id                    bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    reporter_uuid         uuid        NOT NULL REFERENCES hallow.accounts (uuid) ON DELETE RESTRICT,
    target_uuid           uuid        NOT NULL REFERENCES hallow.accounts (uuid) ON DELETE RESTRICT,
    reason                text        NOT NULL CHECK (char_length(reason) BETWEEN 1 AND 256),
    -- The server the reporter was on, if any.
    server                text        CHECK (char_length(server) BETWEEN 1 AND 64),
    status                text        NOT NULL DEFAULT 'open'
                                      CHECK (status IN ('open', 'claimed', 'resolved', 'dismissed')),
    created_at            timestamptz NOT NULL DEFAULT now(),
    claimed_at            timestamptz,
    claimed_by_kind       text        CHECK (claimed_by_kind IN ('account', 'console', 'system', 'anticheat', 'staff')),
    claimed_by_uuid       uuid        REFERENCES hallow.accounts (uuid) ON DELETE RESTRICT,
    claimed_by_name       text        CHECK (char_length(claimed_by_name) BETWEEN 1 AND 64),
    claimed_by_panel_user text        CHECK (char_length(claimed_by_panel_user) BETWEEN 1 AND 64),
    closed_at             timestamptz,
    closed_by_kind        text        CHECK (closed_by_kind IN ('account', 'console', 'system', 'anticheat', 'staff')),
    closed_by_uuid        uuid        REFERENCES hallow.accounts (uuid) ON DELETE RESTRICT,
    closed_by_name        text        CHECK (char_length(closed_by_name) BETWEEN 1 AND 64),
    closed_by_panel_user  text        CHECK (char_length(closed_by_panel_user) BETWEEN 1 AND 64),
    -- Why it was resolved or dismissed.
    note                  text        CHECK (char_length(note) BETWEEN 1 AND 1024),
    -- The punishment a resolved report led to.
    punishment_id         bigint      REFERENCES hallow.punishments (id) ON DELETE SET NULL,
    CHECK (reporter_uuid <> target_uuid),
    -- The claim fields are set together, and only while claimed or after.
    CHECK ((claimed_at IS NULL) = (claimed_by_kind IS NULL)),
    CHECK (status <> 'claimed' OR claimed_at IS NOT NULL),
    CHECK (status <> 'open' OR claimed_at IS NULL),
    -- A closed report says when and by whom; only a closed one does.
    CHECK ((status IN ('resolved', 'dismissed')) = (closed_at IS NOT NULL)),
    CHECK ((closed_at IS NULL) = (closed_by_kind IS NULL)),
    CHECK (closed_at IS NOT NULL OR note IS NULL),
    CHECK (status = 'resolved' OR punishment_id IS NULL),
    -- Actors: an account names its UUID, a staff member a name; no others do.
    CHECK (claimed_by_kind IS NULL OR (claimed_by_kind = 'account') = (claimed_by_uuid IS NOT NULL)),
    CHECK (coalesce(claimed_by_kind = 'staff', false) = (claimed_by_name IS NOT NULL)
           AND (coalesce(claimed_by_kind = 'staff', false) OR claimed_by_panel_user IS NULL)),
    CHECK (closed_by_kind IS NULL OR (closed_by_kind = 'account') = (closed_by_uuid IS NOT NULL)),
    CHECK (coalesce(closed_by_kind = 'staff', false) = (closed_by_name IS NOT NULL)
           AND (coalesce(closed_by_kind = 'staff', false) OR closed_by_panel_user IS NULL))
);

-- The panel's queue: reports by status, newest first.
CREATE INDEX reports_status_created_idx ON hallow.reports (status, created_at DESC, id DESC);
-- Every report, newest first.
CREATE INDEX reports_created_idx ON hallow.reports (created_at DESC, id DESC);
-- A player's reports, and how many they have (repeat offenders).
CREATE INDEX reports_target_idx ON hallow.reports (target_uuid, created_at DESC);
-- The reporter's rate limit: their reports in the last minutes.
CREATE INDEX reports_reporter_idx ON hallow.reports (reporter_uuid, created_at DESC);
-- One report at a time per reporter and target while staff look at it.
CREATE UNIQUE INDEX reports_one_active_idx ON hallow.reports (reporter_uuid, target_uuid)
    WHERE status IN ('open', 'claimed');
