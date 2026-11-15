//! Sessions: one row per connection to the network.

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

/// A session to open at login.
#[derive(Clone, Debug)]
pub struct NewSession<'a> {
    /// Must be an account that has joined (call `upsert_account` first).
    pub account: Uuid,
    /// The proxy's id, as registered with the control plane.
    pub proxy: &'a str,
    /// Java protocol version; `None` for Bedrock through Geyser.
    pub protocol_version: Option<i32>,
    pub ip: Option<IpAddr>,
}

#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct Session {
    pub id: Uuid,
    pub account_uuid: Uuid,
    pub proxy: String,
    pub last_server: Option<String>,
    pub protocol_version: Option<i32>,
    pub ip: Option<IpAddr>,
    pub login_at: DateTime<Utc>,
    /// `None` while the session is open.
    pub logout_at: Option<DateTime<Utc>>,
}

macro_rules! columns {
    () => {
        "id, account_uuid, proxy, last_server, protocol_version, ip, login_at, logout_at"
    };
}

/// Opens a session at login.
pub async fn open_session(pool: &PgPool, new: &NewSession<'_>) -> Result<Session, sqlx::Error> {
    sqlx::query_as(concat!(
        "INSERT INTO hallow.sessions (account_uuid, proxy, protocol_version, ip)
         VALUES ($1, $2, $3, $4)
         RETURNING ",
        columns!(),
        ""
    ))
    .bind(new.account)
    .bind(new.proxy)
    .bind(new.protocol_version)
    .bind(new.ip)
    .fetch_one(pool)
    .await
}

/// Records the server an open session moved to. Returns whether the
/// session was open.
pub async fn set_session_server(
    pool: &PgPool,
    session: Uuid,
    server: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE hallow.sessions SET last_server = $2 WHERE id = $1 AND logout_at IS NULL",
    )
    .bind(session)
    .bind(server)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Closes a session at logout. Returns the closed session, or `None` if it
/// doesn't exist or was already closed.
pub async fn close_session(pool: &PgPool, session: Uuid) -> Result<Option<Session>, sqlx::Error> {
    sqlx::query_as(concat!(
        "UPDATE hallow.sessions SET logout_at = greatest(now(), login_at)
         WHERE id = $1 AND logout_at IS NULL
         RETURNING ",
        columns!(),
        ""
    ))
    .bind(session)
    .fetch_optional(pool)
    .await
}

/// Closes every open session on a proxy, for when the proxy goes away
/// without reporting its players leaving. Returns how many were closed.
pub async fn close_proxy_sessions(pool: &PgPool, proxy: &str) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE hallow.sessions SET logout_at = greatest(now(), login_at)
         WHERE proxy = $1 AND logout_at IS NULL",
    )
    .bind(proxy)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}
