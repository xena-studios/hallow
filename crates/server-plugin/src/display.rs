//! Rank prefixes in chat and the tab list.
//!
//! Every [`REFRESH`], and soon after anyone joins, the task asks
//! `POST /v1/display` for the primary rank of everyone on the server and
//! keeps their prefixes. Then:
//!
//! - **Tab list.** Each player's name in the tab list carries their prefix,
//!   such as `[ADMIN] Steve` in red.
//! - **Chat.** A message from a player whose prefix is known is sent as
//!   `<prefix><name>: <message>` to everyone on the server, in place of
//!   Pumpkin's own line. Players without a rank show their name in grey.
//!   A message another plugin cancelled stays cancelled; one from a player
//!   not yet known goes out as Pumpkin formats it.
//!
//! Prefixes are the panel's, with `&` colour codes. Mutes are enforced on
//! the proxy, before chat reaches the server.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hallow_proto::{
    routes,
    stats::{DisplayRequest, PlayerDisplay},
};
use pumpkin_plugin_api::{
    Context, Server,
    events::{EventHandler, EventPriority, PlayerChatEvent, PlayerJoinEvent},
    events_wit::{PlayerChatEventData, PlayerJoinEventData},
    text::TextComponent,
};
use uuid::Uuid;

use crate::collect::uuid;
use crate::http::{Client, Pending};

/// How often prefixes are fetched again, so rank changes show up.
pub const REFRESH: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Display {
    /// Prefix (with `§` codes) by player; empty for no rank.
    prefixes: BTreeMap<Uuid, String>,
    /// Players whose tab name shows what's in `prefixes`.
    applied: BTreeMap<Uuid, String>,
    fetch: Option<Pending>,
    fetched: Option<Instant>,
    /// Someone joined since the last fetch.
    stale: bool,
}

static DISPLAY: Mutex<Option<Display>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Display) -> R) -> R {
    f(DISPLAY
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .get_or_insert_with(Display::default))
}

/// `&` colour and format codes as `§` codes; other `&`s are kept.
#[must_use]
pub fn legacy(prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len());
    let mut chars = prefix.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&'
            && let Some(code) = chars.peek()
            && matches!(code.to_ascii_lowercase(), '0'..='9' | 'a'..='f' | 'k'..='o' | 'r')
        {
            out.push('§');
            continue;
        }
        out.push(c);
    }
    out
}

/// A chat line: the prefix and name, then the message in white. Players
/// without a rank are grey.
#[must_use]
pub fn chat_line(prefix: &str, name: &str, message: &str) -> String {
    let lead = if prefix.is_empty() { "§7" } else { prefix };
    format!("{lead}{name}§8: §f{message}")
}

/// The tab list name; `None` for no rank, Pumpkin's plain name.
#[must_use]
pub fn tab_name(prefix: &str, name: &str) -> Option<String> {
    (!prefix.is_empty()).then(|| format!("{prefix}{name}"))
}

pub fn register(context: &Context) {
    let results = [
        (
            "join",
            context.register_event_handler(OnJoin, EventPriority::Lowest, false),
        ),
        (
            "chat",
            // Lowest runs last, after any game that cancels chat; blocking,
            // so cancelling Pumpkin's own line takes effect.
            context.register_event_handler(OnChat, EventPriority::Lowest, true),
        ),
    ];
    for (event, result) in results {
        if let Err(err) = result {
            tracing::warn!("hallow-server: can't subscribe to {event} events for ranks: {err}");
        }
    }
}

/// Reads the fetch in flight, starts the next one when due, and puts
/// prefixes on tab names.
pub fn tick(server: &Server, client: &Client) {
    if let Some(pending) = with(|d| d.fetch.take()) {
        match pending.poll_body() {
            None => {
                with(|d| d.fetch = Some(pending));
                return;
            }
            Some(Ok(body)) => match serde_json::from_slice::<Vec<PlayerDisplay>>(&body) {
                Ok(lines) => with(|d| {
                    for line in lines {
                        d.prefixes.insert(line.uuid, legacy(&line.prefix));
                    }
                }),
                Err(err) => tracing::warn!("hallow-server: bad display answer: {err}"),
            },
            // Retried at the next refresh; chat falls back to Pumpkin's.
            Some(Err(err)) => tracing::debug!("hallow-server: couldn't fetch ranks: {err}"),
        }
    }

    let players = server.get_all_players();
    let online: BTreeSet<Uuid> = players.iter().map(|p| uuid(&p.get_id())).collect();
    for player in &players {
        let id = uuid(&player.get_id());
        let Some(prefix) = with(|d| {
            let prefix = d.prefixes.get(&id)?.clone();
            (d.applied.get(&id) != Some(&prefix)).then(|| {
                d.applied.insert(id, prefix.clone());
                prefix
            })
        }) else {
            continue;
        };
        let name = tab_name(&prefix, &player.get_name());
        player.set_tab_list_name(name.as_deref().map(TextComponent::from_legacy_string));
    }

    let due = with(|d| {
        d.prefixes.retain(|id, _| online.contains(id));
        d.applied.retain(|id, _| online.contains(id));
        !online.is_empty() && (d.stale || d.fetched.is_none_or(|at| at.elapsed() >= REFRESH))
    });
    if !due {
        return;
    }
    let request = DisplayRequest {
        players: online.into_iter().collect(),
    };
    with(|d| {
        d.stale = false;
        d.fetched = Some(Instant::now());
    });
    match client.start_post(routes::DISPLAY, &request) {
        Ok(pending) => with(|d| d.fetch = Some(pending)),
        Err(err) => tracing::debug!("hallow-server: couldn't fetch ranks: {err}"),
    }
}

struct OnJoin;

impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, _server: Server, event: PlayerJoinEventData) -> PlayerJoinEventData {
        with(|d| d.stale = true);
        event
    }
}

struct OnChat;

impl EventHandler<PlayerChatEvent> for OnChat {
    fn handle(&self, server: Server, mut event: PlayerChatEventData) -> PlayerChatEventData {
        if event.cancelled {
            return event;
        }
        let id = uuid(&event.player.get_id());
        let Some(prefix) = with(|d| d.prefixes.get(&id).cloned()) else {
            return event;
        };
        let line = chat_line(&prefix, &event.player.get_name(), &event.message);
        tracing::info!("<chat> {}: {}", event.player.get_name(), event.message);
        for player in server.get_all_players() {
            player.send_system_message(TextComponent::from_legacy_string(&line), false);
        }
        event.cancelled = true;
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_become_legacy_codes() {
        assert_eq!(legacy("&c[ADMIN] "), "§c[ADMIN] ");
        assert_eq!(legacy("&6&l[MVP&r&6] "), "§6§l[MVP§r§6] ");
        assert_eq!(legacy("Rock & Roll &z"), "Rock & Roll &z");
        assert_eq!(legacy("&"), "&");
    }

    #[test]
    fn lines() {
        assert_eq!(
            chat_line("§c[ADMIN] ", "Steve", "hi"),
            "§c[ADMIN] Steve§8: §fhi"
        );
        assert_eq!(chat_line("", "Alex", "hey"), "§7Alex§8: §fhey");
        assert_eq!(
            tab_name("§a[VIP] ", "Alex").as_deref(),
            Some("§a[VIP] Alex")
        );
        assert_eq!(tab_name("", "Alex"), None);
    }
}
