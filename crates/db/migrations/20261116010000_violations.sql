-- Anticheat verdicts (docs/ANTICHEAT.md, phase 2). Checks on proxies and
-- servers report violations with evidence; the verdict service keeps a
-- decaying score per player and check, and records a verdict when the score
-- reaches a level that calls for an action. In observe-only mode verdicts
-- are recorded, never carried out (enforced = false). Violations follow the
-- signal retention period; scores and verdicts are kept.
CREATE TABLE hallow.anticheat_violations (
    id           bigserial   PRIMARY KEY,
    account_uuid uuid        NOT NULL REFERENCES hallow.accounts (uuid) ON DELETE CASCADE,
    source       text        NOT NULL CHECK (char_length(source) BETWEEN 1 AND 64),
    check_name   text        NOT NULL CHECK (char_length(check_name) BETWEEN 1 AND 32),
    at           timestamptz NOT NULL,
    severity     double precision NOT NULL CHECK (severity >= 0),
    protocol     integer,
    summary      text        NOT NULL DEFAULT '' CHECK (char_length(summary) <= 512),
    evidence     jsonb       NOT NULL DEFAULT '{}'
);

-- A player's history, newest first.
CREATE INDEX anticheat_violations_account_at_idx ON hallow.anticheat_violations (account_uuid, at DESC);
-- Retention deletes by age.
CREATE INDEX anticheat_violations_at_idx ON hallow.anticheat_violations (at);

-- The decaying score per player and check, as of updated_at, and the action
-- last recorded for it (so a verdict is recorded once per step up).
CREATE TABLE hallow.anticheat_scores (
    account_uuid uuid        NOT NULL REFERENCES hallow.accounts (uuid) ON DELETE CASCADE,
    check_name   text        NOT NULL CHECK (char_length(check_name) BETWEEN 1 AND 32),
    level        double precision NOT NULL CHECK (level >= 0),
    updated_at   timestamptz NOT NULL,
    action       text        NOT NULL DEFAULT 'none'
                 CHECK (action IN ('none', 'review', 'setback', 'kick', 'ban')),
    PRIMARY KEY (account_uuid, check_name)
);

CREATE TABLE hallow.anticheat_verdicts (
    id           bigserial   PRIMARY KEY,
    account_uuid uuid        NOT NULL REFERENCES hallow.accounts (uuid) ON DELETE CASCADE,
    check_name   text        NOT NULL CHECK (char_length(check_name) BETWEEN 1 AND 32),
    action       text        NOT NULL CHECK (action IN ('review', 'setback', 'kick', 'ban')),
    level        double precision NOT NULL,
    at           timestamptz NOT NULL DEFAULT now(),
    enforced     boolean     NOT NULL DEFAULT false,
    reason       text        NOT NULL DEFAULT '' CHECK (char_length(reason) <= 512)
);

CREATE INDEX anticheat_verdicts_account_at_idx ON hallow.anticheat_verdicts (account_uuid, at DESC);
-- The panel's review queue: unenforced verdicts, newest first.
CREATE INDEX anticheat_verdicts_at_idx ON hallow.anticheat_verdicts (at DESC);
