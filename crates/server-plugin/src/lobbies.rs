//! Hubs, lobbies and the way back from a game (`hallow_proto::lobby`).
//!
//! - **The game menu's data.** Another plugin (the hub's) asks for the game
//!   modes over IPC. The first ask starts fetching `GET /v1/lobbies` every
//!   [`REFRESH`] for as long as asks keep coming (within [`KEEP_FETCHING`]);
//!   the reply is the last list fetched.
//! - **Sending players.** `send <uuid> [mode]` starts
//!   `POST /v1/lobbies/send`; the control plane tells the player where they
//!   are going and moves them. A refusal is shown to the player in chat.
//! - **Game over.** While the server is `draining` (a game plugin sets it
//!   when its game ends; see [`state`](crate::state)), everyone still on it
//!   is sent to `HALLOW_RETURN_LOBBY`, from `hallow.toml`'s `lobby`, every
//!   [`REFRESH`] until they've gone. A draining lobby sends them to another
//!   lobby of its own kind. If none is up, the control plane sends them to a
//!   hub. The control plane ends a game instance once it is draining and
//!   empty, so players are always moved before it is deleted.
//!
//! | Message | Reply |
//! |---|---|
//! | `lobbies` | The game modes as [`LobbyOverview`](hallow_proto::lobby::LobbyOverview) JSON, or an error until the first list arrives |
//! | `send <uuid> [mode]` | `ok`; the player goes to a lobby of `mode` (a mode, lobby kind or game kind), or to a hub without one |
//!
//! From a Rust plugin:
//!
//! ```rust,ignore
//! use pumpkin_plugin_api::wit::pumpkin::plugin::ipc::send_ipc_message;
//! let json = send_ipc_message("hallow_server", b"lobbies")??;
//! send_ipc_message("hallow_server", format!("send {uuid} skywars").as_bytes())??;
//! ```

use std::sync::Mutex;
use std::time::{Duration, Instant};

use hallow_proto::{
    lobby::{LobbyOverview, SendToLobby},
    routes,
    server::{ServerRole, ServerState},
};
use pumpkin_plugin_api::{
    Server, text::TextComponent, wit::pumpkin::plugin::uuid::Uuid as WitUuid,
};
use uuid::Uuid;

use crate::collect::uuid;
use crate::http::{self, Client, Pending};

/// How often the list is fetched, and players still on a finished game are
/// sent again.
pub const REFRESH: Duration = Duration::from_secs(5);
/// The list is fetched while it was asked for this recently.
pub const KEEP_FETCHING: Duration = Duration::from_secs(60);

const UNAVAILABLE: &str = "§cThe network is unavailable right now. Try again in a moment.";

#[derive(Default)]
struct Lobbies {
    /// The last list fetched, as JSON.
    overview: Option<Vec<u8>>,
    asked: Option<Instant>,
    fetched: Option<Instant>,
    fetch: Option<Pending>,
    fetch_failing: bool,
    /// Sends waiting for an answer, by player.
    sends: Vec<(Uuid, Pending)>,
    /// When players were last sent off a finished game.
    returned: Option<Instant>,
}

static LOBBIES: Mutex<Option<Lobbies>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Lobbies) -> R) -> R {
    f(LOBBIES
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .get_or_insert_with(Lobbies::default))
}

/// Where a draining server sends its players: a game to its return lobby,
/// a lobby to another of its own kind. `None` means a hub.
pub fn return_target(role: ServerRole, kind: &str, return_lobby: Option<&str>) -> Option<String> {
    match role {
        ServerRole::Game => return_lobby.map(str::to_owned),
        ServerRole::Lobby => Some(kind.to_owned()),
    }
}

/// A `send <uuid> [mode]` message, or `None` if it isn't one.
pub fn parse_send(text: &str) -> Option<Result<(Uuid, Option<String>), String>> {
    let rest = text.strip_prefix("send")?;
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    let mut words = rest.split_whitespace();
    let parsed = (|| {
        let player = words
            .next()
            .and_then(|w| Uuid::parse_str(w).ok())
            .ok_or("send needs a player UUID")?;
        let mode = words.next().map(str::to_ascii_lowercase);
        if words.next().is_some() {
            return Err("send takes a player UUID and at most one mode");
        }
        Ok((player, mode))
    })();
    Some(parsed.map_err(str::to_owned))
}

/// Handles `lobbies` and `send ...`; `None` for any other message.
pub fn handle_ipc(text: &str, client: Option<Client>) -> Option<Result<Vec<u8>, String>> {
    if text == "lobbies" {
        let now = Instant::now();
        let (overview, first) = with(|l| (l.overview.clone(), l.asked.replace(now).is_none()));
        if first && let Some(client) = &client {
            start_fetch(client);
        }
        return Some(overview.ok_or_else(|| "no lobby list yet; ask again in a moment".to_owned()));
    }
    let parsed = parse_send(text)?;
    Some(parsed.and_then(|(player, lobby)| {
        let client = client.ok_or("not linked to a control plane")?;
        send(&client, player, lobby, false);
        Ok(b"ok".to_vec())
    }))
}

fn start_fetch(client: &Client) {
    match client.start_get(routes::LOBBIES) {
        Ok(pending) => with(|l| {
            l.fetch = Some(pending);
            l.fetched = Some(Instant::now());
        }),
        Err(err) => fetch_failed(&err),
    }
}

fn fetch_failed(err: &http::Error) {
    if !with(|l| std::mem::replace(&mut l.fetch_failing, true)) {
        tracing::warn!("hallow-server: couldn't fetch the lobby list: {err}");
    }
}

/// Starts sending `player` to a lobby of `lobby`, or a hub.
fn send(client: &Client, player: Uuid, lobby: Option<String>, fallback_hub: bool) {
    let request = SendToLobby {
        player,
        lobby,
        fallback_hub,
    };
    match client.start_post(routes::LOBBIES_SEND, &request) {
        Ok(pending) => with(|l| l.sends.push((player, pending))),
        Err(err) => tracing::warn!("hallow-server: couldn't send {player} to a lobby: {err}"),
    }
}

/// Runs once a second, from the plugin's task.
pub fn tick(
    server: &Server,
    client: &Client,
    role: ServerRole,
    kind: &str,
    return_lobby: Option<&str>,
    state: ServerState,
) {
    // The list.
    if let Some(pending) = with(|l| l.fetch.take()) {
        match pending.poll_body() {
            None => with(|l| l.fetch = Some(pending)),
            Some(Ok(body)) => match serde_json::from_slice::<LobbyOverview>(&body) {
                Ok(_) => with(|l| {
                    l.overview = Some(body);
                    if std::mem::take(&mut l.fetch_failing) {
                        tracing::info!("hallow-server: the lobby list is back");
                    }
                }),
                Err(err) => fetch_failed(&http::Error::Body(err.to_string())),
            },
            Some(Err(err)) => fetch_failed(&err),
        }
    }
    let fetch_due = with(|l| {
        l.fetch.is_none()
            && l.asked.is_some_and(|at| at.elapsed() < KEEP_FETCHING)
            && l.fetched.is_none_or(|at| at.elapsed() >= REFRESH)
    });
    if fetch_due {
        start_fetch(client);
    }

    // Answers to sends.
    let sends = with(|l| std::mem::take(&mut l.sends));
    let mut waiting = Vec::new();
    for (player, pending) in sends {
        match pending.poll() {
            None => waiting.push((player, pending)),
            Some(Ok(())) => {}
            Some(Err(err)) => {
                let text = match &err {
                    http::Error::Status(404 | 409, Some(api)) => format!("§c{}", api.message),
                    _ => {
                        tracing::warn!("hallow-server: sending {player} to a lobby failed: {err}");
                        UNAVAILABLE.to_owned()
                    }
                };
                tell(server, player, &text);
            }
        }
    }
    with(|l| l.sends.extend(waiting));

    // Game over: send everyone on.
    if state != ServerState::Draining {
        return;
    }
    let due = with(|l| {
        let due = l.returned.is_none_or(|at| at.elapsed() >= REFRESH);
        if due {
            l.returned = Some(Instant::now());
        }
        due
    });
    if !due {
        return;
    }
    let target = return_target(role, kind, return_lobby);
    for player in server.get_all_players() {
        let id = uuid(&player.get_id());
        if !with(|l| l.sends.iter().any(|(p, _)| *p == id)) {
            send(client, id, target.clone(), true);
        }
    }
}

/// Shows `text` to `player` if they're still here.
fn tell(server: &Server, player: Uuid, text: &str) {
    let (high, low) = player.as_u64_pair();
    if let Some(player) = server.get_player_by_uuid(WitUuid { high, low }) {
        player.send_system_message(TextComponent::from_legacy_string(text), false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_messages_parse() {
        let id = Uuid::from_u128(7);
        assert_eq!(parse_send(&format!("send {id}")), Some(Ok((id, None))));
        assert_eq!(
            parse_send(&format!("send {id} SkyWars")),
            Some(Ok((id, Some("skywars".into()))))
        );
        assert!(parse_send("send nobody").unwrap().is_err());
        assert!(parse_send(&format!("send {id} a b")).unwrap().is_err());
        assert_eq!(parse_send("sendx"), None);
        assert_eq!(parse_send("lobbies"), None);
    }

    #[test]
    fn draining_servers_send_players_on() {
        assert_eq!(
            return_target(ServerRole::Game, "skywars", Some("skywars-lobby")).as_deref(),
            Some("skywars-lobby")
        );
        assert_eq!(return_target(ServerRole::Game, "skywars", None), None);
        assert_eq!(
            return_target(ServerRole::Lobby, "skywars-lobby", Some("hub")).as_deref(),
            Some("skywars-lobby")
        );
    }

    #[test]
    fn the_list_is_asked_for_before_it_arrives() {
        assert!(handle_ipc("lobbies", None).unwrap().is_err());
        with(|l| l.overview = Some(b"{}".to_vec()));
        assert_eq!(handle_ipc("lobbies", None).unwrap().unwrap(), b"{}");
        assert!(
            handle_ipc(&format!("send {}", Uuid::nil()), None)
                .unwrap()
                .is_err()
        );
        assert!(handle_ipc("ready", None).is_none());
    }
}
