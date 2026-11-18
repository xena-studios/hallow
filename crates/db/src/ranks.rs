//! Ranks, the ranks an account holds, and their combined permission nodes.

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use hallow_proto::permission::has_permission;

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

/// Which of `accounts` hold `node` now, as [`AccountPermissions::has`]
/// decides, in the order given. Only accounts with an unexpired grant of a
/// rank that could grant the node are checked one by one, so a long list
/// of mostly ordinary players (everyone online) costs two queries; unless
/// the default rank grants it, when every account is checked.
pub async fn accounts_with_permission(
    pool: &PgPool,
    accounts: &[Uuid],
    node: &str,
) -> Result<Vec<Uuid>, sqlx::Error> {
    if accounts.is_empty() {
        return Ok(Vec::new());
    }
    let ranks: Vec<Rank> = sqlx::query_as(
        "SELECT name, display_prefix, weight, permissions, is_default FROM hallow.ranks",
    )
    .fetch_all(pool)
    .await?;
    let granting: Vec<&Rank> = ranks
        .iter()
        .filter(|r| {
            r.permissions
                .iter()
                .any(|n| !n.starts_with('-') && hallow_proto::permission::matches_node(n, node))
        })
        .collect();
    if granting.is_empty() {
        return Ok(Vec::new());
    }
    let candidates: Vec<Uuid> = if granting.iter().any(|r| r.is_default) {
        accounts.to_vec()
    } else {
        let names: Vec<&str> = granting.iter().map(|r| r.name.as_str()).collect();
        let holders: Vec<Uuid> = sqlx::query_scalar(
            "SELECT DISTINCT account_uuid FROM hallow.account_ranks
             WHERE account_uuid = ANY($1) AND rank_name = ANY($2)
               AND (expires_at IS NULL OR expires_at > now())",
        )
        .bind(accounts)
        .bind(&names)
        .fetch_all(pool)
        .await?;
        accounts
            .iter()
            .copied()
            .filter(|a| holders.contains(a))
            .collect()
    };
    let mut found = Vec::new();
    for account in candidates {
        if account_permissions(pool, account).await?.has(node) {
            found.push(account);
        }
    }
    Ok(found)
}

/// A rank with how many accounts hold an unexpired grant of it.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct RankWithMembers {
    #[sqlx(flatten)]
    pub rank: Rank,
    pub members: i64,
}

/// Every rank, highest weight first (ties by name), with its member count.
pub async fn list_ranks(pool: &PgPool) -> Result<Vec<RankWithMembers>, sqlx::Error> {
    sqlx::query_as(
        "SELECT r.name, r.display_prefix, r.weight, r.permissions, r.is_default,
                (SELECT count(*) FROM hallow.account_ranks ar
                 WHERE ar.rank_name = r.name
                   AND (ar.expires_at IS NULL OR ar.expires_at > now())) AS members
         FROM hallow.ranks r
         ORDER BY r.weight DESC, r.name",
    )
    .fetch_all(pool)
    .await
}

/// Like [`upsert_rank`], but making the rank the default first takes the
/// flag off the old default, in one transaction.
pub async fn save_rank(pool: &PgPool, rank: &Rank) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    if rank.is_default {
        sqlx::query("UPDATE hallow.ranks SET is_default = false WHERE is_default AND name <> $1")
            .bind(&rank.name)
            .execute(&mut *tx)
            .await?;
    }
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
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// Deletes a rank and every grant of it. Returns whether it existed.
pub async fn delete_rank(pool: &PgPool, name: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM hallow.ranks WHERE name = $1")
        .bind(name)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// A rank an account was given.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct RankGrant {
    pub rank_name: String,
    pub granted_at: DateTime<Utc>,
    pub granted_by: Option<Uuid>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// The account's unexpired grants, highest-weight rank first.
pub async fn rank_grants(pool: &PgPool, account: Uuid) -> Result<Vec<RankGrant>, sqlx::Error> {
    sqlx::query_as(
        "SELECT ar.rank_name, ar.granted_at, ar.granted_by, ar.expires_at
         FROM hallow.account_ranks ar
         JOIN hallow.ranks r ON r.name = ar.rank_name
         WHERE ar.account_uuid = $1 AND (ar.expires_at IS NULL OR ar.expires_at > now())
         ORDER BY r.weight DESC, r.name",
    )
    .bind(account)
    .fetch_all(pool)
    .await
}
