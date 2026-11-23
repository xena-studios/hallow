//! Network-wide counters for `/metrics`, kept in Redis so every control
//! plane instance serves the same totals.
//!
//! | Key (after the prefix) | Type | Holds |
//! |---|---|---|
//! | `metrics:games-started` | hash | kind → games started (the matchmaker's first players onto an empty ready game server) |
//! | `metrics:queue-wait` | hash | `{kind}\|{bucket}` → entries placed after waiting at most that bucket's seconds and more than the one below (`+Inf` for the rest); `{kind}\|count` and `{kind}\|sum` (seconds) |
//!
//! Buckets are stored per bucket, not cumulatively; [`QueueWait`] adds them
//! up the way Prometheus wants.

use std::collections::BTreeMap;

use redis::AsyncCommands;

use super::{Keys, Result, Store};

/// Upper bounds, in seconds, of the queue wait histogram's buckets.
pub const QUEUE_WAIT_BUCKETS: [u64; 9] = [1, 2, 5, 10, 20, 30, 60, 120, 300];

impl Keys {
    fn games_started(&self) -> String {
        format!("{}:metrics:games-started", self.prefix)
    }
    fn queue_wait(&self) -> String {
        format!("{}:metrics:queue-wait", self.prefix)
    }
}

/// One kind's queue wait histogram.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueueWait {
    /// Per bucket of [`QUEUE_WAIT_BUCKETS`], then `+Inf`; not cumulative.
    pub buckets: [u64; QUEUE_WAIT_BUCKETS.len() + 1],
    pub count: u64,
    pub sum_seconds: f64,
}

impl QueueWait {
    /// `(le, cumulative count)` for each bucket, `+Inf` last.
    #[must_use]
    pub fn cumulative(&self) -> Vec<(String, u64)> {
        let mut total = 0;
        let mut out = Vec::with_capacity(self.buckets.len());
        for (i, n) in self.buckets.iter().enumerate() {
            total += n;
            let le = QUEUE_WAIT_BUCKETS
                .get(i)
                .map_or_else(|| "+Inf".to_owned(), u64::to_string);
            out.push((le, total));
        }
        out
    }
}

/// The bucket field for a wait of `seconds`.
fn bucket(seconds: f64) -> String {
    QUEUE_WAIT_BUCKETS
        .iter()
        .find(|le| seconds <= **le as f64)
        .map_or_else(|| "+Inf".to_owned(), u64::to_string)
}

impl Store {
    /// Adds what one placement means to the counters: a queue entry of
    /// `kind` waited `wait_ms`, and started a game if `first` (the server
    /// had nobody on it).
    pub(super) async fn count_placement(
        &self,
        kind: &str,
        wait_ms: u64,
        first: bool,
    ) -> Result<()> {
        let seconds = wait_ms as f64 / 1000.0;
        let key = self.keys.queue_wait();
        let mut pipe = redis::pipe();
        pipe.atomic()
            .hincr(&key, format!("{kind}|{}", bucket(seconds)), 1)
            .ignore()
            .hincr(&key, format!("{kind}|count"), 1)
            .ignore()
            .cmd("HINCRBYFLOAT")
            .arg(&key)
            .arg(format!("{kind}|sum"))
            .arg(seconds)
            .ignore();
        if first {
            pipe.hincr(self.keys.games_started(), kind, 1).ignore();
        }
        let _: () = pipe.query_async(&mut self.conn()).await?;
        Ok(())
    }

    /// Games started per kind, by kind.
    pub async fn games_started(&self) -> Result<BTreeMap<String, u64>> {
        Ok(self.conn().hgetall(self.keys.games_started()).await?)
    }

    /// Each kind's queue wait histogram, by kind.
    pub async fn queue_waits(&self) -> Result<BTreeMap<String, QueueWait>> {
        let raw: BTreeMap<String, String> = self.conn().hgetall(self.keys.queue_wait()).await?;
        let mut waits: BTreeMap<String, QueueWait> = BTreeMap::new();
        for (field, value) in raw {
            let Some((kind, part)) = field.rsplit_once('|') else {
                continue;
            };
            let wait = waits.entry(kind.to_owned()).or_default();
            match part {
                "count" => wait.count = value.parse().unwrap_or(0),
                "sum" => wait.sum_seconds = value.parse().unwrap_or(0.0),
                "+Inf" => wait.buckets[QUEUE_WAIT_BUCKETS.len()] = value.parse().unwrap_or(0),
                le => {
                    if let Some(i) = QUEUE_WAIT_BUCKETS.iter().position(|b| b.to_string() == le) {
                        wait.buckets[i] = value.parse().unwrap_or(0);
                    }
                }
            }
        }
        Ok(waits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_fall_in_buckets() {
        assert_eq!(bucket(0.2), "1");
        assert_eq!(bucket(1.0), "1");
        assert_eq!(bucket(1.5), "2");
        assert_eq!(bucket(300.0), "300");
        assert_eq!(bucket(301.0), "+Inf");
        let mut wait = QueueWait::default();
        wait.buckets[0] = 2;
        wait.buckets[2] = 1;
        wait.buckets[9] = 1;
        let cumulative = wait.cumulative();
        assert_eq!(cumulative[0], ("1".to_owned(), 2));
        assert_eq!(cumulative[2], ("5".to_owned(), 3));
        assert_eq!(cumulative[8], ("300".to_owned(), 3));
        assert_eq!(cumulative[9], ("+Inf".to_owned(), 4));
    }
}
