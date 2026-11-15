//! Accounts and their username history.

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::decode_error;

/// The client a player joins with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Edition {
    Java,
    Bedrock,
}

impl Edition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Java => "java",
            Self::Bedrock => "bedrock",
        }
    }

    fn parse(value: &str) -> Result<Self, sqlx::Error> {
        match value {
            "java" => Ok(Self::Java),
            "bedrock" => Ok(Self::Bedrock),
            other => Err(decode_error("edition", other)),
        }
    }
}

/// A player who has joined the network at least once.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub uuid: Uuid,
    pub username: String,
    pub edition: Edition,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub last_ip: Option<IpAddr>,
}

#[derive(FromRow)]
struct AccountRow {
    uuid: Uuid,
    username: String,
    edition: String,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
    last_ip: Option<IpAddr>,
}

impl TryFrom<AccountRow> for Account {
    type Error = sqlx::Error;

    fn try_from(row: AccountRow) -> Result<Self, Self::Error> {
        Ok(Self {
            uuid: row.uuid,
            username: row.username,
            edition: Edition::parse(&row.edition)?,
            first_seen: row.first_seen,
            last_seen: row.last_seen,
            last_ip: row.last_ip,
        })
    }
}

macro_rules! account_columns {
    () => {
        "uuid, username, edition, first_seen, last_seen, last_ip"
    };
}

/// What the proxy knows about a player at login.
#[derive(Clone, Debug)]
pub struct Login<'a> {
    pub uuid: Uuid,
    pub username: &'a str,
    pub edition: Edition,
    pub ip: Option<IpAddr>,
}

/// Records a login: creates the account on first join, otherwise updates its
/// username, edition, last-seen time and IP (an unknown IP keeps the old
/// one), and adds the username to the account's history.
pub async fn upsert_account(pool: &PgPool, login: &Login<'_>) -> Result<Account, sqlx::Error> {
    // Foreign keys are checked at the end of the statement, so the history
    // row can reference the account inserted by the same statement.
    let row: AccountRow = sqlx::query_as(
        "WITH account AS (
            INSERT INTO hallow.accounts (uuid, username, edition, last_ip)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (uuid) DO UPDATE SET
                username = EXCLUDED.username,
                edition = EXCLUDED.edition,
                last_seen = greatest(now(), hallow.accounts.last_seen),
                last_ip = coalesce(EXCLUDED.last_ip, hallow.accounts.last_ip)
            RETURNING uuid, username, edition, first_seen, last_seen, last_ip
        ), history AS (
            INSERT INTO hallow.account_usernames (account_uuid, username)
            SELECT uuid, username FROM account
            ON CONFLICT (account_uuid, username) DO UPDATE SET last_used = now()
        )
        SELECT uuid, username, edition, first_seen, last_seen, last_ip FROM account",
    )
    .bind(login.uuid)
    .bind(login.username)
    .bind(login.edition.as_str())
    .bind(login.ip)
    .fetch_one(pool)
    .await?;
    row.try_into()
}

/// The account with this UUID, if it has ever joined.
pub async fn get_account(pool: &PgPool, uuid: Uuid) -> Result<Option<Account>, sqlx::Error> {
    sqlx::query_as::<_, AccountRow>(concat!(
        "SELECT ",
        account_columns!(),
        " FROM hallow.accounts WHERE uuid = $1"
    ))
    .bind(uuid)
    .fetch_optional(pool)
    .await?
    .map(Account::try_from)
    .transpose()
}

/// Accounts whose current username is `username`, ignoring case, most
/// recently seen first. Usually one; more only when a name changed hands
/// and its old owner hasn't logged in since.
pub async fn find_accounts_by_username(
    pool: &PgPool,
    username: &str,
) -> Result<Vec<Account>, sqlx::Error> {
    sqlx::query_as::<_, AccountRow>(concat!(
        "SELECT ",
        account_columns!(),
        " FROM hallow.accounts
         WHERE lower(username) = lower($1)
         ORDER BY last_seen DESC"
    ))
    .bind(username)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(Account::try_from)
    .collect()
}

/// A username an account has used.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct PastUsername {
    pub username: String,
    pub first_used: DateTime<Utc>,
    pub last_used: DateTime<Utc>,
}

/// Every username the account has used, the current one included, most
/// recently used first.
pub async fn username_history(pool: &PgPool, uuid: Uuid) -> Result<Vec<PastUsername>, sqlx::Error> {
    sqlx::query_as(
        "SELECT username, first_used, last_used FROM hallow.account_usernames
         WHERE account_uuid = $1
         ORDER BY last_used DESC",
    )
    .bind(uuid)
    .fetch_all(pool)
    .await
}
