//! Matchmaking queues. A player (or a lobby's plugin for them) queues for a
//! game kind; a party queues together, by its leader. The matchmaker puts
//! each entry on a ready game server of that kind with room for all of it,
//! and sends everyone there with [`crate::command::ProxyCommand::Send`].

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `POST /v1/queues`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinQueue {
    /// The game kind (server repo), such as `skywars`.
    pub kind: String,
    /// Who queues. If they lead a party, the whole party queues; if they're
    /// a member but not the leader, the request is refused.
    pub player: Uuid,
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
