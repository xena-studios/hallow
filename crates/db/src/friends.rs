//! Friend requests and friendships.

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

/// The most friends an account can have.
pub const MAX_FRIENDS: i64 = 200;

/// A friend as their list shows them.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct Friend {
    pub uuid: Uuid,
    pub username: String,
    pub since: DateTime<Utc>,
}

/// What a friend request did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestOutcome {
    /// Stored, waiting for the other player.
    Sent,
    /// They had already asked; now you're friends.
    Accepted,
    AlreadyFriends,
    AlreadySent,
    /// One of you has `MAX_FRIENDS`.
    Full,
}

fn ordered(a: Uuid, b: Uuid) -> (Uuid, Uuid) {
    if a < b { (a, b) } else { (b, a) }
}

pub async fn are_friends(pool: &PgPool, a: Uuid, b: Uuid) -> Result<bool, sqlx::Error> {
    let (a, b) = ordered(a, b);
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM hallow.friendships WHERE a_uuid = $1 AND b_uuid = $2)",
    )
    .bind(a)
    .bind(b)
    .fetch_one(pool)
    .await
}

async fn friend_count(pool: &PgPool, account: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM hallow.friendships WHERE a_uuid = $1 OR b_uuid = $1")
        .bind(account)
        .fetch_one(pool)
        .await
}

/// Asks `to` to be friends with `from`. If `to` had already asked `from`,
/// they become friends instead. Both must be accounts that have joined.
pub async fn request_friend(
    pool: &PgPool,
    from: Uuid,
    to: Uuid,
) -> Result<RequestOutcome, sqlx::Error> {
    if are_friends(pool, from, to).await? {
        return Ok(RequestOutcome::AlreadyFriends);
    }
    if friend_count(pool, from).await? >= MAX_FRIENDS
        || friend_count(pool, to).await? >= MAX_FRIENDS
    {
        return Ok(RequestOutcome::Full);
    }
    if accept_friend(pool, from, to).await? {
        return Ok(RequestOutcome::Accepted);
    }
    let inserted = sqlx::query(
        "INSERT INTO hallow.friend_requests (from_uuid, to_uuid) VALUES ($1, $2)
         ON CONFLICT DO NOTHING",
    )
    .bind(from)
    .bind(to)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(if inserted == 1 {
        RequestOutcome::Sent
    } else {
        RequestOutcome::AlreadySent
    })
}

/// `account` accepts the request `requester` sent. Returns false if there
/// was no such request.
pub async fn accept_friend(
    pool: &PgPool,
    account: Uuid,
    requester: Uuid,
) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let removed =
        sqlx::query("DELETE FROM hallow.friend_requests WHERE from_uuid = $1 AND to_uuid = $2")
            .bind(requester)
            .bind(account)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if removed == 0 {
        return Ok(false);
    }
    let (a, b) = ordered(account, requester);
    sqlx::query(
        "INSERT INTO hallow.friendships (a_uuid, b_uuid) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(a)
    .bind(b)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// `account` turns down `requester`'s request. Returns false if there was none.
pub async fn deny_friend(
    pool: &PgPool,
    account: Uuid,
    requester: Uuid,
) -> Result<bool, sqlx::Error> {
    let removed =
        sqlx::query("DELETE FROM hallow.friend_requests WHERE from_uuid = $1 AND to_uuid = $2")
            .bind(requester)
            .bind(account)
            .execute(pool)
            .await?
            .rows_affected();
    Ok(removed > 0)
}

/// Ends a friendship. Returns false if they weren't friends.
pub async fn remove_friend(pool: &PgPool, a: Uuid, b: Uuid) -> Result<bool, sqlx::Error> {
    let (a, b) = ordered(a, b);
    let removed = sqlx::query("DELETE FROM hallow.friendships WHERE a_uuid = $1 AND b_uuid = $2")
        .bind(a)
        .bind(b)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(removed > 0)
}

/// An account's friends, by username.
pub async fn friends_of(pool: &PgPool, account: Uuid) -> Result<Vec<Friend>, sqlx::Error> {
    sqlx::query_as(
        "SELECT a.uuid, a.username, f.since
         FROM hallow.friendships f
         JOIN hallow.accounts a ON a.uuid = CASE WHEN f.a_uuid = $1 THEN f.b_uuid ELSE f.a_uuid END
         WHERE f.a_uuid = $1 OR f.b_uuid = $1
         ORDER BY lower(a.username)",
    )
    .bind(account)
    .fetch_all(pool)
    .await
}

/// Requests waiting for `account`, oldest first. `since` is when they were sent.
pub async fn pending_requests(pool: &PgPool, account: Uuid) -> Result<Vec<Friend>, sqlx::Error> {
    sqlx::query_as(
        "SELECT a.uuid, a.username, r.created_at AS since
         FROM hallow.friend_requests r
         JOIN hallow.accounts a ON a.uuid = r.from_uuid
         WHERE r.to_uuid = $1
         ORDER BY r.created_at",
    )
    .bind(account)
    .fetch_all(pool)
    .await
}
