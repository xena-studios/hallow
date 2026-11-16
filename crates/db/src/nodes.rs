//! One-time node join tokens and the nodes that joined with them.
//!
//! Tokens are looked up by their SHA-256 (the caller hashes); the token
//! itself is never stored. A join is two steps so a failure in between can
//! be retried with the same token:
//!
//! 1. [`claim_join_token`] takes an unused, unexpired, unclaimed token for a
//!    short lease, atomically, so two joins racing one token can't both win;
//! 2. [`finish_join`] records the node and marks the token used in one
//!    transaction, or [`release_join_token`] gives the claim back.

use std::collections::BTreeMap;
use std::net::IpAddr;

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool, types::Json};
use uuid::Uuid;

/// A token to store.
#[derive(Clone, Debug)]
pub struct NewJoinToken<'a> {
    pub id: Uuid,
    /// SHA-256 of the token.
    pub token_hash: &'a [u8; 32],
    /// `base` or `burst`.
    pub role: &'a str,
    pub region: &'a str,
    pub name: Option<&'a str>,
    pub labels: &'a BTreeMap<String, String>,
    pub expires_at: DateTime<Utc>,
}

/// A stored token, without its hash.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct JoinToken {
    pub id: Uuid,
    pub role: String,
    pub region: String,
    pub name: Option<String>,
    pub labels: Json<BTreeMap<String, String>>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
}

macro_rules! token_columns {
    () => {
        "id, role, region, name, labels, created_at, expires_at, used_at"
    };
}

/// A node that joined.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct Node {
    pub name: String,
    pub panel_uuid: Uuid,
    pub role: String,
    pub region: String,
    pub labels: Json<BTreeMap<String, String>>,
    pub private_ip: IpAddr,
    pub public_ip: Option<IpAddr>,
    pub wireguard_public_key: Option<String>,
    pub join_token_id: Option<Uuid>,
    pub joined_at: DateTime<Utc>,
}

macro_rules! node_columns {
    () => {
        "name, panel_uuid, role, region, labels, private_ip, public_ip, wireguard_public_key, join_token_id, joined_at"
    };
}

/// A node to record at the end of a join.
#[derive(Clone, Debug)]
pub struct NewNode<'a> {
    pub name: &'a str,
    pub panel_uuid: Uuid,
    pub role: &'a str,
    pub region: &'a str,
    pub labels: &'a BTreeMap<String, String>,
    pub private_ip: IpAddr,
    pub public_ip: Option<IpAddr>,
    pub wireguard_public_key: Option<&'a str>,
}

/// Why a join couldn't be recorded.
#[derive(Debug, thiserror::Error)]
pub enum FinishJoinError {
    /// The claim ran out and the token went to someone else, or it was used.
    #[error("the join token is no longer claimed by this join")]
    LostClaim,
    /// A node with this name, panel UUID or WireGuard key already joined.
    #[error("a node with this name or WireGuard key already joined")]
    Duplicate,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

pub async fn add_join_token(
    pool: &PgPool,
    new: &NewJoinToken<'_>,
) -> Result<JoinToken, sqlx::Error> {
    sqlx::query_as(concat!(
        "INSERT INTO hallow.node_join_tokens (id, token_hash, role, region, name, labels, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING ",
        token_columns!()
    ))
    .bind(new.id)
    .bind(&new.token_hash[..])
    .bind(new.role)
    .bind(new.region)
    .bind(new.name)
    .bind(Json(new.labels))
    .bind(new.expires_at)
    .fetch_one(pool)
    .await
}

/// Claims the token with this hash until `now + lease`, if it is unused,
/// unexpired and not claimed by another join. `None` if not.
pub async fn claim_join_token(
    pool: &PgPool,
    token_hash: &[u8; 32],
    now: DateTime<Utc>,
    lease: chrono::TimeDelta,
) -> Result<Option<JoinToken>, sqlx::Error> {
    sqlx::query_as(concat!(
        "UPDATE hallow.node_join_tokens SET claimed_until = $3
         WHERE token_hash = $1 AND used_at IS NULL AND expires_at > $2
           AND (claimed_until IS NULL OR claimed_until <= $2)
         RETURNING ",
        token_columns!()
    ))
    .bind(&token_hash[..])
    .bind(now)
    .bind(now + lease)
    .fetch_optional(pool)
    .await
}

/// Gives a claim back after a failed join, so the token can be used again.
pub async fn release_join_token(pool: &PgPool, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE hallow.node_join_tokens SET claimed_until = NULL
         WHERE id = $1 AND used_at IS NULL",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Records the node and uses the token up, in one transaction. Fails with
/// [`FinishJoinError::LostClaim`] if the claim expired before this (the
/// caller should then undo what it did in the panel).
pub async fn finish_join(
    pool: &PgPool,
    token_id: Uuid,
    node: &NewNode<'_>,
    now: DateTime<Utc>,
) -> Result<Node, FinishJoinError> {
    let mut tx = pool.begin().await?;
    let used = sqlx::query(
        "UPDATE hallow.node_join_tokens SET used_at = $2, claimed_until = NULL
         WHERE id = $1 AND used_at IS NULL AND claimed_until > $2",
    )
    .bind(token_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    if used.rows_affected() != 1 {
        return Err(FinishJoinError::LostClaim);
    }
    let row = sqlx::query_as(concat!(
        "INSERT INTO hallow.nodes (name, panel_uuid, role, region, labels, private_ip, public_ip,
                                   wireguard_public_key, join_token_id, joined_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         RETURNING ",
        node_columns!()
    ))
    .bind(node.name)
    .bind(node.panel_uuid)
    .bind(node.role)
    .bind(node.region)
    .bind(Json(node.labels))
    .bind(node.private_ip)
    .bind(node.public_ip)
    .bind(node.wireguard_public_key)
    .bind(token_id)
    .bind(now)
    .fetch_one(&mut *tx)
    .await;
    let row = match row {
        Ok(row) => row,
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(FinishJoinError::Duplicate);
        }
        Err(e) => return Err(e.into()),
    };
    tx.commit().await?;
    Ok(row)
}

/// Whether a node with this name already joined.
pub async fn node_exists(pool: &PgPool, name: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM hallow.nodes WHERE name = $1)")
        .bind(name)
        .fetch_one(pool)
        .await
}

/// Every node that joined, oldest first.
pub async fn list_nodes(pool: &PgPool) -> Result<Vec<Node>, sqlx::Error> {
    sqlx::query_as(concat!(
        "SELECT ",
        node_columns!(),
        " FROM hallow.nodes ORDER BY joined_at, name"
    ))
    .fetch_all(pool)
    .await
}

/// Deletes tokens that expired before `before`, used or not. Returns how
/// many went.
pub async fn delete_join_tokens_before(
    pool: &PgPool,
    before: DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    Ok(
        sqlx::query("DELETE FROM hallow.node_join_tokens WHERE expires_at < $1")
            .bind(before)
            .execute(pool)
            .await?
            .rows_affected(),
    )
}
