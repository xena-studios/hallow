//! Ranks, the ranks an account holds, and their combined permission nodes.

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::permissions::has_permission;

/// A rank: a display prefix, a weight and permission nodes in Vine's format.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct Rank {
    /// Lowercase letters, digits, `_` and `-`, at most 32.
    pub name: String,
    pub display_prefix: String,
    /// Higher wins, for display order and the primary rank.
    pub weight: i32,
    /// Nodes such as `hallow.staff.find`, `hallow.staff.*`, `*` or a denial
    /// like `-hallow.staff.ban`; lowercase.
    pub permissions: Vec<String>,
    /// Held by every account without a grant.
    pub is_default: bool,
}

/// Creates the rank, or replaces its prefix, weight, nodes and default flag.
/// The database rejects malformed names and nodes, and a second default.
pub async fn upsert_rank(pool: &PgPool, rank: &Rank) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO hallow.ranks (name, display_prefix, weight, permissions, is_default)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (name) DO UPDATE SET
            display_prefix = EXCLUDED.display_prefix,
            weight = EXCLUDED.weight,
            permissions = EXCLUDED.permissions,
            is_default = EXCLUDED.is_default",
    )
    .bind(&rank.name)
    .bind(&rank.display_prefix)
    .bind(rank.weight)
    .bind(&rank.permissions)
    .bind(rank.is_default)
    .execute(pool)
    .await?;
    Ok(())
}

/// Gives the account a rank until `expires_at` (`None`: permanently). Granting
/// a rank the account already holds replaces the expiry and granter.
pub async fn grant_rank(
    pool: &PgPool,
    account: Uuid,
    rank: &str,
    granted_by: Option<Uuid>,
    expires_at: Option<DateTime<Utc>>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO hallow.account_ranks (account_uuid, rank_name, granted_by, expires_at)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (account_uuid, rank_name) DO UPDATE SET
            granted_at = now(),
            granted_by = EXCLUDED.granted_by,
            expires_at = EXCLUDED.expires_at",
    )
    .bind(account)
    .bind(rank)
    .bind(granted_by)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Takes a rank away. Returns whether the account held it.
pub async fn revoke_rank(pool: &PgPool, account: Uuid, rank: &str) -> Result<bool, sqlx::Error> {
    let result =
        sqlx::query("DELETE FROM hallow.account_ranks WHERE account_uuid = $1 AND rank_name = $2")
            .bind(account)
            .bind(rank)
            .execute(pool)
            .await?;
    Ok(result.rows_affected() > 0)
}

/// The account's ranks right now: unexpired grants plus the default rank,
/// highest weight first (ties by name). Works for an account that has never
/// joined too, which then holds only the default rank.
pub async fn active_ranks(pool: &PgPool, account: Uuid) -> Result<Vec<Rank>, sqlx::Error> {
    sqlx::query_as(
        "SELECT name, display_prefix, weight, permissions, is_default
         FROM hallow.ranks r
         WHERE r.is_default
            OR EXISTS (
                SELECT 1 FROM hallow.account_ranks ar
                WHERE ar.account_uuid = $1
                  AND ar.rank_name = r.name
                  AND (ar.expires_at IS NULL OR ar.expires_at > now())
            )
         ORDER BY weight DESC, name",
    )
    .bind(account)
    .fetch_all(pool)
    .await
}

/// An account's active ranks and the nodes they grant together.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountPermissions {
    /// Highest weight first; the first is the primary rank.
    pub ranks: Vec<Rank>,
    /// Every rank's nodes, highest-weight rank first, without duplicates.
    pub nodes: Vec<String>,
}

impl AccountPermissions {
    /// The highest-weight rank, whose prefix the player shows.
    pub fn primary(&self) -> Option<&Rank> {
        self.ranks.first()
    }

    /// Whether the nodes grant `node`, by Vine's rules: a denial in any
    /// rank wins over a grant in any other.
    pub fn has(&self, node: &str) -> bool {
        has_permission(&self.nodes, node)
    }
}

/// The account's active ranks and their combined nodes, for login.
pub async fn account_permissions(
    pool: &PgPool,
    account: Uuid,
) -> Result<AccountPermissions, sqlx::Error> {
    let ranks = active_ranks(pool, account).await?;
    let mut nodes: Vec<String> = Vec::new();
    for node in ranks.iter().flat_map(|r| &r.permissions) {
        if !nodes.contains(node) {
            nodes.push(node.clone());
        }
    }
    Ok(AccountPermissions { ranks, nodes })
}
