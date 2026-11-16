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

use std::sync::Mutex;
use std::time::{Duration, Instant};

use pumpkin_plugin_api::{
    Context, Plugin, PluginMetadata, Server,
    events::{EventHandler, EventPriority, PlayerJoinEvent},
    events_wit::PlayerJoinEventData,
    ipc::send_ipc_message,
    permissions, register_plugin, scheduler,
};

const NAME: &str = "hallow_test_game";
const MATCH_SECS_VAR: &str = "TEST_GAME_MATCH_SECS";
/// Seconds left at which the countdown is announced.
const ANNOUNCE: [u64; 5] = [10, 5, 3, 2, 1];

struct Game {
    length: Duration,
    started: Option<Instant>,
    announced: Option<u64>,
    over: bool,
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
    let Some((left, announce, end)) = with(|game| {
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
        Some((left, announce, end))
    })
    .flatten() else {
        return;
    };
    if end {
        server.broadcast("§6Match over! §7Sending you back to the lobby...");
        set_state("draining");
    } else if announce {
        server.broadcast(&format!("§eThe match ends in §f{left}§e s."));
    }
}

struct OnJoin;

impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, server: Server, event: PlayerJoinEventData) -> PlayerJoinEventData {
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
        });
        context
            .register_event_handler(OnJoin, EventPriority::Normal, false)
            .map_err(|err| format!("test-game: can't subscribe to joins: {err}"))?;
        scheduler::schedule_repeating_task(20, 20, |server| tick(&server));
        tracing::info!("test-game: matches last {secs} s");
        Ok(())
    }
}

register_plugin!(TestGame);
