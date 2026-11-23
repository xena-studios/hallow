//! `GET /metrics` in Prometheus' text format, read from live state at each
//! scrape. Unauthenticated like `/healthz`: it holds counts and server names,
//! nothing secret, but keep it on the private network.

use std::fmt::Write as _;

use hallow_proto::server::{ServerRole, ServerState};

use crate::{AppState, store};

/// Escapes a label value: backslash, quote and newline.
fn label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn role(role: ServerRole) -> &'static str {
    match role {
        ServerRole::Lobby => "lobby",
        ServerRole::Game => "game",
    }
}

fn state(state: ServerState) -> &'static str {
    match state {
        ServerState::Ready => "ready",
        ServerState::InGame => "in_game",
        ServerState::Draining => "draining",
    }
}

/// Starts a metric family with its help and type lines.
fn family(out: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
}

pub async fn render(app: &AppState) -> store::Result<String> {
    let mut out = String::new();
    let online = app.store.online().await?;
    family(
        &mut out,
        "hallow_players_online",
        "gauge",
        "Players online on the network.",
    );
    let _ = writeln!(out, "hallow_players_online {online}");
    family(
        &mut out,
        "hallow_players_max",
        "gauge",
        "The network's player limit.",
    );
    let _ = writeln!(out, "hallow_players_max {}", app.config.max_players);

    let proxies = app.store.proxies().await?;
    family(
        &mut out,
        "hallow_proxy_players",
        "gauge",
        "Players on each proxy, from its heartbeat.",
    );
    for proxy in &proxies {
        let _ = writeln!(
            out,
            "hallow_proxy_players{{proxy=\"{}\",draining=\"{}\"}} {}",
            label(&proxy.id),
            proxy.draining,
            proxy.players
        );
    }

    let (_, servers) = app.store.read_servers().await?;
    family(
        &mut out,
        "hallow_server_players",
        "gauge",
        "Players on each server, from its heartbeat.",
    );
    for s in &servers {
        let _ = writeln!(
            out,
            "hallow_server_players{{server=\"{}\",kind=\"{}\",role=\"{}\",state=\"{}\"}} {}",
            label(&s.name),
            label(&s.kind),
            role(s.role),
            state(s.state),
            s.players
        );
    }
    family(
        &mut out,
        "hallow_server_max_players",
        "gauge",
        "Each server's player limit.",
    );
    for s in &servers {
        let _ = writeln!(
            out,
            "hallow_server_max_players{{server=\"{}\",kind=\"{}\"}} {}",
            label(&s.name),
            label(&s.kind),
            s.max_players
        );
    }
    family(
        &mut out,
        "hallow_server_tick_ms",
        "gauge",
        "Average milliseconds per tick on each server that reports it (50 is the 20 TPS budget).",
    );
    for s in servers.iter().filter(|s| s.tick_ms.is_some()) {
        let _ = writeln!(
            out,
            "hallow_server_tick_ms{{server=\"{}\",kind=\"{}\"}} {}",
            label(&s.name),
            label(&s.kind),
            s.tick_ms.unwrap_or_default()
        );
    }

    family(
        &mut out,
        "hallow_queue_players",
        "gauge",
        "Players waiting in each game's queue.",
    );
    for (kind, players) in app.store.queue_sizes().await? {
        let _ = writeln!(
            out,
            "hallow_queue_players{{kind=\"{}\"}} {players}",
            label(&kind)
        );
    }
    if app.config.fleet.is_some() {
        crate::fleet_manager::metrics(&app.store, &mut out).await?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::label;

    #[test]
    fn labels_are_escaped() {
        assert_eq!(label("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }
}
