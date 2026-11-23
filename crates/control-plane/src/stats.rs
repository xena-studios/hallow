//! Player stats: servers send increases in batches, added to running totals
//! in PostgreSQL per account, server kind and stat (all time and per UTC
//! day). Games report each finished game, which counts `games` and `wins`
//! and is kept for recent games. Leaderboards rank one stat of one kind, all
//! time or this week or month; display lines give a player's rank prefix
//! for chat and the tab list.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::StatusCode,
    routing::{get, post},
};
use chrono::{DateTime, Datelike, NaiveDate, TimeDelta, Utc};
use hallow_db::{PgPool, StatIncrease};
use hallow_proto::stats::{
    DisplayRequest, GameResult, LEADERBOARD_DEFAULT, LEADERBOARD_MAX, Leaderboard,
    LeaderboardEntry, MAX_BATCH_PLAYERS, MAX_DISPLAY_PLAYERS, MAX_GAME_PLAYERS,
    MAX_STATS_PER_PLAYER, PlayerDisplay, PlayerStats, StatsBatch, Window, valid_stat_name,
};
use hallow_proto::{ErrorCode, routes};
use uuid::Uuid;

use crate::{
    AppState,
    accounts::{DbResult, resolve},
    auth::Caller,
    error::Failure,
    store::Store,
};

/// How many days of per-day totals are kept: enough for this month and
/// the one before.
pub const DAILY_RETENTION_DAYS: i64 = 62;

/// The longest a game may say it lasted, and how far in the future its
/// end may be (clock skew).
const MAX_GAME: Duration = Duration::from_secs(24 * 3600);
const MAX_SKEW: Duration = Duration::from_secs(300);

/// Why a batch was refused.
pub fn check(batch: &StatsBatch) -> Result<(), String> {
    if batch.players.len() > MAX_BATCH_PLAYERS {
        return Err(format!("at most {MAX_BATCH_PLAYERS} players per batch"));
    }
    for player in &batch.players {
        check_stats(&player.stats)?;
    }
    Ok(())
}

fn check_stats(stats: &BTreeMap<String, i64>) -> Result<(), String> {
    if stats.len() > MAX_STATS_PER_PLAYER {
        return Err(format!(
            "at most {MAX_STATS_PER_PLAYER} stats per player per batch"
        ));
    }
    for (name, by) in stats {
        if !valid_stat_name(name) {
            return Err(format!(
                "stat {name:?}: names are 1 to 64 of a-z, 0-9, '_' and '.'"
            ));
        }
        if *by < 0 {
            return Err(format!("stat {name:?}: increases can't be negative"));
        }
    }
    Ok(())
}

/// Why a game result was refused. `now_ms` is the control plane's clock.
pub fn check_game(game: &GameResult, now_ms: u64) -> Result<(), String> {
    if game.players.is_empty() {
        return Err("a game needs at least one player".into());
    }
    if game.players.len() > MAX_GAME_PLAYERS {
        return Err(format!("at most {MAX_GAME_PLAYERS} players per game"));
    }
    if game.started_ms > game.ended_ms {
        return Err("a game can't end before it starts".into());
    }
    if game.ended_ms - game.started_ms > millis_of(MAX_GAME) {
        return Err("a game can't last more than a day".into());
    }
    if game.ended_ms > now_ms + millis_of(MAX_SKEW) {
        return Err("a game can't end in the future".into());
    }
    if let Some(map) = &game.map
        && (map.is_empty() || map.chars().count() > 64)
    {
        return Err("a map name is 1 to 64 characters".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    for player in &game.players {
        if !seen.insert(player.player) {
            return Err(format!("player {} is in the game twice", player.player));
        }
        if let Some(team) = &player.team
            && (team.is_empty() || team.chars().count() > 32)
        {
            return Err("a team name is 1 to 32 characters".into());
        }
        check_stats(&player.stats)?;
    }
    Ok(())
}

fn millis_of(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn time_of(ms: u64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(i64::try_from(ms).unwrap_or(i64::MAX)).unwrap_or_default()
}

pub(crate) fn ms_of(at: DateTime<Utc>) -> u64 {
    u64::try_from(at.timestamp_millis()).unwrap_or(0)
}

/// Stores a checked game; `false` if its id was stored already.
pub async fn store_game(pool: &PgPool, game: &GameResult) -> DbResult<bool> {
    let new = hallow_db::NewGame {
        id: game.id,
        kind: &game.kind,
        server: &game.source,
        map: game.map.as_deref(),
        started_at: time_of(game.started_ms),
        ended_at: time_of(game.ended_ms),
        players: game
            .players
            .iter()
            .map(|p| hallow_db::NewGamePlayer {
                account: p.player,
                won: p.won,
                placement: p.placement.and_then(|n| i32::try_from(n).ok()),
                team: p.team.as_deref(),
                stats: &p.stats,
            })
            .collect(),
    };
    hallow_db::record_game(pool, &new).await
}

/// The first UTC day in `window`, as of `today`; `None` for all time.
#[must_use]
pub fn window_start(window: Window, today: NaiveDate) -> Option<NaiveDate> {
    match window {
        Window::All => None,
        Window::Week => {
            Some(today - TimeDelta::days(i64::from(today.weekday().num_days_from_monday())))
        }
        Window::Month => today.with_day(1),
    }
}

/// The rank to show: none for the default rank or a missing one.
fn shown_rank(
    name: Option<String>,
    prefix: Option<String>,
    is_default: Option<bool>,
) -> (Option<String>, String) {
    let prefix = prefix.unwrap_or_default();
    match (name, is_default) {
        (Some(name), Some(false)) => (Some(name), prefix),
        _ => (None, prefix),
    }
}

/// A leaderboard of `stat` in `kind`, `limit` clamped to
/// [`LEADERBOARD_MAX`] (`None` is [`LEADERBOARD_DEFAULT`]).
pub async fn leaderboard(
    pool: &PgPool,
    kind: &str,
    stat: &str,
    window: Window,
    limit: Option<u32>,
) -> Result<Leaderboard, Failure> {
    if !hallow_proto::public::valid_board(kind, stat) {
        return Err(Failure::bad_request(
            "kinds are 1 to 64 of A-Z a-z 0-9 . _ -, stats 1 to 64 of a-z 0-9 _ .",
        ));
    }
    let limit = limit
        .unwrap_or(LEADERBOARD_DEFAULT)
        .clamp(1, LEADERBOARD_MAX);
    let since = window_start(window, Utc::now().date_naive());
    let rows = hallow_db::leaderboard(pool, kind, stat, since, limit).await?;
    let entries = rows
        .into_iter()
        .zip(1..)
        .map(|(row, position)| {
            let (rank, prefix) = shown_rank(row.rank_name, row.prefix, row.is_default);
            LeaderboardEntry {
                position,
                uuid: row.uuid,
                username: row.username,
                value: row.value,
                rank,
                prefix,
            }
        })
        .collect();
    Ok(Leaderboard {
        kind: kind.to_owned(),
        stat: stat.to_owned(),
        window,
        since_ms: since.map(|day| ms_of(day.and_time(chrono::NaiveTime::MIN).and_utc())),
        entries,
    })
}

/// How to show each of `players` that ever joined.
pub async fn display(pool: &PgPool, players: &[Uuid]) -> Result<Vec<PlayerDisplay>, Failure> {
    if players.len() > MAX_DISPLAY_PLAYERS {
        return Err(Failure::bad_request(format!(
            "at most {MAX_DISPLAY_PLAYERS} players per request"
        )));
    }
    Ok(hallow_db::primary_ranks(pool, players)
        .await?
        .into_iter()
        .map(|row| {
            let (rank, prefix) = shown_rank(row.rank_name, row.prefix, row.is_default);
            PlayerDisplay {
                uuid: row.uuid,
                username: row.username,
                rank,
                prefix,
            }
        })
        .collect())
}

/// Deletes per-day totals older than [`DAILY_RETENTION_DAYS`], hourly.
pub async fn retention(state: Arc<AppState>) {
    let Some(db) = state.db.clone() else {
        return;
    };
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let before = Utc::now().date_naive() - TimeDelta::days(DAILY_RETENTION_DAYS);
        match hallow_db::delete_daily_stats_before(&db, before).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(rows = n, "deleted old per-day stats"),
            Err(err) => tracing::warn!(%err, "couldn't delete old per-day stats"),
        }
    }
}

/// Adds a checked batch to the totals; returns how many totals changed.
pub async fn store(pool: &PgPool, batch: &StatsBatch) -> DbResult<u64> {
    let increases: Vec<StatIncrease<'_>> = batch
        .players
        .iter()
        .flat_map(|player| {
            player
                .stats
                .iter()
                .filter(|(_, by)| **by > 0)
                .map(|(stat, by)| StatIncrease {
                    account: player.player,
                    kind: &batch.kind,
                    stat,
                    by: *by,
                })
        })
        .collect();
    hallow_db::add_stats(pool, &increases).await
}

/// A player's totals by UUID or username; `None` if they never joined.
pub async fn of(pool: &PgPool, store: &Store, player: &str) -> DbResult<Option<PlayerStats>> {
    let found = match Uuid::parse_str(player) {
        Ok(uuid) => hallow_db::get_account(pool, uuid)
            .await?
            .map(|a| (a.uuid, a.username)),
        Err(_) => resolve(pool, store, player).await?,
    };
    let Some((uuid, username)) = found else {
        return Ok(None);
    };
    let mut kinds: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    for total in hallow_db::stats_of(pool, uuid).await? {
        kinds
            .entry(total.kind)
            .or_default()
            .insert(total.stat, total.value);
    }
    Ok(Some(PlayerStats {
        uuid,
        username,
        kinds,
    }))
}

/// `POST /v1/games`, `GET /v1/leaderboards`, `GET /v1/leaderboards/{kind}/{stat}` and
/// `POST /v1/display`, behind the API's token check.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(routes::GAMES, post(add_game))
        .route(routes::LEADERBOARDS, get(board_kinds))
        .route(&routes::leaderboard("{kind}", "{stat}"), get(board))
        .route(routes::DISPLAY, post(display_of))
}

fn database(state: &AppState) -> Result<&PgPool, Failure> {
    state.db.as_ref().ok_or_else(|| {
        Failure::new(
            ErrorCode::Unavailable,
            "stats need a database (HALLOW_DATABASE_URL)",
        )
    })
}

fn now_ms() -> u64 {
    ms_of(Utc::now())
}

async fn add_game(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    body: Result<Json<GameResult>, JsonRejection>,
) -> Result<StatusCode, Failure> {
    let Json(game) = body?;
    if !hallow_proto::public::valid_kind(&game.kind)
        || !hallow_proto::public::valid_kind(&game.source)
    {
        return Err(Failure::bad_request(
            "source and kind are 1 to 64 letters, digits, '.', '_' or '-'",
        ));
    }
    caller.check_server(&game.source, Some(&game.kind))?;
    check_game(&game, now_ms()).map_err(Failure::bad_request)?;
    let db = database(&state)?;
    if store_game(db, &game).await? {
        tracing::info!(
            game = %game.id,
            kind = %game.kind,
            server = %game.source,
            players = game.players.len(),
            "game stored"
        );
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn board_kinds(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<hallow_proto::public::BoardKind>>, Failure> {
    let db = database(&state)?;
    Ok(Json(kinds(db).await?))
}

/// Every kind with stats, for the list of leaderboards.
pub async fn kinds(pool: &PgPool) -> DbResult<Vec<hallow_proto::public::BoardKind>> {
    Ok(hallow_db::stat_kinds(pool)
        .await?
        .into_iter()
        .map(|k| hallow_proto::public::BoardKind {
            kind: k.kind,
            stats: k.stats,
            players: u64::try_from(k.players).unwrap_or(0),
        })
        .collect())
}

async fn board(
    State(state): State<Arc<AppState>>,
    Path((kind, stat)): Path<(String, String)>,
    Query(params): Query<crate::public::BoardParams>,
) -> Result<Json<Leaderboard>, Failure> {
    let db = database(&state)?;
    let window = params.window()?;
    Ok(Json(
        leaderboard(db, &kind, &stat, window, params.limit).await?,
    ))
}

async fn display_of(
    State(state): State<Arc<AppState>>,
    body: Result<Json<DisplayRequest>, JsonRejection>,
) -> Result<Json<Vec<PlayerDisplay>>, Failure> {
    let Json(request) = body?;
    let db = database(&state)?;
    Ok(Json(display(db, &request.players).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hallow_proto::stats::{GamePlayer, PlayerStatsDelta};

    fn batch(stats: &[(&str, i64)]) -> StatsBatch {
        StatsBatch {
            source: "game-1".into(),
            kind: "skywars".into(),
            players: vec![PlayerStatsDelta {
                player: Uuid::nil(),
                stats: stats.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect(),
            }],
        }
    }

    fn game(players: Vec<GamePlayer>) -> GameResult {
        GameResult {
            id: Uuid::nil(),
            source: "duels-1".into(),
            kind: "duels".into(),
            map: None,
            started_ms: 1_000,
            ended_ms: 61_000,
            players,
        }
    }

    fn player(n: u128) -> GamePlayer {
        GamePlayer {
            player: Uuid::from_u128(n),
            ..GamePlayer::default()
        }
    }

    #[test]
    fn checks_games() {
        let now = 100_000;
        assert!(check_game(&game(vec![player(1), player(2)]), now).is_ok());
        assert!(check_game(&game(vec![]), now).is_err());
        assert!(check_game(&game(vec![player(1), player(1)]), now).is_err());
        let mut backwards = game(vec![player(1)]);
        backwards.started_ms = 70_000;
        assert!(check_game(&backwards, now).is_err());
        let mut future = game(vec![player(1)]);
        future.ended_ms = now + 3_600_000;
        assert!(check_game(&future, now).is_err());
        let mut bad = player(1);
        bad.stats.insert("Kills".into(), 1);
        assert!(check_game(&game(vec![bad]), now).is_err());
    }

    #[test]
    fn windows_start_on_monday_and_the_first() {
        // 2026-11-18 is a Wednesday.
        let day = NaiveDate::from_ymd_opt(2026, 11, 18).unwrap();
        assert_eq!(window_start(Window::All, day), None);
        assert_eq!(
            window_start(Window::Week, day),
            NaiveDate::from_ymd_opt(2026, 11, 16)
        );
        assert_eq!(
            window_start(Window::Month, day),
            NaiveDate::from_ymd_opt(2026, 11, 1)
        );
        let monday = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        assert_eq!(window_start(Window::Week, monday), Some(monday));
    }

    #[test]
    fn default_ranks_are_not_shown() {
        assert_eq!(
            shown_rank(Some("default".into()), Some(String::new()), Some(true)),
            (None, String::new())
        );
        assert_eq!(
            shown_rank(Some("vip".into()), Some("&a[VIP] ".into()), Some(false)),
            (Some("vip".into()), "&a[VIP] ".into())
        );
    }

    #[test]
    fn checks_names_and_signs() {
        assert!(check(&batch(&[("kills", 2), ("play_time_s", 0)])).is_ok());
        assert!(check(&batch(&[("Kills", 2)])).is_err());
        assert!(check(&batch(&[("kills", -1)])).is_err());
    }
}
