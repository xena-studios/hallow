//! Hallow's server plugin for Pumpkin: links each lobby and game server to
//! the control plane.
//!
//! - **Register.** About a second after the server starts, `POST /v1/servers`
//!   with its name, kind, version, role, address and player limit.
//! - **Heartbeat.** Every 5 s, `POST /v1/servers/{name}/heartbeat` with the
//!   player count, state and everyone on the server. A `404` means the
//!   control plane forgot the server (it expired, or Redis was reset), so it
//!   registers again.
//! - **Presence.** Within a second or two of a join or leave,
//!   `POST /v1/servers/{name}/players` with who joined and left (see
//!   [`players`]). The heartbeat's full list corrects anything lost.
//! - **Stats.** Every 30 s, `POST /v1/stats` with each player's increases
//!   for this server's kind: play time, joins and whatever a game counts
//!   (see [`stats`]). A failed batch goes with the next one.
//! - **Signals.** Every 30 s, `POST /v1/signals` with each player's movement
//!   and combat numbers for the window, for the anticheat to learn from
//!   (see [`signals`]). Nothing acts on them yet.
//! - **Lobbies.** Gives the hub's plugin the game modes and sends players
//!   to lobbies for it, and when a game ends (the state turns `draining`)
//!   sends everyone to the lobby `hallow.toml` names (see [`lobbies`]).
//! - **Unregister.** `DELETE /v1/servers/{name}` when the plugin unloads,
//!   which Pumpkin does on shutdown after kicking players.
//!
//! The state is `ready` until something changes it: another plugin sends
//! this one an IPC message (see [`state`]), or code in this crate calls
//! [`state::set`]. A change is reported within a second or two, not after
//! 5 s.
//!
//! **Not on the tick.** Pumpkin runs a plugin's scheduled tasks on Tokio
//! tasks, not inside the tick, but it runs one call into a plugin at a time,
//! and the signal event handlers are called from inside the tick. So the task
//! never waits on the network: it starts a request and checks for the answer
//! on its next run, a second later (see [`http`]). Requests time out after
//! 2 s; a failed one is logged and retried on the next heartbeat, a failed
//! presence report is left to the next heartbeat, a failed signal batch is
//! dropped, and a failed stats batch is merged into the next (up to
//! [`stats::MAX_RETRIES`] in a row).
//!
//! It needs `http.outbound`, `fs.read.data` and a `sys.env.<NAME>`
//! permission for each variable it reads; settings are in [`config`].

mod collect;
mod config;
mod http;
pub mod lobbies;
mod players;
mod presence;
mod signals;
pub mod state;
mod stats;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use hallow_proto::{
    routes,
    server::{RegisterServer, ServerHeartbeat, ServerPlayersChanged, ServerState},
    signal::SignalBatch,
    stats::{PlayerStatsDelta, StatsBatch},
};
use pumpkin_plugin_api::{
    Context, Plugin, PluginMetadata, Server, permissions, register_plugin, scheduler, wit,
};

use config::{Config, Sandbox};

const NAME: &str = "hallow_server";
const HEARTBEAT: Duration = Duration::from_secs(routes::HEARTBEAT_SECONDS);
/// The task runs once a second: it checks the request in flight, then
/// heartbeats when one is due, so a server running behind on ticks still
/// heartbeats close to every 5 s.
const TASK_PERIOD_TICKS: u64 = 20;

/// Every permission the plugin asks for: `http.outbound`, `fs.read.data`
/// and `sys.env.<NAME>` for each variable in [`config`]. Pumpkin asks the
/// console to approve them unless `pumpkin.toml` pre-approves them, as the
/// base image does.
fn permissions() -> Vec<String> {
    [permissions::HTTP_OUTBOUND, permissions::FS_READ_DATA]
        .into_iter()
        .map(str::to_owned)
        .chain(
            config::ENV_VARS
                .iter()
                .map(|var| format!("{}{var}", permissions::SYS_ENV_PREFIX)),
        )
        .collect()
}

struct Link {
    config: Config,
    client: http::Client,
    task: u32,
    registered: bool,
    /// When the last register or heartbeat started.
    last_heartbeat: Option<Instant>,
    /// The state the control plane last accepted.
    reported: Option<ServerState>,
    /// Log a lost connection once, not every heartbeat.
    reported_down: bool,
    /// The register or heartbeat waiting for an answer.
    in_flight: Option<(http::Pending, Call)>,
    /// The signal batch waiting for an answer.
    signals_in_flight: Option<http::Pending>,
    /// Log failed signal batches once, until one gets through.
    signals_down: bool,
    /// The stats batch waiting for an answer, kept to put back if it fails.
    stats_in_flight: Option<(http::Pending, Vec<PlayerStatsDelta>)>,
    /// When the last stats batch was taken.
    last_stats: Instant,
    /// Stats batches that failed in a row.
    stats_failures: u32,
}

/// Never held across a call out of the plugin, so nothing re-entering the
/// plugin (an IPC message, the next task) can find it locked.
static LINK: Mutex<Option<Link>> = Mutex::new(None);

fn with_link<R>(f: impl FnOnce(&mut Link) -> R) -> Option<R> {
    LINK.lock()
        .unwrap_or_else(|err| err.into_inner())
        .as_mut()
        .map(f)
}

/// What a request in flight was for.
enum Call {
    Register(RegisterServer),
    Heartbeat(String, ServerState),
    Players(String, ServerPlayersChanged),
}

/// Runs once a second, off the tick.
fn tick(server: &Server) {
    heartbeat(server);
    if let Some((client, role, kind, return_lobby)) = with_link(|link| {
        (
            link.client.clone(),
            link.config.role,
            link.config.kind.clone(),
            link.config.return_lobby.clone(),
        )
    }) {
        lobbies::tick(
            server,
            &client,
            role,
            &kind,
            return_lobby.as_deref(),
            state::get(),
        );
    }
    send_signals(server);
    send_stats();
}

/// Checks the register, heartbeat or presence report in flight, then starts
/// the next: a heartbeat if it's due or the state changed, otherwise the
/// joins and leaves since the last report. One at a time, so they arrive in
/// order.
fn heartbeat(server: &Server) {
    let Some(in_flight) = with_link(|link| link.in_flight.take()) else {
        return;
    };
    if let Some((pending, call)) = in_flight {
        match pending.poll() {
            None => {
                with_link(|link| link.in_flight = Some((pending, call)));
                return;
            }
            Some(result) => answered(call, result),
        }
    }

    let state = state::get();
    let Some(Some(call)) = with_link(|link| {
        let due = link
            .last_heartbeat
            .is_none_or(|last| last.elapsed() >= HEARTBEAT);
        if !link.registered {
            // Registering also waits for a heartbeat to fall due, so an
            // unreachable control plane is tried every 5 s, not every second.
            due.then(|| {
                let config = &link.config;
                Call::Register(RegisterServer {
                    name: config.name.clone(),
                    kind: config.kind.clone(),
                    version: config.version.clone(),
                    role: config.role,
                    address: config.address.clone(),
                    max_players: config.max_players,
                })
            })
        } else if due || link.reported != Some(state) {
            Some(Call::Heartbeat(link.config.name.clone(), state))
        } else {
            players::take_changes().map(|changed| Call::Players(link.config.name.clone(), changed))
        }
    }) else {
        return;
    };
    let Some(client) = with_link(|link| link.client.clone()) else {
        return;
    };
    let started = match &call {
        Call::Register(request) => client.start_post(routes::SERVERS, request),
        Call::Heartbeat(name, state) => {
            let beat = ServerHeartbeat {
                players: server.get_player_count(),
                state: *state,
                // For the monitoring dashboard's tick-time graphs.
                #[allow(clippy::cast_possible_truncation)]
                tick_ms: Some(server.get_mspt() as f32),
                roster: Some(players::roster(server)),
            };
            client.start_post(&routes::server_heartbeat(name), &beat)
        }
        Call::Players(name, changed) => client.start_post(&routes::server_players(name), changed),
    };
    if !matches!(call, Call::Players(..)) {
        with_link(|link| link.last_heartbeat = Some(Instant::now()));
    }
    match started {
        Ok(pending) => {
            with_link(|link| link.in_flight = Some((pending, call)));
        }
        Err(err) => answered(call, Err(err)),
    }
}

/// Handles the answer to a register or heartbeat.
fn answered(call: Call, result: Result<(), http::Error>) {
    match call {
        Call::Register(request) => match result {
            Ok(()) => {
                succeeded();
                tracing::info!(
                    "hallow-server: registered {} ({} {}, {:?}) at {}",
                    request.name,
                    request.kind,
                    request.version,
                    request.role,
                    request.address
                );
                // Heartbeat straight away, which resets the server to ready
                // with no players on the control plane.
                with_link(|link| {
                    link.registered = true;
                    link.reported = None;
                    link.last_heartbeat = None;
                });
            }
            Err(err) => failed(&err),
        },
        Call::Heartbeat(name, state) => match result {
            Ok(()) => {
                if succeeded() {
                    tracing::info!("hallow-server: the control plane is back");
                }
                with_link(|link| link.reported = Some(state));
            }
            Err(err) if err.is_not_found() => {
                tracing::warn!("hallow-server: the control plane forgot {name}; registering again");
                with_link(|link| {
                    link.registered = false;
                    link.reported = None;
                    // Register on the next run rather than in 5 s.
                    link.last_heartbeat = None;
                });
            }
            Err(err) => failed(&err),
        },
        // A lost report is corrected by the next heartbeat's full list.
        Call::Players(name, _) => match result {
            Ok(()) => {
                succeeded();
            }
            Err(err) if err.is_not_found() => {
                tracing::warn!("hallow-server: the control plane forgot {name}; registering again");
                with_link(|link| {
                    link.registered = false;
                    link.reported = None;
                    link.last_heartbeat = None;
                });
            }
            Err(err) => failed(&err),
        },
    }
}

/// Checks the last stats batch, then sends the next when one is due. A
/// failed batch is put back to go with the next, up to
/// [`stats::MAX_RETRIES`] in a row; then its increases are dropped.
fn send_stats() {
    let Some(in_flight) = with_link(|link| link.stats_in_flight.take()) else {
        return;
    };
    if let Some((pending, deltas)) = in_flight {
        match pending.poll() {
            None => {
                with_link(|link| link.stats_in_flight = Some((pending, deltas)));
                return;
            }
            Some(result) => stats_answered(result, deltas),
        }
    }

    let due = with_link(|link| link.last_stats.elapsed() >= stats::FLUSH).unwrap_or(false);
    if !due {
        return;
    }
    with_link(|link| link.last_stats = Instant::now());
    let deltas = players::take_stats();
    if deltas.is_empty() {
        return;
    }
    let Some((client, source, kind)) = with_link(|link| {
        (
            link.client.clone(),
            link.config.name.clone(),
            link.config.kind.clone(),
        )
    }) else {
        return;
    };
    let batch = StatsBatch {
        source,
        kind,
        players: deltas,
    };
    match client.start_post(routes::STATS, &batch) {
        Ok(pending) => {
            with_link(|link| link.stats_in_flight = Some((pending, batch.players)));
        }
        Err(err) => stats_answered(Err(err), batch.players),
    }
}

fn stats_answered(result: Result<(), http::Error>, deltas: Vec<PlayerStatsDelta>) {
    let Some(failures) = with_link(|link| {
        link.stats_failures = if result.is_ok() {
            0
        } else {
            link.stats_failures + 1
        };
        link.stats_failures
    }) else {
        return;
    };
    match result {
        Ok(()) => {}
        Err(err) if failures > stats::MAX_RETRIES => {
            tracing::warn!(
                "hallow-server: couldn't send stats {failures} times in a row: {err}; dropping {} players' increases",
                deltas.len()
            );
            with_link(|link| link.stats_failures = 0);
        }
        Err(err) => {
            if failures == 1 {
                tracing::warn!(
                    "hallow-server: couldn't send stats: {err}; retrying with the next batch"
                );
            }
            players::restore_stats(deltas);
        }
    }
}

/// Checks the last signal batch, then sends the window's samples when it's
/// over. A batch that fails is dropped, not retried.
fn send_signals(server: &Server) {
    let Some(in_flight) = with_link(|link| link.signals_in_flight.take()) else {
        return;
    };
    let mut still_in_flight = false;
    if let Some(pending) = in_flight {
        match pending.poll() {
            None => {
                still_in_flight = true;
                with_link(|link| link.signals_in_flight = Some(pending));
            }
            Some(result) => signals_answered(result),
        }
    }

    let Some(samples) = collect::take_due(server) else {
        return;
    };
    if samples.is_empty() {
        return;
    }
    if still_in_flight {
        tracing::warn!(
            "hallow-server: the last signal batch is still in flight; dropping {} samples",
            samples.len()
        );
        return;
    }
    let Some((client, source)) = with_link(|link| (link.client.clone(), link.config.name.clone()))
    else {
        return;
    };
    let batch = SignalBatch { source, samples };
    match client.start_post(routes::SIGNALS, &batch) {
        Ok(pending) => {
            with_link(|link| link.signals_in_flight = Some(pending));
        }
        Err(err) => signals_answered(Err(err)),
    }
}

/// Logs the first failed batch in a row, and when batches get through again.
fn signals_answered(result: Result<(), http::Error>) {
    let failed = result.as_ref().err();
    let was_down = with_link(|link| std::mem::replace(&mut link.signals_down, failed.is_some()))
        .unwrap_or(false);
    match failed {
        Some(err) if !was_down => {
            tracing::warn!("hallow-server: couldn't send signals: {err}; dropping them");
        }
        None if was_down => tracing::info!("hallow-server: signals are getting through again"),
        _ => {}
    }
}

/// Logs the first failure in a row; later ones are quiet until a success.
fn failed(err: &http::Error) {
    let first = with_link(|link| !std::mem::replace(&mut link.reported_down, true));
    if first == Some(true) {
        tracing::warn!("hallow-server: {err}; retrying every 5 s");
    }
}

/// Whether the call before this success had failed.
fn succeeded() -> bool {
    with_link(|link| std::mem::take(&mut link.reported_down)).unwrap_or(false)
}

struct HallowServer;

impl Plugin for HallowServer {
    fn new() -> Self {
        Self
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NAME.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Xena Studios".into()],
            description: "Links this server to Hallow's control plane: registers and heartbeats it, reports its players, and sends stats and anticheat signals."
                .into(),
            dependencies: vec![],
            permissions: permissions(),
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        let source = Sandbox {
            data_folder: context.get_data_folder(),
        };
        let Some(config) = Config::load(&source).map_err(|err| format!("hallow-server: {err}"))?
        else {
            tracing::info!(
                "hallow-server: no control plane configured (HALLOW_CONTROL_PLANE); not linking this server"
            );
            return Ok(());
        };
        tracing::info!(
            "hallow-server: {} links to the control plane at {}",
            config.name,
            config.control_plane
        );
        let client = http::Client {
            authority: config.control_plane.clone(),
            token: config.token.clone(),
        };
        let task =
            scheduler::schedule_repeating_task(TASK_PERIOD_TICKS, TASK_PERIOD_TICKS, |server| {
                tick(&server);
            });
        *LINK.lock().unwrap_or_else(|err| err.into_inner()) = Some(Link {
            config,
            client,
            task,
            registered: false,
            last_heartbeat: None,
            reported: None,
            reported_down: false,
            in_flight: None,
            signals_in_flight: None,
            signals_down: false,
            stats_in_flight: None,
            last_stats: Instant::now(),
            stats_failures: 0,
        });
        collect::register(&context);
        players::register(&context);
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        let Some(link) = LINK.lock().unwrap_or_else(|err| err.into_inner()).take() else {
            return Ok(());
        };
        scheduler::cancel_task(link.task);
        collect::stop();
        // Players are gone by now, so this holds their whole stay. A batch
        // still in flight may or may not have landed; it isn't sent twice.
        let deltas = players::stop();
        if !deltas.is_empty() {
            let batch = StatsBatch {
                source: link.config.name.clone(),
                kind: link.config.kind.clone(),
                players: deltas,
            };
            if let Err(err) = link.client.post(routes::STATS, &batch) {
                tracing::warn!("hallow-server: couldn't send the last stats: {err}");
            }
        }
        if link.registered {
            match link.client.delete(&routes::server(&link.config.name)) {
                Ok(()) => tracing::info!("hallow-server: unregistered {}", link.config.name),
                // Not fatal: the control plane expires it after 15 s.
                Err(err) => tracing::warn!(
                    "hallow-server: couldn't unregister {}: {err}",
                    link.config.name
                ),
            }
        }
        Ok(())
    }

    fn handle_ipc_message(
        &self,
        _sender: wit::PluginId,
        message: wit::IpcMessage,
    ) -> Result<wit::IpcMessage, String> {
        if let Ok(text) = std::str::from_utf8(&message) {
            let text = text.trim();
            let client = with_link(|link| link.client.clone());
            if let Some(reply) = lobbies::handle_ipc(text, client) {
                return reply;
            }
            if let Some(reply) = players::handle_ipc(text) {
                return reply;
            }
        }
        state::handle_ipc(&message)
    }
}

register_plugin!(HallowServer);
