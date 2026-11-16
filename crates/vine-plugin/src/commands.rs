//! Commands that reach players on any proxy, through the control plane.
//!
//! | Command | Permission | |
//! |---|---|---|
//! | `/msg <player> <message>` (`/tell`, `/w`, `/whisper`) | | Private message |
//! | `/r <message>` (`/reply`) | | Reply to the last private message |
//! | `/find <player>` | `hallow.staff.find` | Which server and proxy a player is on |
//! | `/alert <message>` | `hallow.staff.alert` | Message every player on the network |
//! | `/gkick <player> [reason]` | `hallow.staff.kick` | Disconnect a player from the network |
//! | `/party <invite\|accept\|leave\|kick\|disband\|warp\|list\|chat>` (`/p`; `/party <player>` invites) | | Parties |
//! | `/pc <message>` | | Party chat |
//! | `/friend [add\|accept\|deny\|remove\|list\|requests] [player]` (`/f`; `/friend <player>` adds) | | Friends (needs the database) |
//! | `/play <game>` | | Queue for a game, with your party if you lead one |
//! | `/leavequeue` (`/lq`) | | Leave the queue |
//! | `/lobby [mode]` | | Go to a game mode's lobby (`/lobby skywars`), or to a hub without one |
//! | `/hub` | | Go to a hub, from anywhere |
//! | `/ban <player> [duration] <reason>` | `hallow.staff.ban` | Ban; permanent without a duration (`30m`, `2h`, `7d`, `1w`) |
//! | `/unban <player> [reason]` | `hallow.staff.ban` | Lift a ban |
//! | `/mute <player> [duration] <reason>` | `hallow.staff.mute` | Mute: no chat, `/msg`, `/r` or party chat |
//! | `/unmute <player> [reason]` | `hallow.staff.mute` | Lift a mute |
//! | `/warn <player> <reason>` | `hallow.staff.warn` | Warn, on the record |
//! | `/history <player>` | `hallow.staff.history` | A player's last 10 punishments |
//!
//! Punishments are kept in PostgreSQL, so they need a control plane with a
//! database.
//!
//! Staff permissions come from the player's ranks in the database, sent with
//! their login (`hallow.staff.*` and the like; see `hallow_proto::permission`).
//! The console may run everything.

use hallow_proto::permission::has_permission;
use hallow_proto::{
    command::NetworkCommand,
    friend::{FriendAction, FriendReply},
    lobby::{LobbySent, SendToLobby},
    party::{PartyAction, PartyReply},
    presence::PlayerLocation,
    punishment::{
        IssuePunishment, PunishmentKind, PunishmentRecord, PunishmentReply, RevokePunishment,
    },
    queue::{JoinQueue, QueueStatus},
    routes,
};
use uuid::Uuid;

use crate::vine::plugin::{command::reply, proxy, types::CommandSenderType};
use crate::{CommandSender, Context, State, http};

const MSG: u32 = 10;
const REPLY: u32 = 11;
const FIND: u32 = 12;
const ALERT: u32 = 13;
const GKICK: u32 = 14;
const PARTY: u32 = 15;
const PARTY_CHAT: u32 = 16;
const PLAY: u32 = 17;
const LEAVE_QUEUE: u32 = 18;
const BAN: u32 = 19;
const UNBAN: u32 = 20;
const MUTE: u32 = 21;
const UNMUTE: u32 = 22;
const WARN: u32 = 23;
const HISTORY: u32 = 24;
const FRIEND: u32 = 25;
const LOBBY: u32 = 26;
const HUB: u32 = 27;

const FRIEND_USAGE: &str = "/friend <player> | add <player> | accept <player> | deny <player> | remove <player> | list | requests";
const FRIEND_SUBCOMMANDS: [&str; 6] = ["add", "accept", "deny", "remove", "list", "requests"];

const PARTY_USAGE: &str = "/party <player> | invite <player> | accept <player> | leave | kick <player> | disband | warp | list | chat <message>";
const PARTY_SUBCOMMANDS: [&str; 8] = [
    "invite", "accept", "leave", "kick", "disband", "warp", "list", "chat",
];

pub fn register(ctx: &Context) {
    ctx.register_command(FRIEND, "friend", "Friends", &["f".into()], None);
    for (id, name, description) in [
        (BAN, "ban", "Ban a player from the network"),
        (UNBAN, "unban", "Lift a ban"),
        (MUTE, "mute", "Mute a player"),
        (UNMUTE, "unmute", "Lift a mute"),
        (WARN, "warn", "Warn a player"),
        (HISTORY, "history", "Show a player's punishments"),
    ] {
        ctx.register_command(id, name, description, &[], None);
    }
    ctx.register_command(PARTY, "party", "Play with friends", &["p".into()], None);
    ctx.register_command(PARTY_CHAT, "pc", "Message your party", &[], None);
    ctx.register_command(PLAY, "play", "Queue for a game", &[], None);
    ctx.register_command(LOBBY, "lobby", "Go to a game mode's lobby", &[], None);
    ctx.register_command(HUB, "hub", "Go to the hub", &[], None);
    ctx.register_command(
        LEAVE_QUEUE,
        "leavequeue",
        "Leave the queue",
        &["lq".into()],
        None,
    );
    ctx.register_command(
        MSG,
        "msg",
        "Send a private message to a player on any server",
        &["tell".into(), "w".into(), "whisper".into()],
        None,
    );
    ctx.register_command(
        REPLY,
        "r",
        "Reply to your last private message",
        &["reply".into()],
        None,
    );
    ctx.register_command(FIND, "find", "Show which server a player is on", &[], None);
    ctx.register_command(
        ALERT,
        "alert",
        "Message every player on the network",
        &[],
        None,
    );
    ctx.register_command(
        GKICK,
        "gkick",
        "Disconnect a player from the network",
        &[],
        None,
    );
}

fn moderation_node(id: u32) -> Option<&'static str> {
    Some(match id {
        BAN | UNBAN => "hallow.staff.ban",
        MUTE | UNMUTE => "hallow.staff.mute",
        WARN => "hallow.staff.warn",
        HISTORY => "hallow.staff.history",
        _ => return None,
    })
}

/// `30s`, `30m`, `2h`, `7d` or `1w` in seconds; `None` if it isn't one.
pub fn parse_duration(text: &str) -> Option<u64> {
    let (number, unit) = text.split_at(text.len().checked_sub(1)?);
    let n: u64 = number.parse().ok().filter(|n| *n > 0)?;
    let unit = match unit.to_ascii_lowercase().as_str() {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 604_800,
        _ => return None,
    };
    n.checked_mul(unit)
}

/// `3d 4h`, `5h 10m`, `12m`, `40s`: the two largest units.
pub fn duration_text(seconds: u64) -> String {
    let (d, h, m, s) = (
        seconds / 86_400,
        (seconds / 3600) % 24,
        (seconds / 60) % 60,
        seconds % 60,
    );
    match (d, h, m) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, _) => format!("{m}m"),
        (0, _, _) => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h"),
    }
}

fn staff_node(id: u32) -> &'static str {
    match id {
        FIND => "hallow.staff.find",
        ALERT => "hallow.staff.alert",
        _ => "hallow.staff.kick",
    }
}

/// Players can't inject colour codes into what others see.
fn plain(words: &[String]) -> String {
    words.join(" ").replace('§', "")
}

impl State {
    pub fn command(
        &mut self,
        id: u32,
        sender: &CommandSender,
        args: &[String],
    ) -> Result<i32, String> {
        let usage = |text: &str| Err(format!("§cUsage: {text}"));
        // Muted players can't talk to anyone, through any command.
        let talking = matches!(id, MSG | REPLY | PARTY_CHAT)
            || (id == PARTY && args.first().is_some_and(|a| a.eq_ignore_ascii_case("chat")));
        if talking
            && let Some(uuid) = sender.player_uuid.as_deref()
            && let Some(notice) = self.muted(uuid)
        {
            return Err(notice);
        }
        if let Some(node) = moderation_node(id)
            && !self.allowed(sender, node)
        {
            return Err("§cYou don't have permission to do that.".into());
        }
        match (id, args) {
            (MSG, [target, message @ ..]) if !message.is_empty() => {
                self.private_message(sender, target, &plain(message))
            }
            (MSG, _) => usage("/msg <player> <message>"),
            (REPLY, message) if !message.is_empty() => {
                let Some(target) = sender
                    .player_uuid
                    .as_ref()
                    .and_then(|uuid| self.last_from.get(uuid))
                    .cloned()
                else {
                    return Err("§cNobody has messaged you yet.".into());
                };
                self.private_message(sender, &target, &plain(message))
            }
            (REPLY, _) => usage("/r <message>"),
            (FIND | ALERT | GKICK, _) if !self.allowed(sender, staff_node(id)) => {
                Err("§cYou don't have permission to do that.".into())
            }
            (FIND, [name]) => {
                let location = self.locate(name)?;
                let place = location.server.as_deref().unwrap_or("between servers");
                reply(
                    sender,
                    &format!(
                        "§a{} §7is on §f{place} §7({})",
                        location.username, location.proxy
                    ),
                );
                Ok(1)
            }
            (FIND, _) => usage("/find <player>"),
            (ALERT, message) if !message.is_empty() => {
                self.network(&NetworkCommand::Broadcast {
                    text: format!("§c[Alert] §f{}", plain(message)),
                    servers: None,
                })?;
                Ok(1)
            }
            (ALERT, _) => usage("/alert <message>"),
            (GKICK, [name, reason @ ..]) => {
                let location = self.locate(name)?;
                let reason = if reason.is_empty() {
                    "You were kicked from the network.".to_owned()
                } else {
                    plain(reason)
                };
                self.network(&NetworkCommand::Kick {
                    player: location.uuid,
                    reason: format!("§c{reason}"),
                })?;
                reply(sender, &format!("§aKicked {}.", location.username));
                Ok(1)
            }
            (GKICK, _) => usage("/gkick <player> [reason]"),
            (PARTY, args) => {
                let Some(action) = party_action(self.player_id(sender)?, args) else {
                    return usage(PARTY_USAGE);
                };
                self.party(sender, &action)
            }
            (PARTY_CHAT, message) if !message.is_empty() => {
                let action = PartyAction::Chat {
                    player: self.player_id(sender)?,
                    text: plain(message),
                };
                self.party(sender, &action)
            }
            (PARTY_CHAT, _) => usage("/pc <message>"),
            (BAN | MUTE, [target, rest @ ..]) if !rest.is_empty() => {
                let (duration, reason) = match parse_duration(&rest[0]) {
                    Some(seconds) if rest.len() > 1 => (Some(seconds), &rest[1..]),
                    Some(_) => return usage("/ban|/mute <player> [duration] <reason>"),
                    None => (None, rest),
                };
                let kind = if id == BAN {
                    PunishmentKind::Ban
                } else {
                    PunishmentKind::Mute
                };
                self.punish(sender, kind, target, &plain(reason), duration)
            }
            (BAN, _) => usage("/ban <player> [duration] <reason>"),
            (MUTE, _) => usage("/mute <player> [duration] <reason>"),
            (WARN, [target, reason @ ..]) if !reason.is_empty() => {
                self.punish(sender, PunishmentKind::Warn, target, &plain(reason), None)
            }
            (WARN, _) => usage("/warn <player> <reason>"),
            (UNBAN | UNMUTE, [target, reason @ ..]) => {
                let request = RevokePunishment {
                    kind: if id == UNBAN {
                        PunishmentKind::Ban
                    } else {
                        PunishmentKind::Mute
                    },
                    target: target.clone(),
                    issuer: self.issuer(sender),
                    reason: (!reason.is_empty()).then(|| plain(reason)),
                    staff: None,
                };
                self.moderate(sender, routes::PUNISHMENTS_REVOKE, &request)
            }
            (UNBAN, _) => usage("/unban <player> [reason]"),
            (UNMUTE, _) => usage("/unmute <player> [reason]"),
            (HISTORY, [target]) => self.history(sender, target),
            (HISTORY, _) => usage("/history <player>"),
            (FRIEND, args) => {
                let Some(action) = friend_action(self.player_id(sender)?, args) else {
                    return usage(FRIEND_USAGE);
                };
                match self.client.post::<_, FriendReply>(routes::FRIENDS, &action) {
                    Ok(answer) => {
                        for line in answer.message.lines() {
                            reply(sender, line);
                        }
                        Ok(i32::from(answer.ok))
                    }
                    Err(http::Error::Status(503, _)) => {
                        Err("§cFriends need the network's database, which isn't set up.".into())
                    }
                    Err(err) => {
                        crate::log(crate::Level::Warn, &format!("friend action failed: {err}"));
                        Err(crate::UNAVAILABLE.into())
                    }
                }
            }
            (PLAY, [kind]) => {
                let request = JoinQueue {
                    kind: kind.to_ascii_lowercase(),
                    player: self.player_id(sender)?,
                };
                match self.client.post::<_, QueueStatus>(routes::QUEUES, &request) {
                    Ok(status) => {
                        reply(
                            sender,
                            &format!(
                                "§aQueued for {}. §7{} player(s) in the queue.",
                                status.kind, status.queued
                            ),
                        );
                        Ok(1)
                    }
                    Err(http::Error::Status(409, Some(err))) => Err(format!("§c{}", err.message)),
                    Err(err) => {
                        crate::log(crate::Level::Warn, &format!("queueing failed: {err}"));
                        Err(crate::UNAVAILABLE.into())
                    }
                }
            }
            (PLAY, _) => usage("/play <game>"),
            (LEAVE_QUEUE, []) => {
                let player = self.player_id(sender)?;
                match self.client.delete(&routes::queue_entry(&player)) {
                    Ok(()) => {
                        reply(sender, "§eYou left the queue.");
                        Ok(1)
                    }
                    Err(err) if err.is_not_found() => Err("§cYou aren't in a queue.".into()),
                    Err(err) => {
                        crate::log(
                            crate::Level::Warn,
                            &format!("leaving the queue failed: {err}"),
                        );
                        Err(crate::UNAVAILABLE.into())
                    }
                }
            }
            (LEAVE_QUEUE, _) => usage("/leavequeue"),
            (LOBBY, []) | (HUB, []) => self.to_lobby(sender, None),
            (LOBBY, [mode]) => self.to_lobby(sender, Some(mode)),
            (LOBBY, _) => usage("/lobby [mode]"),
            (HUB, _) => usage("/hub"),
            _ => Err("§cUnknown command.".into()),
        }
    }

    /// Names of players on this proxy, for tab completion. Players on other
    /// proxies can still be typed in full.
    pub fn suggest(&self, id: u32, args: &[String]) -> Vec<String> {
        let prefix = args
            .last()
            .map(|a| a.to_ascii_lowercase())
            .unwrap_or_default();
        let names_at = match id {
            MSG | FIND | GKICK => 1,
            PARTY if args.len() <= 1 => {
                return PARTY_SUBCOMMANDS
                    .iter()
                    .filter(|s| s.starts_with(&prefix))
                    .map(|s| (*s).to_owned())
                    .collect();
            }
            PARTY if matches!(args[0].as_str(), "invite" | "accept" | "kick") => 2,
            FRIEND if args.len() <= 1 => {
                return FRIEND_SUBCOMMANDS
                    .iter()
                    .filter(|s| s.starts_with(&prefix))
                    .map(|s| (*s).to_owned())
                    .collect();
            }
            FRIEND if matches!(args[0].as_str(), "add" | "accept" | "deny" | "remove") => 2,
            _ => return Vec::new(),
        };
        if args.len() > names_at {
            return Vec::new();
        }
        proxy::get_players()
            .into_iter()
            .map(|p| p.username)
            .filter(|name| name.to_ascii_lowercase().starts_with(&prefix))
            .collect()
    }

    fn private_message(
        &mut self,
        sender: &CommandSender,
        target: &str,
        text: &str,
    ) -> Result<i32, String> {
        if sender.sender_type != CommandSenderType::Player {
            return Err("§cOnly players can send private messages.".into());
        }
        let location = self.locate(target)?;
        if location.username.eq_ignore_ascii_case(&sender.name) {
            return Err("§cYou can't message yourself.".into());
        }
        self.network(&NetworkCommand::Message {
            player: location.uuid,
            text: format!("§d[From {}] §7{text}", sender.name),
            from: Some(sender.name.clone()),
        })?;
        reply(sender, &format!("§d[To {}] §7{text}", location.username));
        if let Some(uuid) = &sender.player_uuid {
            // Replying to someone you just messaged goes back to them.
            self.last_from.insert(uuid.clone(), location.username);
        }
        Ok(1)
    }

    /// Sends the player to a lobby of `mode`, or to a hub. The control plane
    /// tells them where they're going and moves them.
    fn to_lobby(&self, sender: &CommandSender, mode: Option<&String>) -> Result<i32, String> {
        let request = SendToLobby {
            player: self.player_id(sender)?,
            lobby: mode.map(|m| m.to_ascii_lowercase()),
            fallback_hub: false,
        };
        match self
            .client
            .post::<_, LobbySent>(routes::LOBBIES_SEND, &request)
        {
            Ok(_) => Ok(1),
            Err(http::Error::Status(404 | 409, Some(err))) => Err(format!("§c{}", err.message)),
            Err(err) => {
                crate::log(
                    crate::Level::Warn,
                    &format!("sending to a lobby failed: {err}"),
                );
                Err(crate::UNAVAILABLE.into())
            }
        }
    }

    fn issuer(&self, sender: &CommandSender) -> Option<Uuid> {
        sender
            .player_uuid
            .as_deref()
            .and_then(|uuid| Uuid::parse_str(uuid).ok())
    }

    fn punish(
        &self,
        sender: &CommandSender,
        kind: PunishmentKind,
        target: &str,
        reason: &str,
        duration_seconds: Option<u64>,
    ) -> Result<i32, String> {
        let request = IssuePunishment {
            kind,
            target: target.to_owned(),
            issuer: self.issuer(sender),
            reason: reason.to_owned(),
            duration_seconds,
            staff: None,
        };
        self.moderate(sender, routes::PUNISHMENTS, &request)
    }

    fn moderate<B: serde::Serialize>(
        &self,
        sender: &CommandSender,
        path: &str,
        body: &B,
    ) -> Result<i32, String> {
        match self.client.post::<_, PunishmentReply>(path, body) {
            Ok(answer) => {
                reply(sender, &answer.message);
                Ok(i32::from(answer.ok))
            }
            Err(http::Error::Status(503, _)) => {
                Err("§cPunishments need the network's database, which isn't set up.".into())
            }
            Err(err) => {
                crate::log(crate::Level::Warn, &format!("punishment failed: {err}"));
                Err(crate::UNAVAILABLE.into())
            }
        }
    }

    fn history(&self, sender: &CommandSender, target: &str) -> Result<i32, String> {
        let records: Vec<PunishmentRecord> =
            match self.client.get(&routes::punishment_history(target)) {
                Ok(records) => records,
                Err(err) if err.is_not_found() => {
                    return Err(format!("§cNo player called {target} has joined."));
                }
                Err(http::Error::Status(503, _)) => {
                    return Err(
                        "§cPunishments need the network's database, which isn't set up.".into(),
                    );
                }
                Err(err) => {
                    crate::log(crate::Level::Warn, &format!("history failed: {err}"));
                    return Err(crate::UNAVAILABLE.into());
                }
            };
        if records.is_empty() {
            reply(sender, &format!("§a{target} has a clean record."));
            return Ok(1);
        }
        reply(
            sender,
            &format!("§ePunishments of {target} §7(newest first):"),
        );
        for record in records {
            let kind = match record.kind {
                PunishmentKind::Ban => "Ban",
                PunishmentKind::Mute => "Mute",
                PunishmentKind::Kick => "Kick",
                PunishmentKind::Warn => "Warn",
            };
            let state = if record.active { " §c(active)" } else { "" };
            reply(
                sender,
                &format!(
                    "§7#{} §f{kind}{state} §7by {}: §f{}",
                    record.id, record.issuer, record.reason
                ),
            );
        }
        Ok(1)
    }

    /// Whether `sender` holds `node`. The console holds everything.
    fn allowed(&self, sender: &CommandSender, node: &str) -> bool {
        match (&sender.sender_type, &sender.player_uuid) {
            (CommandSenderType::Console, _) => true,
            (_, Some(uuid)) => self
                .permissions
                .get(uuid)
                .is_some_and(|nodes| has_permission(nodes, node)),
            (_, None) => false,
        }
    }

    fn player_id(&self, sender: &CommandSender) -> Result<Uuid, String> {
        sender
            .player_uuid
            .as_deref()
            .and_then(|uuid| Uuid::parse_str(uuid).ok())
            .ok_or_else(|| "§cOnly players can do that.".to_owned())
    }

    fn party(&self, sender: &CommandSender, action: &PartyAction) -> Result<i32, String> {
        match self.client.post::<_, PartyReply>(routes::PARTIES, action) {
            Ok(answer) => {
                if !answer.message.is_empty() {
                    reply(sender, &answer.message);
                }
                Ok(i32::from(answer.ok))
            }
            Err(err) => {
                crate::log(crate::Level::Warn, &format!("party action failed: {err}"));
                Err(crate::UNAVAILABLE.into())
            }
        }
    }

    fn locate(&self, name: &str) -> Result<PlayerLocation, String> {
        match self.client.get(&routes::player_by_name(name)) {
            Ok(location) => Ok(location),
            Err(err) if err.is_not_found() => Err(format!("§c{name} isn't online.")),
            Err(err) => {
                crate::log(crate::Level::Warn, &format!("player lookup failed: {err}"));
                Err(crate::UNAVAILABLE.into())
            }
        }
    }

    fn network(&self, command: &NetworkCommand) -> Result<(), String> {
        match self.client.post_empty(routes::COMMANDS, command) {
            Ok(()) => Ok(()),
            // The player left between the lookup and the command.
            Err(err) if err.is_not_found() => Err("§cThat player just went offline.".into()),
            Err(err) => {
                crate::log(crate::Level::Warn, &format!("command failed: {err}"));
                Err(crate::UNAVAILABLE.into())
            }
        }
    }
}

/// Reads `/friend ...` into an action; `None` means show the usage.
fn friend_action(player: Uuid, args: &[String]) -> Option<FriendAction> {
    let Some((first, rest)) = args.split_first() else {
        return Some(FriendAction::List { player });
    };
    let target = match rest {
        [name] => Some(name.clone()),
        _ => None,
    };
    Some(match first.to_ascii_lowercase().as_str() {
        "add" => FriendAction::Add {
            player,
            target: target?,
        },
        "accept" => FriendAction::Accept {
            player,
            target: target?,
        },
        "deny" => FriendAction::Deny {
            player,
            target: target?,
        },
        "remove" => FriendAction::Remove {
            player,
            target: target?,
        },
        "list" if rest.is_empty() => FriendAction::List { player },
        "requests" if rest.is_empty() => FriendAction::Requests { player },
        // `/friend <player>` adds.
        name if rest.is_empty() && !FRIEND_SUBCOMMANDS.contains(&name) => FriendAction::Add {
            player,
            target: first.clone(),
        },
        _ => return None,
    })
}

/// Reads `/party ...` into an action; `None` means show the usage.
fn party_action(player: Uuid, args: &[String]) -> Option<PartyAction> {
    let (first, rest) = args.split_first()?;
    let one = |rest: &[String]| match rest {
        [name] => Some(name.clone()),
        _ => None,
    };
    Some(match first.to_ascii_lowercase().as_str() {
        "invite" => PartyAction::Invite {
            player,
            target: one(rest)?,
        },
        "accept" => PartyAction::Accept {
            player,
            leader: one(rest)?,
        },
        "kick" | "remove" => PartyAction::Kick {
            player,
            target: one(rest)?,
        },
        "leave" if rest.is_empty() => PartyAction::Leave { player },
        "disband" if rest.is_empty() => PartyAction::Disband { player },
        "warp" if rest.is_empty() => PartyAction::Warp { player },
        "list" if rest.is_empty() => PartyAction::List { player },
        "chat" if !rest.is_empty() => PartyAction::Chat {
            player,
            text: plain(rest),
        },
        // `/party <player>` invites.
        name if rest.is_empty() && !PARTY_SUBCOMMANDS.contains(&name) => PartyAction::Invite {
            player,
            target: first.clone(),
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &str) -> Vec<String> {
        words.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn friend_commands_parse() {
        let me = Uuid::nil();
        assert_eq!(
            friend_action(me, &[]),
            Some(FriendAction::List { player: me })
        );
        assert_eq!(
            friend_action(me, &args("Steve")),
            Some(FriendAction::Add {
                player: me,
                target: "Steve".into()
            })
        );
        assert_eq!(
            friend_action(me, &args("deny Steve")),
            Some(FriendAction::Deny {
                player: me,
                target: "Steve".into()
            })
        );
        assert_eq!(friend_action(me, &args("remove")), None);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30m"), Some(1800));
        assert_eq!(parse_duration("7D"), Some(604_800));
        assert_eq!(parse_duration("1w"), Some(604_800));
        assert_eq!(parse_duration("0h"), None);
        assert_eq!(parse_duration("rude"), None);
        assert_eq!(parse_duration("h"), None);
        assert_eq!(duration_text(40), "40s");
        assert_eq!(duration_text(3 * 86_400 + 4 * 3600 + 5), "3d 4h");
    }

    #[test]
    fn party_commands_parse() {
        let me = Uuid::nil();
        assert_eq!(
            party_action(me, &args("Steve")),
            Some(PartyAction::Invite {
                player: me,
                target: "Steve".into()
            })
        );
        assert_eq!(
            party_action(me, &args("accept Steve")),
            Some(PartyAction::Accept {
                player: me,
                leader: "Steve".into()
            })
        );
        assert_eq!(
            party_action(me, &args("warp")),
            Some(PartyAction::Warp { player: me })
        );
        assert_eq!(
            party_action(me, &args("chat §ahi all")),
            Some(PartyAction::Chat {
                player: me,
                text: "ahi all".into()
            })
        );
        assert_eq!(party_action(me, &args("kick")), None);
        assert_eq!(party_action(me, &args("leave now")), None);
        assert_eq!(party_action(me, &[]), None);
    }
}
