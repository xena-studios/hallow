//! Feeds Pumpkin's join and leave events into [`presence`](crate::presence)
//! and [`stats`](crate::stats), and takes what they gathered for the
//! repeating task.
//!
//! Pumpkin fires these events from the tick and waits for every handler, so
//! each handler only reads the player's id and name and updates a map: no
//! I/O, and no lock held across a call into Pumpkin.

use std::sync::Mutex;

use hallow_proto::{
    server::{ServerPlayer, ServerPlayersChanged},
    stats::PlayerStatsDelta,
};
use pumpkin_plugin_api::{
    Context, Server,
    events::{EventHandler, EventPriority, PlayerJoinEvent, PlayerLeaveEvent},
    events_wit::{PlayerJoinEventData, PlayerLeaveEventData},
};

use crate::collect::{now_ms, uuid};
use crate::presence::Changes;
use crate::stats::{self, Tally};

struct Gathered {
    changes: Changes,
    tally: Tally,
}

/// Present once [`register`] ran; the handlers do nothing before that.
static GATHERED: Mutex<Option<Gathered>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Gathered) -> R) -> Option<R> {
    GATHERED
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .as_mut()
        .map(f)
}

/// Starts gathering and subscribes to the events. A handler that fails to
/// register is logged and skipped.
pub fn register(context: &Context) {
    *GATHERED.lock().unwrap_or_else(|err| err.into_inner()) = Some(Gathered {
        changes: Changes::default(),
        tally: Tally::default(),
    });
    // Lowest priority: after any plugin that might act on the event first.
    // Join and leave can't be cancelled (that only hides the message).
    let results = [
        (
            "join",
            context.register_event_handler(OnJoin, EventPriority::Lowest, false),
        ),
        (
            "leave",
            context.register_event_handler(OnLeave, EventPriority::Lowest, false),
        ),
    ];
    for (event, result) in results {
        if let Err(err) = result {
            tracing::warn!("hallow-server: can't subscribe to {event} events: {err}");
        }
    }
}

/// Stops gathering and returns the increases not yet sent.
pub fn stop() -> Vec<PlayerStatsDelta> {
    let now = now_ms();
    GATHERED
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .take()
        .map(|mut gathered| gathered.tally.take(now))
        .unwrap_or_default()
}

/// Everyone on the server now, for a heartbeat. The joins and leaves
/// pending are covered by it, so they're dropped.
pub fn roster(server: &Server) -> Vec<ServerPlayer> {
    let players = server
        .get_all_players()
        .iter()
        .map(|player| ServerPlayer {
            uuid: uuid(&player.get_id()),
            username: player.get_name(),
        })
        .collect();
    with(|g| g.changes.clear());
    players
}

/// The joins and leaves since the last report, if any.
pub fn take_changes() -> Option<ServerPlayersChanged> {
    with(|g| g.changes.take()).flatten()
}

/// The stat increases since the last take.
pub fn take_stats() -> Vec<PlayerStatsDelta> {
    let now = now_ms();
    with(|g| g.tally.take(now)).unwrap_or_default()
}

/// Puts back increases whose batch failed.
pub fn restore_stats(deltas: Vec<PlayerStatsDelta>) {
    with(|g| g.tally.restore(deltas));
}

/// Handles a `stat <uuid> <name> [amount]` IPC message; `None` if it isn't
/// one.
pub fn handle_ipc(text: &str) -> Option<Result<Vec<u8>, String>> {
    let parsed = stats::parse_ipc(text)?;
    Some(parsed.and_then(|(player, name, by)| {
        with(|g| g.tally.bump(player, &name, by)).unwrap_or(Err(
            "not linked to a control plane; stats are off".to_owned(),
        ))?;
        Ok(b"ok".to_vec())
    }))
}

struct OnJoin;

impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, _server: Server, event: PlayerJoinEventData) -> PlayerJoinEventData {
        let player = uuid(&event.player.get_id());
        let username = event.player.get_name();
        let now = now_ms();
        with(|g| {
            g.changes.joined(player, username);
            g.tally.joined(player, now);
        });
        event
    }
}

struct OnLeave;

impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _server: Server, event: PlayerLeaveEventData) -> PlayerLeaveEventData {
        let player = uuid(&event.player.get_id());
        let now = now_ms();
        with(|g| {
            g.changes.left(player);
            g.tally.left(player, now);
        });
        event
    }
}
