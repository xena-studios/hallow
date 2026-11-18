//! Finished games: a game plugin reports each game's result over IPC, and
//! this sends it on as `POST /v1/games` (a
//! [`GameResult`](hallow_proto::stats::GameResult)). The control plane adds
//! `games`, `wins` and the players' stats to their totals for this server's
//! kind and keeps the game for their recent games.
//!
//! | Message | Reply |
//! |---|---|
//! | `game_result <json>` | `ok`; the JSON is a [`GameReport`](hallow_proto::stats::GameReport) |
//!
//! The result gets a fresh id, this server's name and kind, the time the
//! server last turned `in_game` as its start (unless the report gives
//! `started_ms`), and now as its end. Results are sent one at a time from
//! the repeating task and retried every [`RETRY`] until the control plane
//! answers, up to [`MAX_ATTEMPTS`] times; the id makes a retry count once.
//! A result the control plane refuses (`4xx`) is logged and dropped. Any
//! left at unload are sent then.
//!
//! From a Rust plugin, at the end of a game, before turning `draining`:
//!
//! ```rust,ignore
//! use pumpkin_plugin_api::wit::pumpkin::plugin::ipc::send_ipc_message;
//! let report = r#"{"players":[{"player":"<uuid>","won":true,"stats":{"kills":3}}]}"#;
//! send_ipc_message("hallow_server", format!("game_result {report}").as_bytes())??;
//! ```

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hallow_proto::{
    routes,
    stats::{GameReport, GameResult, MAX_GAME_PLAYERS, valid_stat_name},
};
use uuid::Uuid;

use crate::collect::unix_ms;
use crate::http::{Client, Pending};

/// How long to wait before sending a failed result again.
pub const RETRY: Duration = Duration::from_secs(5);
/// How many times a result is tried before it's dropped (about ten minutes).
pub const MAX_ATTEMPTS: u32 = 120;
/// Results waiting at once; past this the oldest is dropped.
const MAX_QUEUED: usize = 64;

struct Queued {
    result: GameResult,
    attempts: u32,
}

#[derive(Default)]
struct Games {
    /// When the server last turned `in_game`, in Unix milliseconds.
    started_ms: Option<u64>,
    queue: VecDeque<Queued>,
    in_flight: Option<Pending>,
    last_try: Option<Instant>,
}

static GAMES: Mutex<Option<Games>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Games) -> R) -> R {
    f(GAMES
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .get_or_insert_with(Games::default))
}

/// The server turned `in_game`: a game starts now.
pub fn started() {
    let now = unix_ms();
    with(|g| g.started_ms = Some(now));
}

/// Checks a report the way the control plane will, so a game hears about
/// a mistake in its IPC reply rather than in the server log later.
pub fn check(report: &GameReport) -> Result<(), String> {
    if report.players.is_empty() {
        return Err("a game result needs at least one player".into());
    }
    if report.players.len() > MAX_GAME_PLAYERS {
        return Err(format!("at most {MAX_GAME_PLAYERS} players per game"));
    }
    for player in &report.players {
        for (name, by) in &player.stats {
            if !valid_stat_name(name) {
                return Err(format!(
                    "stat {name:?}: names are 1 to 64 of a-z, 0-9, '_' and '.'"
                ));
            }
            if *by < 0 {
                return Err(format!("stat {name:?}: values can't be negative"));
            }
        }
    }
    Ok(())
}

/// Turns a report into a result for `source` of `kind`, ended at `now_ms`.
pub fn result(
    report: GameReport,
    source: &str,
    kind: &str,
    started_ms: Option<u64>,
    now_ms: u64,
) -> GameResult {
    let started = report
        .started_ms
        .or(started_ms)
        .unwrap_or(now_ms)
        .min(now_ms);
    GameResult {
        id: Uuid::new_v4(),
        source: source.to_owned(),
        kind: kind.to_owned(),
        map: report.map,
        started_ms: started,
        ended_ms: now_ms,
        players: report.players,
    }
}

/// Handles `game_result <json>`; `None` if the message isn't one.
pub fn handle_ipc(text: &str, link: Option<(String, String)>) -> Option<Result<Vec<u8>, String>> {
    let json = text.strip_prefix("game_result")?;
    Some((|| {
        let (source, kind) = link.ok_or("not linked to a control plane; results are off")?;
        let report: GameReport =
            serde_json::from_str(json.trim()).map_err(|err| format!("bad game result: {err}"))?;
        check(&report)?;
        let now = unix_ms();
        with(|g| {
            let result = result(report, &source, &kind, g.started_ms.take(), now);
            tracing::info!(
                "hallow-server: game {} over with {} players",
                result.id,
                result.players.len()
            );
            if g.queue.len() >= MAX_QUEUED {
                g.queue.pop_front();
                tracing::warn!("hallow-server: too many game results waiting; dropped the oldest");
            }
            g.queue.push_back(Queued {
                result,
                attempts: 0,
            });
        });
        Ok(b"ok".to_vec())
    })())
}

/// Checks the result in flight and sends the next when it's time.
pub fn tick(client: &Client) {
    let in_flight = with(|g| g.in_flight.take());
    if let Some(pending) = in_flight {
        match pending.poll() {
            None => {
                with(|g| g.in_flight = Some(pending));
                return;
            }
            Some(answer) => with(|g| answered(g, answer)),
        }
    }
    let next = with(|g| {
        let due = g.last_try.is_none_or(|at| at.elapsed() >= RETRY);
        if !due {
            return None;
        }
        let front = g.queue.front_mut()?;
        front.attempts += 1;
        g.last_try = Some(Instant::now());
        Some(front.result.clone())
    });
    let Some(result) = next else {
        return;
    };
    match client.start_post(routes::GAMES, &result) {
        Ok(pending) => with(|g| g.in_flight = Some(pending)),
        Err(err) => with(|g| answered(g, Err(err))),
    }
}

fn answered(g: &mut Games, answer: Result<(), crate::http::Error>) {
    let Some(front) = g.queue.front() else {
        return;
    };
    match answer {
        Ok(()) => {
            g.queue.pop_front();
            // The next one goes straight away.
            g.last_try = None;
        }
        Err(crate::http::Error::Status(status, err)) if (400..500).contains(&status) => {
            tracing::warn!(
                "hallow-server: the control plane refused game {} ({status}): {}; dropping it",
                front.result.id,
                err.map_or_else(String::new, |e| e.message)
            );
            g.queue.pop_front();
        }
        Err(err) if front.attempts >= MAX_ATTEMPTS => {
            tracing::warn!(
                "hallow-server: couldn't send game {} after {} tries: {err}; dropping it",
                front.result.id,
                front.attempts
            );
            g.queue.pop_front();
        }
        Err(err) => {
            if front.attempts == 1 {
                tracing::warn!(
                    "hallow-server: couldn't send game {}: {err}; retrying every {} s",
                    front.result.id,
                    RETRY.as_secs()
                );
            }
        }
    }
}

/// Sends what's left, waiting for each; for unload.
pub fn flush(client: &Client) {
    let queue = with(|g| std::mem::take(&mut g.queue));
    for queued in queue {
        if let Err(err) = client.post(routes::GAMES, &queued.result) {
            tracing::warn!(
                "hallow-server: couldn't send game {} at shutdown: {err}",
                queued.result.id
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hallow_proto::stats::GamePlayer;

    fn report(stats: &[(&str, i64)]) -> GameReport {
        GameReport {
            players: vec![GamePlayer {
                player: Uuid::from_u128(1),
                won: true,
                stats: stats.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect(),
                ..GamePlayer::default()
            }],
            ..GameReport::default()
        }
    }

    #[test]
    fn checks_reports() {
        assert!(check(&report(&[("kills", 2)])).is_ok());
        assert!(check(&report(&[("Kills", 2)])).is_err());
        assert!(check(&report(&[("kills", -2)])).is_err());
        assert!(check(&GameReport::default()).is_err());
    }

    #[test]
    fn results_start_when_the_game_did() {
        let made = result(report(&[]), "duels-1", "duels", Some(1_000), 61_000);
        assert_eq!((made.started_ms, made.ended_ms), (1_000, 61_000));
        assert_eq!(
            (made.source.as_str(), made.kind.as_str()),
            ("duels-1", "duels")
        );
        let mut own = report(&[]);
        own.started_ms = Some(5_000);
        assert_eq!(result(own, "a", "b", Some(1_000), 61_000).started_ms, 5_000);
        // Never started: a game of no length, never one that ends first.
        assert_eq!(result(report(&[]), "a", "b", None, 9).started_ms, 9);
        let mut late = report(&[]);
        late.started_ms = Some(99);
        assert_eq!(result(late, "a", "b", None, 9).started_ms, 9);
    }

    #[test]
    fn ipc_messages() {
        let link = Some(("duels-1".to_owned(), "duels".to_owned()));
        assert!(handle_ipc("in_game", link.clone()).is_none());
        let json = r#"{"players":[{"player":"00000000-0000-0000-0000-000000000001","won":true}]}"#;
        assert_eq!(
            handle_ipc(&format!("game_result {json}"), link.clone()),
            Some(Ok(b"ok".to_vec()))
        );
        assert!(matches!(
            handle_ipc("game_result {", link.clone()),
            Some(Err(_))
        ));
        assert!(matches!(
            handle_ipc(&format!("game_result {json}"), None),
            Some(Err(_))
        ));
        assert_eq!(with(|g| g.queue.len()), 1);
    }
}
