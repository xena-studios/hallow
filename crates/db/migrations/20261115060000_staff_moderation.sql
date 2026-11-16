-- Moderation from the panel (#25): punishments issued and lifted by panel
-- staff, who have no Minecraft account, and reason templates.
--
-- A panel staff member is the new actor kind `staff`. They are named by
-- their panel username (and the panel user's id, for the audit trail)
-- instead of an account UUID. Existing rows keep their kinds and stay valid.

ALTER TABLE hallow.punishments
    DROP CONSTRAINT punishments_issuer_kind_check,
    DROP CONSTRAINT punishments_revoked_by_kind_check,
    ADD COLUMN issuer_name           text CHECK (char_length(issuer_name) BETWEEN 1 AND 64),
    ADD COLUMN issuer_panel_user     text CHECK (char_length(issuer_panel_user) BETWEEN 1 AND 64),
    ADD COLUMN revoked_by_name       text CHECK (char_length(revoked_by_name) BETWEEN 1 AND 64),
    ADD COLUMN revoked_by_panel_user text CHECK (char_length(revoked_by_panel_user) BETWEEN 1 AND 64),
    ADD CONSTRAINT punishments_issuer_kind_check
        CHECK (issuer_kind IN ('account', 'console', 'system', 'anticheat', 'staff')),
    ADD CONSTRAINT punishments_revoked_by_kind_check
        CHECK (revoked_by_kind IN ('account', 'console', 'system', 'anticheat', 'staff')),
    -- A staff actor carries a name; the others never do.
    ADD CONSTRAINT punishments_issuer_staff_check
        CHECK ((issuer_kind = 'staff') = (issuer_name IS NOT NULL)
               AND (issuer_kind = 'staff' OR issuer_panel_user IS NULL)),
    ADD CONSTRAINT punishments_revoked_by_staff_check
        CHECK (coalesce(revoked_by_kind = 'staff', false) = (revoked_by_name IS NOT NULL)
               AND (coalesce(revoked_by_kind = 'staff', false) OR revoked_by_panel_user IS NULL));

-- The panel's network-wide list, newest first.
CREATE INDEX punishments_created_idx ON hallow.punishments (created_at DESC, id DESC);

-- Reasons staff pick from when punishing, in game or in the panel.
CREATE TABLE hallow.punishment_templates (
    id               bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name             text        NOT NULL CHECK (char_length(name) BETWEEN 1 AND 64),
    kind             text        NOT NULL CHECK (kind IN ('ban', 'mute', 'kick', 'warn')),
    -- NULL is permanent. Only bans and mutes last.
    duration_seconds bigint      CHECK (duration_seconds > 0),
    reason           text        NOT NULL CHECK (char_length(reason) BETWEEN 1 AND 256),
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    CHECK (kind IN ('ban', 'mute') OR duration_seconds IS NULL)
);

CREATE UNIQUE INDEX punishment_templates_name_idx ON hallow.punishment_templates (lower(name));

INSERT INTO hallow.punishment_templates (name, kind, duration_seconds, reason) VALUES
    ('Cheating', 'ban', 30 * 86400, 'Cheating: unfair client modifications'),
    ('Spam', 'mute', 3600, 'Spamming chat'),
    ('Harassment', 'mute', 7 * 86400, 'Harassing other players'),
    ('Advertising', 'ban', NULL, 'Advertising other servers');
