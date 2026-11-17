//! Player stats: running totals per account, server kind and stat, the same
//! per UTC day for windowed leaderboards, and finished games.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{FromRow, PgExecutor, PgPool, types::Json};
use uuid::Uuid;

/// One increase to add to a total.
#[derive(Clone, Debug)]
pub struct StatIncrease<'a> {
    pub account: Uuid,
    pub kind: &'a str,
    pub stat: &'a str,
    pub by: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct StatTotal {
    pub kind: String,
    pub stat: String,
    pub value: i64,
}

/// Adds the increases to their totals in one statement, creating totals
/// that don't exist yet, and to today's (UTC) totals for windowed
/// leaderboards. Several increases to the same total in one call are summed
/// first. Increases for accounts that never joined are skipped. Returns how
/// many totals changed.
pub async fn add_stats(pool: &PgPool, increases: &[StatIncrease<'_>]) -> Result<u64, sqlx::Error> {
    add_stats_with(pool, increases).await
}

async fn add_stats_with<'e>(
    executor: impl PgExecutor<'e>,
    increases: &[StatIncrease<'_>],
) -> Result<u64, sqlx::Error> {
    if increases.is_empty() {
        return Ok(0);
    }
    let accounts: Vec<Uuid> = increases.iter().map(|i| i.account).collect();
    let kinds: Vec<&str> = increases.iter().map(|i| i.kind).collect();
    let stats: Vec<&str> = increases.iter().map(|i| i.stat).collect();
    let bys: Vec<i64> = increases.iter().map(|i| i.by).collect();
    let changed: i64 = sqlx::query_scalar(
        "WITH s AS (
             SELECT s.account, s.kind, s.stat, sum(s.by)::bigint AS by
             FROM unnest($1::uuid[], $2::text[], $3::text[], $4::bigint[]) AS s(account, kind, stat, by)
             WHERE EXISTS (SELECT 1 FROM hallow.accounts a WHERE a.uuid = s.account)
             GROUP BY s.account, s.kind, s.stat
         ), daily AS (
             INSERT INTO hallow.player_stats_daily AS d (account_uuid, kind, stat, day, value)
             SELECT account, kind, stat, (now() AT TIME ZONE 'UTC')::date, by FROM s
             ON CONFLICT (account_uuid, kind, stat, day)
             DO UPDATE SET value = d.value + EXCLUDED.value
         ), totals AS (
             INSERT INTO hallow.player_stats AS t (account_uuid, kind, stat, value)
             SELECT account, kind, stat, by FROM s
             ON CONFLICT (account_uuid, kind, stat)
             DO UPDATE SET value = t.value + EXCLUDED.value, updated_at = now()
             RETURNING 1
         )
         SELECT count(*) FROM totals",
    )
    .bind(&accounts)
    .bind(&kinds)
    .bind(&stats)
    .bind(&bys)
    .fetch_one(executor)
    .await?;
    Ok(u64::try_from(changed).unwrap_or(0))
}

/// Every total of an account, by kind then stat.
pub async fn stats_of(pool: &PgPool, account: Uuid) -> Result<Vec<StatTotal>, sqlx::Error> {
    sqlx::query_as(
        "SELECT kind, stat, value FROM hallow.player_stats
         WHERE account_uuid = $1 ORDER BY kind, stat",
    )
    .bind(account)
    .fetch_all(pool)
    .await
}

/// A finished game to store.
#[derive(Clone, Debug)]
pub struct NewGame<'a> {
    pub id: Uuid,
    pub kind: &'a str,
    pub server: &'a str,
    pub map: Option<&'a str>,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub players: Vec<NewGamePlayer<'a>>,
}

#[derive(Clone, Debug)]
pub struct NewGamePlayer<'a> {
    pub account: Uuid,
    pub won: bool,
    pub placement: Option<i32>,
    pub team: Option<&'a str>,
    pub stats: &'a BTreeMap<String, i64>,
}

/// Stores a game and adds it to its players' totals: `games` +1, `wins` +1
/// for a winner, and each of their stats, all in one transaction. Players
/// who never joined are skipped. Returns `false` if a game with this id was
/// already stored, which then changes nothing.
pub async fn record_game(pool: &PgPool, game: &NewGame<'_>) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO hallow.games (id, kind, server, map, started_at, ended_at, players)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(game.id)
    .bind(game.kind)
    .bind(game.server)
    .bind(game.map)
    .bind(game.started_at)
    .bind(game.ended_at)
    .bind(i32::try_from(game.players.len()).unwrap_or(i32::MAX))
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    if !inserted {
        return Ok(false);
    }
    let accounts: Vec<Uuid> = game.players.iter().map(|p| p.account).collect();
    let won: Vec<bool> = game.players.iter().map(|p| p.won).collect();
    let placements: Vec<Option<i32>> = game.players.iter().map(|p| p.placement).collect();
    let teams: Vec<Option<&str>> = game.players.iter().map(|p| p.team).collect();
    let stats: Vec<Json<&BTreeMap<String, i64>>> =
        game.players.iter().map(|p| Json(p.stats)).collect();
    sqlx::query(
        "INSERT INTO hallow.game_players
            (game_id, account_uuid, won, placement, team, stats, ended_at)
         SELECT $1, p.account, p.won, p.placement, p.team, p.stats, $7
         FROM unnest($2::uuid[], $3::bool[], $4::int[], $5::text[], $6::jsonb[])
            AS p(account, won, placement, team, stats)
         WHERE EXISTS (SELECT 1 FROM hallow.accounts a WHERE a.uuid = p.account)
         ON CONFLICT (game_id, account_uuid) DO NOTHING",
    )
    .bind(game.id)
    .bind(&accounts)
    .bind(&won)
    .bind(&placements)
    .bind(&teams)
    .bind(&stats)
    .bind(game.ended_at)
    .execute(&mut *tx)
    .await?;
    let mut increases = Vec::new();
    for player in &game.players {
        let mut add = |stat: &'static str| {
            increases.push(StatIncrease {
                account: player.account,
                kind: game.kind,
                stat,
                by: 1,
            });
        };
        add(hallow_proto::stats::GAMES);
        if player.won {
            add(hallow_proto::stats::WINS);
        }
    }
    for player in &game.players {
        for (stat, by) in player.stats {
            if *by > 0 {
                increases.push(StatIncrease {
                    account: player.account,
                    kind: game.kind,
                    stat,
                    by: *by,
                });
            }
        }
    }
    add_stats_with(&mut *tx, &increases).await?;
    tx.commit().await?;
    Ok(true)
}

/// A game from one player's side, for their profile.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct PlayedGame {
    pub id: Uuid,
    pub kind: String,
    pub map: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub players: i32,
    pub won: bool,
    pub placement: Option<i32>,
    pub team: Option<String>,
    pub stats: Json<BTreeMap<String, i64>>,
}

/// The account's last `limit` games, newest first.
pub async fn recent_games(
    pool: &PgPool,
    account: Uuid,
    limit: u32,
) -> Result<Vec<PlayedGame>, sqlx::Error> {
    sqlx::query_as(
        "SELECT g.id, g.kind, g.map, g.started_at, g.ended_at, g.players,
                p.won, p.placement, p.team, p.stats
         FROM hallow.game_players p
         JOIN hallow.games g ON g.id = p.game_id
         WHERE p.account_uuid = $1
         ORDER BY p.ended_at DESC
         LIMIT $2",
    )
    .bind(account)
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await
}

/// The highest-weight rank an account holds now (the default rank if
/// nothing else), joined onto a query of `hallow.accounts a`.
macro_rules! primary_rank_join {
    () => {
        " LEFT JOIN LATERAL (
            SELECT r.name AS rank_name, r.display_prefix AS prefix, r.is_default
            FROM hallow.ranks r
            WHERE r.is_default
               OR EXISTS (
                   SELECT 1 FROM hallow.account_ranks ar
                   WHERE ar.account_uuid = a.uuid
                     AND ar.rank_name = r.name
                     AND (ar.expires_at IS NULL OR ar.expires_at > now())
               )
            ORDER BY r.weight DESC, r.name
            LIMIT 1
        ) pr ON true "
    };
}

/// An account with its primary rank.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct RankedAccount {
    pub uuid: Uuid,
    pub username: String,
    /// `None` only when no rank is the default and the account holds none.
    pub rank_name: Option<String>,
    pub prefix: Option<String>,
    pub is_default: Option<bool>,
}

/// The accounts among `accounts` that exist, each with its primary rank.
pub async fn primary_ranks(
    pool: &PgPool,
    accounts: &[Uuid],
) -> Result<Vec<RankedAccount>, sqlx::Error> {
    sqlx::query_as(concat!(
        "SELECT a.uuid, a.username, pr.rank_name, pr.prefix, pr.is_default
         FROM hallow.accounts a",
        primary_rank_join!(),
        "WHERE a.uuid = ANY($1)"
    ))
    .bind(accounts)
    .fetch_all(pool)
    .await
}

/// One line of a leaderboard.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct BoardRow {
    pub uuid: Uuid,
    pub username: String,
    pub value: i64,
    pub rank_name: Option<String>,
    pub prefix: Option<String>,
    pub is_default: Option<bool>,
}

/// The top `limit` totals of `stat` in `kind`: all time, or summed over the
/// UTC days from `since` on. Highest first, ties by name. Zero totals are
/// left out.
pub async fn leaderboard(
    pool: &PgPool,
    kind: &str,
    stat: &str,
    since: Option<NaiveDate>,
    limit: u32,
) -> Result<Vec<BoardRow>, sqlx::Error> {
    let totals = match since {
        None => {
            "SELECT account_uuid, value FROM hallow.player_stats
             WHERE kind = $1 AND stat = $2 AND value > 0
             ORDER BY value DESC LIMIT $3 + 50"
        }
        Some(_) => {
            "SELECT account_uuid, sum(value)::bigint AS value FROM hallow.player_stats_daily
             WHERE kind = $1 AND stat = $2 AND day >= $4
             GROUP BY account_uuid HAVING sum(value) > 0
             ORDER BY value DESC LIMIT $3 + 50"
        }
    };
    // A few extra rows above the limit so ties at the cut are ordered by
    // name, not by whichever came off the index first.
    let query = format!(
        "SELECT a.uuid, a.username, t.value, pr.rank_name, pr.prefix, pr.is_default
         FROM ({totals}) t
         JOIN hallow.accounts a ON a.uuid = t.account_uuid {}
         ORDER BY t.value DESC, lower(a.username), a.uuid
         LIMIT $3",
        primary_rank_join!()
    );
    sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(kind)
        .bind(stat)
        .bind(i64::from(limit))
        .bind(since)
        .fetch_all(pool)
        .await
}

/// A kind with stats, for the list of leaderboards.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct StatKind {
    pub kind: String,
    pub stats: Vec<String>,
    pub players: i64,
}

/// Every kind with a positive total, its stats by name, and how many
/// players have any.
pub async fn stat_kinds(pool: &PgPool) -> Result<Vec<StatKind>, sqlx::Error> {
    sqlx::query_as(
        "SELECT kind, array_agg(DISTINCT stat ORDER BY stat) AS stats,
                count(DISTINCT account_uuid) AS players
         FROM hallow.player_stats WHERE value > 0
         GROUP BY kind ORDER BY kind",
    )
    .fetch_all(pool)
    .await
}

/// How many accounts and stored games there are.
pub async fn network_totals(pool: &PgPool) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM hallow.accounts), (SELECT count(*) FROM hallow.games)",
    )
    .fetch_one(pool)
    .await
}

/// Up to `limit` accounts whose name starts with `prefix`, ignoring case:
/// an exact match first, then the most recently seen.
pub async fn search_accounts(
    pool: &PgPool,
    prefix: &str,
    limit: u32,
) -> Result<Vec<(Uuid, String)>, sqlx::Error> {
    let pattern = format!(
        "{}%",
        prefix
            .to_lowercase()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    sqlx::query_as(
        "SELECT uuid, username FROM hallow.accounts
         WHERE lower(username) LIKE $1
         ORDER BY lower(username) = lower($2) DESC, last_seen DESC
         LIMIT $3",
    )
    .bind(pattern)
    .bind(prefix)
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await
}

/// Deletes daily totals before `day`; returns how many.
pub async fn delete_daily_stats_before(pool: &PgPool, day: NaiveDate) -> Result<u64, sqlx::Error> {
    Ok(
        sqlx::query("DELETE FROM hallow.player_stats_daily WHERE day < $1")
            .bind(day)
            .execute(pool)
            .await?
            .rows_affected(),
    )
}
