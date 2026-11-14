//! Matchmaking queues. A lobby's plugin queues players (alone or as a party)
//! for a game kind; the matchmaker starts or fills a game and sends them there
//! with a [`crate::command::ProxyCommand::Send`].

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `POST /v1/queues`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinQueue {
    /// The game kind (server repo), such as `skywars`.
    pub kind: String,
    /// The player who queued; with a party, its leader.
    pub player: Uuid,
    /// Everyone who plays together, including `player`.
    pub party: Vec<Uuid>,
}

/// The answer to [`JoinQueue`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueStatus {
    pub kind: String,
    /// Players queued for this kind, including this entry.
    pub queued: u32,
    /// Rough wait, if the matchmaker can tell.
    pub estimated_wait_seconds: Option<u32>,
}
