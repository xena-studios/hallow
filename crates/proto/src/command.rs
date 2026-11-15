//! Cross-proxy actions. A service (a server plugin, the panel, the
//! matchmaker) posts a [`NetworkCommand`]; the control plane finds the proxy
//! holding the player and queues a [`ProxyCommand`] for it, which the proxy
//! picks up from its command long poll.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `POST /v1/commands`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NetworkCommand {
    /// Move a player to a server, wherever they are.
    Send { player: Uuid, server: String },
    /// Show a chat message to one player. `from` names the player who sent
    /// it, if one did, so the recipient can reply.
    Message {
        player: Uuid,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
    /// Show a chat message to everyone, or to everyone on `servers` if set.
    Broadcast {
        text: String,
        servers: Option<Vec<String>>,
    },
    /// Disconnect a player with `reason`.
    Kick { player: Uuid, reason: String },
}

/// What one proxy has to do. Same actions as [`NetworkCommand`], resolved to
/// the proxy that holds the player. Messages use `text` as plain text with
/// `§` colour codes, which every client version understands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProxyCommand {
    Send {
        player: Uuid,
        server: String,
    },
    /// `from` is the sending player's username, for `/r`.
    Message {
        player: Uuid,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
    /// Every player on this proxy, or only those on `servers` if set.
    Broadcast {
        text: String,
        servers: Option<Vec<String>>,
    },
    Kick {
        player: Uuid,
        reason: String,
    },
}

impl From<NetworkCommand> for ProxyCommand {
    fn from(command: NetworkCommand) -> Self {
        match command {
            NetworkCommand::Send { player, server } => Self::Send { player, server },
            NetworkCommand::Message { player, text, from } => Self::Message { player, text, from },
            NetworkCommand::Broadcast { text, servers } => Self::Broadcast { text, servers },
            NetworkCommand::Kick { player, reason } => Self::Kick { player, reason },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn commands_are_tagged_by_type() {
        let send = NetworkCommand::Send {
            player: Uuid::nil(),
            server: "skywars-lobby-1".into(),
        };
        assert_eq!(
            serde_json::to_value(&send).unwrap(),
            json!({"type": "send", "player": Uuid::nil(), "server": "skywars-lobby-1"})
        );
        assert_eq!(
            ProxyCommand::from(send),
            ProxyCommand::Send {
                player: Uuid::nil(),
                server: "skywars-lobby-1".into()
            }
        );
    }
}
