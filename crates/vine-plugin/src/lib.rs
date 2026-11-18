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
//! - **Signals.** Per-player anticheat measurements, one batch per window
//!   ([`signals`]).
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

mod commands;
mod config;
mod http;
mod signals;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use hallow_proto::{
    Poll,
    command::ProxyCommand,
    presence::{LoginDecision, LoginRequest, NetworkCount, PlayerLeft, PlayerSwitched},
    proxy::{ProxyHeartbeat, ProxyRegistered, RegisterProxy},
    punishment::MuteInfo,
    routes,
    server::{ServerInfo, ServerState},
    signal::{self, SignalBatch},
    violation::ViolationBatch,
};
use uuid::Uuid;
use vine::plugin::{
    event::{
        EventType, PlayerChatEvent, PlayerCommandEvent, PlayerDisconnectEvent, PlayerLoginEvent,
        PlayerPreLoginEvent, PluginMessageEvent, ProxyPingEvent, ServerConnectEvent,
        ServerConnectedEvent,
    },
    logging::{self, Level},
    packet_timing, proxy, scheduler,
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
const CHAT: u32 = 7;
const COMMAND: u32 = 8;
const PLUGIN_MESSAGE: u32 = 9;
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
/// How long a signal window runs.
const SIGNAL_WINDOW_MS: u64 = signal::WINDOW_SECONDS * 1000;
/// Samples per `POST /v1/signals`; the control plane takes up to 5000.
const SIGNAL_BATCH: usize = 1000;

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
    /// Who last messaged each player here, by UUID, for `/r`.
    last_from: HashMap<String, String>,
    /// Each player's permission nodes from their ranks, from login.
    permissions: HashMap<String, Vec<String>>,
    /// Muted players here, by UUID.
    mutes: HashMap<String, MuteInfo>,
    /// Anticheat signals for the current window.
    signals: signals::Recorder,
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
                last_from: HashMap::new(),
                permissions: HashMap::new(),
                mutes: HashMap::new(),
                signals: signals::Recorder::new(now_ms()),
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

        ctx.register_event(CHAT, EventType::PlayerChat, EventPriority::High);
        // Signals only watch, so they see the final events.
        ctx.register_event(COMMAND, EventType::PlayerCommand, EventPriority::Monitor);
        ctx.register_event(
            PLUGIN_MESSAGE,
            EventType::PluginMessage,
            EventPriority::Monitor,
        );
        commands::register(&ctx);

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
            Event::PlayerChat(chat) => Event::PlayerChat(state.on_chat(chat)),
            Event::PlayerCommand(command) => {
                state.on_command(&command);
                Event::PlayerCommand(command)
            }
            Event::PluginMessage(message) => {
                state.on_plugin_message(&message);
                Event::PluginMessage(message)
            }
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

    fn handle_command(id: u32, sender: CommandSender, args: Vec<String>) -> Result<i32, String> {
        // Messages for the player go out as they are; Vine would prefix an
        // `Err` with "Error:".
        let result = with_state(|state| state.command(id, &sender, &args))
            .unwrap_or_else(|| Err(UNAVAILABLE.into()));
        Ok(result.unwrap_or_else(|message| {
            vine::plugin::command::reply(&sender, &message);
            0
        }))
    }

    fn handle_command_suggestion(
        id: u32,
        _sender: CommandSender,
        args: Vec<String>,
    ) -> Vec<String> {
        with_state(|state| state.suggest(id, &args)).unwrap_or_default()
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
            ip: address(&login.client_ip),
        };
        match self.client.post(routes::PRESENCE_LOGIN, &request) {
            Ok(LoginDecision::Allow {
                server,
                permissions,
                mute,
            }) => {
                self.permissions.insert(login.uuid.clone(), permissions);
                self.set_mute(&login.uuid, mute);
                self.hubs.insert(login.uuid.clone(), server);
                self.admitted.insert(login.uuid.clone(), Instant::now());
                self.signals.joined(uuid, request.protocol, now_ms());
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

    /// Muted players' chat goes nowhere. Every message counts towards the
    /// signals, blocked or not.
    fn on_chat(&mut self, mut chat: PlayerChatEvent) -> PlayerChatEvent {
        if let Ok(uuid) = Uuid::parse_str(&chat.uuid) {
            self.signals.chat(uuid, now_ms());
        }
        if !chat.cancelled
            && let Some(notice) = self.muted(&chat.uuid)
        {
            chat.cancelled = true;
            proxy::send_message(&chat.uuid, &notice);
        }
        chat
    }

    fn on_command(&mut self, command: &PlayerCommandEvent) {
        if let Ok(uuid) = Uuid::parse_str(&command.uuid) {
            self.signals.command(uuid, now_ms());
        }
    }

    /// Vine only fires these for messages from the client.
    fn on_plugin_message(&mut self, message: &PluginMessageEvent) {
        if let Some(uuid) = message
            .player_uuid
            .as_deref()
            .and_then(|uuid| Uuid::parse_str(uuid).ok())
        {
            self.signals.plugin_message(uuid, now_ms());
        }
    }

    fn set_mute(&mut self, uuid: &str, mute: Option<MuteInfo>) {
        match mute {
            Some(mute) => self.mutes.insert(uuid.to_owned(), mute),
            None => self.mutes.remove(uuid),
        };
    }

    /// If the player is muted now, what to tell them. Expired mutes are
    /// dropped here.
    pub(crate) fn muted(&mut self, uuid: &str) -> Option<String> {
        let mute = self.mutes.get(uuid)?;
        let left = match mute.until_ms {
            None => None,
            Some(until) => {
                let now = now_ms();
                if until <= now {
                    self.mutes.remove(uuid);
                    return None;
                }
                Some(commands::duration_text((until - now) / 1000))
            }
        };
        Some(match left {
            Some(left) => format!(
                "§cYou are muted for {left} more. §7Reason: §f{}",
                mute.reason
            ),
            None => format!("§cYou are muted. §7Reason: §f{}", mute.reason),
        })
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
        self.signals.connected(uuid, now_ms());
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
        self.permissions.remove(&gone.uuid);
        self.mutes.remove(&gone.uuid);
        self.last_from.remove(&gone.uuid);
        if let Ok(uuid) = Uuid::parse_str(&gone.uuid) {
            self.signals.left(uuid, now_ms());
        }
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
        if self.signals.due(SIGNAL_WINDOW_MS, now_ms()) {
            self.send_signals();
        }
    }

    /// Ends the signal window and sends its samples. Measurement only: a
    /// batch that fails is dropped.
    fn send_signals(&mut self) {
        let live: Vec<Uuid> = proxy::get_players()
            .iter()
            .filter_map(|player| Uuid::parse_str(&player.uuid).ok())
            .collect();
        let now = now_ms();
        let grace = u64::try_from(SWEEP_AFTER.as_millis()).unwrap_or(u64::MAX);
        self.signals.prune(&live, grace, now);
        let mut samples = self.signals.flush(now);
        let window = u32::try_from(SIGNAL_WINDOW_MS).unwrap_or(u32::MAX);
        signals::merge_timing(
            &mut samples,
            packet_timing::take_packet_timing()
                .into_iter()
                .filter_map(|timing| {
                    let uuid = Uuid::parse_str(&timing.uuid).ok()?;
                    let protocol = i32::try_from(timing.protocol_version).unwrap_or(0);
                    Some((uuid, protocol, timing.metrics))
                }),
            now,
            window,
        );
        // Checks on Vine's packet timing: violations with evidence for the
        // verdict service, which only records them for now.
        let limits = hallow_anticheat::Limits::default();
        let violations: Vec<_> = samples
            .iter()
            .flat_map(|sample| hallow_anticheat::proxy_checks(sample, &limits))
            .collect();
        for chunk in violations.chunks(SIGNAL_BATCH) {
            let batch = ViolationBatch {
                source: self.config.proxy_id.clone(),
                violations: chunk.to_vec(),
            };
            if let Err(err) = self.client.post_empty(routes::VIOLATIONS, &batch) {
                log(Level::Warn, &format!("violation batch dropped: {err}"));
                break;
            }
        }
        for chunk in samples.chunks(SIGNAL_BATCH) {
            let batch = SignalBatch {
                source: self.config.proxy_id.clone(),
                samples: chunk.to_vec(),
            };
            if let Err(err) = self.client.post_empty(routes::SIGNALS, &batch) {
                log(Level::Debug, &format!("signal batch dropped: {err}"));
                return;
            }
        }
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
        // Staff drained this proxy before it (re)started.
        if registered.draining != proxy::is_draining() {
            proxy::set_draining(registered.draining);
        }
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
                ip: address(&player.client_ip),
            };
            match self
                .client
                .post::<_, LoginDecision>(routes::PRESENCE_LOGIN, &request)
            {
                Ok(LoginDecision::Allow {
                    permissions, mute, ..
                }) => {
                    self.permissions.insert(player.uuid.clone(), permissions);
                    self.set_mute(&player.uuid, mute);
                    self.admitted.insert(player.uuid.clone(), Instant::now());
                    self.signals.joined(uuid, request.protocol, now_ms());
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
                    self.run(command);
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

/// Wall-clock time, Unix milliseconds.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The IP part of a client address Vine reports (`1.2.3.4`, `1.2.3.4:5678`,
/// `[::1]:5678`), or `None` if it isn't one.
fn address(client: &str) -> Option<String> {
    let ip = client
        .parse::<std::net::SocketAddr>()
        .map(|socket| socket.ip())
        .or_else(|_| client.parse::<std::net::IpAddr>())
        .ok()?;
    Some(ip.to_string())
}

impl State {
    /// Carries out a command the control plane queued for this proxy.
    fn run(&mut self, command: ProxyCommand) {
        match command {
            ProxyCommand::Send { player, server } => {
                proxy::connect_player(&player.to_string(), &server);
            }
            ProxyCommand::Message { player, text, from } => {
                let player = player.to_string();
                if proxy::send_message(&player, &text)
                    && let Some(from) = from
                {
                    self.last_from.insert(player, from);
                }
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
            ProxyCommand::Mute { player, mute } => self.set_mute(&player.to_string(), mute),
            ProxyCommand::Drain { draining } => {
                proxy::set_draining(draining);
                log(
                    Level::Info,
                    if draining {
                        "draining: refusing new logins"
                    } else {
                        "no longer draining"
                    },
                );
            }
        }
    }
}

export!(Hallow);
