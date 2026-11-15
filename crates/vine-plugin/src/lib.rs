//! Hallow's Vine plugin: links a proxy to the control plane so several
//! proxies run as one network ([Multi-proxy]).
//!
//! - **Presence.** Every login asks the control plane first. It refuses a
//!   second login for an account that's online on another proxy, refuses
//!   logins when the network is full, and picks the hub the player lands on.
//!   Switches and leaves are reported back.
//! - **Global ping.** Server-list pings show the network-wide count.
//! - **One server list.** Servers registered with the control plane are
//!   mirrored into Vine's runtime registry, so every proxy can route to them.
//! - **Commands.** Sends, messages, broadcasts and kicks queued for this
//!   proxy are carried out.
//!
//! Vine plugin calls are synchronous, so nothing here waits long: a repeating
//! task polls the control plane every `poll_ms` with `?wait=0`, and each HTTP
//! call has a 2 s timeout. Grant the plugin `http.outbound` and
//! `fs.read.data`; its settings are in [`config`].
//!
//! [Multi-proxy]: https://github.com/xena-studios/hallow/blob/main/docs/MULTI-PROXY.md

// The bindings generated from Vine's WIT include calls with many arguments.
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: "../../upstream/Vine/wit",
    world: "plugin",
});

mod config;
mod http;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use hallow_proto::{
    Poll,
    command::ProxyCommand,
    presence::{LoginDecision, LoginRequest, NetworkCount, PlayerLeft, PlayerSwitched},
    proxy::{ProxyHeartbeat, ProxyRegistered, RegisterProxy},
    routes,
    server::{ServerInfo, ServerState},
};
use uuid::Uuid;
use vine::plugin::{
    event::{
        EventType, PlayerDisconnectEvent, PlayerLoginEvent, PlayerPreLoginEvent, ProxyPingEvent,
        ServerConnectEvent, ServerConnectedEvent,
    },
    logging::{self, Level},
    proxy, scheduler,
    types::{EventPriority, ForwardingMode},
};

use config::{Config, Forwarding};

/// Handler and task ids.
const PING: u32 = 1;
const PRE_LOGIN: u32 = 2;
const LOGIN: u32 = 3;
const CONNECT: u32 = 4;
const CONNECTED: u32 = 5;
const DISCONNECT: u32 = 6;
const TICK: u32 = 100;

const HEARTBEAT: Duration = Duration::from_secs(routes::HEARTBEAT_SECONDS);
/// How often the network count shown in pings is refreshed.
const COUNT_EVERY: Duration = Duration::from_secs(1);
/// A player we admitted who still isn't a live session after this long (the
/// backend was unreachable, a plugin cancelled the connect, the leave failed)
/// is reported as gone. Vine doesn't fire a disconnect event in those cases.
const SWEEP_AFTER: Duration = Duration::from_secs(10);
/// Pre-login protocol versions are kept this long for the login that follows.
const PROTOCOL_TTL: Duration = Duration::from_secs(30);

const UNAVAILABLE: &str = "§cThe network is unavailable right now. Try again in a moment.";

struct State {
    config: Config,
    client: http::Client,
    registered: bool,
    servers_revision: u64,
    commands_revision: u64,
    /// Servers we put into Vine's registry, by name, with their address.
    mirrored: BTreeMap<String, String>,
    count: Option<NetworkCount>,
    last_heartbeat: Option<Instant>,
    last_count: Option<Instant>,
    /// The hub the control plane picked for each admitted player, until their
    /// first connect.
    hubs: HashMap<String, String>,
    /// Players the control plane counts on this proxy, with when they were
    /// admitted.
    admitted: HashMap<String, Instant>,
    /// Protocol versions from pre-login, by username.
    protocols: HashMap<String, (i32, Instant)>,
    /// Log a lost connection once, not every poll.
    reported_down: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|state| state.borrow_mut().as_mut().map(f))
}

fn log(level: Level, message: &str) {
    logging::log(level, &format!("hallow: {message}"));
}

struct Hallow;

impl Guest for Hallow {
    fn init_plugin() {}

    fn on_load(ctx: Context) -> Result<(), String> {
        let config = Config::load()?;
        let client = http::Client {
            authority: config.control_plane.clone(),
            token: config.token.clone(),
        };
        let poll_ms = config.poll_ms;
        log(
            Level::Info,
            &format!(
                "proxy {} using the control plane at {}",
                config.proxy_id, config.control_plane
            ),
        );
        STATE.with(|state| {
            *state.borrow_mut() = Some(State {
                config,
                client,
                registered: false,
                servers_revision: 0,
                commands_revision: 0,
                mirrored: BTreeMap::new(),
                count: None,
                last_heartbeat: None,
                last_count: None,
                hubs: HashMap::new(),
                admitted: HashMap::new(),
                protocols: HashMap::new(),
                reported_down: false,
            });
        });

        // Monitor priority sees what other plugins decided; logins are
        // checked late so a ban plugin can refuse first.
        ctx.register_event(PING, EventType::ProxyPing, EventPriority::Normal);
        ctx.register_event(PRE_LOGIN, EventType::PlayerPreLogin, EventPriority::Monitor);
        ctx.register_event(LOGIN, EventType::PlayerLogin, EventPriority::High);
        ctx.register_event(CONNECT, EventType::ServerConnect, EventPriority::Low);
        ctx.register_event(
            CONNECTED,
            EventType::ServerConnected,
            EventPriority::Monitor,
        );
        ctx.register_event(
            DISCONNECT,
            EventType::PlayerDisconnect,
            EventPriority::Monitor,
        );

        with_state(State::tick);
        scheduler::schedule_repeating_task(TICK, poll_ms, poll_ms);
        Ok(())
    }

    fn on_unload(_ctx: Context) -> Result<(), String> {
        with_state(|state| {
            if state.registered
                && let Err(err) = state.client.delete(&routes::proxy(&state.config.proxy_id))
            {
                log(Level::Warn, &format!("couldn't unregister: {err}"));
            }
        });
        Ok(())
    }

    fn get_metadata() -> PluginMetadata {
        PluginMetadata {
            name: "hallow".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Xena Studios".into()],
            description: Some(env!("CARGO_PKG_DESCRIPTION").into()),
            dependencies: vec![],
        }
    }

    fn handle_event(_handler_id: u32, evt: Event) -> Event {
        with_state(|state| match evt {
            Event::ProxyPing(ping) => Event::ProxyPing(state.on_ping(ping)),
            Event::PlayerPreLogin(pre) => Event::PlayerPreLogin(state.on_pre_login(pre)),
            Event::PlayerLogin(login) => Event::PlayerLogin(state.on_login(login)),
            Event::ServerConnect(connect) => Event::ServerConnect(state.on_connect(connect)),
            Event::ServerConnected(connected) => {
                state.on_connected(&connected);
                Event::ServerConnected(connected)
            }
            Event::PlayerDisconnect(gone) => {
                state.on_disconnect(&gone);
                Event::PlayerDisconnect(gone)
            }
            other => other,
        })
        .unwrap_or_else(unreachable_without_state)
    }

    fn handle_command(_: u32, _: CommandSender, _: Vec<String>) -> Result<i32, String> {
        Err("hallow has no commands".into())
    }

    fn handle_command_suggestion(_: u32, _: CommandSender, _: Vec<String>) -> Vec<String> {
        Vec::new()
    }

    fn handle_task(handler_id: u32) {
        if handler_id == TICK {
            with_state(State::tick);
        }
    }
}

/// Events only arrive after `on_load` set the state up.
fn unreachable_without_state() -> Event {
    panic!("hallow: event before on_load")
}

impl State {
    // Events

    fn on_ping(&mut self, mut ping: ProxyPingEvent) -> ProxyPingEvent {
        if let Some(count) = self.count {
            ping.online_players = count.online;
            ping.max_players = count.max;
        }
        ping
    }

    fn on_pre_login(&mut self, pre: PlayerPreLoginEvent) -> PlayerPreLoginEvent {
        if !pre.cancelled {
            let protocol = i32::try_from(pre.protocol_version).unwrap_or(i32::MAX);
            self.protocols
                .insert(pre.username.clone(), (protocol, Instant::now()));
        }
        pre
    }

    fn on_login(&mut self, mut login: PlayerLoginEvent) -> PlayerLoginEvent {
        if login.cancelled {
            return login;
        }
        let refuse = |login: &mut PlayerLoginEvent, reason: &str| {
            login.cancelled = true;
            login.cancel_reason = Some(reason.to_owned());
        };
        let Ok(uuid) = Uuid::parse_str(&login.uuid) else {
            refuse(&mut login, UNAVAILABLE);
            return login;
        };
        if !self.registered {
            // Not linked to the network yet; a login now could be a duplicate.
            refuse(&mut login, UNAVAILABLE);
            return login;
        }
        let request = LoginRequest {
            uuid,
            username: login.username.clone(),
            proxy: self.config.proxy_id.clone(),
            protocol: self.protocols.remove(&login.username).map(|(p, _)| p),
        };
        match self.client.post(routes::PRESENCE_LOGIN, &request) {
            Ok(LoginDecision::Allow { server }) => {
                self.hubs.insert(login.uuid.clone(), server);
                self.admitted.insert(login.uuid.clone(), Instant::now());
            }
            Ok(LoginDecision::Deny { reason }) => refuse(&mut login, &reason),
            Err(err) => {
                if err.is_not_found() {
                    self.registered = false;
                }
                log(
                    Level::Warn,
                    &format!("login check for {} failed: {err}", login.username),
                );
                refuse(&mut login, UNAVAILABLE);
            }
        }
        login
    }

    /// Sends a player's first connect to the hub the control plane picked.
    fn on_connect(&mut self, mut connect: ServerConnectEvent) -> ServerConnectEvent {
        if connect.current_server.is_none()
            && let Some(hub) = self.hubs.remove(&connect.uuid)
            && self.mirrored.contains_key(&hub)
        {
            connect.target_server = hub;
        }
        connect
    }

    fn on_connected(&mut self, connected: &ServerConnectedEvent) {
        let Ok(uuid) = Uuid::parse_str(&connected.uuid) else {
            return;
        };
        let switched = PlayerSwitched {
            uuid,
            server: connected.server_name.clone(),
        };
        if let Err(err) = self.client.post_empty(routes::PRESENCE_SWITCH, &switched) {
            log(Level::Debug, &format!("switch report failed: {err}"));
        }
    }

    fn on_disconnect(&mut self, gone: &PlayerDisconnectEvent) {
        self.hubs.remove(&gone.uuid);
        if self.leave(&gone.uuid) {
            self.admitted.remove(&gone.uuid);
        }
        // Otherwise the sweep retries once the control plane is back.
    }

    /// Reports a player gone. Returns false if the control plane couldn't be
    /// told.
    fn leave(&self, uuid: &str) -> bool {
        let Ok(uuid) = Uuid::parse_str(uuid) else {
            return true;
        };
        let left = PlayerLeft {
            uuid,
            proxy: self.config.proxy_id.clone(),
        };
        self.client
            .post_empty(routes::PRESENCE_LEAVE, &left)
            .is_ok()
    }

    // The repeating task

    fn tick(&mut self) {
        if !self.registered && !self.register() {
            return;
        }
        let now = Instant::now();
        if self.last_heartbeat.is_none_or(|at| now - at >= HEARTBEAT) {
            self.heartbeat();
            self.sweep();
        }
        if !self.registered {
            return;
        }
        self.poll_servers();
        self.poll_commands();
        if self.last_count.is_none_or(|at| now - at >= COUNT_EVERY) {
            self.refresh_count();
        }
        self.protocols.retain(|_, (_, at)| now - *at < PROTOCOL_TTL);
    }

    /// Registers this proxy and reports the players already on it (after the
    /// control plane lost its state, or this proxy expired).
    fn register(&mut self) -> bool {
        let request = RegisterProxy {
            id: self.config.proxy_id.clone(),
            address: self.config.address.clone(),
            version: format!("hallow-vine {}", env!("CARGO_PKG_VERSION")),
        };
        let registered: ProxyRegistered = match self.client.post(routes::PROXIES, &request) {
            Ok(registered) => registered,
            Err(err) => {
                if !self.reported_down {
                    log(
                        Level::Warn,
                        &format!("can't register with the control plane: {err}"),
                    );
                    self.reported_down = true;
                }
                return false;
            }
        };
        self.reported_down = false;
        self.registered = true;
        self.commands_revision = registered.commands_revision;
        // Take the whole list on the next poll.
        self.servers_revision = 0;
        self.last_heartbeat = Some(Instant::now());
        log(Level::Info, "registered with the control plane");

        self.admitted.clear();
        for player in proxy::get_players() {
            let Ok(uuid) = Uuid::parse_str(&player.uuid) else {
                continue;
            };
            let request = LoginRequest {
                uuid,
                username: player.username.clone(),
                proxy: self.config.proxy_id.clone(),
                protocol: i32::try_from(player.protocol_version).ok(),
            };
            match self
                .client
                .post::<_, LoginDecision>(routes::PRESENCE_LOGIN, &request)
            {
                Ok(LoginDecision::Allow { .. }) => {
                    self.admitted.insert(player.uuid.clone(), Instant::now());
                    let switched = PlayerSwitched {
                        uuid,
                        server: player.current_server.clone(),
                    };
                    let _ = self.client.post_empty(routes::PRESENCE_SWITCH, &switched);
                }
                // Online elsewhere since: the newer session wins.
                Ok(LoginDecision::Deny { .. }) => {
                    proxy::disconnect_player(
                        &player.uuid,
                        "§cYou logged in from another location.",
                    );
                }
                Err(err) => log(
                    Level::Warn,
                    &format!("re-reporting {} failed: {err}", player.username),
                ),
            }
        }
        true
    }

    fn heartbeat(&mut self) {
        self.last_heartbeat = Some(Instant::now());
        let beat = ProxyHeartbeat {
            players: proxy::get_player_count(),
            draining: proxy::is_draining(),
        };
        let path = routes::proxy_heartbeat(&self.config.proxy_id);
        match self.client.post_empty(&path, &beat) {
            Ok(()) => self.reported_down = false,
            Err(err) => self.lost(&err),
        }
    }

    /// Reports admitted players who never became, or are no longer, live
    /// sessions.
    fn sweep(&mut self) {
        let live: Vec<String> = proxy::get_players().into_iter().map(|p| p.uuid).collect();
        let now = Instant::now();
        let stale: Vec<String> = self
            .admitted
            .iter()
            .filter(|(uuid, at)| now - **at >= SWEEP_AFTER && !live.contains(uuid))
            .map(|(uuid, _)| uuid.clone())
            .collect();
        for uuid in stale {
            if self.leave(&uuid) {
                self.admitted.remove(&uuid);
                self.hubs.remove(&uuid);
            }
        }
    }

    fn poll_servers(&mut self) {
        let path = routes::since_wait(routes::SERVERS, self.servers_revision, 0);
        match self.client.get::<Poll<Vec<ServerInfo>>>(&path) {
            Ok(Poll::Changed { revision, data }) => {
                self.servers_revision = revision;
                self.mirror(data);
            }
            Ok(Poll::Unchanged { .. }) => {}
            Err(err) => self.lost(&err),
        }
    }

    /// Makes Vine's registry match the control plane's list. Draining servers
    /// are left out so nobody new is sent there; players already on them stay.
    fn mirror(&mut self, servers: Vec<ServerInfo>) {
        let wanted: BTreeMap<String, String> = servers
            .into_iter()
            .filter(|s| s.state != ServerState::Draining)
            .map(|s| (s.name, s.address))
            .collect();
        let gone: Vec<String> = self
            .mirrored
            .keys()
            .filter(|name| !wanted.contains_key(*name))
            .cloned()
            .collect();
        for name in gone {
            proxy::unregister_server(&name);
            self.mirrored.remove(&name);
        }
        let forwarding = match self.config.forwarding {
            Forwarding::None => ForwardingMode::None,
            Forwarding::Bungeecord => ForwardingMode::Bungeecord,
            Forwarding::Velocity => ForwardingMode::Velocity,
            Forwarding::Vine => ForwardingMode::Vine,
        };
        for (name, address) in wanted {
            if self.mirrored.get(&name) == Some(&address) {
                continue;
            }
            if proxy::register_server(
                &name,
                &address,
                forwarding,
                self.config.forwarding_secret.as_deref(),
            ) {
                self.mirrored.insert(name, address);
            } else {
                log(
                    Level::Warn,
                    &format!("Vine refused server {name} at {address}"),
                );
            }
        }
    }

    fn poll_commands(&mut self) {
        let path = routes::since_wait(
            &routes::proxy_commands(&self.config.proxy_id),
            self.commands_revision,
            0,
        );
        match self.client.get::<Poll<Vec<ProxyCommand>>>(&path) {
            Ok(Poll::Changed { revision, data }) => {
                self.commands_revision = revision;
                for command in data {
                    run(command);
                }
            }
            Ok(Poll::Unchanged { .. }) => {}
            Err(err) => self.lost(&err),
        }
    }

    fn refresh_count(&mut self) {
        self.last_count = Some(Instant::now());
        match self.client.get::<NetworkCount>(routes::PRESENCE_COUNT) {
            Ok(count) => self.count = Some(count),
            Err(err) => self.lost(&err),
        }
    }

    /// A failed call: a 404 means the control plane forgot this proxy, so
    /// register again on the next tick.
    fn lost(&mut self, err: &http::Error) {
        if err.is_not_found() {
            if self.registered {
                log(
                    Level::Warn,
                    "the control plane forgot this proxy; registering again",
                );
            }
            self.registered = false;
        } else if !self.reported_down {
            log(Level::Warn, &format!("{err}"));
            self.reported_down = true;
        }
    }
}

fn run(command: ProxyCommand) {
    match command {
        ProxyCommand::Send { player, server } => {
            proxy::connect_player(&player.to_string(), &server);
        }
        ProxyCommand::Message { player, text } => {
            proxy::send_message(&player.to_string(), &text);
        }
        ProxyCommand::Broadcast {
            text,
            servers: None,
        } => proxy::broadcast(&text),
        ProxyCommand::Broadcast {
            text,
            servers: Some(servers),
        } => {
            for player in proxy::get_players() {
                if servers.contains(&player.current_server) {
                    proxy::send_message(&player.uuid, &text);
                }
            }
        }
        ProxyCommand::Kick { player, reason } => {
            proxy::disconnect_player(&player.to_string(), &reason);
        }
    }
}

export!(Hallow);
