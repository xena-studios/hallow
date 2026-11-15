//! Parties: players who play together. A party lives as long as its members
//! are online; it follows its leader through queues and warps.
//!
//! Every action goes through one endpoint and answers with a [`PartyReply`]
//! for the player who acted. The control plane tells the other members what
//! happened with [`crate::command::ProxyCommand::Message`].

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How many players a party can hold, leader included.
pub const MAX_PARTY_SIZE: usize = 8;
/// How long an invite stays open.
pub const INVITE_SECONDS: u64 = 60;

/// `POST /v1/parties`. `player` is who acts; names are usernames, matched
/// without case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PartyAction {
    /// Invite `target`, creating a party with `player` as leader if needed.
    /// Only the leader can invite.
    Invite { player: Uuid, target: String },
    /// Join the party of `leader`, who must have invited `player`.
    Accept { player: Uuid, leader: String },
    /// Leave the party. A leaving leader hands over to the longest member.
    Leave { player: Uuid },
    /// Remove `target` from the party (leader only).
    Kick { player: Uuid, target: String },
    /// End the party (leader only).
    Disband { player: Uuid },
    /// Bring every member to the leader's server (leader only).
    Warp { player: Uuid },
    /// Message every member.
    Chat { player: Uuid, text: String },
    /// Show the party.
    List { player: Uuid },
}

/// The answer to a [`PartyAction`], shown to the player who acted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartyReply {
    /// False if the action was refused; `message` says why.
    pub ok: bool,
    pub message: String,
}

/// `GET /v1/parties/{uuid}`: the party a player is in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartyInfo {
    pub id: String,
    pub leader: Uuid,
    /// In the order they joined; the leader first.
    pub members: Vec<PartyMember>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartyMember {
    pub uuid: Uuid,
    pub username: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn actions_are_tagged() {
        let invite = PartyAction::Invite {
            player: Uuid::nil(),
            target: "Steve".into(),
        };
        assert_eq!(
            serde_json::to_value(&invite).unwrap(),
            json!({"action": "invite", "player": Uuid::nil(), "target": "Steve"})
        );
    }
}
