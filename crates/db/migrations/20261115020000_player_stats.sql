-- Player stats: a running total per account, server kind and stat name, such
-- as (player, 'skywars', 'kills'). Servers send increases in batches and the
-- control plane adds them in (INSERT ... ON CONFLICT DO UPDATE value + n).
-- Stat names are free-form so a game can count something new without a
-- schema change.
CREATE TABLE hallow.player_stats (
    account_uuid uuid        NOT NULL REFERENCES hallow.accounts (uuid) ON DELETE CASCADE,
    kind         text        NOT NULL CHECK (char_length(kind) BETWEEN 1 AND 64),
    stat         text        NOT NULL CHECK (char_length(stat) BETWEEN 1 AND 64),
    value        bigint      NOT NULL DEFAULT 0,
    updated_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (account_uuid, kind, stat)
);

-- Leaderboards: the top values of one stat in one kind.
CREATE INDEX player_stats_kind_stat_value_idx ON hallow.player_stats (kind, stat, value DESC);
