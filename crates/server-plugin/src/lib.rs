//! Hallow's server plugin for Pumpkin: links each lobby and game server to
//! the control plane.
//!
//! - **Register.** About a second after the server starts, `POST /v1/servers`
//!   with its name, kind, version, role, address and player limit.
//! - **Heartbeat.** Every 5 s, `POST /v1/servers/{name}/heartbeat` with the
//!   player count and state. A `404` means the control plane forgot the
//!   server (it expired, or Redis was reset), so it registers again.
//! - **Unregister.** `DELETE /v1/servers/{name}` when the plugin unloads,
//!   which Pumpkin does on shutdown after kicking players.
//!
//! The state is `ready` until something changes it: another plugin sends
//! this one an IPC message (see [`state`]), or code in this crate calls
//! [`state::set`]. A change is reported on the next tick, not after 5 s.
//!
//! **Not on the tick.** Pumpkin runs a plugin's scheduled tasks on Tokio
//! tasks (`server.spawn_task`), not inside the tick, and its `wasi:http` is
//! async on the host. A slow control plane holds up this plugin, never the
//! server. Calls still time out after 2 s, and a failed call is logged and
//! retried on the next heartbeat.
//!
//! It needs `http.outbound`, `fs.read.data` and a `sys.env.<NAME>`
//! permission for each variable it reads; settings are in [`config`].

mod config;
mod http;
pub mod state;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use hallow_proto::{
    routes,
    server::{RegisterServer, ServerHeartbeat, ServerState},
};
use pumpkin_plugin_api::{
    Context, Plugin, PluginMetadata, Server, permissions, register_plugin, scheduler, wit,
};

use config::{Config, Sandbox};

const NAME: &str = "hallow_server";
const HEARTBEAT: Duration = Duration::from_secs(routes::HEARTBEAT_SECONDS);
/// The task runs once a second and heartbeats when one is due, so a server
/// running behind on ticks still heartbeats close to every 5 s.
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
    last_heartbeat: Option<Instant>,
    /// The state the control plane last accepted.
    reported: Option<ServerState>,
    /// Log a lost connection once, not every heartbeat.
    reported_down: bool,
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

enum Step {
    Register(http::Client, RegisterServer),
    Heartbeat(http::Client, String),
}

/// Runs once a second: registers, or heartbeats when one is due or the
/// state changed.
fn tick(server: &Server) {
    let state = state::get();
    let Some(Some(step)) = with_link(|link| {
        let due = link
            .last_heartbeat
            .is_none_or(|last| last.elapsed() >= HEARTBEAT);
        if !link.registered {
            // Registering also waits for a heartbeat to fall due, so an
            // unreachable control plane is tried every 5 s, not every second.
            due.then(|| {
                let config = &link.config;
                Step::Register(
                    link.client.clone(),
                    RegisterServer {
                        name: config.name.clone(),
                        kind: config.kind.clone(),
                        version: config.version.clone(),
                        role: config.role,
                        address: config.address.clone(),
                        max_players: config.max_players,
                    },
                )
            })
        } else if due || link.reported != Some(state) {
            Some(Step::Heartbeat(
                link.client.clone(),
                link.config.name.clone(),
            ))
        } else {
            None
        }
    }) else {
        return;
    };

    let (client, name) = match step {
        Step::Register(client, request) => {
            let result = client.post_empty(routes::SERVERS, &request);
            let ok = result.is_ok();
            match result {
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
                }
                Err(err) => failed(&err),
            }
            with_link(|link| {
                link.registered = ok;
                link.last_heartbeat = Some(Instant::now());
            });
            if !ok {
                return;
            }
            (client, request.name)
        }
        Step::Heartbeat(client, name) => (client, name),
    };

    // Also sent right after registering, which resets the server to ready
    // with no players.
    let beat = ServerHeartbeat {
        players: server.get_player_count(),
        state,
        // For the monitoring dashboard's tick-time graphs.
        #[allow(clippy::cast_possible_truncation)]
        tick_ms: Some(server.get_mspt() as f32),
    };
    let result = client.post_empty(&routes::server_heartbeat(&name), &beat);
    let mut forgotten = false;
    match &result {
        Ok(()) => {
            if succeeded() {
                tracing::info!("hallow-server: the control plane is back");
            }
        }
        Err(err) if err.is_not_found() => {
            tracing::warn!("hallow-server: the control plane forgot {name}; registering again");
            forgotten = true;
        }
        Err(err) => failed(err),
    }
    with_link(|link| {
        link.last_heartbeat = Some(Instant::now());
        if result.is_ok() {
            link.reported = Some(state);
        }
        if forgotten {
            link.registered = false;
            link.reported = None;
            // Register on the next tick rather than in 5 s.
            link.last_heartbeat = None;
        }
    });
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
            description: "Registers this server with Hallow's control plane and heartbeats it."
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
        });
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> Result<(), String> {
        let Some(link) = LINK.lock().unwrap_or_else(|err| err.into_inner()).take() else {
            return Ok(());
        };
        scheduler::cancel_task(link.task);
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
        state::handle_ipc(&message)
    }
}

register_plugin!(HallowServer);
