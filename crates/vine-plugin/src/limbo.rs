//! Never dropped (docs/ARCHITECTURE.md): the proxy's side of keeping every
//! player connected.
//!
//! - **Limbo.** Vine's built-in limbo server (`[limbo]` in `vine.toml`) is
//!   where a player waits when the network can't place them: the control
//!   plane said wait (full, no hub), it can't be reached, or every other
//!   server failed. Players there see why on their action bar.
//! - **Unreachable control plane.** A login it can't answer goes to limbo
//!   *unverified*: the player can't leave it, and the login is asked again
//!   every [`RETRY_EVERY`] until the control plane answers, then they're
//!   moved on (or disconnected, if it refuses a duplicate login).
//! - **Banned players** (`restricted`) only move where the control plane
//!   sends them, to a banned lobby or limbo. `/server` and every Hallow
//!   command but `/friend list` are refused here; the control plane refuses
//!   the rest.
//! - **Server kicks.** When a server kicks a player, crashes, stops or can't
//!   be reached, Vine fires `server-kick` and this picks where they go,
//!   asking the control plane ([`routes::PRESENCE_FALLBACK`]), or on its own
//!   if it can't: a hub for most, limbo for the banned and unverified.
//! - **Draining.** A draining proxy with `transfer_to` set hands players on
//!   lobbies and in limbo to the load balancer with the Transfer packet
//!   (1.20.5 and later), a few a second; players in a game go when they
//!   leave it. Older clients stay until they leave.

use std::time::{Duration, Instant};

use hallow_proto::{
    presence::{Fallback, FallbackRequest, LIMBO, LoginDecision, LoginRequest, PlayerSwitched},
    punishment::BanInfo,
    routes,
    server::ServerRole,
};
use uuid::Uuid;

use crate::vine::plugin::{event::ServerKickEvent, logging::Level, proxy};
use crate::{State, commands::duration_text, log, now_ms};

/// How often an unverified login is asked about again.
pub const RETRY_EVERY: Duration = Duration::from_secs(2);
/// How often limbo players' action bars are redrawn.
const STATUS_EVERY: Duration = Duration::from_secs(1);
/// Players transferred per tick while draining.
const TRANSFERS_PER_TICK: usize = 5;

/// A player let into limbo without the control plane's answer.
pub struct Unverified {
    pub request: LoginRequest,
    pub asked: Option<Instant>,
}

/// What a player in limbo is waiting for, for their action bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Waiting {
    /// The control plane said wait, with why.
    Place(String),
}

/// The banned lobby's countdown line: "ends in 1h 3m" or "permanent".
#[must_use]
pub fn ban_line(ban: &BanInfo, now_ms: u64) -> String {
    match ban.expires_ms {
        None => format!("§cBanned §7(#{}) · permanent", ban.id),
        Some(at) if at <= now_ms => "§aYour ban has ended. Sending you on...".to_owned(),
        Some(at) => format!(
            "§cBanned §7(#{}) · ends in §f{}",
            ban.id,
            duration_text((at - now_ms).div_ceil(1000))
        ),
    }
}

/// The chat block a banned player sees on arrival in limbo (the banned
/// lobby shows its own).
#[must_use]
pub fn ban_notice(ban: &BanInfo, now_ms: u64) -> String {
    let mut lines = vec![
        "§cYou are banned from this network.".to_owned(),
        format!("§7Reason: §f{}", ban.reason),
        format!("§7Issued by: §f{}", ban.issuer),
        ban_line(ban, now_ms),
    ];
    if let Some(appeal) = &ban.appeal {
        lines.push(format!("§7{appeal}"));
    }
    if let Some(url) = &ban.appeal_url {
        lines.push(format!("§7Appeal at §f{url}"));
    }
    lines.join("\n")
}

/// Where a player goes when the control plane can't say: limbo for the
/// banned and unverified, else the emptiest other hub.
#[must_use]
pub fn local_fallback<'a>(
    locked: bool,
    lost: &str,
    hubs: impl Iterator<Item = (&'a str, u32)>,
    limbo: &str,
) -> String {
    if locked {
        return limbo.to_owned();
    }
    hubs.filter(|(name, _)| *name != lost)
        .min_by_key(|(name, players)| (*players, *name))
        .map_or_else(|| limbo.to_owned(), |(name, _)| name.to_owned())
}

/// `host:port` for the Transfer packet.
#[must_use]
pub fn parse_target(text: &str) -> Option<(String, u16)> {
    let (host, port) = text.rsplit_once(':')?;
    let port = port.parse().ok()?;
    (!host.is_empty()).then(|| (host.trim_matches(['[', ']']).to_owned(), port))
}

impl State {
    /// The built-in limbo's server name.
    pub fn limbo(&self) -> String {
        proxy::limbo_server().unwrap_or_else(|| LIMBO.to_owned())
    }

    /// Whether only the control plane may move this player.
    pub fn locked(&self, uuid: &str) -> bool {
        self.restricted.contains_key(uuid) || self.unverified.contains_key(uuid)
    }

    /// Takes a decision the control plane made for a player already on
    /// this proxy (a retried login, or a re-report after it lost track):
    /// moves them if they're in limbo and it names somewhere else.
    pub fn apply_decision(&mut self, uuid: &str, decision: LoginDecision, current: Option<&str>) {
        let limbo = self.limbo();
        let target = match decision {
            LoginDecision::Allow {
                server,
                permissions,
                mute,
                ban,
            } => {
                self.permissions.insert(uuid.to_owned(), permissions);
                self.set_mute(uuid, mute);
                match ban {
                    Some(ban) => {
                        self.restricted.insert(uuid.to_owned(), ban);
                    }
                    None => {
                        self.restricted.remove(uuid);
                    }
                }
                self.waiting.remove(uuid);
                Some(server)
            }
            LoginDecision::Wait {
                reason,
                permissions,
                mute,
                ..
            } => {
                self.permissions.insert(uuid.to_owned(), permissions);
                self.set_mute(uuid, mute);
                self.restricted.remove(uuid);
                self.waiting
                    .insert(uuid.to_owned(), Waiting::Place(reason.clone()));
                proxy::send_message(
                    uuid,
                    &format!("§e{reason} §7You'll be moved on by yourself."),
                );
                Some(limbo.clone())
            }
            LoginDecision::Deny { reason } => {
                proxy::disconnect_player(uuid, &reason);
                None
            }
        };
        if let Some(target) = target
            && current == Some(limbo.as_str())
            && target != limbo
        {
            self.sending.insert(uuid.to_owned(), target.clone());
            proxy::connect_player(uuid, &target);
        }
    }

    /// Asks the control plane again about players let in while it was
    /// unreachable.
    pub fn retry_unverified(&mut self) {
        if !self.registered {
            return;
        }
        let now = Instant::now();
        let due: Vec<String> = self
            .unverified
            .iter()
            .filter(|(_, u)| u.asked.is_none_or(|at| now - at >= RETRY_EVERY))
            .map(|(uuid, _)| uuid.clone())
            .collect();
        for uuid in due {
            let Some(entry) = self.unverified.get_mut(&uuid) else {
                continue;
            };
            entry.asked = Some(now);
            let request = entry.request.clone();
            match self
                .client
                .post::<_, LoginDecision>(routes::PRESENCE_LOGIN, &request)
            {
                Ok(decision) => {
                    self.unverified.remove(&uuid);
                    self.waiting.remove(&uuid);
                    if !matches!(decision, LoginDecision::Deny { .. }) {
                        self.admitted.insert(uuid.clone(), Instant::now());
                    }
                    let current = proxy::get_player_by_uuid(&uuid).map(|p| p.current_server);
                    if let Some(server) = &current {
                        let switched = PlayerSwitched {
                            uuid: request.uuid,
                            server: server.clone(),
                        };
                        let _ = self.client.post_empty(routes::PRESENCE_SWITCH, &switched);
                    }
                    log(
                        Level::Info,
                        &format!(
                            "{} verified after the control plane came back",
                            request.username
                        ),
                    );
                    self.apply_decision(&uuid, decision, current.as_deref());
                }
                Err(err) => {
                    if err.is_not_found() {
                        self.registered = false;
                    }
                    break;
                }
            }
        }
    }

    /// `server-kick`: picks where the player goes instead of being
    /// disconnected.
    pub fn on_server_kick(&mut self, mut kick: ServerKickEvent) -> ServerKickEvent {
        let limbo = self.limbo();
        let asked = Uuid::parse_str(&kick.uuid)
            .ok()
            .filter(|_| self.registered && !self.unverified.contains_key(&kick.uuid));
        let answer = asked.and_then(|uuid| {
            let request = FallbackRequest {
                uuid,
                from: kick.server_name.clone(),
                reason: kick.reason.clone(),
            };
            match self
                .client
                .post::<_, Fallback>(routes::PRESENCE_FALLBACK, &request)
            {
                Ok(answer) => Some(answer),
                Err(err) => {
                    log(
                        Level::Warn,
                        &format!("fallback for {} failed: {err}", kick.username),
                    );
                    None
                }
            }
        });
        let (server, message) = match answer {
            Some(Fallback { server, message }) => (server, message),
            None => {
                let hub_kind = self.config.hub_kind.clone();
                let hubs = self
                    .servers
                    .iter()
                    .filter(|(_, s)| s.kind == hub_kind)
                    .map(|(name, s)| (name.as_str(), s.players));
                let server =
                    local_fallback(self.locked(&kick.uuid), &kick.server_name, hubs, &limbo);
                let why = match kick.reason.as_deref() {
                    Some(reason) if !reason.trim().is_empty() => {
                        format!("§c{} sent you away: §f{reason}", kick.server_name)
                    }
                    _ => format!("§c{} went offline.", kick.server_name),
                };
                (server, why)
            }
        };
        if server == limbo && !self.restricted.contains_key(&kick.uuid) {
            self.waiting
                .entry(kick.uuid.clone())
                .or_insert_with(|| Waiting::Place("Nowhere has room yet.".into()));
        }
        // The move Vine makes for this isn't refused by `on_connect`.
        self.sending.insert(kick.uuid.clone(), server.clone());
        kick.fallback_server = Some(server);
        if !message.is_empty() {
            kick.message = Some(message);
        }
        kick
    }

    /// Redraws limbo players' action bars and transfers players off a
    /// draining proxy.
    pub fn tend_players(&mut self) {
        let now = Instant::now();
        if self.last_status.is_some_and(|at| now - at < STATUS_EVERY) {
            return;
        }
        self.last_status = Some(now);
        let limbo = self.limbo();
        let wall = now_ms();
        let players = proxy::get_players();
        for player in &players {
            if player.current_server != limbo {
                continue;
            }
            let line = if let Some(ban) = self.restricted.get(&player.uuid) {
                ban_line(ban, wall)
            } else if self.unverified.contains_key(&player.uuid) {
                "§eThe network is having trouble. §7You'll be moved on when it's back.".to_owned()
            } else {
                match self.waiting.get(&player.uuid) {
                    Some(Waiting::Place(reason)) => {
                        format!("§e{reason} §7Waiting for a place...")
                    }
                    None => "§7Waiting for a place...".to_owned(),
                }
            };
            proxy::send_action_bar(&player.uuid, &line);
        }

        // Draining: hand players on lobbies and in limbo to the load
        // balancer, which picks another proxy.
        if !proxy::is_draining() {
            return;
        }
        let Some((host, port)) = self.config.transfer_to.as_deref().and_then(parse_target) else {
            return;
        };
        let mut moved = 0;
        for player in players {
            if moved >= TRANSFERS_PER_TICK || self.untransferable.contains(&player.uuid) {
                continue;
            }
            let in_game = self
                .servers
                .get(&player.current_server)
                .is_some_and(|s| s.role == ServerRole::Game);
            if in_game {
                continue;
            }
            if proxy::transfer_player(&player.uuid, &host, port) {
                moved += 1;
            } else {
                // Older than 1.20.5: no Transfer packet. They stay until
                // they leave or the proxy stops.
                self.untransferable.insert(player.uuid.clone());
            }
        }
    }

    /// Before a player's first connect: banned players see their ban.
    pub fn arrived(&mut self, uuid: &str, server: &str) {
        if server != self.limbo() {
            return;
        }
        if let Some(ban) = self.restricted.get(uuid) {
            proxy::send_message(uuid, &ban_notice(ban, now_ms()));
        } else if self.unverified.contains_key(uuid) {
            proxy::send_message(
                uuid,
                "§eThe network is having trouble right now. §7You're safe here and will be moved on as soon as it's back.",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hallow_proto::punishment::IssuerKind;

    fn ban(expires_ms: Option<u64>) -> BanInfo {
        BanInfo {
            id: 42,
            reason: "Cheating".into(),
            issuer: "mod-alice".into(),
            issuer_kind: IssuerKind::Staff,
            created_ms: 0,
            expires_ms,
            appeal: Some("Appeal on our Discord.".into()),
            appeal_url: Some("https://example.net/appeal".into()),
        }
    }

    #[test]
    fn ban_lines_count_down() {
        assert_eq!(ban_line(&ban(None), 0), "§cBanned §7(#42) · permanent");
        assert_eq!(
            ban_line(&ban(Some(3_600_000 + 61_000)), 0),
            "§cBanned §7(#42) · ends in §f1h 1m"
        );
        assert!(ban_line(&ban(Some(5)), 10).contains("ended"));
        let notice = ban_notice(&ban(None), 0);
        assert!(notice.contains("Cheating") && notice.contains("mod-alice"));
        assert!(notice.contains("https://example.net/appeal"));
    }

    #[test]
    fn local_fallbacks() {
        let hubs = [("hub-a", 3), ("hub-b", 1)];
        assert_eq!(
            local_fallback(false, "game-1", hubs.into_iter(), "limbo"),
            "hub-b"
        );
        assert_eq!(
            local_fallback(false, "hub-b", hubs.into_iter(), "limbo"),
            "hub-a"
        );
        assert_eq!(
            local_fallback(true, "game-1", hubs.into_iter(), "limbo"),
            "limbo"
        );
        assert_eq!(
            local_fallback(false, "hub-a", [("hub-a", 0)].into_iter(), "limbo"),
            "limbo"
        );
    }

    #[test]
    fn transfer_targets() {
        assert_eq!(
            parse_target("play.example.net:25565"),
            Some(("play.example.net".into(), 25565))
        );
        assert_eq!(parse_target("[::1]:25565"), Some(("::1".into(), 25565)));
        assert_eq!(parse_target("nope"), None);
        assert_eq!(parse_target(":25565"), None);
    }
}
