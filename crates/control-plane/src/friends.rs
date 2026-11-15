//! Friends: requests and friendships in PostgreSQL, online status from
//! presence, and notices through the players' proxies.

use hallow_db::{PgPool, RequestOutcome};
use hallow_proto::{
    command::ProxyCommand,
    friend::{FriendAction, FriendReply},
};
use uuid::Uuid;

use crate::{
    accounts::{DbResult, resolve},
    store::Store,
};

fn ok(message: impl Into<String>) -> FriendReply {
    FriendReply {
        ok: true,
        message: message.into(),
    }
}

fn refuse(message: impl Into<String>) -> FriendReply {
    FriendReply {
        ok: false,
        message: format!("§c{}", message.into()),
    }
}

/// Tells `player` `text` on their proxy, if they're online.
async fn tell(store: &Store, player: Uuid, text: String) {
    if let Ok(Some(location)) = store.player(&player).await {
        let message = ProxyCommand::Message {
            player,
            text,
            from: None,
        };
        if let Err(err) = store.send_to_proxy(&location.proxy, &message).await {
            tracing::warn!(%err, "couldn't reach a friend's proxy");
        }
    }
}

pub async fn act(pool: &PgPool, store: &Store, action: FriendAction) -> DbResult<FriendReply> {
    let player = match &action {
        FriendAction::Add { player, .. }
        | FriendAction::Accept { player, .. }
        | FriendAction::Deny { player, .. }
        | FriendAction::Remove { player, .. }
        | FriendAction::List { player }
        | FriendAction::Requests { player } => *player,
    };
    let Ok(Some(me)) = store.player(&player).await else {
        return Ok(refuse("You aren't online."));
    };
    let target = match &action {
        FriendAction::Add { target, .. }
        | FriendAction::Accept { target, .. }
        | FriendAction::Deny { target, .. }
        | FriendAction::Remove { target, .. } => {
            let Some((uuid, name)) = resolve(pool, store, target).await? else {
                return Ok(refuse(format!("No player called {target} has joined.")));
            };
            if uuid == player {
                return Ok(refuse("That's you."));
            }
            Some((uuid, name))
        }
        _ => None,
    };
    let name = me.username;
    match (action, target) {
        (FriendAction::Add { .. }, Some((them, their_name))) => {
            Ok(match hallow_db::request_friend(pool, player, them).await? {
                RequestOutcome::Sent => {
                    tell(
                        store,
                        them,
                        format!(
                            "§e{name} sent you a friend request. §7Type §f/friend accept {name}§7."
                        ),
                    )
                    .await;
                    ok(format!("§eSent a friend request to {their_name}."))
                }
                RequestOutcome::Accepted => {
                    tell(store, them, format!("§a{name} is now your friend.")).await;
                    ok(format!("§aYou and {their_name} are now friends."))
                }
                RequestOutcome::AlreadyFriends => {
                    refuse(format!("You're already friends with {their_name}."))
                }
                RequestOutcome::AlreadySent => {
                    refuse(format!("You already sent {their_name} a request."))
                }
                RequestOutcome::Full => refuse("One of you has too many friends."),
            })
        }
        (FriendAction::Accept { .. }, Some((them, their_name))) => {
            if hallow_db::accept_friend(pool, player, them).await? {
                tell(
                    store,
                    them,
                    format!("§a{name} accepted your friend request."),
                )
                .await;
                Ok(ok(format!("§aYou and {their_name} are now friends.")))
            } else {
                Ok(refuse(format!("{their_name} hasn't sent you a request.")))
            }
        }
        (FriendAction::Deny { .. }, Some((them, their_name))) => {
            if hallow_db::deny_friend(pool, player, them).await? {
                Ok(ok(format!("§eDenied {their_name}'s friend request.")))
            } else {
                Ok(refuse(format!("{their_name} hasn't sent you a request.")))
            }
        }
        (FriendAction::Remove { .. }, Some((them, their_name))) => {
            if hallow_db::remove_friend(pool, player, them).await? {
                Ok(ok(format!("§eRemoved {their_name} from your friends.")))
            } else {
                Ok(refuse(format!("{their_name} isn't your friend.")))
            }
        }
        (FriendAction::List { .. }, _) => {
            let friends = hallow_db::friends_of(pool, player).await?;
            if friends.is_empty() {
                return Ok(ok(
                    "§eYou have no friends yet. §7Add one with §f/friend add <player>§7.",
                ));
            }
            let mut online = Vec::new();
            let mut offline = Vec::new();
            for friend in friends {
                match store.player(&friend.uuid).await {
                    Ok(Some(location)) => {
                        let place = location.server.unwrap_or_else(|| "joining".into());
                        online.push(format!("§a● {} §7({place})", friend.username));
                    }
                    _ => offline.push(format!("§8● {}", friend.username)),
                }
            }
            let mut lines = vec![format!(
                "§eFriends §7({} online of {}):",
                online.len(),
                online.len() + offline.len()
            )];
            lines.extend(online);
            lines.extend(offline);
            Ok(ok(lines.join("\n")))
        }
        (FriendAction::Requests { .. }, _) => {
            let pending = hallow_db::pending_requests(pool, player).await?;
            if pending.is_empty() {
                return Ok(ok("§eNo friend requests."));
            }
            let names: Vec<String> = pending.into_iter().map(|p| p.username).collect();
            Ok(ok(format!(
                "§eFriend requests: §f{}\n§7Answer with §f/friend accept <player>§7 or §f/friend deny <player>§7.",
                names.join(", ")
            )))
        }
        _ => Ok(refuse("Unknown friend action.")),
    }
}

/// Tells a player's online friends that they joined or left the network.
pub async fn announce(pool: &PgPool, store: &Store, player: Uuid, username: &str, joined: bool) {
    let friends = match hallow_db::friends_of(pool, player).await {
        Ok(friends) => friends,
        Err(err) => {
            tracing::warn!(%err, "couldn't load friends to announce");
            return;
        }
    };
    let text = if joined {
        format!("§aFriend > §f{username} §ajoined.")
    } else {
        format!("§eFriend > §f{username} §eleft.")
    };
    for friend in friends {
        tell(store, friend.uuid, text.clone()).await;
    }
}
