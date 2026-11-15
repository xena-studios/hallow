//! Change notifications between control plane instances.
//!
//! Every write that a long poll could be waiting for publishes a topic on one
//! Redis channel: [`SERVERS`] or [`commands`]`(proxy)`. Each instance
//! subscribes and rebroadcasts topics in-process, where the long-poll handlers
//! wait for them. A topic only says "look again"; the data is always read
//! from Redis, so a missed or duplicate event costs at most one extra read.

use std::{sync::Arc, time::Duration};

use futures_util::StreamExt;
use tokio::sync::broadcast;

/// The server list changed.
pub const SERVERS: &str = "servers";
/// Sent after the subscription (re)connects, when events may have been
/// missed: every waiter looks again.
pub const ALL: &str = "*";

/// A command was queued for `proxy`.
#[must_use]
pub fn commands(proxy: &str) -> String {
    format!("commands:{proxy}")
}

#[derive(Clone)]
pub struct Events {
    tx: broadcast::Sender<Arc<str>>,
}

impl Events {
    /// Subscribes to `channel` in the background, reconnecting with backoff.
    #[must_use]
    pub fn listen(client: redis::Client, channel: String) -> Self {
        let (tx, _) = broadcast::channel(1024);
        let events = Self { tx };
        let sender = events.tx.clone();
        tokio::spawn(async move {
            let mut backoff = Duration::from_millis(100);
            loop {
                match subscribe(&client, &channel, &sender).await {
                    Ok(()) => tracing::warn!("event subscription ended; reconnecting"),
                    Err(err) => tracing::warn!(%err, "event subscription failed; reconnecting"),
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        });
        events
    }

    /// Starts listening before the caller reads state, so nothing published
    /// after that read is missed.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<str>> {
        self.tx.subscribe()
    }
}

async fn subscribe(
    client: &redis::Client,
    channel: &str,
    sender: &broadcast::Sender<Arc<str>>,
) -> redis::RedisResult<()> {
    let mut pubsub = client.get_async_pubsub().await?;
    pubsub.subscribe(channel).await?;
    tracing::info!(channel, "listening for change events");
    // Anything published while we were disconnected was lost.
    let _ = sender.send(ALL.into());
    let mut messages = pubsub.on_message();
    while let Some(message) = messages.next().await {
        let topic: String = message.get_payload()?;
        // No receivers just means no long polls are open.
        let _ = sender.send(topic.into());
    }
    Ok(())
}

/// Whether `topic` should wake a waiter for `wanted`.
#[must_use]
pub fn matches(topic: &str, wanted: &str) -> bool {
    topic == wanted || topic == ALL
}
