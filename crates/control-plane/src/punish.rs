//! Issuing and lifting punishments, and enforcing them on the proxy that
//! holds the player: a ban or kick disconnects them, a mute is pushed so the
//! proxy blocks their chat. In-game staff (a player UUID), panel staff (a
//! [`StaffMember`]) and the console all go through the same path, so a ban
//! from the panel reaches the proxies exactly like `/ban`.
//!
//! Also the panel's network-wide search and the reason templates.

use chrono::{DateTime, TimeDelta, Utc};
use hallow_db::{Actor, NewPunishment, NewTemplate, PgPool, Punishment, TemplateError};
use hallow_proto::{
    command::ProxyCommand,
    fleet::FieldErrors,
    punishment::{
        IssuePunishment, IssuerKind, MuteInfo, PunishmentEntry, PunishmentKind, PunishmentPage,
        PunishmentRecord, PunishmentReply, PunishmentSearch, PunishmentTemplate, RevocationInfo,
        RevokePunishment, SaveTemplate, StaffMember,
    },
    routes,
};
use uuid::Uuid;

use crate::{
    accounts::{DbResult, ban_message, remaining, resolve},
    error::Failure,
    store::Store,
};

/// The longest punishment that isn't permanent: ten years.
const MAX_SECONDS: u64 = 10 * 365 * 24 * 3600;

fn ok(message: impl Into<String>, id: i64) -> PunishmentReply {
    PunishmentReply {
        ok: true,
        message: message.into(),
        id: Some(id),
    }
}

fn refuse(message: impl Into<String>) -> PunishmentReply {
    PunishmentReply {
        ok: false,
        message: format!("§c{}", message.into()),
        id: None,
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

/// Panel staff win over an issuer UUID; neither is the console.
fn actor(issuer: Option<Uuid>, staff: Option<&StaffMember>) -> Result<Actor, String> {
    let Some(staff) = staff else {
        return Ok(issuer.map_or(Actor::Console, Actor::Account));
    };
    let name = staff.name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err("The staff member's name must be 1 to 64 characters.".into());
    }
    let panel_user = staff
        .panel_user
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty());
    if panel_user.is_some_and(|id| id.chars().count() > 64) {
        return Err("The panel user id must be at most 64 characters.".into());
    }
    Ok(Actor::Staff {
        name: name.to_owned(),
        panel_user: panel_user.map(str::to_owned),
    })
}

/// How an actor is shown: a username, a staff name, or `console` and the like.
async fn actor_name(pool: &PgPool, actor: &Actor) -> DbResult<(String, IssuerKind)> {
    Ok(match actor {
        Actor::Account(uuid) => (
            hallow_db::get_account(pool, *uuid)
                .await?
                .map_or_else(|| uuid.to_string(), |a| a.username),
            IssuerKind::Player,
        ),
        Actor::Staff { name, .. } => (name.clone(), IssuerKind::Staff),
        Actor::Console => ("console".into(), IssuerKind::Console),
        Actor::System => ("system".into(), IssuerKind::System),
        Actor::Anticheat => ("anticheat".into(), IssuerKind::Anticheat),
    })
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
    let issuer = match actor(req.issuer, req.staff.as_ref()) {
        Ok(issuer) => issuer,
        Err(message) => return Ok(refuse(message)),
    };
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
    if req.issuer == Some(target) && req.staff.is_none() {
        return Ok(refuse("You can't punish yourself."));
    }
    // A kick is a disconnect; there's nothing to record for someone offline.
    if req.kind == PunishmentKind::Kick && matches!(store.player(&target).await, Ok(None)) {
        return Ok(refuse(format!("{username} isn't online.")));
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
            issuer,
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
    tracing::info!(
        kind = ?req.kind,
        %target,
        id = punishment.id,
        issuer = ?punishment.issuer,
        "punishment issued"
    );
    Ok(ok(summary, punishment.id))
}

pub async fn revoke(
    pool: &PgPool,
    store: &Store,
    req: RevokePunishment,
) -> DbResult<PunishmentReply> {
    let by = match actor(req.issuer, req.staff.as_ref()) {
        Ok(by) => by,
        Err(message) => return Ok(refuse(message)),
    };
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
    let reason = req
        .reason
        .as_deref()
        .map(|r| r.trim().replace('§', ""))
        .filter(|r| !r.is_empty());
    hallow_db::revoke_punishment(pool, active.id, &by, reason.as_deref()).await?;
    tracing::info!(kind = ?req.kind, %target, id = active.id, by = ?by, "punishment revoked");
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
        Ok(ok(format!("§aUnmuted {username}."), active.id))
    } else {
        Ok(ok(format!("§aUnbanned {username}."), active.id))
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
        let (issuer, _) = actor_name(pool, &p.issuer).await?;
        records.push(PunishmentRecord {
            id: p.id,
            kind: wire_kind(p.kind),
            active: in_force(&p, now),
            reason: p.reason,
            issuer,
            created_ms: millis(p.created_at),
            expires_ms: p.expires_at.map(millis),
        });
    }
    Ok(Some(records))
}

/// Kicks and warnings are done once given; bans and mutes last until they
/// end or are lifted.
fn in_force(p: &Punishment, now: DateTime<Utc>) -> bool {
    matches!(
        p.kind,
        hallow_db::PunishmentKind::Ban | hallow_db::PunishmentKind::Mute
    ) && p.is_active_at(now)
}

/// The panel's list: every punishment matching `search`, newest first.
/// `None` if `search.player` names nobody who has joined.
pub async fn search(
    pool: &PgPool,
    store: &Store,
    search: &PunishmentSearch,
) -> DbResult<Option<PunishmentPage>> {
    let target = match search.player.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => match resolve(pool, store, name).await? {
            Some((uuid, _)) => Some(uuid),
            None => return Ok(None),
        },
        _ => None,
    };
    let filter = hallow_db::PunishmentFilter {
        target,
        kind: search.kind.map(db_kind),
        active: search.active,
    };
    let limit = search
        .limit
        .unwrap_or(routes::PUNISHMENT_SEARCH_DEFAULT)
        .clamp(1, routes::PUNISHMENT_SEARCH_MAX);
    let offset = search.offset.unwrap_or(0);
    let (total, found) =
        hallow_db::search_punishments(pool, &filter, i64::from(limit), i64::from(offset)).await?;
    let now = Utc::now();
    let mut entries = Vec::with_capacity(found.len());
    for row in found {
        let p = row.punishment;
        let (issuer, issuer_kind) = actor_name(pool, &p.issuer).await?;
        let revoked = match &p.revoked {
            Some(r) => {
                let (by, by_kind) = actor_name(pool, &r.by).await?;
                Some(RevocationInfo {
                    at_ms: millis(r.at),
                    by,
                    by_kind,
                    reason: r.reason.clone(),
                })
            }
            None => None,
        };
        entries.push(PunishmentEntry {
            id: p.id,
            kind: wire_kind(p.kind),
            target: p.target,
            target_name: row.target_name,
            active: in_force(&p, now),
            reason: p.reason,
            issuer,
            issuer_kind,
            created_ms: millis(p.created_at),
            expires_ms: p.expires_at.map(millis),
            revoked,
        });
    }
    Ok(Some(PunishmentPage { total, entries }))
}

fn wire_template(t: hallow_db::PunishmentTemplate) -> PunishmentTemplate {
    PunishmentTemplate {
        id: t.id,
        name: t.name,
        kind: wire_kind(t.kind),
        duration_seconds: t.duration_seconds.and_then(|s| u64::try_from(s).ok()),
        reason: t.reason,
        updated_ms: millis(t.updated_at),
    }
}

pub async fn templates(pool: &PgPool) -> DbResult<Vec<PunishmentTemplate>> {
    Ok(hallow_db::list_templates(pool)
        .await?
        .into_iter()
        .map(wire_template)
        .collect())
}

/// Checks a template, trimmed; a `400` naming each bad field.
fn check_template(save: &SaveTemplate) -> Result<(String, Option<i64>, String), Failure> {
    let mut fields = FieldErrors::new();
    let name = save.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 64 {
        fields.insert("name".into(), "must be 1 to 64 characters".into());
    }
    let reason = save.reason.trim().replace('§', "");
    if reason.is_empty() || reason.chars().count() > 256 {
        fields.insert("reason".into(), "must be 1 to 256 characters".into());
    }
    let lasting = matches!(save.kind, PunishmentKind::Ban | PunishmentKind::Mute);
    let duration = match save.duration_seconds {
        Some(_) if !lasting => {
            fields.insert(
                "duration_seconds".into(),
                "only bans and mutes have a length".into(),
            );
            None
        }
        Some(0) => {
            fields.insert(
                "duration_seconds".into(),
                "must be positive, or left out for permanent".into(),
            );
            None
        }
        Some(seconds) => Some(i64::try_from(seconds.min(MAX_SECONDS)).unwrap_or(0)),
        None => None,
    };
    if fields.is_empty() {
        Ok((name, duration, reason))
    } else {
        Err(Failure::invalid_fields("the template isn't valid", fields))
    }
}

fn template_failure(err: TemplateError) -> Failure {
    match err {
        TemplateError::NameTaken => {
            let mut fields = FieldErrors::new();
            fields.insert("name".into(), "another template is called that".into());
            Failure::invalid_fields("another template is called that", fields)
        }
        TemplateError::Database(err) => err.into(),
    }
}

/// Creates a template (`id` `None`) or replaces one; `Ok(None)` if `id`
/// names no template.
pub async fn save_template(
    pool: &PgPool,
    id: Option<i64>,
    save: &SaveTemplate,
) -> Result<Option<PunishmentTemplate>, Failure> {
    let (name, duration_seconds, reason) = check_template(save)?;
    let new = NewTemplate {
        name: &name,
        kind: db_kind(save.kind),
        duration_seconds,
        reason: &reason,
    };
    let saved = match id {
        None => Some(
            hallow_db::add_template(pool, &new)
                .await
                .map_err(template_failure)?,
        ),
        Some(id) => hallow_db::update_template(pool, id, &new)
            .await
            .map_err(template_failure)?,
    };
    Ok(saved.map(wire_template))
}
