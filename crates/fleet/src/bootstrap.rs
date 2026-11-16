//! Cloud-init user data that turns a new machine into a Hallow node.
//!
//! [`render_cloud_init`] fills `deploy/node/cloud-init.yaml` with the node's
//! settings and `deploy/node/install.sh`, both compiled in, so the user data
//! always matches the binary that sends it. The machine runs the same script
//! with the same settings a staff member would pass by hand on a dedicated
//! machine (see `deploy/node/README.md`). The result goes into
//! [`NodeSpec::user_data`](crate::NodeSpec::user_data).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::provider::NodeKind;

/// `deploy/node/install.sh`, as compiled in.
pub const INSTALL_SCRIPT: &str = include_str!("../../../deploy/node/install.sh");
/// `deploy/node/cloud-init.yaml`, the template [`render_cloud_init`] fills.
pub const CLOUD_INIT_TEMPLATE: &str = include_str!("../../../deploy/node/cloud-init.yaml");
/// Where cloud-init writes the settings, read by `install.sh --env-file`.
pub const ENV_FILE: &str = "/etc/hallow/node.env";
/// The smallest user-data limit among Hallow's providers (Hetzner Cloud:
/// 32 KiB). [`render_cloud_init`] refuses anything bigger.
pub const USER_DATA_LIMIT: usize = 32 * 1024;

/// Everything a new node needs to set itself up and join.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeBootstrap {
    /// Where the node exchanges its join token, e.g. `https://cp.example.net`.
    pub control_plane_url: String,
    /// A one-time join token from `POST /v1/nodes/join-tokens`.
    pub join_token: String,
    pub role: NodeKind,
    /// The provider's region, e.g. `fsn1`.
    pub region: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// The private network. Without it the node needs a provider private
    /// network in [`private_interfaces`](Self::private_interfaces).
    pub wireguard: Option<WireGuard>,
    /// Extra trusted private interfaces (Hetzner vSwitch or Cloud Network,
    /// OVHcloud vRack), such as `ens10`.
    #[serde(default)]
    pub private_interfaces: Vec<String>,
    /// CIDRs allowed to reach SSH; empty allows anywhere.
    #[serde(default)]
    pub ssh_allow: Vec<String>,
    /// CIDRs allowed to reach Wings' API and SFTP besides the private network.
    #[serde(default)]
    pub wings_allow: Vec<String>,
    /// The panel allocation range for game servers, `(low, high)`; `None`
    /// keeps the script's default (30000-30999).
    pub game_ports: Option<(u16, u16)>,
    /// A Wings build other than the one pinned in `install.sh`.
    pub wings: Option<WingsBuild>,
    /// Keys for the image's default user, in `authorized_keys` format.
    #[serde(default)]
    pub ssh_authorized_keys: Vec<String>,
}

/// This node's WireGuard interface (`wg0`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireGuard {
    /// IPv4 with prefix, e.g. `10.42.1.7/16`.
    pub address: String,
    /// `None` keeps the default, 51820.
    pub listen_port: Option<u16>,
    pub peers: Vec<WireGuardPeer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireGuardPeer {
    /// Base64, 44 characters.
    pub public_key: String,
    /// `host:port`, or `None` for a peer that connects to us.
    pub endpoint: Option<String>,
    pub allowed_ips: Vec<String>,
    pub persistent_keepalive: Option<u16>,
}

/// A Wings release to install instead of the pinned one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WingsBuild {
    pub version: String,
    /// Download URL; `{version}` and `{arch}` (`x86_64`, `aarch64`) are
    /// filled in. `None` uses Calagopus' GitHub releases.
    pub url: Option<String>,
    /// SHA-256 of the binary, lowercase hex.
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BootstrapError {
    #[error("invalid {field}: {reason}")]
    Invalid { field: &'static str, reason: String },
    #[error("the user data is {0} bytes, over the {USER_DATA_LIMIT}-byte limit")]
    TooLarge(usize),
}

fn invalid(field: &'static str, reason: impl Into<String>) -> BootstrapError {
    BootstrapError::Invalid {
        field,
        reason: reason.into(),
    }
}

/// A value that goes on one `KEY=VALUE` line: printable, no spaces.
fn word(field: &'static str, value: &str) -> Result<(), BootstrapError> {
    if value.is_empty() {
        return Err(invalid(field, "empty"));
    }
    if let Some(c) = value.chars().find(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid(field, format!("{c:?} not allowed in {value:?}")));
    }
    Ok(())
}

fn charset(
    field: &'static str,
    value: &str,
    max: usize,
    extra: &str,
) -> Result<(), BootstrapError> {
    let ok = value.len() <= max
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(c));
    if ok {
        Ok(())
    } else {
        Err(invalid(
            field,
            format!("{value:?}: at most {max} of letters, digits and {extra:?}"),
        ))
    }
}

/// An IP address with an optional prefix, such as `10.0.0.0/8` or `2001:db8::1`.
fn cidr(field: &'static str, value: &str) -> Result<(), BootstrapError> {
    let (ip, prefix) = value.split_once('/').unwrap_or((value, ""));
    let max = match ip.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(_)) => 32,
        Ok(std::net::IpAddr::V6(_)) => 128,
        Err(_) => return Err(invalid(field, format!("{value:?} is not an IP or CIDR"))),
    };
    if prefix.is_empty() || prefix.parse::<u8>().is_ok_and(|p| p <= max) {
        Ok(())
    } else {
        Err(invalid(field, format!("{value:?} has a bad prefix")))
    }
}

fn url(field: &'static str, value: &str) -> Result<(), BootstrapError> {
    word(field, value)?;
    if value.starts_with("https://") || value.starts_with("http://") {
        Ok(())
    } else {
        Err(invalid(field, format!("{value:?} is not an http(s) URL")))
    }
}

impl NodeBootstrap {
    /// Checks every value against what `install.sh` accepts, so a bad value
    /// fails here rather than on a machine already paid for.
    pub fn validate(&self) -> Result<(), BootstrapError> {
        url("control_plane_url", &self.control_plane_url)?;
        word("join_token", &self.join_token)?;
        word("region", &self.region)?;
        charset("region", &self.region, 64, "._-")?;
        for (key, value) in &self.labels {
            word("labels", key)?;
            charset("labels", key, 63, "._/-")?;
            charset("labels", value, 63, "._-")?;
        }
        if let Some(wg) = &self.wireguard {
            let (ip, prefix) = wg
                .address
                .split_once('/')
                .ok_or_else(|| invalid("wireguard.address", "needs a prefix, e.g. /16"))?;
            if ip.parse::<std::net::Ipv4Addr>().is_err() || prefix.parse::<u8>().is_err() {
                return Err(invalid(
                    "wireguard.address",
                    format!("{:?} is not IPv4/prefix", wg.address),
                ));
            }
            if wg.peers.is_empty() {
                return Err(invalid("wireguard.peers", "at least one peer"));
            }
            for peer in &wg.peers {
                let key = &peer.public_key;
                if key.len() != 44
                    || !key.ends_with('=')
                    || !key[..43]
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/')
                {
                    return Err(invalid(
                        "wireguard.peers.public_key",
                        format!("{key:?} is not a WireGuard key"),
                    ));
                }
                if let Some(endpoint) = &peer.endpoint {
                    word("wireguard.peers.endpoint", endpoint)?;
                    if endpoint.contains('|') {
                        return Err(invalid("wireguard.peers.endpoint", "'|' not allowed"));
                    }
                }
                if peer.allowed_ips.is_empty() {
                    return Err(invalid("wireguard.peers.allowed_ips", "at least one"));
                }
                for ip in &peer.allowed_ips {
                    cidr("wireguard.peers.allowed_ips", ip)?;
                }
            }
        }
        for name in &self.private_interfaces {
            word("private_interfaces", name)?;
            charset("private_interfaces", name, 15, "._-")?;
        }
        for c in &self.ssh_allow {
            cidr("ssh_allow", c)?;
        }
        for c in &self.wings_allow {
            cidr("wings_allow", c)?;
        }
        if let Some((low, high)) = self.game_ports
            && !(1024 <= low && low <= high && high < 40000)
        {
            return Err(invalid(
                "game_ports",
                format!("{low}-{high} must be within 1024-39999"),
            ));
        }
        if let Some(wings) = &self.wings {
            word("wings.version", &wings.version)?;
            if let Some(u) = &wings.url {
                url("wings.url", u)?;
            }
            if wings.sha256.len() != 64
                || !wings
                    .sha256
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
            {
                return Err(invalid("wings.sha256", "64 lowercase hex digits"));
            }
        }
        for key in &self.ssh_authorized_keys {
            if key.trim().is_empty() || key.contains(['\n', '\r']) {
                return Err(invalid("ssh_authorized_keys", "one key per line"));
            }
        }
        Ok(())
    }

    /// The `HALLOW_*` settings `install.sh --env-file` reads, one per line.
    pub fn env(&self) -> Result<String, BootstrapError> {
        self.validate()?;
        let mut vars: Vec<(&str, String)> = vec![
            ("HALLOW_CONTROL_PLANE_URL", self.control_plane_url.clone()),
            ("HALLOW_JOIN_TOKEN", self.join_token.clone()),
            ("HALLOW_ROLE", self.role.to_string()),
            ("HALLOW_REGION", self.region.clone()),
        ];
        if !self.labels.is_empty() {
            let labels: Vec<String> = self
                .labels
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            vars.push(("HALLOW_LABELS", labels.join(" ")));
        }
        if let Some(wg) = &self.wireguard {
            vars.push(("HALLOW_WG_ADDRESS", wg.address.clone()));
            if let Some(port) = wg.listen_port {
                vars.push(("HALLOW_WG_PORT", port.to_string()));
            }
            let peers: Vec<String> = wg
                .peers
                .iter()
                .map(|p| {
                    format!(
                        "{}|{}|{}|{}",
                        p.public_key,
                        p.endpoint.as_deref().unwrap_or_default(),
                        p.allowed_ips.join(","),
                        p.persistent_keepalive
                            .map(|k| k.to_string())
                            .unwrap_or_default()
                    )
                })
                .collect();
            vars.push(("HALLOW_WG_PEERS", peers.join(" ")));
        }
        for (key, list) in [
            ("HALLOW_PRIVATE_IFS", &self.private_interfaces),
            ("HALLOW_SSH_ALLOW", &self.ssh_allow),
            ("HALLOW_WINGS_ALLOW", &self.wings_allow),
        ] {
            if !list.is_empty() {
                vars.push((key, list.join(" ")));
            }
        }
        if let Some((low, high)) = self.game_ports {
            vars.push(("HALLOW_GAME_PORTS", format!("{low}-{high}")));
        }
        if let Some(wings) = &self.wings {
            vars.push(("HALLOW_WINGS_VERSION", wings.version.clone()));
            if let Some(u) = &wings.url {
                vars.push(("HALLOW_WINGS_URL", u.clone()));
            }
            vars.push(("HALLOW_WINGS_SHA256", wings.sha256.clone()));
        }
        let mut out = String::new();
        for (key, value) in vars {
            let _ = writeln!(out, "{key}={value}");
        }
        Ok(out)
    }
}

/// `install.sh`, gzipped and base64-encoded for `encoding: gz+b64`. It is
/// about a third of its plain size, which keeps the user data well under
/// [`USER_DATA_LIMIT`].
fn packed_script() -> String {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz.write_all(INSTALL_SCRIPT.as_bytes())
        .expect("writing to a Vec can't fail");
    let bytes = gz.finish().expect("writing to a Vec can't fail");
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// YAML double-quoted string.
fn yaml_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The cloud-config for one node: `deploy/node/cloud-init.yaml` with its
/// settings, `install.sh`, and any SSH keys filled in.
pub fn render_cloud_init(bootstrap: &NodeBootstrap) -> Result<String, BootstrapError> {
    let env: String = bootstrap
        .env()?
        .lines()
        .map(|line| format!("      {line}\n"))
        .collect();
    let keys = if bootstrap.ssh_authorized_keys.is_empty() {
        String::new()
    } else {
        let mut keys = String::from("ssh_authorized_keys:\n");
        for key in &bootstrap.ssh_authorized_keys {
            let _ = writeln!(keys, "  - {}", yaml_quote(key.trim()));
        }
        keys
    };
    let rendered = CLOUD_INIT_TEMPLATE
        .replace("{{env}}\n", &env)
        .replace("{{install_script}}", &packed_script())
        .replace("{{ssh_authorized_keys}}\n", &keys);
    if rendered.len() > USER_DATA_LIMIT {
        return Err(BootstrapError::TooLarge(rendered.len()));
    }
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;

    const HUB_KEY: &str = "xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=";

    pub(crate) fn sample() -> NodeBootstrap {
        NodeBootstrap {
            control_plane_url: "https://cp.example.net".into(),
            join_token: "hjt_0123456789abcdef".into(),
            role: NodeKind::Burst,
            region: "fsn1".into(),
            labels: [("pool".to_owned(), "games".to_owned())].into(),
            wireguard: Some(WireGuard {
                address: "10.42.1.7/16".into(),
                listen_port: None,
                peers: vec![WireGuardPeer {
                    public_key: HUB_KEY.into(),
                    endpoint: Some("hub.example.net:51820".into()),
                    allowed_ips: vec!["10.42.0.0/16".into()],
                    persistent_keepalive: Some(25),
                }],
            }),
            private_interfaces: vec![],
            ssh_allow: vec!["203.0.113.0/24".into()],
            wings_allow: vec![],
            game_ports: None,
            wings: None,
            ssh_authorized_keys: vec![],
        }
    }

    fn parse(yaml: &str) -> serde_norway::Value {
        serde_norway::from_str(yaml).expect("rendered cloud-init is valid YAML")
    }

    fn file<'a>(doc: &'a serde_norway::Value, path: &str) -> &'a serde_norway::Value {
        doc["write_files"]
            .as_sequence()
            .unwrap()
            .iter()
            .find(|f| f["path"].as_str() == Some(path))
            .unwrap_or_else(|| panic!("no {path} in write_files"))
    }

    #[test]
    fn renders_a_cloud_config() {
        let yaml = render_cloud_init(&sample()).unwrap();
        assert!(yaml.starts_with("#cloud-config\n"));
        assert!(!yaml.contains("{{"), "a placeholder was left:\n{yaml}");
        let doc = parse(&yaml);
        let env = file(&doc, ENV_FILE);
        assert_eq!(env["permissions"].as_str(), Some("0600"));
        assert_eq!(
            env["content"].as_str().unwrap(),
            "HALLOW_CONTROL_PLANE_URL=https://cp.example.net\n\
             HALLOW_JOIN_TOKEN=hjt_0123456789abcdef\n\
             HALLOW_ROLE=burst\n\
             HALLOW_REGION=fsn1\n\
             HALLOW_LABELS=pool=games\n\
             HALLOW_WG_ADDRESS=10.42.1.7/16\n\
             HALLOW_WG_PEERS=xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=|hub.example.net:51820|10.42.0.0/16|25\n\
             HALLOW_SSH_ALLOW=203.0.113.0/24\n"
        );
        let runcmd = doc["runcmd"][0].as_sequence().unwrap();
        let runcmd: Vec<&str> = runcmd.iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(
            runcmd,
            [
                "bash",
                "/usr/local/sbin/hallow-node-install",
                "--env-file",
                ENV_FILE
            ]
        );
        assert!(doc.get("ssh_authorized_keys").is_none());
    }

    #[test]
    fn embeds_install_sh_byte_for_byte() {
        let doc = parse(&render_cloud_init(&sample()).unwrap());
        let script = file(&doc, "/usr/local/sbin/hallow-node-install");
        assert_eq!(script["encoding"].as_str(), Some("gz+b64"));
        assert_eq!(script["permissions"].as_str(), Some("0755"));
        let gz = base64::engine::general_purpose::STANDARD
            .decode(script["content"].as_str().unwrap())
            .unwrap();
        let mut plain = String::new();
        flate2::read::GzDecoder::new(&gz[..])
            .read_to_string(&mut plain)
            .unwrap();
        assert_eq!(plain, INSTALL_SCRIPT);
    }

    #[test]
    fn every_setting_reaches_the_env_file() {
        let mut b = sample();
        b.role = NodeKind::Base;
        b.wireguard.as_mut().unwrap().listen_port = Some(51900);
        b.wireguard.as_mut().unwrap().peers.push(WireGuardPeer {
            public_key: HUB_KEY.replace('x', "y"),
            endpoint: None,
            allowed_ips: vec!["10.42.0.2/32".into(), "10.43.0.0/16".into()],
            persistent_keepalive: None,
        });
        b.private_interfaces = vec!["ens10".into()];
        b.wings_allow = vec!["10.0.0.2/32".into(), "2001:db8::/32".into()];
        b.game_ports = Some((25600, 25700));
        b.wings = Some(WingsBuild {
            version: "1.3.0".into(),
            url: Some("https://example.net/wings/{version}/wings-{arch}".into()),
            sha256: "ab".repeat(32),
        });
        let env = b.env().unwrap();
        for line in [
            "HALLOW_ROLE=base",
            "HALLOW_WG_PORT=51900",
            "HALLOW_WG_PEERS=xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=|hub.example.net:51820|10.42.0.0/16|25 \
             yTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=||10.42.0.2/32,10.43.0.0/16|",
            "HALLOW_PRIVATE_IFS=ens10",
            "HALLOW_WINGS_ALLOW=10.0.0.2/32 2001:db8::/32",
            "HALLOW_GAME_PORTS=25600-25700",
            "HALLOW_WINGS_VERSION=1.3.0",
            "HALLOW_WINGS_URL=https://example.net/wings/{version}/wings-{arch}",
        ] {
            assert!(env.lines().any(|l| l == line), "no {line:?} in\n{env}");
        }
        assert!(
            env.lines()
                .any(|l| l == format!("HALLOW_WINGS_SHA256={}", "ab".repeat(32)))
        );
    }

    #[test]
    fn ssh_keys_go_to_the_default_user() {
        let mut b = sample();
        b.ssh_authorized_keys = vec!["ssh-ed25519 AAAAC3Nz staff@hallow".into()];
        let doc = parse(&render_cloud_init(&b).unwrap());
        assert_eq!(
            doc["ssh_authorized_keys"][0].as_str(),
            Some("ssh-ed25519 AAAAC3Nz staff@hallow")
        );
    }

    #[test]
    fn refuses_values_that_could_inject_settings() {
        let mut b = sample();
        b.region = "fsn1\nHALLOW_SSH_ALLOW=0.0.0.0/0".into();
        assert!(matches!(
            render_cloud_init(&b),
            Err(BootstrapError::Invalid {
                field: "region",
                ..
            })
        ));
        let mut b = sample();
        b.join_token = "hjt_x y".into();
        assert!(render_cloud_init(&b).is_err());
        let mut b = sample();
        b.labels.insert("pool".into(), "a b".into());
        assert!(render_cloud_init(&b).is_err());
        let mut b = sample();
        b.ssh_authorized_keys = vec!["ssh-ed25519 A\nruncmd: [reboot]".into()];
        assert!(render_cloud_init(&b).is_err());
    }

    #[test]
    fn refuses_what_install_sh_would() {
        type Break = Box<dyn Fn(&mut NodeBootstrap)>;
        let cases: Vec<(&str, Break)> = vec![
            (
                "control_plane_url",
                Box::new(|b| b.control_plane_url = "cp.example.net".into()),
            ),
            (
                "wireguard.address",
                Box::new(|b| b.wireguard.as_mut().unwrap().address = "10.42.1.7".into()),
            ),
            (
                "wireguard.peers",
                Box::new(|b| b.wireguard.as_mut().unwrap().peers.clear()),
            ),
            (
                "wireguard.peers.public_key",
                Box::new(|b| b.wireguard.as_mut().unwrap().peers[0].public_key = "short=".into()),
            ),
            (
                "wireguard.peers.allowed_ips",
                Box::new(|b| b.wireguard.as_mut().unwrap().peers[0].allowed_ips.clear()),
            ),
            (
                "game_ports",
                Box::new(|b| b.game_ports = Some((39000, 41000))),
            ),
            ("game_ports", Box::new(|b| b.game_ports = Some((80, 90)))),
            (
                "private_interfaces",
                Box::new(|b| b.private_interfaces = vec!["a-very-long-interface".into()]),
            ),
            ("ssh_allow", Box::new(|b| b.ssh_allow = vec!["any".into()])),
            (
                "wings.sha256",
                Box::new(|b| {
                    b.wings = Some(WingsBuild {
                        version: "1.3.0".into(),
                        url: None,
                        sha256: "ABC".into(),
                    })
                }),
            ),
        ];
        for (field, break_it) in cases {
            let mut b = sample();
            break_it(&mut b);
            match b.validate() {
                Err(BootstrapError::Invalid { field: f, .. }) => assert_eq!(f, field),
                other => panic!("{field}: {other:?}"),
            }
        }
    }

    #[test]
    fn fits_the_smallest_user_data_limit() {
        let mut b = sample();
        b.ssh_authorized_keys = vec!["ssh-ed25519 AAAA staff".into(); 8];
        let yaml = render_cloud_init(&b).unwrap();
        assert!(
            yaml.len() < USER_DATA_LIMIT / 2,
            "user data is {} bytes; keep install.sh small",
            yaml.len()
        );
    }
}
