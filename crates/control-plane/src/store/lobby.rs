//! Picking a hub or lobby instance for a player, and moving them there
//! (`crate::lobbies` decides which kind).

use hallow_proto::{
    command::ProxyCommand,
    server::{ServerInfo, ServerRole, ServerState},
};
use uuid::Uuid;

use super::{Result, Store};

/// The least full of `servers` that is a ready `kind` lobby with room and
/// not in `closed`. Least full spreads players over a kind's instances, so
/// none of them fills while another stands empty, and empty spares stay
/// cheap to end.
pub fn least_full<'a>(
    servers: &'a [ServerInfo],
    kind: &str,
    closed: &[String],
) -> Option<&'a ServerInfo> {
    servers
        .iter()
        .filter(|s| {
            s.role == ServerRole::Lobby
                && s.kind == kind
                && s.state == ServerState::Ready
                && s.players < s.max_players
                && !closed.contains(&s.name)
        })
        .min_by_key(|s| (s.players, s.name.as_str()))
}

impl Store {
    /// The least full ready lobby (or hub) of `kind` with room, skipping
    /// instances that are ending.
    pub async fn pick_lobby(&self, kind: &str) -> Result<Option<ServerInfo>> {
        let closed = self.closed_instances().await?;
        let (_, servers) = self.read_servers().await?;
        Ok(least_full(&servers, kind, &closed).cloned())
    }

    /// Tells `player` where they're going and has `proxy` move them to
    /// `server`, the way the matchmaker does.
    pub async fn move_player(&self, proxy: &str, player: Uuid, server: &str) -> Result<()> {
        self.push_command(
            proxy,
            &ProxyCommand::Message {
                player,
                text: format!("§aSending you to {server}..."),
                from: None,
            },
        )
        .await?;
        self.push_command(
            proxy,
            &ProxyCommand::Send {
                player,
                server: server.to_owned(),
            },
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lobby(name: &str, kind: &str, players: u32, state: ServerState) -> ServerInfo {
        ServerInfo {
            name: name.into(),
            kind: kind.into(),
            version: "dev".into(),
            role: ServerRole::Lobby,
            address: format!("{name}:25565"),
            max_players: 10,
            players,
            state,
            tick_ms: None,
        }
    }

    #[test]
    fn picks_the_least_full_open_lobby() {
        let servers = vec![
            lobby("a", "sw-lobby", 4, ServerState::Ready),
            lobby("b", "sw-lobby", 2, ServerState::Ready),
            lobby("c", "sw-lobby", 0, ServerState::Draining),
            lobby("d", "sw-lobby", 1, ServerState::Ready),
            lobby("e", "sw-lobby", 10, ServerState::Ready),
            lobby("f", "hub", 0, ServerState::Ready),
        ];
        let pick = |closed: &[String]| least_full(&servers, "sw-lobby", closed).map(|s| &*s.name);
        assert_eq!(pick(&[]), Some("d"));
        assert_eq!(pick(&["d".into()]), Some("b"));
        assert_eq!(pick(&["a".into(), "b".into(), "d".into()]), None);
        assert_eq!(least_full(&servers, "nope", &[]), None);
    }
}
