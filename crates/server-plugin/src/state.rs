//! The state reported in heartbeats: `ready` until a game changes it.
//!
//! Game code runs in its own plugin, so it sets the state over Pumpkin's
//! plugin IPC: send `hallow_server` a message whose bytes are the state name.
//!
//! | Message | Effect | Reply |
//! |---|---|---|
//! | `ready`, `in_game` or `draining` | Sets the state; the next heartbeat, within a second or two, reports it | The new state |
//! | empty | Nothing | The current state |
//!
//! Anything else is an error. From a Rust plugin:
//!
//! ```rust,ignore
//! use pumpkin_plugin_api::wit::pumpkin::plugin::ipc::send_ipc_message;
//! send_ipc_message("hallow_server", b"in_game")??;
//! ```
//!
//! The Hallow SDK will wrap this. Code inside this crate calls [`set`].

use std::sync::atomic::{AtomicU8, Ordering};

use hallow_proto::server::ServerState;

static STATE: AtomicU8 = AtomicU8::new(0);

const ALL: [ServerState; 3] = [
    ServerState::Ready,
    ServerState::InGame,
    ServerState::Draining,
];

pub fn get() -> ServerState {
    ALL[usize::from(STATE.load(Ordering::Relaxed)).min(ALL.len() - 1)]
}

pub fn set(state: ServerState) {
    let index = ALL.iter().position(|s| *s == state).unwrap_or(0);
    STATE.store(u8::try_from(index).unwrap_or(0), Ordering::Relaxed);
}

fn name(state: ServerState) -> &'static str {
    match state {
        ServerState::Ready => "ready",
        ServerState::InGame => "in_game",
        ServerState::Draining => "draining",
    }
}

fn parse(text: &str) -> Option<ServerState> {
    ALL.into_iter().find(|state| name(*state) == text)
}

/// Handles an IPC message from another plugin; see the module docs.
pub fn handle_ipc(message: &[u8]) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(message)
        .map_err(|_| "the message must be UTF-8".to_owned())?
        .trim();
    if !text.is_empty() {
        let state = parse(text).ok_or_else(|| {
            format!("unknown message {text:?}: send ready, in_game, draining, or nothing")
        })?;
        set(state);
    }
    Ok(name(get()).as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_sets_and_reads_the_state() {
        assert_eq!(handle_ipc(b"").unwrap(), b"ready");
        assert_eq!(handle_ipc(b"in_game\n").unwrap(), b"in_game");
        assert_eq!(get(), ServerState::InGame);
        assert!(handle_ipc(b"lobby").is_err());
        assert_eq!(handle_ipc(b"").unwrap(), b"in_game");
        set(ServerState::Ready);
        assert_eq!(get(), ServerState::Ready);
    }

    #[test]
    fn names_match_the_api() {
        for state in ALL {
            let json = serde_json::to_value(state).unwrap();
            assert_eq!(json, name(state));
        }
    }
}
