//! HAProxy agent checks, so the load balancer only sends new players to
//! proxies that take them.
//!
//! HAProxy connects, sends the proxy id configured with `agent-send` (one
//! line), and reads one line back:
//!
//! - `up ready`: registered, heartbeating and not draining.
//! - `drain`: draining. HAProxy sets the server's weight to 0, so it gets no
//!   new connections while the players on it stay.
//! - `down`: unknown or expired, or the control plane can't tell.
//!
//! The port speaks no secrets and changes nothing, so it carries no token.
//! Keep it on the private network.

use std::{sync::Arc, time::Duration};

use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};

use crate::{AppState, store::AgentState};

/// Ids longer than this aren't ids.
const MAX_LINE: usize = 128;

pub async fn serve(listener: TcpListener, state: Arc<AppState>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(err) = answer(stream, &state).await {
                        tracing::debug!(%err, "agent check failed");
                    }
                });
            }
            Err(err) => {
                tracing::warn!(%err, "agent listener accept failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn answer(stream: TcpStream, state: &AppState) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    let read = tokio::time::timeout(
        Duration::from_secs(2),
        BufReader::new(read)
            .take(MAX_LINE as u64)
            .read_line(&mut line),
    )
    .await;
    let reply = match read {
        Ok(Ok(_)) => reply_for(state, line.trim()).await,
        _ => "down",
    };
    write.write_all(reply.as_bytes()).await?;
    write.write_all(b"\n").await?;
    write.shutdown().await
}

async fn reply_for(state: &AppState, id: &str) -> &'static str {
    if id.is_empty() {
        return "down";
    }
    match state.store.agent_state(id).await {
        Ok(AgentState::Ready) => "up ready",
        Ok(AgentState::Drain) => "drain",
        Ok(AgentState::Down) => "down",
        Err(err) => {
            tracing::warn!(%err, proxy = id, "agent check couldn't read state");
            "down"
        }
    }
}
