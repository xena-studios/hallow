//! The proxy layer: drains Vine proxies the players don't need, down to
//! `min_proxies`, and gives them back as players arrive. Draining uses the
//! proxies' own drain mode (the load balancer's agent check stops sending
//! new players; players on it stay). Pure, so every rule is unit tested.
//!
//! Only proxies the fleet manager drained itself are given back; a proxy
//! staff drained stays drained. Starting new proxies isn't automatic yet:
//! when the players need more proxies than are registered, the shortfall is
//! reported (and alerted) for staff.

use std::collections::BTreeSet;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

/// A proxy as the layer sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyLoad {
    pub id: String,
    pub players: u32,
    pub draining: bool,
}

/// What the layer remembers between ticks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyMemory {
    pub surplus_since: Option<DateTime<Utc>>,
    /// Proxies the fleet manager drained.
    pub drained: BTreeSet<String>,
}

/// How full the remaining proxies may be, in percent of
/// `players_per_proxy`, for one to be drained.
pub const DRAIN_BELOW_PERCENT: u32 = 80;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxyPlan {
    /// Proxies the players need, at least the minimum.
    pub wanted: u32,
    pub drain: Vec<String>,
    pub undrain: Vec<String>,
    /// Proxies wanted beyond those registered.
    pub missing: u32,
}

/// Plans one tick. `players_per_proxy` 0 turns the layer off: proxies it
/// drained are given back.
pub fn plan(
    proxies: &[ProxyLoad],
    players_per_proxy: u32,
    min_proxies: u32,
    scale_down_after: Duration,
    now: DateTime<Utc>,
    memory: &mut ProxyMemory,
) -> ProxyPlan {
    let ids: BTreeSet<&str> = proxies.iter().map(|p| p.id.as_str()).collect();
    memory.drained.retain(|id| ids.contains(id.as_str()));
    let count = proxies.len() as u32;
    let mut plan = ProxyPlan {
        wanted: count,
        ..ProxyPlan::default()
    };
    if players_per_proxy == 0 {
        plan.undrain = std::mem::take(&mut memory.drained).into_iter().collect();
        memory.surplus_since = None;
        return plan;
    }
    let min = min_proxies.max(1);
    let players: u32 = proxies.iter().map(|p| p.players).sum();
    plan.wanted = min.max(players.div_ceil(players_per_proxy));
    plan.missing = plan.wanted.saturating_sub(count);
    let active = proxies.iter().filter(|p| !p.draining).count() as u32;

    if active < plan.wanted {
        memory.surplus_since = None;
        let mut ours: Vec<&ProxyLoad> = proxies
            .iter()
            .filter(|p| p.draining && memory.drained.contains(&p.id))
            .collect();
        ours.sort_by(|a, b| b.players.cmp(&a.players).then(a.id.cmp(&b.id)));
        for proxy in ours.into_iter().take((plan.wanted - active) as usize) {
            memory.drained.remove(&proxy.id);
            plan.undrain.push(proxy.id.clone());
        }
        return plan;
    }

    // One fewer still carries everyone under DRAIN_BELOW_PERCENT?
    let fewer = active.saturating_sub(1);
    let surplus = active > plan.wanted
        && fewer >= min
        && u64::from(players) * 100
            <= u64::from(fewer) * u64::from(players_per_proxy) * u64::from(DRAIN_BELOW_PERCENT);
    if !surplus {
        memory.surplus_since = None;
        return plan;
    }
    let since = *memory.surplus_since.get_or_insert(now);
    if now - since < TimeDelta::from_std(scale_down_after).unwrap_or(TimeDelta::MAX) {
        return plan;
    }
    if let Some(proxy) = proxies
        .iter()
        .filter(|p| !p.draining)
        .min_by(|a, b| a.players.cmp(&b.players).then(b.id.cmp(&a.id)))
    {
        memory.drained.insert(proxy.id.clone());
        plan.drain.push(proxy.id.clone());
    }
    memory.surplus_since = None;
    plan
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn proxy(id: &str, players: u32, draining: bool) -> ProxyLoad {
        ProxyLoad {
            id: id.into(),
            players,
            draining,
        }
    }

    fn at(m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 20, m, 0).unwrap()
    }

    const AFTER: Duration = Duration::from_secs(600);

    #[test]
    fn idle_proxies_drain_down_to_the_minimum_one_at_a_time() {
        let proxies = [
            proxy("p1", 3, false),
            proxy("p2", 0, false),
            proxy("p3", 1, false),
        ];
        let mut memory = ProxyMemory::default();
        let first = plan(&proxies, 100, 1, AFTER, at(0), &mut memory);
        assert!(first.drain.is_empty(), "waits for the surplus to hold");
        let later = plan(&proxies, 100, 1, AFTER, at(10), &mut memory);
        assert_eq!(later.drain, ["p2"]);
        assert_eq!(later.wanted, 1);
        let proxies = [
            proxy("p1", 3, false),
            proxy("p2", 0, true),
            proxy("p3", 1, false),
        ];
        let next = plan(&proxies, 100, 1, AFTER, at(11), &mut memory);
        assert!(next.drain.is_empty(), "the wait starts again");
        let last = plan(&proxies, 100, 1, AFTER, at(21), &mut memory);
        assert_eq!(last.drain, ["p3"]);
        let proxies = [
            proxy("p1", 4, false),
            proxy("p2", 0, true),
            proxy("p3", 0, true),
        ];
        assert!(
            plan(&proxies, 100, 1, AFTER, at(40), &mut memory)
                .drain
                .is_empty(),
            "never below the minimum"
        );
    }

    #[test]
    fn busy_proxies_stay() {
        // 170 players on two proxies of 100: one would be at 170%.
        let proxies = [proxy("p1", 90, false), proxy("p2", 80, false)];
        let mut memory = ProxyMemory::default();
        plan(&proxies, 100, 1, Duration::ZERO, at(0), &mut memory);
        let p = plan(&proxies, 100, 1, Duration::ZERO, at(1), &mut memory);
        assert!(p.drain.is_empty());
        assert_eq!(p.wanted, 2);
    }

    #[test]
    fn players_arriving_get_drained_proxies_back_but_not_staffs() {
        let mut memory = ProxyMemory {
            drained: ["p2".to_owned()].into(),
            ..ProxyMemory::default()
        };
        // p3 was drained by staff; 250 players want three proxies.
        let proxies = [
            proxy("p1", 250, false),
            proxy("p2", 0, true),
            proxy("p3", 0, true),
        ];
        let p = plan(&proxies, 100, 1, AFTER, at(0), &mut memory);
        assert_eq!(p.undrain, ["p2"]);
        assert_eq!(p.wanted, 3);
        assert_eq!(p.missing, 0);
        assert!(memory.drained.is_empty());
    }

    #[test]
    fn a_shortfall_is_reported() {
        let mut memory = ProxyMemory::default();
        let p = plan(
            &[proxy("p1", 450, false)],
            200,
            1,
            AFTER,
            at(0),
            &mut memory,
        );
        assert_eq!((p.wanted, p.missing), (3, 2));
    }

    #[test]
    fn turning_the_layer_off_gives_proxies_back() {
        let mut memory = ProxyMemory {
            drained: ["p2".to_owned(), "gone".to_owned()].into(),
            ..ProxyMemory::default()
        };
        let proxies = [proxy("p1", 0, false), proxy("p2", 0, true)];
        let p = plan(&proxies, 0, 1, AFTER, at(0), &mut memory);
        assert_eq!(p.undrain, ["p2"]);
        assert!(memory.drained.is_empty());
    }
}
