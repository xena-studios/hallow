//! Renders cloud-init for a node, takes the env file and the script back out
//! of it exactly as cloud-init would write them, and runs the script with
//! `--dry-run` against a fake Debian 12 `/etc/os-release`. Checks that the
//! settings the fleet manager chose are the ones the machine acts on.
//!
//! Needs bash 4+ and jq; skips without them (except in CI).

use std::io::Read as _;
use std::process::Command;

use base64::Engine as _;
use hallow_fleet::{NodeBootstrap, NodeKind, WireGuard, WireGuardPeer, render_cloud_init};

fn tools_present() -> bool {
    let bash4 = Command::new("bash")
        .args(["-c", "((BASH_VERSINFO[0] >= 4))"])
        .status()
        .is_ok_and(|s| s.success());
    let jq = Command::new("jq")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !(bash4 && jq) {
        assert!(
            std::env::var_os("CI").is_none(),
            "bash 4+ and jq are needed in CI"
        );
        eprintln!("bash 4+ or jq missing; skipping");
    }
    bash4 && jq
}

fn bootstrap() -> NodeBootstrap {
    NodeBootstrap {
        control_plane_url: "https://cp.example.net".into(),
        join_token: "hjt_secret_never_printed".into(),
        role: NodeKind::Base,
        region: "nbg1".into(),
        labels: [("pool".to_owned(), "games".to_owned())].into(),
        wireguard: Some(WireGuard {
            address: "10.42.3.4/16".into(),
            listen_port: Some(51821),
            peers: vec![WireGuardPeer {
                public_key: "xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=".into(),
                endpoint: Some("hub.example.net:51820".into()),
                allowed_ips: vec!["10.42.0.0/16".into()],
                persistent_keepalive: Some(25),
            }],
        }),
        private_interfaces: vec!["ens10".into()],
        ssh_allow: vec!["203.0.113.0/24".into()],
        wings_allow: vec![],
        game_ports: Some((31000, 31099)),
        wings: None,
        ssh_authorized_keys: vec![],
    }
}

/// The `content` of one `write_files` entry, decoded as cloud-init would.
fn written(doc: &serde_norway::Value, path: &str) -> String {
    let entry = doc["write_files"]
        .as_sequence()
        .unwrap()
        .iter()
        .find(|f| f["path"].as_str() == Some(path))
        .unwrap();
    let content = entry["content"].as_str().unwrap();
    match entry["encoding"].as_str() {
        None => content.to_owned(),
        Some("gz+b64") => {
            let gz = base64::engine::general_purpose::STANDARD
                .decode(content)
                .unwrap();
            let mut out = String::new();
            flate2::read::GzDecoder::new(&gz[..])
                .read_to_string(&mut out)
                .unwrap();
            out
        }
        Some(other) => panic!("unexpected encoding {other}"),
    }
}

#[test]
fn the_rendered_settings_drive_install_sh() {
    if !tools_present() {
        return;
    }
    let yaml = render_cloud_init(&bootstrap()).unwrap();
    let doc: serde_norway::Value = serde_norway::from_str(&yaml).unwrap();

    let dir = std::env::temp_dir().join(format!("hallow-bootstrap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let env = dir.join("node.env");
    let script = dir.join("install.sh");
    let os_release = dir.join("os-release");
    std::fs::write(&env, written(&doc, "/etc/hallow/node.env")).unwrap();
    std::fs::write(
        &script,
        written(&doc, "/usr/local/sbin/hallow-node-install"),
    )
    .unwrap();
    std::fs::write(
        &os_release,
        "ID=debian\nVERSION_ID=\"12\"\nVERSION_CODENAME=bookworm\n",
    )
    .unwrap();

    let output = Command::new("bash")
        .arg(&script)
        .args(["--dry-run", "--env-file"])
        .arg(&env)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HALLOW_OS_RELEASE", &os_release)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    std::fs::remove_dir_all(&dir).ok();
    assert!(output.status.success(), "install.sh failed:\n{out}");

    for want in [
        "role base, region nbg1, register by join",
        "Address = 10.42.3.4/16",
        "ListenPort = 51821",
        "Endpoint = hub.example.net:51820",
        "udp dport 51821 accept",
        "ip saddr { 203.0.113.0/24 } tcp dport 22 accept",
        r#"iifname { "wg0", "ens10" } tcp dport { 8080, 2022 } accept"#,
        "th dport 31000-31099 drop",
        "POST https://cp.example.net/v1/nodes/join",
        r#""private_ip": "10.42.3.4""#,
        r#""pool": "games""#,
        "systemctl restart wings",
    ] {
        assert!(out.contains(want), "no {want:?} in:\n{out}");
    }
    assert!(
        !out.contains("hjt_secret_never_printed"),
        "the join token was printed:\n{out}"
    );
}
