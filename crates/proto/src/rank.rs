//! Ranks: a prefix, a weight and permission nodes in Vine's format
//! ([`permission`](crate::permission)). They are where in-game staff power
//! comes from; panel staff get their panel permissions from panel roles
//! instead. A player's nodes are sent with their login, so a change reaches
//! a player the next time they join.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::punishment::StaffMember;

/// One line of `GET /v1/ranks`, highest weight first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankInfo {
    pub name: String,
    /// With legacy `&` colour codes.
    pub display_prefix: String,
    /// Higher wins, for display order and the primary rank.
    pub weight: i32,
    pub permissions: Vec<String>,
    /// Held by every account without a grant.
    pub is_default: bool,
    /// Accounts holding an unexpired grant (the default rank's holders
    /// aren't counted).
    pub members: u64,
}

/// `PUT /v1/ranks/{name}`: creates the rank or replaces it. Making a rank
/// the default takes the flag off the old default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveRank {
    #[serde(default)]
    pub display_prefix: String,
    #[serde(default)]
    pub weight: i32,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub is_default: bool,
}

/// A rank an account was given.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankGrant {
    pub rank: String,
    pub granted_ms: u64,
    /// `None` is permanent.
    pub expires_ms: Option<u64>,
}

/// `GET /v1/rank-grants?name=`: an account's ranks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerRanks {
    pub uuid: Uuid,
    pub username: String,
    /// Unexpired grants, highest-weight rank first.
    pub grants: Vec<RankGrant>,
    /// Every rank the account holds now, the default included, highest
    /// weight first.
    pub ranks: Vec<String>,
    /// The nodes those ranks grant together.
    pub permissions: Vec<String>,
}

/// `POST /v1/rank-grants`: give a player a rank. Giving one they hold
/// replaces its expiry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantRank {
    /// The player's username, matched like a punishment target.
    pub player: String,
    pub rank: String,
    /// `None` is permanent.
    #[serde(default)]
    pub duration_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staff: Option<StaffMember>,
}

/// `POST /v1/rank-grants/revoke`: take a rank away.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokeRank {
    pub player: String,
    pub rank: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staff: Option<StaffMember>,
}

/// Whether `name` can name a rank: 1 to 32 of `a-z 0-9 _ -`.
#[must_use]
pub fn valid_rank_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

/// Whether `node` is a permission node the database accepts: `*`, or
/// dot-separated lowercase segments of `a-z 0-9 _ -`, optionally ending in
/// `.*`, optionally denied with a leading `-`.
#[must_use]
pub fn valid_node(node: &str) -> bool {
    let node = node.strip_prefix('-').unwrap_or(node);
    if node == "*" {
        return true;
    }
    let node = node.strip_suffix(".*").unwrap_or(node);
    !node.is_empty()
        && node.split('.').all(|segment| {
            !segment.is_empty()
                && segment.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-')
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_nodes() {
        assert!(valid_rank_name("mod"));
        assert!(valid_rank_name("senior-mod_2"));
        assert!(!valid_rank_name(""));
        assert!(!valid_rank_name("Mod"));
        assert!(!valid_rank_name(&"a".repeat(33)));
        for good in [
            "*",
            "-*",
            "hallow.staff.ban",
            "hallow.staff.*",
            "-hallow.staff.ban",
            "a",
        ] {
            assert!(valid_node(good), "{good}");
        }
        for bad in [
            "",
            "-",
            "Hallow.staff",
            "hallow..ban",
            "hallow.",
            ".*",
            "hallow.*.ban",
            "a b",
        ] {
            assert!(!valid_node(bad), "{bad}");
        }
    }
}
