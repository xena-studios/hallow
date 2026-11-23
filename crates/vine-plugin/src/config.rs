//! `config.toml` in the plugin's data folder (`plugins/data/hallow_vine/`,
//! mounted at `/data` with the `fs.read.data` permission).
//!
//! ```toml
//! control_plane = "control-plane:8080"   # host:port, plain HTTP on the private network
//! token_file = "token"                   # or token = "...", read from /data
//! proxy_id = "proxy-1"                   # unique per proxy
//! address = "proxy-1:25577"              # where the load balancer reaches this proxy
//! forwarding = "vine"                    # how to forward to servers from the control plane
//! forwarding_secret_file = "forwarding.secret"   # or forwarding_secret = "..."
//! poll_ms = 250                          # how often to poll for changes
//! hub_kind = "hub"                       # where players go if a server drops them and the control plane can't say
//! transfer_to = "play.example.net:25565" # optional: the load balancer; a draining proxy transfers players there
//! ```

use serde::Deserialize;

pub const PATH: &str = "/data/config.toml";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    control_plane: String,
    token: Option<String>,
    token_file: Option<String>,
    proxy_id: String,
    address: String,
    #[serde(default = "default_forwarding")]
    forwarding: String,
    forwarding_secret: Option<String>,
    forwarding_secret_file: Option<String>,
    #[serde(default = "default_poll_ms")]
    poll_ms: u64,
    #[serde(default = "default_hub_kind")]
    hub_kind: String,
    transfer_to: Option<String>,
}

fn default_hub_kind() -> String {
    "hub".into()
}

fn default_forwarding() -> String {
    "vine".into()
}

fn default_poll_ms() -> u64 {
    250
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forwarding {
    None,
    Bungeecord,
    Velocity,
    Vine,
}

pub struct Config {
    pub control_plane: String,
    pub token: String,
    pub proxy_id: String,
    pub address: String,
    pub forwarding: Forwarding,
    pub forwarding_secret: Option<String>,
    pub poll_ms: u64,
    pub hub_kind: String,
    /// `host:port` a draining proxy transfers players to.
    pub transfer_to: Option<String>,
}

/// A secret given inline or as a file name under `/data`.
fn secret(
    what: &str,
    inline: Option<String>,
    file: Option<String>,
) -> Result<Option<String>, String> {
    match (inline, file) {
        (Some(_), Some(_)) => Err(format!("set {what} or {what}_file, not both")),
        (Some(value), None) => Ok(Some(value)),
        (None, Some(file)) => std::fs::read_to_string(format!("/data/{file}"))
            .map(|value| Some(value.trim().to_owned()))
            .map_err(|err| format!("can't read /data/{file}: {err}")),
        (None, None) => Ok(None),
    }
}

impl Config {
    pub fn load() -> Result<Self, String> {
        let text = std::fs::read_to_string(PATH).map_err(|err| {
            format!("can't read {PATH} (is fs.read.data granted to hallow_vine?): {err}")
        })?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let file: File = toml::from_str(text).map_err(|err| format!("{PATH}: {err}"))?;
        let token = secret("token", file.token, file.token_file)?
            .filter(|token| !token.is_empty())
            .ok_or("set token or token_file")?;
        let forwarding = match file.forwarding.as_str() {
            "none" => Forwarding::None,
            "bungeecord" => Forwarding::Bungeecord,
            "velocity" => Forwarding::Velocity,
            "vine" => Forwarding::Vine,
            other => {
                return Err(format!(
                    "forwarding must be none, bungeecord, velocity or vine, not {other}"
                ));
            }
        };
        let forwarding_secret = secret(
            "forwarding_secret",
            file.forwarding_secret,
            file.forwarding_secret_file,
        )?;
        if matches!(forwarding, Forwarding::Velocity | Forwarding::Vine)
            && forwarding_secret.is_none()
        {
            return Err(format!(
                "{} forwarding needs forwarding_secret",
                file.forwarding
            ));
        }
        Ok(Self {
            control_plane: file.control_plane,
            token,
            proxy_id: file.proxy_id,
            address: file.address,
            forwarding,
            forwarding_secret,
            poll_ms: file.poll_ms.clamp(50, 5000),
            hub_kind: file.hub_kind,
            transfer_to: file.transfer_to.filter(|t| !t.is_empty()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inline_secrets() {
        let config = Config::parse(
            r#"
            control_plane = "cp:8080"
            token = "t"
            proxy_id = "proxy-1"
            address = "proxy-1:25577"
            forwarding_secret = "s"
            "#,
        )
        .unwrap();
        assert_eq!(config.forwarding, Forwarding::Vine);
        assert_eq!(config.poll_ms, 250);
    }

    #[test]
    fn vine_forwarding_needs_a_secret() {
        let err = Config::parse(
            r#"
            control_plane = "cp:8080"
            token = "t"
            proxy_id = "proxy-1"
            address = "proxy-1:25577"
            "#,
        )
        .err()
        .unwrap();
        assert!(err.contains("forwarding_secret"), "{err}");
    }
}
