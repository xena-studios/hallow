//! Commands that reach players on any proxy, through the control plane.
//!
//! | Command | Permission | |
//! |---|---|---|
//! | `/msg <player> <message>` (`/tell`, `/w`, `/whisper`) | | Private message |
//! | `/r <message>` (`/reply`) | | Reply to the last private message |
//! | `/find <player>` | `hallow.staff.find` | Which server and proxy a player is on |
//! | `/alert <message>` | `hallow.staff.alert` | Message every player on the network |
//! | `/gkick <player> [reason]` | `hallow.staff.kick` | Disconnect a player from the network |
//!
//! Permissions are Vine's (`[permissions]` in `vine.toml`) until ranks come
//! from the database.

use hallow_proto::{command::NetworkCommand, presence::PlayerLocation, routes};

use crate::vine::plugin::{command::reply, proxy, types::CommandSenderType};
use crate::{CommandSender, Context, State};

const MSG: u32 = 10;
const REPLY: u32 = 11;
const FIND: u32 = 12;
const ALERT: u32 = 13;
const GKICK: u32 = 14;

pub fn register(ctx: &Context) {
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
    ctx.register_command(
        FIND,
        "find",
        "Show which server a player is on",
        &[],
        Some("hallow.staff.find"),
    );
    ctx.register_command(
        ALERT,
        "alert",
        "Message every player on the network",
        &[],
        Some("hallow.staff.alert"),
    );
    ctx.register_command(
        GKICK,
        "gkick",
        "Disconnect a player from the network",
        &[],
        Some("hallow.staff.kick"),
    );
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
            _ => Err("§cUnknown command.".into()),
        }
    }

    /// Names of players on this proxy, for tab completion. Players on other
    /// proxies can still be typed in full.
    pub fn suggest(&self, id: u32, args: &[String]) -> Vec<String> {
        if !matches!(id, MSG | FIND | GKICK) || args.len() > 1 {
            return Vec::new();
        }
        let prefix = args
            .first()
            .map(|a| a.to_ascii_lowercase())
            .unwrap_or_default();
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
