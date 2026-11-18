//! The game's link to the control plane, through Hallow's server plugin.
//!
//! Every server image carries `hallow_server`, which holds the control
//! plane's address and the instance's token. A game talks to it over
//! Pumpkin's plugin IPC, so the game plugin needs no network access and no
//! token of its own:
//!
//! | Call | IPC message | What the control plane sees |
//! |---|---|---|
//! | [`set_state`] | `ready`, `in_game`, `draining` | The server's state in its next heartbeat, within a second or two. The matchmaker only fills `ready` servers; a `draining` game sends its players to its lobby and is ended once empty |
//! | [`add_stat`] | `stat <uuid> <name> <amount>` | The increase in the next stats batch (every 30 s), added to the player's totals for this server kind |
//! | [`send_to_lobby`] | `send <uuid> [mode]` | `POST /v1/lobbies/send`: the player's proxy moves them |
//!
//! [`Match`](crate::Match) calls these itself as the match moves on; games
//! only call them directly for their own stats or moves.
//!
//! Without `hallow_server` (a plain Pumpkin server in a test), the calls log
//! a warning once and do nothing.

use std::sync::atomic::{AtomicBool, Ordering};

pub use hallow_proto::server::ServerState as State;
use pumpkin_plugin_api::ipc::send_ipc_message;
use uuid::Uuid;

/// Hallow's server plugin, in every server image.
pub const SERVER_PLUGIN: &str = "hallow_server";

static MISSING_LOGGED: AtomicBool = AtomicBool::new(false);

/// The IPC message that sets `state`.
#[must_use]
pub fn state_message(state: State) -> &'static str {
    match state {
        State::Ready => "ready",
        State::InGame => "in_game",
        State::Draining => "draining",
    }
}

/// The IPC message that adds `amount` to a player's `name` stat, or why it
/// can't be sent: names are 1 to 64 of `a-z 0-9 _ .`.
pub fn stat_message(player: Uuid, name: &str, amount: u32) -> Result<String, String> {
    if hallow_proto::stats::valid_stat_name(name) {
        Ok(format!("stat {player} {name} {amount}"))
    } else {
        Err(format!(
            "{name:?} isn't a stat name: 1 to 64 of a-z, 0-9, _ and ."
        ))
    }
}

/// The IPC message that sends a player to a lobby: `mode` (a game mode,
/// lobby kind or game kind), or a hub when `None`.
#[must_use]
pub fn send_message(player: Uuid, mode: Option<&str>) -> String {
    match mode {
        Some(mode) => format!("send {player} {mode}"),
        None => format!("send {player}"),
    }
}

/// Sends `message` to `hallow_server` and returns its reply.
pub fn ask(message: &str) -> Result<String, String> {
    match send_ipc_message(SERVER_PLUGIN, message.as_bytes()) {
        Ok(Ok(reply)) => Ok(String::from_utf8_lossy(&reply).into_owned()),
        Ok(Err(err)) => Err(err),
        Err(()) => {
            if !MISSING_LOGGED.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    "hallow-sdk: {SERVER_PLUGIN} isn't loaded, so the control plane won't hear from this game"
                );
            }
            Err(format!("{SERVER_PLUGIN} isn't loaded"))
        }
    }
}

fn logged(what: &str, result: Result<String, String>) -> bool {
    match result {
        Ok(_) => true,
        Err(err) => {
            if MISSING_LOGGED.load(Ordering::Relaxed) && err.ends_with("isn't loaded") {
                return false;
            }
            tracing::warn!("hallow-sdk: {what}: {err}");
            false
        }
    }
}

/// Reports the server's state to the control plane. Returns whether
/// `hallow_server` took it.
pub fn set_state(state: State) -> bool {
    logged(
        &format!("can't set the state to {}", state_message(state)),
        ask(state_message(state)),
    )
}

/// Adds `amount` to `player`'s `name` stat for this server kind. Returns
/// whether `hallow_server` took it; it sends it within 30 s.
pub fn add_stat(player: Uuid, name: &str, amount: u32) -> bool {
    match stat_message(player, name, amount) {
        Ok(message) => logged(&format!("can't count {name}"), ask(&message)),
        Err(err) => {
            tracing::warn!("hallow-sdk: {err}");
            false
        }
    }
}

/// Sends `player` to a lobby of `mode`, or to a hub with `None`. The
/// control plane picks the instance and tells the player if none has room.
pub fn send_to_lobby(player: Uuid, mode: Option<&str>) -> bool {
    logged(
        &format!("can't send {player} on"),
        ask(&send_message(player, mode)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: Uuid = Uuid::from_u64_pair(1, 2);

    #[test]
    fn messages_match_the_server_plugin() {
        assert_eq!(state_message(State::Ready), "ready");
        assert_eq!(state_message(State::InGame), "in_game");
        assert_eq!(state_message(State::Draining), "draining");
        assert_eq!(
            stat_message(ID, "wins", 1).unwrap(),
            format!("stat {ID} wins 1")
        );
        assert!(stat_message(ID, "Wins", 1).is_err());
        assert!(stat_message(ID, "", 1).is_err());
        assert_eq!(
            send_message(ID, Some("skywars")),
            format!("send {ID} skywars")
        );
        assert_eq!(send_message(ID, None), format!("send {ID}"));
    }
}
