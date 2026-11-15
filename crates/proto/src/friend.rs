//! Friends: kept in PostgreSQL, so these need a control plane with a
//! database. Like parties, every action goes through one endpoint and
//! answers with a message for the player who acted; the other player is told
//! through their proxy if they're online. Friends are told when each other
//! join or leave the network.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `POST /v1/friends`. `player` is who acts, and must be online; `target` is
/// a username, online or not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum FriendAction {
    /// Ask to be friends; accepts instead if they already asked you.
    Add {
        player: Uuid,
        target: String,
    },
    Accept {
        player: Uuid,
        target: String,
    },
    Deny {
        player: Uuid,
        target: String,
    },
    Remove {
        player: Uuid,
        target: String,
    },
    /// Your friends, with who's online and where.
    List {
        player: Uuid,
    },
    /// Requests waiting for you.
    Requests {
        player: Uuid,
    },
}

/// The answer to a [`FriendAction`]; `message` may span several lines.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FriendReply {
    pub ok: bool,
    pub message: String,
}
