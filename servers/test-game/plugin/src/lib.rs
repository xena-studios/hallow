//! The dev network's test game: the smallest game that has an end.
//!
//! With `TEST_GAME_MATCH_SECS` set, the first player to join starts a
//! match that long. Without it nothing ends a match, and the game is over
//! when its players leave, as before: the static `game-1` of the compose
//! networks runs that way, and the dynamic instances get the variable from
//! the control plane (`HALLOW_INSTANCE_ENV` in `compose.instances.yaml`).
//!
//! When a match starts the server turns `in_game`, so
//! the matchmaker sends nobody else, and counts down in chat. When time is
//! up it turns `draining`: Hallow's server plugin sends everyone to the
//! lobby `hallow.toml` names (`HALLOW_RETURN_LOBBY`), and the control plane
//! ends the instance once it's empty. Both state changes go to
//! `hallow_server` over IPC, the way a real game's plugin (or `hallow-sdk`)
//! does it.
//!
//! **Result.** The player who sent the most chat messages during the match
//! wins (ties go to whoever joined first). Just before turning `draining`,
//! the game sends `hallow_server` a `game_result` with everyone who played:
//! their placement, `won`, and a `messages` stat. The control plane counts
//! `games`, `wins` and `messages` for `test-game` from it.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hallow_proto::stats::{GamePlayer, GameReport};
use pumpkin_plugin_api::{
    Context, Plugin, PluginMetadata, Server,
    events::{EventHandler, EventPriority, PlayerChatEvent, PlayerJoinEvent},
    events_wit::{PlayerChatEventData, PlayerJoinEventData},
    ipc::send_ipc_message,
    permissions, register_plugin, scheduler,
};
use uuid::Uuid;

const NAME: &str = "hallow_test_game";
const MATCH_SECS_VAR: &str = "TEST_GAME_MATCH_SECS";
/// Seconds left at which the countdown is announced.
const ANNOUNCE: [u64; 5] = [10, 5, 3, 2, 1];

struct Game {
    length: Duration,
    started: Option<Instant>,
    announced: Option<u64>,
    over: bool,
    /// Everyone who played, in the order they joined, with how many chat
    /// messages they sent.
    players: Vec<(Uuid, i64)>,
}

/// Placements by messages sent, most first; ties keep join order.
fn report(players: &[(Uuid, i64)]) -> GameReport {
    let mut order: Vec<(usize, &(Uuid, i64))> = players.iter().enumerate().collect();
    order.sort_by(|(i, (_, a)), (j, (_, b))| b.cmp(a).then(i.cmp(j)));
    GameReport {
        players: order
            .into_iter()
            .zip(1u32..)
            .map(|((_, (player, messages)), placement)| GamePlayer {
                player: *player,
                won: placement == 1,
                placement: Some(placement),
                team: None,
                stats: BTreeMap::from([("messages".to_owned(), *messages)]),
            })
            .collect(),
        ..GameReport::default()
    }
}

/// Sends the result to Hallow's server plugin.
fn send_result(players: &[(Uuid, i64)]) {
    if players.is_empty() {
        return;
    }
    let Ok(json) = serde_json::to_string(&report(players)) else {
        return;
    };
    match send_ipc_message("hallow_server", format!("game_result {json}").as_bytes()) {
        Ok(Ok(_)) => {}
        Ok(Err(err)) => tracing::warn!("test-game: hallow_server refused the result: {err}"),
        Err(()) => tracing::warn!("test-game: hallow_server isn't loaded; result lost"),
    }
}

static GAME: Mutex<Option<Game>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Game) -> R) -> Option<R> {
    GAME.lock()
        .unwrap_or_else(|err| err.into_inner())
        .as_mut()
        .map(f)
}

/// Tells Hallow's server plugin the server's state.
fn set_state(state: &str) {
    match send_ipc_message("hallow_server", state.as_bytes()) {
        Ok(Ok(_)) => {}
        Ok(Err(err)) => tracing::warn!("test-game: hallow_server refused {state}: {err}"),
        Err(()) => tracing::warn!("test-game: hallow_server isn't loaded; can't set {state}"),
    }
}

/// Seconds left, if a match runs.
fn left(game: &Game) -> Option<u64> {
    let started = game.started?;
    Some(game.length.saturating_sub(started.elapsed()).as_secs())
}

fn tick(server: &Server) {
    let Some((left, announce, end, players)) = with(|game| {
        let left = left(game)?;
        if game.over {
            return None;
        }
        let announce = ANNOUNCE.contains(&left) && game.announced != Some(left);
        if announce {
            game.announced = Some(left);
        }
        let end = left == 0;
        game.over = end;
        let players = if end {
            std::mem::take(&mut game.players)
        } else {
            Vec::new()
        };
        Some((left, announce, end, players))
    })
    .flatten() else {
        return;
    };
    if end {
        let winner = report(&players)
            .players
            .first()
            .and_then(|p| server.get_player_by_uuid(to_wit(p.player)))
            .map(|p| p.get_name());
        match winner {
            Some(name) => server.broadcast(&format!(
                "§6Match over! §e{name} §6wins. §7Sending you back to the lobby..."
            )),
            None => server.broadcast("§6Match over! §7Sending you back to the lobby..."),
        }
        send_result(&players);
        set_state("draining");
    } else if announce {
        server.broadcast(&format!("§eThe match ends in §f{left}§e s."));
    }
}

struct OnJoin;

fn to_wit(id: Uuid) -> pumpkin_plugin_api::wit::pumpkin::plugin::uuid::Uuid {
    let (high, low) = id.as_u64_pair();
    pumpkin_plugin_api::wit::pumpkin::plugin::uuid::Uuid { high, low }
}

fn from_wit(id: &pumpkin_plugin_api::wit::pumpkin::plugin::uuid::Uuid) -> Uuid {
    Uuid::from_u64_pair(id.high, id.low)
}

struct OnChat;

impl EventHandler<PlayerChatEvent> for OnChat {
    fn handle(&self, _server: Server, event: PlayerChatEventData) -> PlayerChatEventData {
        let id = from_wit(&event.player.get_id());
        with(|game| {
            if game.started.is_some()
                && !game.over
                && let Some(entry) = game.players.iter_mut().find(|(p, _)| *p == id)
            {
                entry.1 += 1;
            }
        });
        event
    }
}

impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, server: Server, event: PlayerJoinEventData) -> PlayerJoinEventData {
        let id = from_wit(&event.player.get_id());
        with(|game| {
            if !game.over && !game.players.iter().any(|(p, _)| *p == id) {
                game.players.push((id, 0));
            }
        });
        let start = with(|game| {
            let start = game.started.is_none();
            if start {
                game.started = Some(Instant::now());
            }
            start.then_some(game.length.as_secs())
        })
        .flatten();
        if let Some(secs) = start {
            set_state("in_game");
            server.broadcast(&format!("§aThe match has started! §7It lasts {secs} s."));
        }
        event
    }
}

struct TestGame;

impl Plugin for TestGame {
    fn new() -> Self {
        Self
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NAME.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Xena Studios".into()],
            description: "A timed test match that ends and sends its players back to their lobby."
                .into(),
            dependencies: vec![],
            permissions: vec![format!("{}{MATCH_SECS_VAR}", permissions::SYS_ENV_PREFIX)],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        let Some(secs) = std::env::var(MATCH_SECS_VAR)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|secs| *secs > 0)
        else {
            tracing::info!("test-game: {MATCH_SECS_VAR} isn't set; matches don't end on their own");
            return Ok(());
        };
        *GAME.lock().unwrap_or_else(|err| err.into_inner()) = Some(Game {
            length: Duration::from_secs(secs),
            started: None,
            announced: None,
            over: false,
            players: Vec::new(),
        });
        context
            .register_event_handler(OnJoin, EventPriority::Normal, false)
            .map_err(|err| format!("test-game: can't subscribe to joins: {err}"))?;
        // Highest runs first, so a message counts even if a later plugin
        // (Hallow's server plugin, for rank prefixes) cancels Pumpkin's line.
        context
            .register_event_handler(OnChat, EventPriority::Highest, false)
            .map_err(|err| format!("test-game: can't subscribe to chat: {err}"))?;
        scheduler::schedule_repeating_task(20, 20, |server| tick(&server));
        tracing::info!("test-game: matches last {secs} s");
        Ok(())
    }
}

register_plugin!(TestGame);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn most_messages_wins_ties_by_join_order() {
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let made = report(&[(a, 1), (b, 3), (c, 3)]);
        let order: Vec<_> = made
            .players
            .iter()
            .map(|p| (p.player, p.won, p.placement))
            .collect();
        assert_eq!(
            order,
            [(b, true, Some(1)), (c, false, Some(2)), (a, false, Some(3))]
        );
        assert_eq!(made.players[0].stats["messages"], 3);
    }
}
