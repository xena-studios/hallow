//! Matchmaking queues in Redis, and the matchmaker pass.
//!
//! | Key (after the prefix) | Type | Holds |
//! |---|---|---|
//! | `queue:{kind}` | list | queue entries (JSON), oldest first |
//! | `queue-kinds` | set | kinds that may have entries |
//! | `queued` | hash | player UUID → `{kind}\|{entry id}` |
//! | `matchmaker-lock` | string with TTL | held by the instance running a pass |
//!
//! An entry is a player or a whole party. A pass walks each queue oldest
//! first and puts every entry that fits on a ready game server of its kind:
//! the fullest server with room for all of it, so games fill and start. An
//! entry that fits nowhere waits; smaller ones behind it can still go.
//! Servers whose instance is ending are skipped. Starting new instances when
//! none has room is the instance lifecycle's job (`crate::instances`), which
//! plans under the same lock just before each pass.

use hallow_proto::{
    command::ProxyCommand,
    queue::{JoinQueue, QueueStatus},
    server::{ServerRole, ServerState},
};
use redis::{AsyncCommands, Script};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Keys, Result, Store, now_ms};

/// How long one instance may hold the matchmaker lock.
const LOCK_MS: u64 = 2000;

/// Deletes lock `KEYS[1]` if it holds `ARGV[1]`.
const UNLOCK: &str = r"
if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('DEL', KEYS[1]) end
return 0
";

impl Keys {
    fn queue(&self, kind: &str) -> String {
        format!("{}:queue:{kind}", self.prefix)
    }
    fn queue_kinds(&self) -> String {
        format!("{}:queue-kinds", self.prefix)
    }
    fn queued(&self) -> String {
        format!("{}:queued", self.prefix)
    }
    fn matchmaker_lock(&self) -> String {
        format!("{}:matchmaker-lock", self.prefix)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    id: String,
    leader: Uuid,
    members: Vec<Uuid>,
    since_ms: u64,
}

/// Where a pass put an entry.
#[derive(Debug, PartialEq, Eq)]
pub struct Placement {
    pub kind: String,
    pub server: String,
    pub players: Vec<Uuid>,
}

impl Store {
    /// Queues a player, or the party they lead. `Err` carries a message for
    /// the player. `startable` says the control plane can start instances
    /// of the kind, so it needn't have a server now.
    pub async fn join_queue(
        &self,
        req: JoinQueue,
        startable: bool,
    ) -> Result<std::result::Result<QueueStatus, String>> {
        if self.player(&req.player).await?.is_none() {
            return Ok(Err("You aren't online.".into()));
        }
        let (servers_rev, servers) = self.read_servers().await?;
        let _ = servers_rev;
        if !startable
            && !servers
                .iter()
                .any(|s| s.kind == req.kind && s.role == ServerRole::Game)
        {
            return Ok(Err(format!("There are no {} games right now.", req.kind)));
        }
        let members = match self.party_of(&req.player).await? {
            Some(party) if party.leader != req.player => {
                return Ok(Err("Only your party leader can queue the party.".into()));
            }
            Some(party) => party.members.into_iter().map(|m| m.uuid).collect(),
            None => vec![req.player],
        };
        // Queuing again (for this kind or another) replaces the old entry.
        for member in &members {
            self.dequeue(member).await?;
        }
        let entry = Entry {
            id: Uuid::new_v4().simple().to_string(),
            leader: req.player,
            members: members.clone(),
            since_ms: now_ms(),
        };
        let tag = format!("{}|{}", req.kind, entry.id);
        let mut pipe = redis::pipe();
        pipe.atomic()
            .rpush(self.keys.queue(&req.kind), serde_json::to_string(&entry)?)
            .ignore()
            .sadd(self.keys.queue_kinds(), &req.kind)
            .ignore();
        for member in &members {
            pipe.hset(self.keys.queued(), member.to_string(), &tag)
                .ignore();
        }
        let _: () = pipe.query_async(&mut self.conn()).await?;
        if members.len() > 1 {
            let others: Vec<String> = members
                .iter()
                .filter(|m| **m != req.player)
                .map(Uuid::to_string)
                .collect();
            self.notify(
                &others,
                &format!("§aYour party leader queued you for {}.", req.kind),
            )
            .await?;
        }
        Ok(Ok(QueueStatus {
            queued: self.queued_players(&req.kind).await?,
            kind: req.kind,
            estimated_wait_seconds: None,
        }))
    }

    /// Takes the entry holding `player` out of its queue. Returns its kind
    /// and members, if there was one.
    pub async fn dequeue(&self, player: &Uuid) -> Result<Option<(String, Vec<Uuid>)>> {
        let tag: Option<String> = self
            .conn()
            .hget(self.keys.queued(), player.to_string())
            .await?;
        let Some((kind, id)) = tag.as_deref().and_then(|t| t.split_once('|')) else {
            return Ok(None);
        };
        let raw: Vec<String> = self.conn().lrange(self.keys.queue(kind), 0, -1).await?;
        let mut members = vec![*player];
        for raw in raw {
            let entry: Entry = serde_json::from_str(&raw)?;
            if entry.id == id {
                let _: () = self.conn().lrem(self.keys.queue(kind), 1, &raw).await?;
                members = entry.members;
                break;
            }
        }
        let fields: Vec<String> = members.iter().map(Uuid::to_string).collect();
        let _: () = self.conn().hdel(self.keys.queued(), fields).await?;
        Ok(Some((kind.to_owned(), members)))
    }

    /// Takes a player who left the network out of the queue, with their
    /// party: a game with someone missing isn't what they queued for.
    pub(super) async fn queue_on_leave(&self, uuid: &str, username: &str) -> Result<()> {
        let Ok(player) = Uuid::parse_str(uuid) else {
            return Ok(());
        };
        if let Some((kind, members)) = self.dequeue(&player).await? {
            let others: Vec<String> = members
                .iter()
                .filter(|m| **m != player)
                .map(Uuid::to_string)
                .collect();
            self.notify(
                &others,
                &format!("§eYou left the {kind} queue because {username} disconnected."),
            )
            .await?;
        }
        Ok(())
    }

    /// Players queued for each kind that has a queue.
    pub async fn queue_sizes(&self) -> Result<Vec<(String, u32)>> {
        let mut kinds: Vec<String> = self.conn().smembers(self.keys.queue_kinds()).await?;
        kinds.sort();
        let mut sizes = Vec::with_capacity(kinds.len());
        for kind in kinds {
            let players = self.queued_players(&kind).await?;
            sizes.push((kind, players));
        }
        Ok(sizes)
    }

    async fn queued_players(&self, kind: &str) -> Result<u32> {
        let raw: Vec<String> = self.conn().lrange(self.keys.queue(kind), 0, -1).await?;
        let mut total = 0;
        for raw in raw {
            let entry: Entry = serde_json::from_str(&raw)?;
            total += u32::try_from(entry.members.len()).unwrap_or(u32::MAX);
        }
        Ok(total)
    }

    /// One matchmaker pass over every queue. Only one instance runs a pass
    /// at a time; the others return at once.
    pub async fn match_pass(&self, instance: &str) -> Result<Vec<Placement>> {
        if !self.lock_matchmaker(instance).await? {
            return Ok(Vec::new());
        }
        let placements = self.place_all().await;
        self.unlock_matchmaker(instance).await?;
        placements
    }

    /// Takes the matchmaker lock for `instance` for up to [`LOCK_MS`].
    /// Returns false if another instance holds it.
    pub async fn lock_matchmaker(&self, instance: &str) -> Result<bool> {
        Ok(redis::cmd("SET")
            .arg(self.keys.matchmaker_lock())
            .arg(instance)
            .arg("NX")
            .arg("PX")
            .arg(LOCK_MS)
            .query_async::<Option<String>>(&mut self.conn())
            .await?
            .is_some())
    }

    /// Gives the lock back, if `instance` still holds it.
    pub async fn unlock_matchmaker(&self, instance: &str) -> Result<()> {
        let _: i64 = Script::new(UNLOCK)
            .key(self.keys.matchmaker_lock())
            .arg(instance)
            .invoke_async(&mut self.conn())
            .await?;
        Ok(())
    }

    /// Every queue's entries' members, oldest first.
    pub(super) async fn queue_entries(&self) -> Result<Vec<(String, Vec<Vec<Uuid>>)>> {
        let mut kinds: Vec<String> = self.conn().smembers(self.keys.queue_kinds()).await?;
        kinds.sort();
        let mut all = Vec::with_capacity(kinds.len());
        for kind in kinds {
            let raw: Vec<String> = self.conn().lrange(self.keys.queue(&kind), 0, -1).await?;
            let entries = raw
                .iter()
                .map(|raw| serde_json::from_str::<Entry>(raw).map(|e| e.members))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            all.push((kind, entries));
        }
        Ok(all)
    }

    /// Places what it can. The caller holds the matchmaker lock.
    pub async fn place_all(&self) -> Result<Vec<Placement>> {
        let mut placements = Vec::new();
        let kinds: Vec<String> = self.conn().smembers(self.keys.queue_kinds()).await?;
        if kinds.is_empty() {
            return Ok(placements);
        }
        let closed = self.closed_instances().await?;
        let (_, mut servers) = self.read_servers().await?;
        servers.retain(|s| !closed.contains(&s.name));
        for kind in kinds {
            let raw: Vec<String> = self.conn().lrange(self.keys.queue(&kind), 0, -1).await?;
            if raw.is_empty() {
                let _: () = self.conn().srem(self.keys.queue_kinds(), &kind).await?;
                continue;
            }
            for raw in raw {
                let entry: Entry = serde_json::from_str(&raw)?;
                let mut online = Vec::new();
                for member in &entry.members {
                    if let Some(location) = self.player(member).await? {
                        online.push((*member, location.proxy));
                    }
                }
                let size = u32::try_from(online.len()).unwrap_or(u32::MAX);
                // The fullest ready server with room for the whole entry.
                let target = servers
                    .iter_mut()
                    .filter(|s| {
                        s.kind == kind
                            && s.role == ServerRole::Game
                            && s.state == ServerState::Ready
                            && s.max_players.saturating_sub(s.players) >= size
                    })
                    .max_by_key(|s| s.players);
                if size > 0 && target.is_none() {
                    continue;
                }
                let _: () = self.conn().lrem(self.keys.queue(&kind), 1, &raw).await?;
                let fields: Vec<String> = entry.members.iter().map(Uuid::to_string).collect();
                let _: () = self.conn().hdel(self.keys.queued(), fields).await?;
                let Some(server) = target else {
                    // Everyone in it went offline.
                    continue;
                };
                // Count them now; the server's next heartbeat has the truth.
                server.players += size;
                let name = server.name.clone();
                let _: () = self
                    .conn()
                    .hset(self.keys.servers(), &name, serde_json::to_string(&*server)?)
                    .await?;
                for (player, proxy) in &online {
                    self.push_command(
                        proxy,
                        &ProxyCommand::Message {
                            player: *player,
                            text: format!("§aSending you to {name}..."),
                            from: None,
                        },
                    )
                    .await?;
                    self.push_command(
                        proxy,
                        &ProxyCommand::Send {
                            player: *player,
                            server: name.clone(),
                        },
                    )
                    .await?;
                }
                placements.push(Placement {
                    kind: kind.clone(),
                    server: name,
                    players: online.into_iter().map(|(p, _)| p).collect(),
                });
            }
        }
        Ok(placements)
    }
}
