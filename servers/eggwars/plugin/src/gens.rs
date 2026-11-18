//! Resource generators: how fast each tier makes its resource at each
//! level, how much it holds when nobody collects, and what an upgrade
//! costs. A generator's resource goes to the players standing on it.

use hallow_islands::map::Tier;

use crate::shop::{Currency, Price};

/// The highest level.
pub const MAX_LEVEL: u8 = 3;

/// One level of a tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Level {
    /// Game ticks per resource.
    pub every: u32,
    /// Resources held for collection, at most.
    pub cap: u32,
}

/// The tier's levels, 1 to [`MAX_LEVEL`].
#[must_use]
pub const fn level(tier: Tier, level: u8) -> Option<Level> {
    let (every, cap) = match (tier, level) {
        (Tier::Iron, 1) => (36, 48),
        (Tier::Iron, 2) => (22, 64),
        (Tier::Iron, 3) => (12, 64),
        (Tier::Gold, 1) => (120, 16),
        (Tier::Gold, 2) => (70, 24),
        (Tier::Gold, 3) => (40, 32),
        (Tier::Diamond, 1) => (500, 4),
        (Tier::Diamond, 2) => (300, 6),
        (Tier::Diamond, 3) => (180, 8),
        _ => return None,
    };
    Some(Level { every, cap })
}

/// What going from `level` to `level + 1` costs.
#[must_use]
pub const fn upgrade_cost(tier: Tier, level: u8) -> Option<Price> {
    use Currency::{Diamond, Gold, Iron};
    Some(match (tier, level) {
        (Tier::Iron, 1) => Price(24, Iron),
        (Tier::Iron, 2) => Price(10, Gold),
        (Tier::Gold, 0) => Price(16, Iron),
        (Tier::Gold, 1) => Price(12, Gold),
        (Tier::Gold, 2) => Price(4, Diamond),
        (Tier::Diamond, 0) => Price(16, Gold),
        (Tier::Diamond, 1) => Price(8, Diamond),
        (Tier::Diamond, 2) => Price(16, Diamond),
        _ => return None,
    })
}

/// The resource item.
#[must_use]
pub const fn resource(tier: Tier) -> Currency {
    match tier {
        Tier::Iron => Currency::Iron,
        Tier::Gold => Currency::Gold,
        Tier::Diamond => Currency::Diamond,
    }
}

#[must_use]
pub const fn tier_name(tier: Tier) -> &'static str {
    match tier {
        Tier::Iron => "§fIron",
        Tier::Gold => "§6Gold",
        Tier::Diamond => "§bDiamond",
    }
}

/// A generator in a match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Generator {
    pub tier: Tier,
    pub level: u8,
    /// Ticks towards the next resource.
    progress: u32,
    /// Made but not collected.
    pub stored: u32,
}

impl Generator {
    #[must_use]
    pub const fn new(tier: Tier, level: u8) -> Self {
        Self {
            tier,
            level,
            progress: 0,
            stored: 0,
        }
    }

    /// Runs `ticks` game ticks.
    pub fn run(&mut self, ticks: u32) {
        let Some(level) = level(self.tier, self.level) else {
            return;
        };
        self.progress += ticks;
        while self.progress >= level.every {
            self.progress -= level.every;
            self.stored = (self.stored + 1).min(level.cap);
        }
    }

    /// Hands over everything stored.
    pub const fn collect(&mut self) -> u32 {
        std::mem::replace(&mut self.stored, 0)
    }

    /// The next upgrade's price, if there is one.
    #[must_use]
    pub const fn next_cost(&self) -> Option<Price> {
        upgrade_cost(self.tier, self.level)
    }

    /// Goes up a level (after it's been paid for).
    pub const fn upgrade(&mut self) {
        if self.level < MAX_LEVEL {
            self.level += 1;
        }
    }

    /// Seconds per resource now, if running.
    #[must_use]
    pub fn seconds_per(&self) -> Option<f32> {
        level(self.tier, self.level).map(|l| l.every as f32 / 20.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generators_make_and_hold() {
        let mut iron = Generator::new(Tier::Iron, 1);
        iron.run(36 * 3 + 10);
        assert_eq!(iron.stored, 3);
        iron.run(26);
        assert_eq!(iron.collect(), 4);
        assert_eq!(iron.stored, 0);
        for _ in 0..1000 {
            iron.run(36);
        }
        assert_eq!(iron.stored, 48, "capped");
    }

    #[test]
    fn off_until_upgraded() {
        let mut diamond = Generator::new(Tier::Diamond, 0);
        diamond.run(10_000);
        assert_eq!(diamond.stored, 0);
        assert_eq!(diamond.next_cost(), Some(Price(16, Currency::Gold)));
        diamond.upgrade();
        diamond.run(500);
        assert_eq!(diamond.stored, 1);
    }

    #[test]
    fn upgrades_get_faster_and_stop_at_the_top() {
        for tier in [Tier::Iron, Tier::Gold, Tier::Diamond] {
            let speeds: Vec<u32> = (1..=MAX_LEVEL)
                .map(|l| level(tier, l).unwrap().every)
                .collect();
            assert!(speeds.windows(2).all(|w| w[0] > w[1]));
            assert_eq!(upgrade_cost(tier, MAX_LEVEL), None);
            let mut g = Generator::new(tier, MAX_LEVEL);
            g.upgrade();
            assert_eq!(g.level, MAX_LEVEL);
        }
    }
}
