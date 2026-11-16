//! Node bootstrap: one-time join tokens, and the join a new machine makes
//! with one (`hallow_proto::node`, docs/FLEET.md).
//!
//! A join claims its token for [`CLAIM_LEASE`], creates the node, its
//! game-port allocations and a Wings enrollment code in the panel, then
//! records the node and uses the token up in one transaction. If anything
//! fails the panel node is deleted again and the claim given back, so the
//! machine can retry with the same token; only a finished join spends it.

use std::{collections::BTreeMap, net::IpAddr, sync::Arc, time::Duration};

use chrono::{DateTime, TimeDelta, Utc};
use hallow_db::{FinishJoinError, NewJoinToken, NewNode, PgPool};
use hallow_proto::{
    ErrorCode,
    node::{IssueJoinToken, JoinReply, JoinRequest, JoinToken, NodeInfo, NodeRole},
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    AppState,
    error::Failure,
    panel::{NewPanelNode, Panel, PanelConfig, PanelError},
};

/// Every join token starts with this.
pub const TOKEN_PREFIX: &str = "hjt_";
/// How long a join holds its token while it talks to the panel.
pub const CLAIM_LEASE: TimeDelta = TimeDelta::seconds(120);
/// The longest a token may live.
pub const MAX_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
/// Expired tokens are kept this long (for the audit trail), then deleted.
const KEEP_EXPIRED: TimeDelta = TimeDelta::days(7);

type Reply<T> = Result<T, Failure>;

fn millis(at: DateTime<Utc>) -> u64 {
    u64::try_from(at.timestamp_millis()).unwrap_or(0)
}

fn role_from(text: &str) -> NodeRole {
    if text == "base" {
        NodeRole::Base
    } else {
        NodeRole::Burst
    }
}

/// A new random token: the prefix and 32 random bytes in hex.
fn new_token() -> Reply<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|err| {
        tracing::error!(%err, "no randomness for a join token");
        Failure::new(ErrorCode::Unavailable, "couldn't make a token; retry")
    })?;
    let mut token = String::with_capacity(TOKEN_PREFIX.len() + 64);
    token.push_str(TOKEN_PREFIX);
    for byte in bytes {
        token.push_str(&format!("{byte:02x}"));
    }
    Ok(token)
}

fn hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// 1 to 64 of `A-Z a-z 0-9 . _ -`: node names, regions.
fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn check_labels(labels: &BTreeMap<String, String>) -> Reply<()> {
    if labels.len() > 32 {
        return Err(Failure::bad_request("at most 32 labels"));
    }
    for (key, value) in labels {
        let key_ok = !key.is_empty()
            && key.len() <= 63
            && key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'));
        let value_ok = value.len() <= 63
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if !key_ok || !value_ok {
            return Err(Failure::bad_request(format!(
                "bad label {key:?}={value:?}: up to 63 letters, digits, '.', '_', '-' (and '/' in keys)"
            )));
        }
    }
    Ok(())
}

fn need_db(state: &AppState) -> Reply<&PgPool> {
    state.db.as_ref().ok_or_else(|| {
        Failure::new(
            ErrorCode::Unavailable,
            "node joins need the database (HALLOW_DATABASE_URL)",
        )
    })
}

fn need_panel(state: &AppState) -> Reply<(&PanelConfig, &dyn Panel)> {
    match (&state.config.panel, &state.panel) {
        (Some(config), Some(panel)) => Ok((config, panel.as_ref())),
        _ => Err(Failure::new(
            ErrorCode::Unavailable,
            "node joins need the panel (HALLOW_PANEL_URL, HALLOW_PANEL_API_KEY)",
        )),
    }
}

/// `POST /v1/nodes/join-tokens`.
pub async fn issue(state: &AppState, req: IssueJoinToken) -> Reply<JoinToken> {
    let db = need_db(state)?;
    let (panel, _) = need_panel(state)?;
    if !is_name(&req.region) {
        return Err(Failure::bad_request(
            "region must be 1 to 64 letters, digits, '.', '_' or '-'",
        ));
    }
    if panel.location(&req.region).is_none() {
        return Err(Failure::bad_request(format!(
            "no panel location for region {} (HALLOW_PANEL_LOCATIONS)",
            req.region
        )));
    }
    if let Some(name) = &req.name {
        if !is_name(name) {
            return Err(Failure::bad_request(
                "name must be 1 to 64 letters, digits, '.', '_' or '-'",
            ));
        }
        if hallow_db::node_exists(db, name).await? {
            return Err(Failure::new(
                ErrorCode::Conflict,
                format!("a node named {name} already joined"),
            ));
        }
    }
    check_labels(&req.labels)?;
    let ttl = req
        .ttl_seconds
        .map_or(state.config.join_token_ttl, Duration::from_secs);
    if ttl.is_zero() || ttl > MAX_TTL {
        return Err(Failure::bad_request("ttl_seconds must be 1 to 604800"));
    }
    let token = new_token()?;
    let expires_at = Utc::now() + TimeDelta::from_std(ttl).unwrap_or(TimeDelta::days(1));
    let stored = hallow_db::add_join_token(
        db,
        &NewJoinToken {
            id: Uuid::new_v4(),
            token_hash: &hash(&token),
            role: req.role.as_str(),
            region: &req.region,
            name: req.name.as_deref(),
            labels: &req.labels,
            expires_at,
        },
    )
    .await?;
    tracing::info!(token = %stored.id, role = req.role.as_str(), region = %req.region,
        expires = %expires_at, "join token issued");
    Ok(JoinToken {
        id: stored.id,
        token,
        role: req.role,
        region: stored.region,
        name: stored.name,
        labels: stored.labels.0,
        expires_at_ms: millis(stored.expires_at),
    })
}

/// A machine's request, checked.
struct Checked {
    name: String,
    private_ip: IpAddr,
    public_ip: Option<IpAddr>,
    wireguard_public_key: Option<String>,
}

fn check_join(req: &JoinRequest, name: Option<&str>) -> Reply<Checked> {
    let private_ip: IpAddr = req
        .private_ip
        .parse()
        .map_err(|_| Failure::bad_request("private_ip must be an IP address"))?;
    let public_ip = match req.public_ip.as_deref() {
        None | Some("") => None,
        Some(ip) => Some(
            ip.parse()
                .map_err(|_| Failure::bad_request("public_ip must be an IP address"))?,
        ),
    };
    if let Some(key) = &req.wireguard_public_key {
        let ok = key.len() == 44
            && key.ends_with('=')
            && key[..43]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/');
        if !ok {
            return Err(Failure::bad_request(
                "wireguard_public_key isn't a WireGuard key",
            ));
        }
    }
    let name = match name {
        Some(name) => name.to_owned(),
        None => {
            // The hostname, cut to what node names allow.
            let host: String = req
                .hostname
                .split('.')
                .next()
                .unwrap_or_default()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
                .take(64)
                .collect();
            if host.is_empty() {
                return Err(Failure::bad_request("hostname gives no usable node name"));
            }
            host
        }
    };
    Ok(Checked {
        name,
        private_ip,
        public_ip,
        wireguard_public_key: req.wireguard_public_key.clone(),
    })
}

impl From<PanelError> for Failure {
    fn from(err: PanelError) -> Self {
        tracing::warn!(%err, "panel call failed");
        match err {
            PanelError::Conflict(m) => Self::new(ErrorCode::Conflict, format!("panel: {m}")),
            // A key without permission or a config mistake: the machine
            // can't fix it, but staff can, so let it retry.
            PanelError::Rejected { .. } | PanelError::Unavailable(_) => Self::new(
                ErrorCode::Unavailable,
                "the panel couldn't set the node up; retry",
            ),
        }
    }
}

/// `POST /v1/nodes/join`, authorized by the join token in `bearer`.
pub async fn join(state: &AppState, bearer: &str, req: JoinRequest) -> Reply<JoinReply> {
    let db = need_db(state)?;
    let (panel_config, panel) = need_panel(state)?;
    let refused = || {
        Failure::new(
            ErrorCode::Unauthorized,
            "the join token is unknown, expired, already used or in use",
        )
    };
    if !bearer.starts_with(TOKEN_PREFIX) || bearer.len() != TOKEN_PREFIX.len() + 64 {
        return Err(refused());
    }
    let Some(token) =
        hallow_db::claim_join_token(db, &hash(bearer), Utc::now(), CLAIM_LEASE).await?
    else {
        return Err(refused());
    };
    let result = join_claimed(db, panel_config, panel, &token, &req).await;
    if result.is_err()
        && let Err(err) = hallow_db::release_join_token(db, token.id).await
    {
        tracing::warn!(%err, token = %token.id, "couldn't give the join token back");
    }
    result
}

async fn join_claimed(
    db: &PgPool,
    config: &PanelConfig,
    panel: &dyn Panel,
    token: &hallow_db::JoinToken,
    req: &JoinRequest,
) -> Reply<JoinReply> {
    let checked = check_join(req, token.name.as_deref())?;
    if hallow_db::node_exists(db, &checked.name).await? {
        return Err(Failure::new(
            ErrorCode::Conflict,
            format!("a node named {} already joined", checked.name),
        ));
    }
    let role = role_from(&token.role);
    let location = config.location(&token.region).ok_or_else(|| {
        Failure::new(
            ErrorCode::Unavailable,
            format!("no panel location for region {}", token.region),
        )
    })?;
    let labels: Vec<String> = token
        .labels
        .0
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    let mut description = format!(
        "Hallow {} node in {} ({}, {})",
        role.as_str(),
        token.region,
        req.hostname.chars().take(64).collect::<String>(),
        req.arch.chars().take(16).collect::<String>()
    );
    if !labels.is_empty() {
        description.push_str("; labels ");
        description.push_str(&labels.join(" "));
    }
    let host = match checked.private_ip {
        IpAddr::V6(ip) => format!("[{ip}]"),
        IpAddr::V4(ip) => ip.to_string(),
    };
    let new_node = NewPanelNode {
        location_uuid: location,
        backup_configuration_uuid: None,
        name: checked.name.clone(),
        description: Some(description.chars().take(1024).collect()),
        deployment_enabled: true,
        maintenance_enabled: false,
        public_url: None,
        url: format!("http://{host}:{}", config.wings_port),
        sftp_host: Some(checked.private_ip.to_string()),
        sftp_port: config.sftp_port,
        memory: i64::try_from(req.memory_mb).unwrap_or(i64::MAX),
        disk: i64::try_from(req.disk_mb).unwrap_or(i64::MAX),
    };
    let panel_uuid = panel.create_node(&new_node).await?;
    // From here on, a failure deletes the panel node again.
    let finished = async {
        let ports: Vec<u16> = (config.game_ports.0..=config.game_ports.1).collect();
        panel
            .add_allocations(panel_uuid, checked.private_ip, &ports)
            .await?;
        let enrollment = panel
            .create_enrollment(
                panel_uuid,
                Some(config.wings_remote.as_deref().unwrap_or(&config.public_url)),
            )
            .await?;
        let node = hallow_db::finish_join(
            db,
            token.id,
            &NewNode {
                name: &checked.name,
                panel_uuid,
                role: &token.role,
                region: &token.region,
                labels: &token.labels.0,
                private_ip: checked.private_ip,
                public_ip: checked.public_ip,
                wireguard_public_key: checked.wireguard_public_key.as_deref(),
            },
            Utc::now(),
        )
        .await
        .map_err(|err| match err {
            FinishJoinError::LostClaim => Failure::new(
                ErrorCode::Unavailable,
                "the join took too long and lost its token; retry",
            ),
            FinishJoinError::Duplicate => Failure::new(
                ErrorCode::Conflict,
                "a node with this name or WireGuard key already joined",
            ),
            FinishJoinError::Database(err) => err.into(),
        })?;
        Ok::<_, Failure>((node, enrollment))
    }
    .await;
    match finished {
        Ok((node, enrollment)) => {
            tracing::info!(node = %node.name, panel_uuid = %panel_uuid, role = %node.role,
                region = %node.region, private_ip = %node.private_ip, "node joined");
            Ok(JoinReply {
                node: panel_uuid,
                name: node.name,
                role,
                region: node.region,
                panel_url: config.public_url.clone(),
                enroll_code: enrollment.code,
                enroll_expires_at_ms: millis(enrollment.expires),
            })
        }
        Err(failure) => {
            if let Err(err) = panel.delete_node(panel_uuid).await {
                tracing::error!(%err, %panel_uuid, "couldn't delete the half-joined node from the panel");
            }
            Err(failure)
        }
    }
}

/// `GET /v1/nodes`.
pub async fn list(state: &AppState) -> Reply<Vec<NodeInfo>> {
    let db = need_db(state)?;
    Ok(hallow_db::list_nodes(db)
        .await?
        .into_iter()
        .map(|n| NodeInfo {
            role: role_from(&n.role),
            name: n.name,
            panel_uuid: n.panel_uuid,
            region: n.region,
            labels: n.labels.0,
            private_ip: n.private_ip.to_string(),
            public_ip: n.public_ip.map(|ip| ip.to_string()),
            wireguard_public_key: n.wireguard_public_key,
            joined_at_ms: millis(n.joined_at),
        })
        .collect())
}

/// Deletes long-expired tokens, hourly. Every instance runs it; deleting
/// twice is harmless.
pub async fn retention(state: Arc<AppState>) {
    let Some(pool) = state.db.clone() else { return };
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    loop {
        tick.tick().await;
        match hallow_db::delete_join_tokens_before(&pool, Utc::now() - KEEP_EXPIRED).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(deleted = n, "expired join tokens deleted"),
            Err(err) => tracing::warn!(%err, "join token cleanup failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_long_random_and_prefixed() {
        let (a, b) = (new_token().unwrap(), new_token().unwrap());
        assert_ne!(a, b);
        assert!(a.starts_with(TOKEN_PREFIX));
        assert_eq!(a.len(), TOKEN_PREFIX.len() + 64);
        assert!(a[4..].bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(hash(&a), hash(&b));
    }

    fn request(hostname: &str) -> JoinRequest {
        JoinRequest {
            hostname: hostname.into(),
            private_ip: "10.42.1.7".into(),
            public_ip: Some("203.0.113.9".into()),
            wireguard_public_key: Some("xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=".into()),
            arch: "x86_64".into(),
            memory_mb: 16000,
            disk_mb: 300_000,
        }
    }

    #[test]
    fn node_name_comes_from_the_token_or_the_hostname() {
        assert_eq!(
            check_join(&request("x"), Some("burst-7")).unwrap().name,
            "burst-7"
        );
        assert_eq!(
            check_join(&request("hallow-burst-3.fsn1.example.net"), None)
                .unwrap()
                .name,
            "hallow-burst-3"
        );
        assert!(check_join(&request("..."), None).is_err());
    }

    #[test]
    fn join_requests_are_checked() {
        let mut bad = request("n");
        bad.private_ip = "not-an-ip".into();
        assert!(check_join(&bad, None).is_err());
        let mut bad = request("n");
        bad.wireguard_public_key = Some("short".into());
        assert!(check_join(&bad, None).is_err());
        let mut ok = request("n");
        ok.public_ip = Some(String::new());
        assert_eq!(check_join(&ok, None).unwrap().public_ip, None);
    }

    #[test]
    fn labels_are_checked() {
        assert!(check_labels(&[("pool".into(), "games".into())].into()).is_ok());
        assert!(check_labels(&[("pool".into(), "a b".into())].into()).is_err());
        assert!(check_labels(&[(String::new(), "x".into())].into()).is_err());
    }
}
