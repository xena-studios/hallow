-- Anticheat signals: one row per player per measured window, from a proxy
-- (packet timing) or a server (movement, combat). Kept for a retention period
-- (14 days by default, deleted by the control plane) to tune checks and
-- review players before any check acts. Metrics are free-form JSON numbers.
CREATE TABLE hallow.signal_samples (
    id           bigserial   PRIMARY KEY,
    account_uuid uuid        NOT NULL REFERENCES hallow.accounts (uuid) ON DELETE CASCADE,
    source       text        NOT NULL CHECK (char_length(source) BETWEEN 1 AND 64),
    at           timestamptz NOT NULL,
    window_ms    integer     NOT NULL CHECK (window_ms > 0),
    protocol     integer,
    metrics      jsonb       NOT NULL DEFAULT '{}'
);

-- A player's history, newest first.
CREATE INDEX signal_samples_account_at_idx ON hallow.signal_samples (account_uuid, at DESC);
-- Retention deletes by age.
CREATE INDEX signal_samples_at_idx ON hallow.signal_samples (at);
