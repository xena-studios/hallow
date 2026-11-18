//! Who waits in limbo and who is banned while online (docs/ARCHITECTURE.md,
//! "Never dropped").
//!
//! | Key (after the prefix) | Type | Holds |
//! |---|---|---|
//! | `limbo` | sorted set | player UUID scored by when they started waiting (Unix ms): everyone the network still has to place, oldest first |
//! | `limbo-sent` | hash | player UUID → when the control plane last sent them on (Unix ms), so a move isn't repeated every pass |
//! | `limbo-told` | hash | player UUID → when they were last told their place (Unix ms) |
//! | `limbo-hold` | hash | player UUID → until when a kicked player stays in limbo (Unix ms) |
//! | `restricted` | hash | player UUID → [`BanInfo`] JSON of a banned player who is online: they can only be moved by the control plane, to a banned lobby or limbo |
//!
//! A player leaving the network leaves all of them.

use hallow_proto::{command::ProxyCommand, punishment::BanInfo};
use redis::AsyncCommands;
use uuid::Uuid;

use super::{Result, Store};

impl super::Keys {
    pub(super) fn limbo(&self) -> String {
        format!("{}:limbo", self.prefix)
    }
    pub(super) fn limbo_sent(&self) -> String {
        format!("{}:limbo-sent", self.prefix)
    }
    pub(super) fn limbo_told(&self) -> String {
        format!("{}:limbo-told", self.prefix)
    }
    pub(super) fn limbo_hold(&self) -> String {
        format!("{}:limbo-hold", self.prefix)
    }
    pub(super) fn restricted(&self) -> String {
        format!("{}:restricted", self.prefix)
    }
}

/// One player waiting in limbo, as [`Store::waiting`] lists them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Waiting {
    pub uuid: Uuid,
    /// When they started waiting.
    pub since_ms: u64,
    /// When they were last sent on, if ever.
    pub sent_ms: Option<u64>,
    /// When they were last told their place, if ever.
    pub told_ms: Option<u64>,
    /// A kicked player waits until then.
    pub hold_until_ms: Option<u64>,
}

impl Store {
    /// Starts `player` waiting, unless they already are (their place is kept).
    pub async fn start_waiting(&self, player: &Uuid, now_ms: u64) -> Result<()> {
        let _: () = redis::cmd("ZADD")
            .arg(self.keys.limbo())
            .arg("NX")
            .arg(now_ms)
            .arg(player.to_string())
            .query_async(&mut self.conn())
            .await?;
        Ok(())
    }

    /// The player was placed somewhere (or left): they no longer wait.
    pub async fn stop_waiting(&self, player: &Uuid) -> Result<()> {
        let uuid = player.to_string();
        let _: () = redis::pipe()
            .atomic()
            .zrem(self.keys.limbo(), &uuid)
            .ignore()
            .hdel(self.keys.limbo_sent(), &uuid)
            .ignore()
            .hdel(self.keys.limbo_told(), &uuid)
            .ignore()
            .hdel(self.keys.limbo_hold(), &uuid)
            .ignore()
            .query_async(&mut self.conn())
            .await?;
        Ok(())
    }

    /// Their place among the waiting, from 1.
    pub async fn waiting_position(&self, player: &Uuid) -> Result<Option<u32>> {
        let rank: Option<u64> = self
            .conn()
            .zrank(self.keys.limbo(), player.to_string())
            .await?;
        Ok(rank.map(|r| u32::try_from(r + 1).unwrap_or(u32::MAX)))
    }

    pub async fn waiting_count(&self) -> Result<u32> {
        Ok(self.conn().zcard(self.keys.limbo()).await?)
    }

    /// Everyone waiting, oldest first.
    pub async fn waiting(&self) -> Result<Vec<Waiting>> {
        type Pairs = Vec<(String, u64)>;
        let (entries, sent, told, hold): (Pairs, Pairs, Pairs, Pairs) = redis::pipe()
            .atomic()
            .zrange_withscores(self.keys.limbo(), 0, -1)
            .hgetall(self.keys.limbo_sent())
            .hgetall(self.keys.limbo_told())
            .hgetall(self.keys.limbo_hold())
            .query_async(&mut self.conn())
            .await?;
        let find = |list: &[(String, u64)], uuid: &str| {
            list.iter().find(|(u, _)| u == uuid).map(|(_, at)| *at)
        };
        Ok(entries
            .iter()
            .filter_map(|(uuid, since)| {
                Some(Waiting {
                    uuid: Uuid::parse_str(uuid).ok()?,
                    since_ms: *since,
                    sent_ms: find(&sent, uuid),
                    told_ms: find(&told, uuid),
                    hold_until_ms: find(&hold, uuid),
                })
            })
            .collect())
    }

    pub async fn mark_sent(&self, player: &Uuid, now_ms: u64) -> Result<()> {
        let _: () = self
            .conn()
            .hset(self.keys.limbo_sent(), player.to_string(), now_ms)
            .await?;
        Ok(())
    }

    pub async fn mark_told(&self, player: &Uuid, now_ms: u64) -> Result<()> {
        let _: () = self
            .conn()
            .hset(self.keys.limbo_told(), player.to_string(), now_ms)
            .await?;
        Ok(())
    }

    /// Keeps a kicked player in limbo until `until_ms`.
    pub async fn hold(&self, player: &Uuid, until_ms: u64) -> Result<()> {
        let _: () = self
            .conn()
            .hset(self.keys.limbo_hold(), player.to_string(), until_ms)
            .await?;
        Ok(())
    }

    /// Records an online player's ban: from now on only the control plane
    /// moves them.
    pub async fn restrict(&self, player: &Uuid, ban: &BanInfo) -> Result<()> {
        let _: () = self
            .conn()
            .hset(
                self.keys.restricted(),
                player.to_string(),
                serde_json::to_string(ban)?,
            )
            .await?;
        Ok(())
    }

    /// Lifts it; returns the ban it lifted, if there was one.
    pub async fn unrestrict(&self, player: &Uuid) -> Result<Option<BanInfo>> {
        let uuid = player.to_string();
        let (raw, _): (Option<String>, i64) = redis::pipe()
            .atomic()
            .hget(self.keys.restricted(), &uuid)
            .hdel(self.keys.restricted(), &uuid)
            .query_async(&mut self.conn())
            .await?;
        Ok(raw.map(|raw| serde_json::from_str(&raw)).transpose()?)
    }

    pub async fn restriction(&self, player: &Uuid) -> Result<Option<BanInfo>> {
        let raw: Option<String> = self
            .conn()
            .hget(self.keys.restricted(), player.to_string())
            .await?;
        Ok(raw.map(|raw| serde_json::from_str(&raw)).transpose()?)
    }

    /// Every online banned player.
    pub async fn restricted(&self) -> Result<Vec<(Uuid, BanInfo)>> {
        let raw: Vec<(String, String)> = self.conn().hgetall(self.keys.restricted()).await?;
        let mut out = Vec::with_capacity(raw.len());
        for (uuid, ban) in raw {
            if let Ok(uuid) = Uuid::parse_str(&uuid) {
                out.push((uuid, serde_json::from_str(&ban)?));
            }
        }
        Ok(out)
    }

    /// Forgets everything here about a player who left the network.
    pub(super) async fn limbo_on_leave(&self, uuid: &str) -> Result<()> {
        let _: () = redis::pipe()
            .atomic()
            .zrem(self.keys.limbo(), uuid)
            .ignore()
            .hdel(self.keys.limbo_sent(), uuid)
            .ignore()
            .hdel(self.keys.limbo_told(), uuid)
            .ignore()
            .hdel(self.keys.limbo_hold(), uuid)
            .ignore()
            .hdel(self.keys.restricted(), uuid)
            .ignore()
            .query_async(&mut self.conn())
            .await?;
        Ok(())
    }

    /// Queues `command` for the proxy holding `player`. False if they aren't
    /// online.
    pub async fn tell_player(&self, player: &Uuid, command: &ProxyCommand) -> Result<bool> {
        let Some(location) = self.player(player).await? else {
            return Ok(false);
        };
        self.push_command(&location.proxy, command).await?;
        Ok(true)
    }
}
