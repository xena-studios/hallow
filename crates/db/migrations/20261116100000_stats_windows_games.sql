-- Stats per day, for weekly and monthly leaderboards. Every increase added
-- to hallow.player_stats is added here too, under the UTC day it arrived.
-- Old days are deleted after a while (the control plane keeps 62).
CREATE TABLE hallow.player_stats_daily (
    account_uuid uuid   NOT NULL REFERENCES hallow.accounts (uuid) ON DELETE CASCADE,
    kind         text   NOT NULL CHECK (char_length(kind) BETWEEN 1 AND 64),
    stat         text   NOT NULL CHECK (char_length(stat) BETWEEN 1 AND 64),
    day          date   NOT NULL,
    value        bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (account_uuid, kind, stat, day)
);

-- Window leaderboards: one kind and stat since a day.
CREATE INDEX player_stats_daily_board_idx ON hallow.player_stats_daily (kind, stat, day);

-- Finished games, as their servers reported them. The id comes from the
-- server, so a retried report is stored once.
CREATE TABLE hallow.games (
    id         uuid        PRIMARY KEY,
    kind       text        NOT NULL CHECK (char_length(kind) BETWEEN 1 AND 64),
    server     text        NOT NULL,
    map        text,
    started_at timestamptz NOT NULL,
    ended_at   timestamptz NOT NULL,
    players    integer     NOT NULL,
    stored_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX games_kind_ended_idx ON hallow.games (kind, ended_at DESC);

-- Each player's part in a game.
CREATE TABLE hallow.game_players (
    game_id      uuid    NOT NULL REFERENCES hallow.games (id) ON DELETE CASCADE,
    account_uuid uuid    NOT NULL REFERENCES hallow.accounts (uuid) ON DELETE CASCADE,
    won          boolean NOT NULL DEFAULT false,
    placement    integer,
    team         text,
    stats        jsonb   NOT NULL DEFAULT '{}',
    -- The game's end, copied here so a player's recent games come off the
    -- index in order.
    ended_at     timestamptz NOT NULL,
    PRIMARY KEY (game_id, account_uuid)
);

-- A player's recent games.
CREATE INDEX game_players_account_idx ON hallow.game_players (account_uuid, ended_at DESC);

-- Search matches names by prefix, ignoring case. text_pattern_ops serves
-- both that and the exact lookups the old index served.
DROP INDEX hallow.accounts_username_lower_idx;
CREATE INDEX accounts_username_lower_idx ON hallow.accounts (lower(username) text_pattern_ops);
