//! Live state in Redis.
//!
//! | Key (after the prefix) | Type | Holds |
//! |---|---|---|
//! | `servers` | hash | server name → [`ServerInfo`] JSON |
//! | `servers:rev` | integer | server list revision, bumped when a server is added, removed or changes state |
//! | `server-alive:{name}` | string with TTL | present while the server heartbeats |
//! | `server-players:{name}` | hash | player UUID → username, as that server reports them |
//! | `player-servers` | hash | player UUID → the server that last reported them joining, the servers' view of where everyone is |
//! | `proxies` | hash | proxy id → [`ProxyInfo`] JSON |
//! | `proxy-alive:{id}` | string with TTL | present while the proxy heartbeats |
//! | `proxy-drain:{id}` | string | present while staff want the proxy drained; kept across restarts and expiry |
//! | `online` | hash | player UUID → [`PlayerLocation`] JSON |
//! | `proxy-players:{id}` | set | UUIDs on that proxy, to clean up when it goes |
//! | `sessions` | hash | player UUID → their open PostgreSQL session id |
//! | `names` | hash | lower-case username → UUID of online players, for lookups by name |
//! | `commands:{id}` | sorted set | `"{rev}\|{json}"` [`ProxyCommand`]s scored by revision, last [`COMMANDS_KEPT`] |
//! | `commands:{id}:rev` | integer | that proxy's command revision |
//!
//! Changes that need several keys to agree run as Lua scripts, so concurrent
//! control plane instances never see half of one.

use std::time::{SystemTime, UNIX_EPOCH};

use hallow_proto::{
    command::{NetworkCommand, ProxyCommand},
    presence::{LoginDecision, LoginRequest, PlayerLeft, PlayerLocation, PlayerSwitched},
    proxy::{ProxyHeartbeat, ProxyInfo, ProxyRegistered, RegisterProxy},
    server::{
        RegisterServer, ServerHeartbeat, ServerInfo, ServerPlayer, ServerPlayersChanged,
        ServerState,
    },
};
use redis::{AsyncCommands, Script, aio::ConnectionManager};
use uuid::Uuid;

use crate::{Config, DuplicateLogin, events};

mod instances;
mod lobby;
mod party;
mod queue;

pub use queue::Placement;

/// Commands kept per proxy for polls that fall behind.
pub const COMMANDS_KEPT: u64 = 1000;

#[derive(Debug)]
pub enum StoreError {
    Redis(redis::RedisError),
    /// A stored value didn't parse; only possible if something else wrote it.
    Corrupt(serde_json::Error),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Redis(err) => write!(f, "redis: {err}"),
            Self::Corrupt(err) => write!(f, "bad value in redis: {err}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<redis::RedisError> for StoreError {
    fn from(err: redis::RedisError) -> Self {
        Self::Redis(err)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(err: serde_json::Error) -> Self {
        Self::Corrupt(err)
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Key names under one prefix.
#[derive(Clone, Debug)]
pub struct Keys {
    prefix: String,
}

impl Keys {
    #[must_use]
    pub fn new(prefix: &str) -> Self {
        Self {
            prefix: prefix.to_owned(),
        }
    }
    fn servers(&self) -> String {
        format!("{}:servers", self.prefix)
    }
    fn servers_rev(&self) -> String {
        format!("{}:servers:rev", self.prefix)
    }
    fn server_alive(&self, name: &str) -> String {
        format!("{}:server-alive:{name}", self.prefix)
    }
    fn server_players_prefix(&self) -> String {
        format!("{}:server-players:", self.prefix)
    }
    fn server_players(&self, name: &str) -> String {
        format!("{}{name}", self.server_players_prefix())
    }
    fn player_servers(&self) -> String {
        format!("{}:player-servers", self.prefix)
    }
    fn proxies(&self) -> String {
        format!("{}:proxies", self.prefix)
    }
    fn proxy_alive(&self, id: &str) -> String {
        format!("{}:proxy-alive:{id}", self.prefix)
    }
    fn proxy_drain(&self, id: &str) -> String {
        format!("{}:proxy-drain:{id}", self.prefix)
    }
    fn online(&self) -> String {
        format!("{}:online", self.prefix)
    }
    fn sessions(&self) -> String {
        format!("{}:sessions", self.prefix)
    }
    fn names(&self) -> String {
        format!("{}:names", self.prefix)
    }
    fn proxy_players(&self, id: &str) -> String {
        format!("{}:proxy-players:{id}", self.prefix)
    }
    fn commands(&self, id: &str) -> String {
        format!("{}:commands:{id}", self.prefix)
    }
    fn commands_rev(&self, id: &str) -> String {
        format!("{}:commands:{id}:rev", self.prefix)
    }
    /// The pub/sub channel for [`events`] topics.
    #[must_use]
    pub fn channel(&self) -> String {
        format!("{}:events", self.prefix)
    }
}

/// Writes `ARGV[2]` to field `ARGV[1]` of hash `KEYS[1]` and refreshes the
/// alive key `KEYS[2]` for `ARGV[3]` ms, only if the field already exists.
/// With `ARGV[4] == "1"` it also bumps revision `KEYS[3]` and publishes
/// `ARGV[6]` on channel `ARGV[5]`. Returns 1 if written.
const UPDATE_IF_PRESENT: &str = r"
if redis.call('HEXISTS', KEYS[1], ARGV[1]) == 0 then return 0 end
redis.call('HSET', KEYS[1], ARGV[1], ARGV[2])
redis.call('SET', KEYS[2], '1', 'PX', ARGV[3])
if ARGV[4] == '1' then
  redis.call('INCR', KEYS[3])
  redis.call('PUBLISH', ARGV[5], ARGV[6])
end
return 1
";

/// Removes field `ARGV[1]` from hash `KEYS[1]` unless its alive key `KEYS[2]`
/// exists. Returns 1 if removed.
const REAP: &str = r"
if redis.call('EXISTS', KEYS[2]) == 1 then return 0 end
return redis.call('HDEL', KEYS[1], ARGV[1])
";

/// Removes player `ARGV[1]` from `online` (`KEYS[1]`), from proxy set
/// `KEYS[2]` and from the name index `KEYS[3]`, only if they are on proxy
/// `ARGV[2]`. A late leave from an old session must not remove a newer login
/// elsewhere. Returns 1 if removed.
const LEAVE: &str = r"
local v = redis.call('HGET', KEYS[1], ARGV[1])
if not v then return 0 end
local p = cjson.decode(v)
if p.proxy ~= ARGV[2] then return 0 end
redis.call('HDEL', KEYS[1], ARGV[1])
redis.call('SREM', KEYS[2], ARGV[1])
local name = string.lower(p.username)
if redis.call('HGET', KEYS[3], name) == ARGV[1] then
  redis.call('HDEL', KEYS[3], name)
end
return 1
";

/// Sets player `ARGV[1]`'s server to `ARGV[2]` in `online` (`KEYS[1]`), if
/// they are online. Returns 1 if updated.
const SWITCH: &str = r"
local v = redis.call('HGET', KEYS[1], ARGV[1])
if not v then return 0 end
local p = cjson.decode(v)
p.server = ARGV[2]
redis.call('HSET', KEYS[1], ARGV[1], cjson.encode(p))
return 1
";

/// Applies a server's player report, if server `ARGV[1]` is still in the
/// server hash `KEYS[1]`. `KEYS[2]` is `player-servers`, `KEYS[3]` the
/// server's `server-players` hash and `ARGV[2]` the prefix of the other
/// servers' ones. `ARGV[4]` is a JSON list of `[uuid, username]` who joined
/// and `ARGV[5]` one of UUIDs who left. With `ARGV[3] == "1"` the joined list
/// is everyone on the server, and anyone else is taken off it. A player who
/// joins comes off the server the index had them on. Returns 1 if applied.
const SERVER_PLAYERS: &str = r"
if redis.call('HEXISTS', KEYS[1], ARGV[1]) == 0 then return 0 end
local joined = cjson.decode(ARGV[4])
local left = cjson.decode(ARGV[5])
if ARGV[3] == '1' then
  local keep = {}
  for _, p in ipairs(joined) do keep[p[1]] = true end
  for _, uuid in ipairs(redis.call('HKEYS', KEYS[3])) do
    if not keep[uuid] then table.insert(left, uuid) end
  end
end
for _, uuid in ipairs(left) do
  redis.call('HDEL', KEYS[3], uuid)
  if redis.call('HGET', KEYS[2], uuid) == ARGV[1] then
    redis.call('HDEL', KEYS[2], uuid)
  end
end
for _, p in ipairs(joined) do
  local old = redis.call('HGET', KEYS[2], p[1])
  if old and old ~= ARGV[1] then
    redis.call('HDEL', ARGV[2] .. old, p[1])
  end
  redis.call('HSET', KEYS[3], p[1], p[2])
  redis.call('HSET', KEYS[2], p[1], ARGV[1])
end
return 1
";

/// Forgets every player of server `ARGV[1]`: its `server-players` hash
/// `KEYS[2]`, and its entries in `player-servers` (`KEYS[1]`).
const DROP_SERVER_PLAYERS: &str = r"
for _, uuid in ipairs(redis.call('HKEYS', KEYS[2])) do
  if redis.call('HGET', KEYS[1], uuid) == ARGV[1] then
    redis.call('HDEL', KEYS[1], uuid)
  end
end
redis.call('DEL', KEYS[2])
return 1
";

/// Appends command `ARGV[1]` to sorted set `KEYS[1]` under the next revision
/// from `KEYS[2]`, keeps the last `ARGV[2]`, and publishes `ARGV[4]` on
/// channel `ARGV[3]`. Returns the new revision.
const PUSH_COMMAND: &str = r"
local rev = redis.call('INCR', KEYS[2])
redis.call('ZADD', KEYS[1], rev, rev .. '|' .. ARGV[1])
redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', rev - tonumber(ARGV[2]))
redis.call('PUBLISH', ARGV[3], ARGV[4])
return rev
";

/// Why a server registration was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum RegisterError {
    /// Another live server already uses this name, at a different address.
    NameTaken,
}

/// A proxy's state for the load balancer's agent check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    /// Registered, heartbeating and taking logins.
    Ready,
    /// Taking no new logins; players on it stay.
    Drain,
    /// Unknown or expired.
    Down,
}

/// What [`Store::reap`] removed.
#[derive(Debug, Default)]
pub struct Reaped {
    pub servers: Vec<String>,
    pub proxies: Vec<String>,
}

/// The result of [`Store::login`].
#[derive(Debug, PartialEq, Eq)]
pub enum LoginOutcome {
    Decided(LoginDecision),
    /// The proxy asking isn't registered (or expired); it should re-register.
    UnknownProxy,
}

pub struct Store {
    conn: ConnectionManager,
    keys: Keys,
    expiry_ms: u64,
    update_if_present: Script,
    reap: Script,
    leave: Script,
    switch: Script,
    push_command: Script,
    server_players: Script,
    drop_server_players: Script,
}

impl Store {
    pub async fn new(client: &redis::Client, config: &Config) -> Result<Self> {
        Ok(Self {
            conn: client.get_connection_manager().await?,
            keys: Keys::new(&config.key_prefix),
            expiry_ms: u64::try_from(config.expiry.as_millis()).unwrap_or(u64::MAX),
            update_if_present: Script::new(UPDATE_IF_PRESENT),
            reap: Script::new(REAP),
            leave: Script::new(LEAVE),
            switch: Script::new(SWITCH),
            push_command: Script::new(PUSH_COMMAND),
            server_players: Script::new(SERVER_PLAYERS),
            drop_server_players: Script::new(DROP_SERVER_PLAYERS),
        })
    }

    #[must_use]
    pub fn keys(&self) -> &Keys {
        &self.keys
    }

    fn conn(&self) -> ConnectionManager {
        self.conn.clone()
    }

    pub async fn ping(&self) -> Result<()> {
        let _: String = redis::cmd("PING").query_async(&mut self.conn()).await?;
        Ok(())
    }

    // Proxies

    /// Registers (or re-registers, after a restart) a proxy. A restarted proxy
    /// has no players, so any presence it left behind is dropped.
    pub async fn register_proxy(&self, req: RegisterProxy) -> Result<ProxyRegistered> {
        self.drop_proxy_players(&req.id).await?;
        let draining: bool = self.conn().exists(self.keys.proxy_drain(&req.id)).await?;
        let info = ProxyInfo {
            id: req.id.clone(),
            address: req.address,
            version: req.version,
            players: 0,
            draining,
            last_heartbeat_ms: now_ms(),
        };
        let (servers_revision, commands_revision): (Option<u64>, Option<u64>) = redis::pipe()
            .atomic()
            .hset(self.keys.proxies(), &req.id, serde_json::to_string(&info)?)
            .ignore()
            .pset_ex(self.keys.proxy_alive(&req.id), 1, self.expiry_ms)
            .ignore()
            .get(self.keys.servers_rev())
            .get(self.keys.commands_rev(&req.id))
            .query_async(&mut self.conn())
            .await?;
        Ok(ProxyRegistered {
            servers_revision: servers_revision.unwrap_or(0),
            commands_revision: commands_revision.unwrap_or(0),
            draining,
        })
    }

    /// Returns false if the proxy isn't registered.
    pub async fn proxy_heartbeat(&self, id: &str, beat: ProxyHeartbeat) -> Result<bool> {
        let Some(mut info) = self.proxy(id).await? else {
            return Ok(false);
        };
        info.players = beat.players;
        info.draining = beat.draining;
        info.last_heartbeat_ms = now_ms();
        let written: i64 = self
            .update_if_present
            .key(self.keys.proxies())
            .key(self.keys.proxy_alive(id))
            .key(self.keys.servers_rev())
            .arg(id)
            .arg(serde_json::to_string(&info)?)
            .arg(self.expiry_ms)
            .arg("0")
            .invoke_async(&mut self.conn())
            .await?;
        Ok(written == 1)
    }

    pub async fn proxy(&self, id: &str) -> Result<Option<ProxyInfo>> {
        let raw: Option<String> = self.conn().hget(self.keys.proxies(), id).await?;
        Ok(raw.map(|raw| serde_json::from_str(&raw)).transpose()?)
    }

    /// Returns false if the proxy wasn't registered.
    pub async fn remove_proxy(&self, id: &str) -> Result<bool> {
        let (removed,): (i64,) = redis::pipe()
            .atomic()
            .hdel(self.keys.proxies(), id)
            .del(self.keys.proxy_alive(id))
            .ignore()
            .del(self.keys.proxy_drain(id))
            .ignore()
            .query_async(&mut self.conn())
            .await?;
        self.forget_proxy(id).await?;
        Ok(removed == 1)
    }

    /// Records whether staff want a proxy drained and tells the proxy.
    /// Returns false if the proxy isn't registered.
    pub async fn set_draining(&self, id: &str, draining: bool) -> Result<bool> {
        let Some(mut info) = self.proxy(id).await? else {
            return Ok(false);
        };
        let key = self.keys.proxy_drain(id);
        if draining {
            let _: () = self.conn().set(key, 1).await?;
        } else {
            let _: () = self.conn().del(key).await?;
        }
        // Take effect for the load balancer now; the proxy's next heartbeat
        // confirms it.
        info.draining = draining;
        let _: () = self
            .conn()
            .hset(self.keys.proxies(), id, serde_json::to_string(&info)?)
            .await?;
        self.push_command(id, &ProxyCommand::Drain { draining })
            .await?;
        Ok(true)
    }

    /// Every registered proxy, sorted by id. `draining` is true if the proxy
    /// says so or staff asked for it.
    pub async fn proxies(&self) -> Result<Vec<ProxyInfo>> {
        let raw: Vec<String> = self.conn().hvals(self.keys.proxies()).await?;
        let mut proxies = Vec::with_capacity(raw.len());
        for raw in raw {
            let mut info: ProxyInfo = serde_json::from_str(&raw)?;
            let drain: bool = self.conn().exists(self.keys.proxy_drain(&info.id)).await?;
            info.draining |= drain;
            proxies.push(info);
        }
        proxies.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(proxies)
    }

    /// What the load balancer should do with a proxy.
    pub async fn agent_state(&self, id: &str) -> Result<AgentState> {
        let (alive, drain, raw): (bool, bool, Option<String>) = redis::pipe()
            .exists(self.keys.proxy_alive(id))
            .exists(self.keys.proxy_drain(id))
            .hget(self.keys.proxies(), id)
            .query_async(&mut self.conn())
            .await?;
        let Some(raw) = raw.filter(|_| alive) else {
            return Ok(AgentState::Down);
        };
        let info: ProxyInfo = serde_json::from_str(&raw)?;
        Ok(if drain || info.draining {
            AgentState::Drain
        } else {
            AgentState::Ready
        })
    }

    /// Clears what a removed proxy leaves behind: its players and queued commands.
    async fn forget_proxy(&self, id: &str) -> Result<()> {
        self.drop_proxy_players(id).await?;
        let _: () = self.conn().del(self.keys.commands(id)).await?;
        Ok(())
    }

    async fn drop_proxy_players(&self, id: &str) -> Result<()> {
        let players: Vec<String> = self.conn().smembers(self.keys.proxy_players(id)).await?;
        for uuid in players {
            self.leave_one(&uuid, id).await?;
            // The proxy's sessions are closed in PostgreSQL all at once.
            let _: () = self.conn().hdel(self.keys.sessions(), &uuid).await?;
        }
        let _: () = self.conn().del(self.keys.proxy_players(id)).await?;
        Ok(())
    }

    /// The proxy's command revision and the commands after `since`. If
    /// `since` is ahead of the revision (Redis was reset), no commands are
    /// returned and the caller adopts the lower revision.
    pub async fn read_commands(&self, id: &str, since: u64) -> Result<(u64, Vec<ProxyCommand>)> {
        let (revision, entries): (Option<u64>, Vec<String>) = redis::pipe()
            .atomic()
            .get(self.keys.commands_rev(id))
            .zrangebyscore(self.keys.commands(id), format!("({since}"), "+inf")
            .query_async(&mut self.conn())
            .await?;
        let revision = revision.unwrap_or(0);
        if since > revision {
            return Ok((revision, Vec::new()));
        }
        let commands = entries
            .iter()
            .map(|entry| {
                let json = entry
                    .split_once('|')
                    .map_or(entry.as_str(), |(_, json)| json);
                serde_json::from_str(json)
            })
            .collect::<std::result::Result<_, _>>()?;
        Ok((revision, commands))
    }

    /// Queues `command` for `proxy`.
    pub async fn send_to_proxy(&self, proxy: &str, command: &ProxyCommand) -> Result<u64> {
        self.push_command(proxy, command).await
    }

    async fn push_command(&self, proxy: &str, command: &ProxyCommand) -> Result<u64> {
        Ok(self
            .push_command
            .key(self.keys.commands(proxy))
            .key(self.keys.commands_rev(proxy))
            .arg(serde_json::to_string(command)?)
            .arg(COMMANDS_KEPT)
            .arg(self.keys.channel())
            .arg(events::commands(proxy))
            .invoke_async(&mut self.conn())
            .await?)
    }

    /// Routes a command to the proxy holding the player, or to every proxy
    /// for a broadcast. Returns false if the player isn't online.
    pub async fn send_command(&self, command: NetworkCommand) -> Result<bool> {
        let player = match &command {
            NetworkCommand::Send { player, .. }
            | NetworkCommand::Message { player, .. }
            | NetworkCommand::Kick { player, .. } => Some(*player),
            NetworkCommand::Broadcast { .. } => None,
        };
        let command = ProxyCommand::from(command);
        match player {
            Some(uuid) => {
                let Some(location) = self.player(&uuid).await? else {
                    return Ok(false);
                };
                self.push_command(&location.proxy, &command).await?;
            }
            None => {
                let proxies: Vec<String> = self.conn().hkeys(self.keys.proxies()).await?;
                for proxy in proxies {
                    self.push_command(&proxy, &command).await?;
                }
            }
        }
        Ok(true)
    }

    // Servers

    pub async fn register_server(
        &self,
        req: RegisterServer,
    ) -> Result<std::result::Result<(), RegisterError>> {
        if let Some(existing) = self.server(&req.name).await? {
            let alive: bool = self
                .conn()
                .exists(self.keys.server_alive(&req.name))
                .await?;
            if alive && existing.address != req.address {
                return Ok(Err(RegisterError::NameTaken));
            }
        }
        // A server registering again restarted: nobody is on it yet.
        self.drop_server_players(&req.name).await?;
        let info = ServerInfo {
            name: req.name.clone(),
            kind: req.kind,
            version: req.version,
            role: req.role,
            address: req.address,
            max_players: req.max_players,
            players: 0,
            state: ServerState::Ready,
            tick_ms: None,
        };
        let _: () = redis::pipe()
            .atomic()
            .hset(
                self.keys.servers(),
                &req.name,
                serde_json::to_string(&info)?,
            )
            .ignore()
            .pset_ex(self.keys.server_alive(&req.name), 1, self.expiry_ms)
            .ignore()
            .incr(self.keys.servers_rev(), 1)
            .ignore()
            .publish(self.keys.channel(), events::SERVERS)
            .ignore()
            .query_async(&mut self.conn())
            .await?;
        Ok(Ok(()))
    }

    /// Returns false if the server isn't registered. Player counts change on
    /// every heartbeat, so only a state change bumps the list revision. A
    /// roster replaces the server's player list.
    pub async fn server_heartbeat(&self, name: &str, beat: ServerHeartbeat) -> Result<bool> {
        let Some(mut info) = self.server(name).await? else {
            return Ok(false);
        };
        let state_changed = info.state != beat.state;
        info.players = beat.players;
        info.state = beat.state;
        if beat.tick_ms.is_some() {
            info.tick_ms = beat.tick_ms;
        }
        let written: i64 = self
            .update_if_present
            .key(self.keys.servers())
            .key(self.keys.server_alive(name))
            .key(self.keys.servers_rev())
            .arg(name)
            .arg(serde_json::to_string(&info)?)
            .arg(self.expiry_ms)
            .arg(if state_changed { "1" } else { "0" })
            .arg(self.keys.channel())
            .arg(events::SERVERS)
            .invoke_async(&mut self.conn())
            .await?;
        if written != 1 {
            return Ok(false);
        }
        if let Some(roster) = &beat.roster {
            self.apply_server_players(name, roster, &[], true).await?;
        }
        Ok(true)
    }

    /// Applies joins and leaves a server reported. Returns false if the
    /// server isn't registered.
    pub async fn server_players_changed(
        &self,
        name: &str,
        changed: &ServerPlayersChanged,
    ) -> Result<bool> {
        self.apply_server_players(name, &changed.joined, &changed.left, false)
            .await
    }

    async fn apply_server_players(
        &self,
        name: &str,
        joined: &[ServerPlayer],
        left: &[Uuid],
        replace: bool,
    ) -> Result<bool> {
        let joined: Vec<(String, &str)> = joined
            .iter()
            .map(|p| (p.uuid.to_string(), p.username.as_str()))
            .collect();
        let left: Vec<String> = left.iter().map(Uuid::to_string).collect();
        let applied: i64 = self
            .server_players
            .key(self.keys.servers())
            .key(self.keys.player_servers())
            .key(self.keys.server_players(name))
            .arg(name)
            .arg(self.keys.server_players_prefix())
            .arg(if replace { "1" } else { "0" })
            .arg(serde_json::to_string(&joined)?)
            .arg(serde_json::to_string(&left)?)
            .invoke_async(&mut self.conn())
            .await?;
        Ok(applied == 1)
    }

    /// Who a server reports being on it, by username ignoring case; `None`
    /// if the server isn't registered.
    pub async fn server_players(&self, name: &str) -> Result<Option<Vec<ServerPlayer>>> {
        let (registered, raw): (bool, Vec<(String, String)>) = redis::pipe()
            .atomic()
            .hexists(self.keys.servers(), name)
            .hgetall(self.keys.server_players(name))
            .query_async(&mut self.conn())
            .await?;
        if !registered {
            return Ok(None);
        }
        let mut players: Vec<ServerPlayer> = raw
            .into_iter()
            .filter_map(|(uuid, username)| {
                Some(ServerPlayer {
                    uuid: Uuid::parse_str(&uuid).ok()?,
                    username,
                })
            })
            .collect();
        players.sort_by_cached_key(|p| (p.username.to_ascii_lowercase(), p.uuid));
        Ok(Some(players))
    }

    /// The server that last reported `uuid` joining it, if they haven't left.
    pub async fn server_of(&self, uuid: &Uuid) -> Result<Option<String>> {
        Ok(self
            .conn()
            .hget(self.keys.player_servers(), uuid.to_string())
            .await?)
    }

    async fn drop_server_players(&self, name: &str) -> Result<()> {
        let _: i64 = self
            .drop_server_players
            .key(self.keys.player_servers())
            .key(self.keys.server_players(name))
            .arg(name)
            .invoke_async(&mut self.conn())
            .await?;
        Ok(())
    }

    pub async fn server(&self, name: &str) -> Result<Option<ServerInfo>> {
        let raw: Option<String> = self.conn().hget(self.keys.servers(), name).await?;
        Ok(raw.map(|raw| serde_json::from_str(&raw)).transpose()?)
    }

    /// Returns false if the server wasn't registered.
    pub async fn remove_server(&self, name: &str) -> Result<bool> {
        let removed: i64 = self.conn().hdel(self.keys.servers(), name).await?;
        let _: () = self.conn().del(self.keys.server_alive(name)).await?;
        self.drop_server_players(name).await?;
        if removed == 1 {
            self.servers_changed().await?;
        }
        Ok(removed == 1)
    }

    async fn servers_changed(&self) -> Result<()> {
        let _: () = redis::pipe()
            .atomic()
            .incr(self.keys.servers_rev(), 1)
            .ignore()
            .publish(self.keys.channel(), events::SERVERS)
            .ignore()
            .query_async(&mut self.conn())
            .await?;
        Ok(())
    }

    /// The server list revision and every server, sorted by name.
    pub async fn read_servers(&self) -> Result<(u64, Vec<ServerInfo>)> {
        let (revision, raw): (Option<u64>, Vec<String>) = redis::pipe()
            .atomic()
            .get(self.keys.servers_rev())
            .hvals(self.keys.servers())
            .query_async(&mut self.conn())
            .await?;
        let mut servers = raw
            .iter()
            .map(|raw| serde_json::from_str::<ServerInfo>(raw))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        servers.sort_by(|a, b| a.name.cmp(&b.name));
        Ok((revision.unwrap_or(0), servers))
    }

    // Presence

    /// Decides whether a player may join, and where they land.
    pub async fn login(&self, req: LoginRequest, config: &Config) -> Result<LoginOutcome> {
        let proxy_alive: bool = self
            .conn()
            .exists(self.keys.proxy_alive(&req.proxy))
            .await?;
        if !proxy_alive {
            return Ok(LoginOutcome::UnknownProxy);
        }
        // Before anything that changes state, such as kicking an old session.
        let Some(hub) = self.pick_hub(&config.hub_kind).await? else {
            return Ok(deny("No lobby is free right now. Try again in a moment."));
        };

        let uuid = req.uuid.to_string();
        let existing = self.player(&req.uuid).await?;
        let mut replaces = None;
        if let Some(old) = existing {
            let old_alive: bool = self
                .conn()
                .exists(self.keys.proxy_alive(&old.proxy))
                .await?;
            if old_alive && old.proxy != req.proxy {
                match config.duplicate_login {
                    DuplicateLogin::Deny => {
                        return Ok(deny("You are already online on this network."));
                    }
                    DuplicateLogin::KickOld => {
                        self.push_command(
                            &old.proxy,
                            &ProxyCommand::Kick {
                                player: req.uuid,
                                reason: "You logged in from another location.".into(),
                            },
                        )
                        .await?;
                    }
                }
            }
            // A stale entry (its proxy is gone or this is the same proxy) or a
            // kicked session: the new login takes its place.
            replaces = Some(old.proxy);
        } else {
            let online: u32 = self.conn().hlen(self.keys.online()).await?;
            if online >= config.max_players {
                return Ok(deny("The network is full."));
            }
        }

        let name = req.username.to_ascii_lowercase();
        let location = serde_json::to_string(&PlayerLocation {
            uuid: req.uuid,
            username: req.username,
            proxy: req.proxy.clone(),
            server: None,
        })?;
        match replaces {
            None => {
                // Two proxies can race the same new login; only one wins.
                let claimed: bool = self
                    .conn()
                    .hset_nx(self.keys.online(), &uuid, &location)
                    .await?;
                if !claimed {
                    return Ok(deny("You are already online on this network."));
                }
            }
            Some(old_proxy) => {
                let _: () = redis::pipe()
                    .atomic()
                    .hset(self.keys.online(), &uuid, &location)
                    .ignore()
                    .srem(self.keys.proxy_players(&old_proxy), &uuid)
                    .ignore()
                    .query_async(&mut self.conn())
                    .await?;
            }
        }
        let _: () = redis::pipe()
            .atomic()
            .sadd(self.keys.proxy_players(&req.proxy), &uuid)
            .ignore()
            .hset(self.keys.names(), &name, &uuid)
            .ignore()
            .query_async(&mut self.conn())
            .await?;
        Ok(LoginOutcome::Decided(LoginDecision::Allow {
            server: hub,
            permissions: Vec::new(),
            mute: None,
        }))
    }

    /// The emptiest hub that is ready, has room and isn't ending.
    async fn pick_hub(&self, kind: &str) -> Result<Option<String>> {
        Ok(self.pick_lobby(kind).await?.map(|s| s.name))
    }

    /// Returns false if the player isn't online.
    pub async fn switch(&self, moved: PlayerSwitched) -> Result<bool> {
        let updated: i64 = self
            .switch
            .key(self.keys.online())
            .arg(moved.uuid.to_string())
            .arg(moved.server)
            .invoke_async(&mut self.conn())
            .await?;
        Ok(updated == 1)
    }

    /// Ignored (returns false) unless the player is online on that proxy.
    pub async fn leave(&self, left: PlayerLeft) -> Result<bool> {
        self.leave_one(&left.uuid.to_string(), &left.proxy).await
    }

    /// Removes a player who was on `proxy`, and takes them out of their party.
    async fn leave_one(&self, uuid: &str, proxy: &str) -> Result<bool> {
        let username = match Uuid::parse_str(uuid) {
            Ok(id) => self.player(&id).await?.map(|p| p.username),
            Err(_) => None,
        };
        let removed: i64 = self
            .leave
            .key(self.keys.online())
            .key(self.keys.proxy_players(proxy))
            .key(self.keys.names())
            .arg(uuid)
            .arg(proxy)
            .invoke_async(&mut self.conn())
            .await?;
        if removed == 1 {
            let username = username.as_deref().unwrap_or("Someone");
            // Queue first: it reads the party to know who queued together.
            self.queue_on_leave(uuid, username).await?;
            self.party_on_leave(uuid, username).await?;
        }
        Ok(removed == 1)
    }

    /// Remembers a player's open PostgreSQL session.
    pub async fn set_session(&self, uuid: &Uuid, session: &Uuid) -> Result<()> {
        let _: () = self
            .conn()
            .hset(self.keys.sessions(), uuid.to_string(), session.to_string())
            .await?;
        Ok(())
    }

    pub async fn session(&self, uuid: &Uuid) -> Result<Option<Uuid>> {
        let id: Option<String> = self
            .conn()
            .hget(self.keys.sessions(), uuid.to_string())
            .await?;
        Ok(id.and_then(|id| Uuid::parse_str(&id).ok()))
    }

    /// Forgets and returns a player's session, at logout.
    pub async fn take_session(&self, uuid: &Uuid) -> Result<Option<Uuid>> {
        let session = self.session(uuid).await?;
        let _: () = self
            .conn()
            .hdel(self.keys.sessions(), uuid.to_string())
            .await?;
        Ok(session)
    }

    pub async fn online(&self) -> Result<u32> {
        Ok(self.conn().hlen(self.keys.online()).await?)
    }

    /// Looks an online player up by username, ignoring ASCII case.
    pub async fn player_by_name(&self, name: &str) -> Result<Option<PlayerLocation>> {
        let uuid: Option<String> = self
            .conn()
            .hget(self.keys.names(), name.to_ascii_lowercase())
            .await?;
        match uuid.and_then(|uuid| Uuid::parse_str(&uuid).ok()) {
            Some(uuid) => self.player(&uuid).await,
            None => Ok(None),
        }
    }

    pub async fn player(&self, uuid: &Uuid) -> Result<Option<PlayerLocation>> {
        let raw: Option<String> = self
            .conn()
            .hget(self.keys.online(), uuid.to_string())
            .await?;
        Ok(raw.map(|raw| serde_json::from_str(&raw)).transpose()?)
    }

    // Expiry

    /// Removes servers and proxies whose alive keys have expired, with the
    /// players and commands of removed proxies.
    pub async fn reap(&self) -> Result<Reaped> {
        let mut reaped = Reaped::default();
        let servers: Vec<String> = self.conn().hkeys(self.keys.servers()).await?;
        for name in servers {
            let removed: i64 = self
                .reap
                .key(self.keys.servers())
                .key(self.keys.server_alive(&name))
                .arg(&name)
                .invoke_async(&mut self.conn())
                .await?;
            if removed == 1 {
                self.drop_server_players(&name).await?;
                reaped.servers.push(name);
            }
        }
        if !reaped.servers.is_empty() {
            self.servers_changed().await?;
        }
        let proxies: Vec<String> = self.conn().hkeys(self.keys.proxies()).await?;
        for id in proxies {
            let removed: i64 = self
                .reap
                .key(self.keys.proxies())
                .key(self.keys.proxy_alive(&id))
                .arg(&id)
                .invoke_async(&mut self.conn())
                .await?;
            if removed == 1 {
                self.forget_proxy(&id).await?;
                reaped.proxies.push(id);
            }
        }
        Ok(reaped)
    }
}

fn deny(reason: &str) -> LoginOutcome {
    LoginOutcome::Decided(LoginDecision::Deny {
        reason: reason.into(),
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
