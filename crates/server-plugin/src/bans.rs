//! A banned player's ban, for the banned lobby (`servers/banned`).
//!
//! The banned lobby's own plugin asks over IPC, `ban <uuid>`, and shows the
//! reply: the reason, who issued it, when it ends and how to appeal. The
//! first ask for a player starts `GET /v1/bans/{uuid}`; the reply is the last
//! answer fetched, as [`BanInfo`](hallow_proto::punishment::BanInfo) JSON, or
//! an error until the first one arrives. While a player is asked about, the
//! ban is fetched again every [`REFRESH`], so an extended or shortened ban
//! shows. Players who left the server are forgotten.
//!
//! | Message | Reply |
//! |---|---|
//! | `ban <uuid>` | [`BanInfo`](hallow_proto::punishment::BanInfo) JSON, `not banned` (error) if the player has no active ban, or an error while it's fetched |
//!
//! The control plane answers a server's scoped token for players on that
//! server only.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hallow_proto::{punishment::BanInfo, routes};
use pumpkin_plugin_api::{Server, wit::pumpkin::plugin::uuid::Uuid as WitUuid};
use uuid::Uuid;

use crate::http::{self, Client, Pending};

/// How often a player's ban is fetched again while asked about.
pub const REFRESH: Duration = Duration::from_secs(5);

/// The reply while a player has no active ban.
pub const NOT_BANNED: &str = "not banned";

#[derive(Default)]
struct Entry {
    /// The last answer: the ban as JSON, or `None` for not banned.
    answer: Option<Option<Vec<u8>>>,
    fetch: Option<Pending>,
    fetched: Option<Instant>,
}

static BANS: Mutex<Option<BTreeMap<Uuid, Entry>>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut BTreeMap<Uuid, Entry>) -> R) -> R {
    f(BANS
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .get_or_insert_with(BTreeMap::new))
}

/// A `ban <uuid>` message, or `None` if it isn't one.
pub fn parse(text: &str) -> Option<Result<Uuid, String>> {
    let rest = text.strip_prefix("ban")?;
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    let mut words = rest.split_whitespace();
    let parsed = match (words.next().map(Uuid::parse_str), words.next()) {
        (Some(Ok(player)), None) => Ok(player),
        _ => Err("ban takes one player UUID".to_owned()),
    };
    Some(parsed)
}

/// Handles `ban <uuid>`; `None` for any other message.
pub fn handle_ipc(text: &str, client: Option<Client>) -> Option<Result<Vec<u8>, String>> {
    let player = match parse(text)? {
        Ok(player) => player,
        Err(err) => return Some(Err(err)),
    };
    let (answer, start) = with(|bans| {
        let entry = bans.entry(player).or_default();
        (
            entry.answer.clone(),
            entry.fetch.is_none() && entry.fetched.is_none(),
        )
    });
    if start && let Some(client) = &client {
        start_fetch(client, player);
    }
    Some(match answer {
        Some(Some(json)) => Ok(json),
        Some(None) => Err(NOT_BANNED.into()),
        None if client.is_none() => Err("not linked to a control plane".into()),
        None => Err("fetching the ban; ask again in a moment".into()),
    })
}

fn start_fetch(client: &Client, player: Uuid) {
    match client.start_get(&routes::ban_of(&player.to_string())) {
        Ok(pending) => with(|bans| {
            let entry = bans.entry(player).or_default();
            entry.fetch = Some(pending);
            entry.fetched = Some(Instant::now());
        }),
        Err(err) => tracing::warn!("hallow-server: couldn't fetch {player}'s ban: {err}"),
    }
}

/// What a finished fetch means for the cache: `Some(answer)` to keep, or
/// `None` to keep the last answer (the control plane couldn't say).
fn answer_of(result: Result<Vec<u8>, http::Error>) -> Option<Option<Vec<u8>>> {
    match result {
        Ok(body) if serde_json::from_slice::<BanInfo>(&body).is_ok() => Some(Some(body)),
        Ok(_) => {
            tracing::warn!("hallow-server: the control plane sent a ban that doesn't parse");
            None
        }
        Err(err) if err.is_not_found() => Some(None),
        Err(err) => {
            tracing::warn!("hallow-server: fetching a ban failed: {err}");
            None
        }
    }
}

/// Runs once a second, from the plugin's task: finishes fetches, refetches
/// what's due, and forgets players who left.
pub fn tick(server: &Server, client: &Client) {
    let players: Vec<Uuid> = with(|bans| bans.keys().copied().collect());
    for player in players {
        let (high, low) = player.as_u64_pair();
        if server.get_player_by_uuid(WitUuid { high, low }).is_none() {
            with(|bans| bans.remove(&player));
            continue;
        }
        let pending = with(|bans| bans.get_mut(&player).and_then(|e| e.fetch.take()));
        if let Some(pending) = pending {
            match pending.poll_body() {
                None => with(|bans| {
                    if let Some(entry) = bans.get_mut(&player) {
                        entry.fetch = Some(pending);
                    }
                }),
                Some(result) => {
                    if let Some(answer) = answer_of(result) {
                        with(|bans| {
                            if let Some(entry) = bans.get_mut(&player) {
                                entry.answer = Some(answer);
                            }
                        });
                    }
                }
            }
        }
        let due = with(|bans| {
            bans.get(&player).is_some_and(|e| {
                e.fetch.is_none() && e.fetched.is_none_or(|at| at.elapsed() >= REFRESH)
            })
        });
        if due {
            start_fetch(client, player);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ban_messages_parse() {
        let id = Uuid::from_u128(9);
        assert_eq!(parse(&format!("ban {id}")), Some(Ok(id)));
        assert!(parse("ban").unwrap().is_err());
        assert!(parse("ban nobody").unwrap().is_err());
        assert!(parse(&format!("ban {id} extra")).unwrap().is_err());
        assert_eq!(parse("banned"), None);
        assert_eq!(parse("lobbies"), None);
    }

    #[test]
    fn answers_come_from_the_cache() {
        let id = Uuid::from_u128(10);
        // Not linked: nothing to fetch with.
        assert!(handle_ipc(&format!("ban {id}"), None).unwrap().is_err());
        with(|bans| bans.entry(id).or_default().answer = Some(Some(b"{}".to_vec())));
        assert_eq!(
            handle_ipc(&format!("ban {id}"), None).unwrap().unwrap(),
            b"{}"
        );
        with(|bans| bans.entry(id).or_default().answer = Some(None));
        assert_eq!(
            handle_ipc(&format!("ban {id}"), None).unwrap().unwrap_err(),
            NOT_BANNED
        );
        assert!(handle_ipc("lobbies", None).is_none());
    }

    #[test]
    fn not_found_means_not_banned() {
        assert_eq!(answer_of(Err(http::Error::Status(404, None))), Some(None));
        assert_eq!(answer_of(Err(http::Error::Status(503, None))), None);
        assert_eq!(answer_of(Ok(b"nonsense".to_vec())), None);
        let ban = serde_json::json!({
            "id": 4, "reason": "Cheating", "issuer": "console", "issuer_kind": "console",
            "created_ms": 1, "expires_ms": null
        });
        let body = serde_json::to_vec(&ban).unwrap();
        assert_eq!(answer_of(Ok(body.clone())), Some(Some(body)));
    }
}
