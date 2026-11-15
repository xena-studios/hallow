//! Issuing and lifting punishments, and enforcing them on the proxy that
//! holds the player: a ban or kick disconnects them, a mute is pushed so the
//! proxy blocks their chat.

use chrono::{DateTime, TimeDelta, Utc};
use hallow_db::{Actor, NewPunishment, PgPool, Punishment};
use hallow_proto::{
    command::ProxyCommand,
    punishment::{
        IssuePunishment, MuteInfo, PunishmentKind, PunishmentRecord, PunishmentReply,
        RevokePunishment,
    },
};
use uuid::Uuid;

use crate::{
    accounts::{DbResult, ban_message, remaining, resolve},
    store::Store,
};

/// The longest punishment that isn't permanent: ten years.
const MAX_SECONDS: u64 = 10 * 365 * 24 * 3600;

fn ok(message: impl Into<String>) -> PunishmentReply {
    PunishmentReply {
        ok: true,
        message: message.into(),
    }
}

fn refuse(message: impl Into<String>) -> PunishmentReply {
    PunishmentReply {
        ok: false,
        message: format!("§c{}", message.into()),
    }
}

fn db_kind(kind: PunishmentKind) -> hallow_db::PunishmentKind {
    match kind {
        PunishmentKind::Ban => hallow_db::PunishmentKind::Ban,
        PunishmentKind::Mute => hallow_db::PunishmentKind::Mute,
        PunishmentKind::Kick => hallow_db::PunishmentKind::Kick,
        PunishmentKind::Warn => hallow_db::PunishmentKind::Warn,
    }
}

fn wire_kind(kind: hallow_db::PunishmentKind) -> PunishmentKind {
    match kind {
        hallow_db::PunishmentKind::Ban => PunishmentKind::Ban,
        hallow_db::PunishmentKind::Mute => PunishmentKind::Mute,
        hallow_db::PunishmentKind::Kick => PunishmentKind::Kick,
        hallow_db::PunishmentKind::Warn => PunishmentKind::Warn,
    }
}

fn millis(at: DateTime<Utc>) -> u64 {
    u64::try_from(at.timestamp_millis()).unwrap_or(0)
}

/// What a proxy needs to block a muted player's chat.
pub fn mute_info(mute: &Punishment) -> MuteInfo {
    MuteInfo {
        reason: mute.reason.clone(),
        until_ms: mute.expires_at.map(millis),
    }
}

fn actor(issuer: Option<Uuid>) -> Actor {
    issuer.map_or(Actor::Console, Actor::Account)
}

/// Sends `command` to the proxy holding `player`, if they're online.
async fn tell_proxy(store: &Store, player: Uuid, command: ProxyCommand) {
    if let Ok(Some(location)) = store.player(&player).await
        && let Err(err) = store.send_to_proxy(&location.proxy, &command).await
    {
        tracing::warn!(%err, "couldn't reach the player's proxy");
    }
}

pub async fn issue(
    pool: &PgPool,
    store: &Store,
    req: IssuePunishment,
) -> DbResult<PunishmentReply> {
    let reason = req.reason.trim().replace('§', "");
    if reason.is_empty() {
        return Ok(refuse("Give a reason."));
    }
    let Some((target, username)) = resolve(pool, store, &req.target).await? else {
        return Ok(refuse(format!(
            "No player called {} has joined.",
            req.target
        )));
    };
    if req.issuer == Some(target) {
        return Ok(refuse("You can't punish yourself."));
    }
    let lasting = matches!(req.kind, PunishmentKind::Ban | PunishmentKind::Mute);
    let expires_at = match req.duration_seconds {
        Some(seconds) if lasting && seconds > 0 => Some(
            Utc::now() + TimeDelta::seconds(i64::try_from(seconds.min(MAX_SECONDS)).unwrap_or(0)),
        ),
        _ => None,
    };
    let already = match req.kind {
        PunishmentKind::Ban => hallow_db::active_ban(pool, target).await?,
        PunishmentKind::Mute => hallow_db::active_mute(pool, target).await?,
        _ => None,
    };
    if already.is_some() {
        let what = if req.kind == PunishmentKind::Ban {
            "banned"
        } else {
            "muted"
        };
        return Ok(refuse(format!("{username} is already {what}.")));
    }
    let punishment = hallow_db::add_punishment(
        pool,
        &NewPunishment {
            kind: db_kind(req.kind),
            target,
            issuer: actor(req.issuer),
            reason: &reason,
            expires_at,
        },
    )
    .await?;
    // Worded from the requested length: a moment after issuing, the time
    // left already rounds down a minute.
    let length = match (expires_at, req.duration_seconds) {
        (Some(_), Some(seconds)) => format!(
            "for {}",
            remaining(TimeDelta::seconds(
                i64::try_from(seconds.min(MAX_SECONDS)).unwrap_or(0)
            ))
        ),
        _ => "permanently".to_owned(),
    };
    let summary = match req.kind {
        PunishmentKind::Ban => {
            tell_proxy(
                store,
                target,
                ProxyCommand::Kick {
                    player: target,
                    reason: ban_message(&punishment),
                },
            )
            .await;
            format!("§aBanned {username} {length}: §f{reason}")
        }
        PunishmentKind::Mute => {
            tell_proxy(
                store,
                target,
                ProxyCommand::Mute {
                    player: target,
                    mute: Some(mute_info(&punishment)),
                },
            )
            .await;
            tell_proxy(
                store,
                target,
                ProxyCommand::Message {
                    player: target,
                    text: format!("§cYou were muted {length}. §7Reason: §f{reason}"),
                    from: None,
                },
            )
            .await;
            format!("§aMuted {username} {length}: §f{reason}")
        }
        PunishmentKind::Kick => {
            tell_proxy(
                store,
                target,
                ProxyCommand::Kick {
                    player: target,
                    reason: format!("§cYou were kicked.\n§7Reason: §f{reason}"),
                },
            )
            .await;
            format!("§aKicked {username}: §f{reason}")
        }
        PunishmentKind::Warn => {
            tell_proxy(
                store,
                target,
                ProxyCommand::Message {
                    player: target,
                    text: format!("§cYou were warned: §f{reason}"),
                    from: None,
                },
            )
            .await;
            format!("§aWarned {username}: §f{reason}")
        }
    };
    tracing::info!(kind = ?req.kind, %target, id = punishment.id, "punishment issued");
    Ok(ok(summary))
}

pub async fn revoke(
    pool: &PgPool,
    store: &Store,
    req: RevokePunishment,
) -> DbResult<PunishmentReply> {
    let Some((target, username)) = resolve(pool, store, &req.target).await? else {
        return Ok(refuse(format!(
            "No player called {} has joined.",
            req.target
        )));
    };
    let active = match req.kind {
        PunishmentKind::Ban => hallow_db::active_ban(pool, target).await?,
        PunishmentKind::Mute => hallow_db::active_mute(pool, target).await?,
        _ => return Ok(refuse("Only bans and mutes can be lifted.")),
    };
    let Some(active) = active else {
        let what = if req.kind == PunishmentKind::Ban {
            "banned"
        } else {
            "muted"
        };
        return Ok(refuse(format!("{username} isn't {what}.")));
    };
    let reason = req.reason.as_deref().map(|r| r.replace('§', ""));
    hallow_db::revoke_punishment(pool, active.id, actor(req.issuer), reason.as_deref()).await?;
    if req.kind == PunishmentKind::Mute {
        tell_proxy(
            store,
            target,
            ProxyCommand::Mute {
                player: target,
                mute: None,
            },
        )
        .await;
        tell_proxy(
            store,
            target,
            ProxyCommand::Message {
                player: target,
                text: "§aYou were unmuted.".into(),
                from: None,
            },
        )
        .await;
        Ok(ok(format!("§aUnmuted {username}.")))
    } else {
        Ok(ok(format!("§aUnbanned {username}.")))
    }
}

/// The target's last 10 punishments, newest first; `None` if nobody by that
/// name has joined.
pub async fn history(
    pool: &PgPool,
    store: &Store,
    name: &str,
) -> DbResult<Option<Vec<PunishmentRecord>>> {
    let Some((target, _)) = resolve(pool, store, name).await? else {
        return Ok(None);
    };
    let now = Utc::now();
    let mut records = Vec::new();
    for p in hallow_db::punishment_history(pool, target, 10).await? {
        let issuer = match p.issuer {
            Actor::Account(uuid) => hallow_db::get_account(pool, uuid)
                .await?
                .map_or_else(|| uuid.to_string(), |a| a.username),
            Actor::Console => "console".into(),
            Actor::System => "system".into(),
            Actor::Anticheat => "anticheat".into(),
        };
        records.push(PunishmentRecord {
            id: p.id,
            kind: wire_kind(p.kind),
            // Kicks and warnings are done once given.
            active: matches!(
                p.kind,
                hallow_db::PunishmentKind::Ban | hallow_db::PunishmentKind::Mute
            ) && p.is_active_at(now),
            reason: p.reason,
            issuer,
            created_ms: millis(p.created_at),
            expires_ms: p.expires_at.map(millis),
        });
    }
    Ok(Some(records))
}
