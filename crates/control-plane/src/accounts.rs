//! What the control plane keeps in PostgreSQL (`hallow-db`): accounts, bans,
//! ranks and sessions. Only used when `database_url` is set; then it fails
//! closed, because a login that can't be checked for a ban isn't let in.

use std::net::IpAddr;

use chrono::Utc;
use hallow_db::{Edition, Login, NewSession, PgPool, Punishment};
use hallow_proto::{presence::LoginRequest, punishment::MuteInfo};
use uuid::Uuid;

pub type DbResult<T> = Result<T, hallow_db::sqlx::Error>;

/// The rank `admins` from the config get: everything, highest weight.
pub const ADMIN_RANK: &str = "admin";

/// What an allowed login carries from the database.
pub struct Grant {
    pub permissions: Vec<String>,
    pub mute: Option<MuteInfo>,
}

/// Records the login and returns the player's permissions and mute, or the
/// reason they're refused (a ban).
pub async fn check_login(
    pool: &PgPool,
    req: &LoginRequest,
    admins: &[String],
) -> DbResult<Result<Grant, String>> {
    let ip = req.ip.as_deref().and_then(|ip| ip.parse::<IpAddr>().ok());
    let account = hallow_db::upsert_account(
        pool,
        &Login {
            uuid: req.uuid,
            username: &req.username,
            edition: if req.protocol.is_some() {
                Edition::Java
            } else {
                Edition::Bedrock
            },
            ip,
        },
    )
    .await?;
    if let Some(ban) = hallow_db::active_ban(pool, account.uuid).await? {
        return Ok(Err(ban_message(&ban)));
    }
    if admins.iter().any(|a| a.eq_ignore_ascii_case(&req.username)) {
        ensure_admin(pool, account.uuid).await?;
    }
    let permissions = hallow_db::account_permissions(pool, account.uuid)
        .await?
        .nodes;
    let mute = hallow_db::active_mute(pool, account.uuid)
        .await?
        .as_ref()
        .map(crate::punish::mute_info);
    Ok(Ok(Grant { permissions, mute }))
}

/// Gives the account the admin rank, creating the rank (`*`) the first time.
/// An existing admin rank is left as staff edited it.
async fn ensure_admin(pool: &PgPool, account: Uuid) -> DbResult<()> {
    hallow_db::sqlx::query(
        "INSERT INTO hallow.ranks (name, display_prefix, weight, permissions, is_default)
         VALUES ($1, '&c[ADMIN] ', 1000, ARRAY['*'], false)
         ON CONFLICT (name) DO NOTHING",
    )
    .bind(ADMIN_RANK)
    .execute(pool)
    .await?;
    let held = hallow_db::active_ranks(pool, account).await?;
    if !held.iter().any(|r| r.name == ADMIN_RANK) {
        hallow_db::grant_rank(pool, account, ADMIN_RANK, None, None).await?;
        tracing::info!(%account, "granted the admin rank from HALLOW_ADMINS");
    }
    Ok(())
}

/// Opens the session for an allowed login; returns its id.
pub async fn open_session(pool: &PgPool, req: &LoginRequest) -> DbResult<Uuid> {
    let ip = req.ip.as_deref().and_then(|ip| ip.parse::<IpAddr>().ok());
    let session = hallow_db::open_session(
        pool,
        &NewSession {
            account: req.uuid,
            proxy: &req.proxy,
            protocol_version: req.protocol,
            ip,
        },
    )
    .await?;
    Ok(session.id)
}

/// What a banned player sees.
pub fn ban_message(ban: &Punishment) -> String {
    let until = match ban.expires_at {
        None => "§7This ban is permanent.".to_owned(),
        Some(at) => format!("§7It ends in §f{}§7.", remaining(at - Utc::now())),
    };
    format!(
        "§cYou are banned from this network.\n§7Reason: §f{}\n{until}",
        ban.reason
    )
}

/// `3d 4h`, `5h 10m`, `12m`: the two largest units, rounded down.
pub fn remaining(left: chrono::TimeDelta) -> String {
    let minutes = left.num_minutes().max(1);
    let (days, hours, mins) = (minutes / 1440, (minutes / 60) % 24, minutes % 60);
    match (days, hours) {
        (0, 0) => format!("{mins}m"),
        (0, _) => format!("{hours}h {mins}m"),
        _ => format!("{days}d {hours}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    #[test]
    fn remaining_shows_two_units() {
        assert_eq!(remaining(TimeDelta::seconds(30)), "1m");
        assert_eq!(remaining(TimeDelta::minutes(12)), "12m");
        assert_eq!(remaining(TimeDelta::minutes(310)), "5h 10m");
        assert_eq!(remaining(TimeDelta::hours(76)), "3d 4h");
    }
}
