//! Live state in Redis.
//!
//! | Key (after the prefix) | Type | Holds |
//! |---|---|---|
//! | `servers` | hash | server name → [`ServerInfo`] JSON |
//! | `servers:rev` | integer | server list revision, bumped when a server is added, removed or changes state |
//! | `server-alive:{name}` | string with TTL | present while the server heartbeats |
//! | `proxies` | hash | proxy id → [`ProxyInfo`] JSON |
//! | `proxy-alive:{id}` | string with TTL | present while the proxy heartbeats |
//! | `online` | hash | player UUID → [`PlayerLocation`] JSON |
//! | `proxy-players:{id}` | set | UUIDs on that proxy, to clean up when it goes |
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
    server::{RegisterServer, ServerHeartbeat, ServerInfo, ServerRole, ServerState},
};
use redis::{AsyncCommands, Script, aio::ConnectionManager};
use uuid::Uuid;

use crate::{Config, DuplicateLogin, events};

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
    fn proxies(&self) -> String {
        format!("{}:proxies", self.prefix)
    }
    fn proxy_alive(&self, id: &str) -> String {
        format!("{}:proxy-alive:{id}", self.prefix)
    }
    fn online(&self) -> String {
        format!("{}:online", self.prefix)
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

/// Removes player `ARGV[1]` from `online` (`KEYS[1]`) and from proxy set
/// `KEYS[2]`, only if they are on proxy `ARGV[2]`. A late leave from an old
/// session must not remove a newer login elsewhere. Returns 1 if removed.
const LEAVE: &str = r"
local v = redis.call('HGET', KEYS[1], ARGV[1])
if not v then return 0 end
if cjson.decode(v).proxy ~= ARGV[2] then return 0 end
redis.call('HDEL', KEYS[1], ARGV[1])
redis.call('SREM', KEYS[2], ARGV[1])
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
        let info = ProxyInfo {
            id: req.id.clone(),
            address: req.address,
            version: req.version,
            players: 0,
            draining: false,
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
            .query_async(&mut self.conn())
            .await?;
        self.forget_proxy(id).await?;
        Ok(removed == 1)
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
            let _: i64 = self
                .leave
                .key(self.keys.online())
                .key(self.keys.proxy_players(id))
                .arg(&uuid)
                .arg(id)
                .invoke_async(&mut self.conn())
                .await?;
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
        let info = ServerInfo {
            name: req.name.clone(),
            kind: req.kind,
            version: req.version,
            role: req.role,
            address: req.address,
            max_players: req.max_players,
            players: 0,
            state: ServerState::Ready,
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
    /// every heartbeat, so only a state change bumps the list revision.
    pub async fn server_heartbeat(&self, name: &str, beat: ServerHeartbeat) -> Result<bool> {
        let Some(mut info) = self.server(name).await? else {
            return Ok(false);
        };
        let state_changed = info.state != beat.state;
        info.players = beat.players;
        info.state = beat.state;
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
        Ok(written == 1)
    }

    pub async fn server(&self, name: &str) -> Result<Option<ServerInfo>> {
        let raw: Option<String> = self.conn().hget(self.keys.servers(), name).await?;
        Ok(raw.map(|raw| serde_json::from_str(&raw)).transpose()?)
    }

    /// Returns false if the server wasn't registered.
    pub async fn remove_server(&self, name: &str) -> Result<bool> {
        let removed: i64 = self.conn().hdel(self.keys.servers(), name).await?;
        let _: () = self.conn().del(self.keys.server_alive(name)).await?;
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
        let _: () = self
            .conn()
            .sadd(self.keys.proxy_players(&req.proxy), &uuid)
            .await?;
        Ok(LoginOutcome::Decided(LoginDecision::Allow { server: hub }))
    }

    /// The emptiest hub that is ready and has room.
    async fn pick_hub(&self, kind: &str) -> Result<Option<String>> {
        let (_, servers) = self.read_servers().await?;
        Ok(servers
            .into_iter()
            .filter(|s| {
                s.role == ServerRole::Lobby
                    && s.kind == kind
                    && s.state == ServerState::Ready
                    && s.players < s.max_players
            })
            .min_by_key(|s| s.players)
            .map(|s| s.name))
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
        let removed: i64 = self
            .leave
            .key(self.keys.online())
            .key(self.keys.proxy_players(&left.proxy))
            .arg(left.uuid.to_string())
            .arg(&left.proxy)
            .invoke_async(&mut self.conn())
            .await?;
        Ok(removed == 1)
    }

    pub async fn online(&self) -> Result<u32> {
        Ok(self.conn().hlen(self.keys.online()).await?)
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
