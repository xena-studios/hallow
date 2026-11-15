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
//! | `RUST_LOG` | `info` | Log filter |

use std::{env, process::ExitCode};

use hallow_control_plane::{Config, DuplicateLogin, serve_all};
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
