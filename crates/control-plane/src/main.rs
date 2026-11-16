//! The control plane binary. Settings come from the environment:
//!
//! | Variable | Default | |
//! |---|---|---|
//! | `HALLOW_TOKEN` or `HALLOW_TOKEN_FILE` | required | Bearer token every caller sends |
//! | `HALLOW_REDIS_URL` | `redis://127.0.0.1:6379` | |
//! | `HALLOW_LISTEN` | `0.0.0.0:8080` | |
//! | `HALLOW_AGENT_LISTEN` | `0.0.0.0:8081` | HAProxy agent checks (plain TCP, no token); `off` disables |
//! | `HALLOW_KEY_PREFIX` | `hallow` | Prefix for Redis keys, to share one Redis between networks |
//! | `HALLOW_HUB_KIND` | `hub` | Server kind players land on after login |
//! | `HALLOW_MAX_PLAYERS` | `1000` | Network player limit |
//! | `HALLOW_DUPLICATE_LOGIN` | `deny` | `deny` refuses a second login; `kick_old` kicks the first session |
//! | `HALLOW_DATABASE_URL` | none | PostgreSQL for accounts, bans, ranks and sessions; without it they're off |
//! | `HALLOW_ADMINS` | none | Comma-separated usernames given the `admin` rank (`*`) at login |
//! | `HALLOW_SIGNAL_RETENTION_DAYS` | `14` | How long anticheat signal samples are kept |
//! | `HALLOW_PANEL_URL` | none | Where the control plane calls the panel's admin API; without it node joins are off |
//! | `HALLOW_PANEL_API_KEY` or `HALLOW_PANEL_API_KEY_FILE` | required with the panel | Admin API key (`nodes.create`, `nodes.allocations`, `nodes.reset-token`, `nodes.delete`; `nodes.read-token` for Wings instances) |
//! | `HALLOW_PANEL_PUBLIC_URL` | `HALLOW_PANEL_URL` | The panel URL nodes' Wings use |
//! | `HALLOW_PANEL_LOCATIONS` | none | Panel location per region: `fsn1=<uuid>,nbg1=<uuid>`; `*=<uuid>` for any other |
//! | `HALLOW_NODE_GAME_PORTS` | `30000-30999` | Game-port allocations made on each node; match `install.sh --game-ports` |
//! | `HALLOW_NODE_WINGS_PORT` / `HALLOW_NODE_SFTP_PORT` | `8080` / `2022` | Wings' API and SFTP ports on nodes |
//! | `HALLOW_JOIN_TOKEN_TTL_HOURS` | `24` | Default lifetime of a node join token (at most 168) |
//! | `HALLOW_NODE_WINGS_REMOTE` | the panel's public URL | The remote URL new nodes' Wings are enrolled with; the control plane's own URL for Wings instances |
//! | `HALLOW_ORCHESTRATOR` | `off` | `docker` or `wings` runs instances of registered kinds (docs/INSTANCES.md); needs the database |
//! | `HALLOW_INSTANCE_CONTROL_PLANE` | required with an orchestrator | `host:port` instances reach the control plane at |
//! | `HALLOW_INSTANCE_ENV` | none | Comma-separated `NAME=value` for every instance, such as `HALLOW_VINE_SECRET=...` |
//! | `HALLOW_KINDS_FILE` | none | Kinds registered at start: `[[kind]]` tables of `image` and `manifest` (a `hallow.toml` path) |
//! | `HALLOW_INSTANCE_START_TIMEOUT_SECS` | `120` | An instance whose server hasn't registered by then is deleted |
//! | `HALLOW_INSTANCE_EMPTY_SECS` | `30` | A game instance empty this long after having players is deleted |
//! | `HALLOW_INSTANCE_IDLE_SECS` | `300` | An unused instance beyond the kind's `warm` count is deleted after this |
//! | `HALLOW_DOCKER_HOST` | `unix:///var/run/docker.sock` | Docker orchestrator: the Engine API (`unix://` or `tcp://`) |
//! | `HALLOW_DOCKER_NETWORK` | required with Docker | Docker orchestrator: the network instances join, such as `gatetest_default` |
//! | `HALLOW_DOCKER_OWNER` | the network | Docker orchestrator: label value marking this network's containers |
//! | `RUST_LOG` | `info` | Log filter |

use std::{env, process::ExitCode};

use hallow_control_plane::{
    Config, DuplicateLogin, InstancesSetup, OrchestratorChoice,
    orchestrator::docker::{DockerConfig, Endpoint},
    panel::PanelConfig,
    serve_all,
};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let config = match config() {
        Ok(config) => config,
        Err(err) => {
            tracing::error!("{err}");
            return ExitCode::FAILURE;
        }
    };
    let listener = match tokio::net::TcpListener::bind(config.listen).await {
        Ok(listener) => listener,
        Err(err) => {
            tracing::error!(%err, address = %config.listen, "can't listen");
            return ExitCode::FAILURE;
        }
    };
    let agent_listen = env::var("HALLOW_AGENT_LISTEN").unwrap_or_else(|_| "0.0.0.0:8081".into());
    let agent = if agent_listen == "off" {
        None
    } else {
        match tokio::net::TcpListener::bind(&agent_listen).await {
            Ok(listener) => Some(listener),
            Err(err) => {
                tracing::error!(%err, address = %agent_listen, "can't listen for agent checks");
                return ExitCode::FAILURE;
            }
        }
    };
    if let Err(err) = serve_all(listener, agent, config).await {
        tracing::error!(%err, "control plane stopped");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn config() -> Result<Config, String> {
    let token = match (env::var("HALLOW_TOKEN"), env::var("HALLOW_TOKEN_FILE")) {
        (Ok(token), _) => token,
        (_, Ok(path)) => std::fs::read_to_string(&path)
            .map_err(|err| format!("can't read HALLOW_TOKEN_FILE {path}: {err}"))?
            .trim()
            .to_owned(),
        _ => return Err("set HALLOW_TOKEN or HALLOW_TOKEN_FILE".into()),
    };
    if token.len() < 16 {
        return Err("the token must be at least 16 characters".into());
    }
    let redis_url =
        env::var("HALLOW_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
    let mut config = Config::new(redis_url, token);
    if let Ok(listen) = env::var("HALLOW_LISTEN") {
        config.listen = listen
            .parse()
            .map_err(|err| format!("HALLOW_LISTEN {listen}: {err}"))?;
    }
    if let Ok(prefix) = env::var("HALLOW_KEY_PREFIX") {
        config.key_prefix = prefix;
    }
    if let Ok(kind) = env::var("HALLOW_HUB_KIND") {
        config.hub_kind = kind;
    }
    if let Ok(max) = env::var("HALLOW_MAX_PLAYERS") {
        config.max_players = max
            .parse()
            .map_err(|err| format!("HALLOW_MAX_PLAYERS {max}: {err}"))?;
    }
    if let Ok(url) = env::var("HALLOW_DATABASE_URL") {
        config.database_url = Some(url);
    }
    if let Ok(admins) = env::var("HALLOW_ADMINS") {
        config.admins = admins
            .split(',')
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_owned)
            .collect();
    }
    if let Ok(days) = env::var("HALLOW_SIGNAL_RETENTION_DAYS") {
        config.signal_retention_days = days
            .parse()
            .map_err(|err| format!("HALLOW_SIGNAL_RETENTION_DAYS {days}: {err}"))?;
    }
    if let Ok(hours) = env::var("HALLOW_JOIN_TOKEN_TTL_HOURS") {
        let hours: u64 = hours
            .parse()
            .ok()
            .filter(|h| (1..=168).contains(h))
            .ok_or_else(|| format!("HALLOW_JOIN_TOKEN_TTL_HOURS must be 1 to 168, not {hours}"))?;
        config.join_token_ttl = std::time::Duration::from_secs(hours * 3600);
    }
    config.panel = panel_config()?;
    config.instances = instances_setup()?;
    if let Ok(mode) = env::var("HALLOW_DUPLICATE_LOGIN") {
        config.duplicate_login = match mode.as_str() {
            "deny" => DuplicateLogin::Deny,
            "kick_old" => DuplicateLogin::KickOld,
            other => {
                return Err(format!(
                    "HALLOW_DUPLICATE_LOGIN must be deny or kick_old, not {other}"
                ));
            }
        };
    }
    Ok(config)
}

fn panel_config() -> Result<Option<PanelConfig>, String> {
    let Ok(url) = env::var("HALLOW_PANEL_URL") else {
        return Ok(None);
    };
    let api_key = match (
        env::var("HALLOW_PANEL_API_KEY"),
        env::var("HALLOW_PANEL_API_KEY_FILE"),
    ) {
        (Ok(key), _) => key,
        (_, Ok(path)) => std::fs::read_to_string(&path)
            .map_err(|err| format!("can't read HALLOW_PANEL_API_KEY_FILE {path}: {err}"))?
            .trim()
            .to_owned(),
        _ => return Err("HALLOW_PANEL_URL needs HALLOW_PANEL_API_KEY or _FILE".into()),
    };
    let mut panel = PanelConfig::new(url, api_key);
    if let Ok(public) = env::var("HALLOW_PANEL_PUBLIC_URL") {
        panel.public_url = public;
    }
    if let Ok(locations) = env::var("HALLOW_PANEL_LOCATIONS") {
        for entry in locations
            .split(',')
            .map(str::trim)
            .filter(|e| !e.is_empty())
        {
            let (region, uuid) = entry
                .split_once('=')
                .ok_or_else(|| format!("HALLOW_PANEL_LOCATIONS: {entry} isn't region=uuid"))?;
            let uuid = uuid
                .trim()
                .parse()
                .map_err(|err| format!("HALLOW_PANEL_LOCATIONS: {entry}: {err}"))?;
            if region.trim() == "*" {
                panel.default_location = Some(uuid);
            } else {
                panel.locations.insert(region.trim().to_owned(), uuid);
            }
        }
    }
    if let Ok(remote) = env::var("HALLOW_NODE_WINGS_REMOTE") {
        panel.wings_remote = Some(remote);
    }
    if let Ok(range) = env::var("HALLOW_NODE_GAME_PORTS") {
        let parsed = range
            .split_once('-')
            .and_then(|(lo, hi)| {
                Some((
                    lo.trim().parse::<u16>().ok()?,
                    hi.trim().parse::<u16>().ok()?,
                ))
            })
            .filter(|(lo, hi)| 1024 <= *lo && lo <= hi && *hi < 40000);
        panel.game_ports = parsed.ok_or_else(|| {
            format!("HALLOW_NODE_GAME_PORTS must be LO-HI within 1024-39999, not {range}")
        })?;
    }
    for (var, port) in [
        ("HALLOW_NODE_WINGS_PORT", &mut panel.wings_port),
        ("HALLOW_NODE_SFTP_PORT", &mut panel.sftp_port),
    ] {
        if let Ok(value) = env::var(var) {
            *port = value
                .parse()
                .map_err(|err| format!("{var} {value}: {err}"))?;
        }
    }
    Ok(Some(panel))
}

fn instances_setup() -> Result<Option<InstancesSetup>, String> {
    let choice = env::var("HALLOW_ORCHESTRATOR").unwrap_or_else(|_| "off".into());
    let orchestrator = match choice.as_str() {
        "off" => return Ok(None),
        "wings" => OrchestratorChoice::Wings,
        "docker" => {
            let host = env::var("HALLOW_DOCKER_HOST")
                .unwrap_or_else(|_| "unix:///var/run/docker.sock".into());
            let endpoint =
                Endpoint::parse(&host).map_err(|err| format!("HALLOW_DOCKER_HOST: {err}"))?;
            let network = env::var("HALLOW_DOCKER_NETWORK")
                .map_err(|_| "HALLOW_ORCHESTRATOR=docker needs HALLOW_DOCKER_NETWORK")?;
            let mut docker = DockerConfig::new(endpoint, network);
            if let Ok(owner) = env::var("HALLOW_DOCKER_OWNER") {
                docker.owner = owner;
            }
            OrchestratorChoice::Docker(docker)
        }
        other => {
            return Err(format!(
                "HALLOW_ORCHESTRATOR must be off, docker or wings, not {other}"
            ));
        }
    };
    let control_plane = env::var("HALLOW_INSTANCE_CONTROL_PLANE")
        .map_err(|_| "an orchestrator needs HALLOW_INSTANCE_CONTROL_PLANE")?;
    let mut setup = InstancesSetup::new(orchestrator, control_plane);
    if let Ok(list) = env::var("HALLOW_INSTANCE_ENV") {
        for entry in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let (name, value) = entry
                .split_once('=')
                .ok_or_else(|| format!("HALLOW_INSTANCE_ENV: {entry} isn't NAME=value"))?;
            setup.env.insert(name.trim().to_owned(), value.to_owned());
        }
    }
    if let Ok(path) = env::var("HALLOW_KINDS_FILE") {
        setup.kinds_file = Some(path.into());
    }
    for (var, field) in [
        (
            "HALLOW_INSTANCE_START_TIMEOUT_SECS",
            &mut setup.settings.start_timeout_ms,
        ),
        (
            "HALLOW_INSTANCE_EMPTY_SECS",
            &mut setup.settings.empty_grace_ms,
        ),
        ("HALLOW_INSTANCE_IDLE_SECS", &mut setup.settings.idle_ms),
    ] {
        if let Ok(value) = env::var(var) {
            let secs: u64 = value
                .parse()
                .ok()
                .filter(|s| *s <= 86_400)
                .ok_or_else(|| format!("{var} must be 0 to 86400 seconds, not {value}"))?;
            *field = secs * 1000;
        }
    }
    Ok(Some(setup))
}
