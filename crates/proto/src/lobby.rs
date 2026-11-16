//! Moving players between hubs and lobbies (docs/SERVERS.md).
//!
//! A **game mode** is a lobby kind (`role = "lobby"` in its `hallow.toml`),
//! such as `skywars-lobby`; its name is the kind without `-lobby`
//! (`skywars`). `/lobby skywars`, the hub's game menu and a finished game
//! all go through [`SendToLobby`]: the control plane picks the least full
//! ready instance of the lobby kind and moves the player there with
//! [`crate::command::ProxyCommand::Send`]. Leaving `lobby` out means a hub.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `POST /v1/lobbies/send`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendToLobby {
    pub player: Uuid,
    /// A game mode (`skywars`), a lobby kind (`skywars-lobby`), or a game
    /// kind whose `lobby` names one. `None` sends the player to a hub.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lobby: Option<String>,
    /// If no lobby of that kind is up (or the name is unknown), send the
    /// player to a hub instead of refusing. A finished game sets it, so
    /// nobody is left behind.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fallback_hub: bool,
}

/// The answer to [`SendToLobby`]: where the player is going.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LobbySent {
    /// The server (instance) the player is sent to.
    pub server: String,
    /// Its kind.
    pub kind: String,
}

/// `GET /v1/lobbies`: the game modes players can pick from a hub, and how
/// busy they are.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LobbyOverview {
    /// Players on the whole network.
    pub online: u32,
    /// The hub kind, where `/hub` goes.
    pub hub: String,
    /// Every game mode, by name.
    pub modes: Vec<GameMode>,
}

/// One game mode in [`LobbyOverview`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameMode {
    /// What players type: the lobby kind without `-lobby`.
    pub name: String,
    /// The lobby kind.
    pub lobby: String,
    /// Players in the mode's lobbies and its games.
    pub players: u32,
    /// Lobby instances ready for players.
    pub lobbies: u32,
}

/// A game mode's name from its lobby kind: `skywars-lobby` → `skywars`.
#[must_use]
pub fn mode_name(lobby_kind: &str) -> &str {
    lobby_kind
        .strip_suffix("-lobby")
        .filter(|name| !name.is_empty())
        .unwrap_or(lobby_kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mode_names() {
        assert_eq!(mode_name("skywars-lobby"), "skywars");
        assert_eq!(mode_name("arcade"), "arcade");
        assert_eq!(mode_name("-lobby"), "-lobby");
    }

    #[test]
    fn send_shape() {
        let send = SendToLobby {
            player: Uuid::nil(),
            lobby: None,
            fallback_hub: false,
        };
        assert_eq!(
            serde_json::to_value(&send).unwrap(),
            json!({"player": Uuid::nil()})
        );
        let back: SendToLobby =
            serde_json::from_value(json!({"player": Uuid::nil(), "lobby": "skywars"})).unwrap();
        assert_eq!(back.lobby.as_deref(), Some("skywars"));
        assert!(!back.fallback_hub);
    }
}
