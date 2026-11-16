//! Who joined and left since the last report, for
//! `POST /v1/servers/{name}/players`. Pure: [`crate::players`] feeds it from
//! Pumpkin's join and leave events, and the repeating task takes it.
//!
//! The heartbeat carries everyone on the server, which covers whatever is
//! pending, so the task [clears](Changes::clear) it when it heartbeats.

use std::collections::{BTreeMap, BTreeSet};

use hallow_proto::server::{ServerPlayer, ServerPlayersChanged};
use uuid::Uuid;

#[derive(Debug, Default)]
pub struct Changes {
    joined: BTreeMap<Uuid, String>,
    left: BTreeSet<Uuid>,
}

impl Changes {
    pub fn joined(&mut self, player: Uuid, username: String) {
        self.left.remove(&player);
        self.joined.insert(player, username);
    }

    /// A leave cancels a join not yet reported, and is reported anyway in
    /// case an earlier report of the join got through.
    pub fn left(&mut self, player: Uuid) {
        self.joined.remove(&player);
        self.left.insert(player);
    }

    pub fn is_empty(&self) -> bool {
        self.joined.is_empty() && self.left.is_empty()
    }

    pub fn clear(&mut self) {
        self.joined.clear();
        self.left.clear();
    }

    /// What's pending, emptying it; `None` if nothing is.
    pub fn take(&mut self) -> Option<ServerPlayersChanged> {
        if self.is_empty() {
            return None;
        }
        let joined = std::mem::take(&mut self.joined)
            .into_iter()
            .map(|(uuid, username)| ServerPlayer { uuid, username })
            .collect();
        let left = std::mem::take(&mut self.left).into_iter().collect();
        Some(ServerPlayersChanged { joined, left })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_and_leaves_coalesce() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut changes = Changes::default();
        assert!(changes.take().is_none());

        changes.joined(a, "alex".into());
        changes.joined(b, "steve".into());
        changes.left(b);
        let taken = changes.take().unwrap();
        assert_eq!(
            taken.joined,
            [ServerPlayer {
                uuid: a,
                username: "alex".into()
            }]
        );
        assert_eq!(taken.left, [b]);
        assert!(changes.is_empty());

        // Leaving and coming back before a report is a join.
        changes.left(a);
        changes.joined(a, "alex".into());
        let taken = changes.take().unwrap();
        assert_eq!(taken.joined.len(), 1);
        assert!(taken.left.is_empty());

        changes.joined(b, "steve".into());
        changes.clear();
        assert!(changes.take().is_none());
    }
}
