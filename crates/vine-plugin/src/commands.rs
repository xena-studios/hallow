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
//! | `/play <game>` | | Queue for a game, with your party if you lead one |
//! | `/leavequeue` (`/lq`) | | Leave the queue |
//!
//! Staff permissions come from the player's ranks in the database, sent with
//! their login (`hallow.staff.*` and the like; see `hallow_proto::permission`).
//! The console may run everything.

use hallow_proto::permission::has_permission;
use hallow_proto::{
    command::NetworkCommand,
    party::{PartyAction, PartyReply},
    presence::PlayerLocation,
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

const PARTY_USAGE: &str = "/party <player> | invite <player> | accept <player> | leave | kick <player> | disband | warp | list | chat <message>";
const PARTY_SUBCOMMANDS: [&str; 8] = [
    "invite", "accept", "leave", "kick", "disband", "warp", "list", "chat",
];

pub fn register(ctx: &Context) {
    ctx.register_command(PARTY, "party", "Play with friends", &["p".into()], None);
    ctx.register_command(PARTY_CHAT, "pc", "Message your party", &[], None);
    ctx.register_command(PLAY, "play", "Queue for a game", &[], None);
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
            .ok_or_else(|| "§cOnly players can use parties.".to_owned())
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
