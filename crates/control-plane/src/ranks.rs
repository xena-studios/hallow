//! Ranks for the panel: list, create or edit and delete them, and give or
//! take them from players. Ranks carry in-game staff power (Vine permission
//! nodes, sent with each login); panel staff get their panel permissions
//! from panel roles. A change reaches an online player at their next join.

use chrono::{DateTime, TimeDelta, Utc};
use hallow_db::{PgPool, Rank};
use hallow_proto::{
    ErrorCode,
    fleet::FieldErrors,
    rank::{
        GrantRank, PlayerRanks, RankGrant, RankInfo, RevokeRank, SaveRank, valid_node,
        valid_rank_name,
    },
};

use crate::{accounts::resolve, error::Failure, store::Store};

type Reply<T> = Result<T, Failure>;

/// The longest grant that isn't permanent: ten years.
const MAX_SECONDS: u64 = 10 * 365 * 24 * 3600;

fn millis(at: DateTime<Utc>) -> u64 {
    u64::try_from(at.timestamp_millis()).unwrap_or(0)
}

fn info(rank: hallow_db::RankWithMembers) -> RankInfo {
    RankInfo {
        name: rank.rank.name,
        display_prefix: rank.rank.display_prefix,
        weight: rank.rank.weight,
        permissions: rank.rank.permissions,
        is_default: rank.rank.is_default,
        members: u64::try_from(rank.members).unwrap_or(0),
    }
}

pub async fn list(pool: &PgPool) -> Reply<Vec<RankInfo>> {
    Ok(hallow_db::list_ranks(pool)
        .await?
        .into_iter()
        .map(info)
        .collect())
}

fn no_such_rank(name: &str) -> Failure {
    Failure::not_found(format!("there is no rank called {name}"))
}

/// Creates or replaces the rank `name`. Nodes are trimmed, lowercased and
/// deduplicated; a bad name or node is a `400` naming the field.
pub async fn save(pool: &PgPool, name: &str, save: SaveRank) -> Reply<RankInfo> {
    let mut fields = FieldErrors::new();
    if !valid_rank_name(name) {
        fields.insert(
            "name".into(),
            "must be 1 to 32 lowercase letters, digits, '_' or '-'".into(),
        );
    }
    let mut permissions: Vec<String> = Vec::new();
    for node in &save.permissions {
        let node = node.trim().to_ascii_lowercase();
        if node.is_empty() {
            continue;
        }
        if !valid_node(&node) {
            fields.insert(
                "permissions".into(),
                format!(
                    "{node:?} isn't a permission node, such as hallow.staff.ban or hallow.staff.*"
                ),
            );
        } else if !permissions.contains(&node) {
            permissions.push(node);
        }
    }
    // Kept as typed: a trailing space separates the prefix from the name.
    let prefix = save.display_prefix.replace('§', "&");
    if prefix.chars().count() > 64 {
        fields.insert(
            "display_prefix".into(),
            "must be at most 64 characters".into(),
        );
    }
    if !fields.is_empty() {
        return Err(Failure::invalid_fields("the rank isn't valid", fields));
    }
    hallow_db::save_rank(
        pool,
        &Rank {
            name: name.to_owned(),
            display_prefix: prefix,
            weight: save.weight,
            permissions,
            is_default: save.is_default,
        },
    )
    .await?;
    tracing::info!(rank = name, "rank saved");
    hallow_db::list_ranks(pool)
        .await?
        .into_iter()
        .find(|r| r.rank.name == name)
        .map(info)
        .ok_or_else(|| no_such_rank(name))
}

/// Deletes a rank and its grants. The default rank can't go: every player
/// holds it.
pub async fn delete(pool: &PgPool, name: &str) -> Reply<()> {
    let ranks = hallow_db::list_ranks(pool).await?;
    let Some(rank) = ranks.iter().find(|r| r.rank.name == name) else {
        return Err(no_such_rank(name));
    };
    if rank.rank.is_default {
        return Err(Failure::new(
            ErrorCode::Conflict,
            "the default rank can't be deleted; make another rank the default first",
        ));
    }
    hallow_db::delete_rank(pool, name).await?;
    tracing::info!(rank = name, "rank deleted");
    Ok(())
}

/// A player's ranks by name; `None` if nobody by that name has joined.
pub async fn of_player(pool: &PgPool, store: &Store, name: &str) -> Reply<Option<PlayerRanks>> {
    let Some((uuid, username)) = resolve(pool, store, name).await? else {
        return Ok(None);
    };
    let grants = hallow_db::rank_grants(pool, uuid)
        .await?
        .into_iter()
        .map(|g| RankGrant {
            rank: g.rank_name,
            granted_ms: millis(g.granted_at),
            expires_ms: g.expires_at.map(millis),
        })
        .collect();
    let held = hallow_db::account_permissions(pool, uuid).await?;
    Ok(Some(PlayerRanks {
        uuid,
        username,
        grants,
        ranks: held.ranks.into_iter().map(|r| r.name).collect(),
        permissions: held.nodes,
    }))
}

fn nobody(name: &str) -> Failure {
    Failure::not_found(format!("nobody called {name} has joined"))
}

pub async fn grant(pool: &PgPool, store: &Store, req: GrantRank) -> Reply<PlayerRanks> {
    let Some((uuid, _)) = resolve(pool, store, &req.player).await? else {
        return Err(nobody(&req.player));
    };
    let ranks = hallow_db::list_ranks(pool).await?;
    let Some(rank) = ranks.iter().find(|r| r.rank.name == req.rank) else {
        return Err(no_such_rank(&req.rank));
    };
    if rank.rank.is_default {
        return Err(Failure::bad_request(
            "everyone holds the default rank already",
        ));
    }
    let expires_at = match req.duration_seconds {
        Some(0) => return Err(Failure::bad_request("duration_seconds must be positive")),
        Some(seconds) => Some(
            Utc::now() + TimeDelta::seconds(i64::try_from(seconds.min(MAX_SECONDS)).unwrap_or(0)),
        ),
        None => None,
    };
    hallow_db::grant_rank(pool, uuid, &req.rank, None, expires_at).await?;
    tracing::info!(rank = req.rank, %uuid, staff = ?req.staff.map(|s| s.name), "rank granted");
    of_player(pool, store, &req.player)
        .await?
        .ok_or_else(|| nobody(&req.player))
}

pub async fn revoke(pool: &PgPool, store: &Store, req: RevokeRank) -> Reply<PlayerRanks> {
    let Some((uuid, _)) = resolve(pool, store, &req.player).await? else {
        return Err(nobody(&req.player));
    };
    if !hallow_db::revoke_rank(pool, uuid, &req.rank).await? {
        return Err(Failure::not_found(format!(
            "{} doesn't hold {}",
            req.player, req.rank
        )));
    }
    tracing::info!(rank = req.rank, %uuid, staff = ?req.staff.map(|s| s.name), "rank revoked");
    of_player(pool, store, &req.player)
        .await?
        .ok_or_else(|| nobody(&req.player))
}
