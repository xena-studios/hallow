//! Hallow's anticheat checks (docs/ANTICHEAT.md, phase 2): pure functions from
//! one player's window of signals to the violations it shows, each with its
//! evidence.
//!
//! The checks run where their data is measured: the Vine plugin runs
//! [`proxy_checks`] on Vine's packet timing, the server plugin runs
//! [`server_checks`] on what Pumpkin's events show. Neither acts on a
//! player. Violations go to the control plane's verdict service, which
//! scores them and decides; in phase 2 it only records what it would do.
//!
//! | Check | Runs on | Fails when | Signals |
//! |---|---|---|---|
//! | `speed` | server | horizontal speed over [`Limits::speed`] in any one second | `horizontal_speed_max` |
//! | `reach` | server | an attack on a player from further than [`Limits::reach`] | `attack_reach_max` |
//! | `fly` | proxy | off the ground without falling for over [`Limits::hover_ms`], or rising over [`Limits::air_rise`] in one jump | `hover_ms_max`, `air_rise_max` |
//! | `click_rate` | proxy | over [`Limits::cps`] clicks in a second, or clicks too even to be a hand (gaps varying under [`Limits::click_stddev_ms`]) | `cps_max`, `swing_interval_*` |
//! | `timer` | proxy | client ticks running over [`Limits::timer_balance_ms`] ahead of real time | `timer_balance_max_ms` |
//!
//! Limits are generous on purpose: phase 2 observes, and beta data tunes
//! them before anything acts (phase 3). Known blind spots, each a false
//! positive the verdict service has to weigh: Vine can't see blocks, so
//! ladders, water and levitation look like `fly`; ice, speed effects and
//! knockback can pass the `speed` limit; network jitter makes clicks look
//! less even, never more.

use std::collections::BTreeMap;

use hallow_proto::signal::PlayerSignals;
use hallow_proto::violation::Violation;

/// Every check's name.
pub const CHECKS: [&str; 5] = ["speed", "reach", "fly", "click_rate", "timer"];

/// Where a check's limits sit.
#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    /// Blocks per second, horizontally, in any one second. Vanilla sprint
    /// jumping peaks near 7.
    pub speed: f64,
    /// Blocks from the attacker's eyes to the target's hitbox. Vanilla
    /// survival reach is 3, on every version.
    pub reach: f64,
    /// Milliseconds off the ground without falling. A vanilla jump hovers
    /// for about 300.
    pub hover_ms: f64,
    /// Blocks risen in one run off the ground. A vanilla jump rises 1.25.
    pub air_rise: f64,
    /// Clicks in any one second.
    pub cps: f64,
    /// Gaps between clicks varying less than this (standard deviation, ms)
    /// look like a machine.
    pub click_stddev_ms: f64,
    /// How many gaps the evenness check needs.
    pub click_gaps: f64,
    /// Only clicking faster than this (mean gap, ms) is judged for evenness.
    pub click_mean_ms: f64,
    /// Milliseconds the client's ticks may run ahead of real time.
    pub timer_balance_ms: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            speed: 10.0,
            reach: 3.6,
            hover_ms: 2000.0,
            air_rise: 3.5,
            cps: 20.0,
            click_stddev_ms: 10.0,
            click_gaps: 20.0,
            click_mean_ms: 150.0,
            timer_balance_ms: 500.0,
        }
    }
}

/// Builds violations for one sample.
struct Judge<'a> {
    sample: &'a PlayerSignals,
    found: Vec<Violation>,
}

impl<'a> Judge<'a> {
    fn new(sample: &'a PlayerSignals) -> Self {
        Self {
            sample,
            found: Vec::new(),
        }
    }

    fn metric(&self, name: &str) -> Option<f64> {
        self.sample
            .metrics
            .get(name)
            .copied()
            .filter(|v| v.is_finite())
    }

    fn fail(&mut self, check: &str, severity: f64, summary: String, evidence: &[(&str, f64)]) {
        self.found.push(Violation {
            player: self.sample.player,
            at_ms: self.sample.at_ms,
            check: check.to_owned(),
            severity: round(severity),
            protocol: self.sample.protocol,
            summary,
            evidence: evidence
                .iter()
                .map(|&(name, value)| (name.to_owned(), round(value)))
                .collect::<BTreeMap<_, _>>(),
        });
    }

    /// Fails `check` when `metric` is over `limit`.
    fn over(&mut self, check: &str, metric: &str, limit: f64, unit: &str) {
        if let Some(value) = self.metric(metric)
            && value > limit
        {
            self.fail(
                check,
                value / limit,
                format!(
                    "{} {} {unit}, limit {}",
                    metric.replace('_', " "),
                    round(value),
                    round(limit)
                ),
                &[(metric, value), ("limit", limit)],
            );
        }
    }
}

/// Checks on a server's sample (movement and combat from Pumpkin's events).
#[must_use]
pub fn server_checks(sample: &PlayerSignals, limits: &Limits) -> Vec<Violation> {
    let mut judge = Judge::new(sample);
    judge.over("speed", "horizontal_speed_max", limits.speed, "blocks/s");
    judge.over("reach", "attack_reach_max", limits.reach, "blocks");
    judge.found
}

/// Checks on a proxy's sample (Vine's packet timing).
#[must_use]
pub fn proxy_checks(sample: &PlayerSignals, limits: &Limits) -> Vec<Violation> {
    let mut judge = Judge::new(sample);
    fly(&mut judge, limits);
    click_rate(&mut judge, limits);
    judge.over(
        "timer",
        "timer_balance_max_ms",
        limits.timer_balance_ms,
        "ms ahead",
    );
    judge.found
}

fn fly(judge: &mut Judge<'_>, limits: &Limits) {
    let hover = judge.metric("hover_ms_max").unwrap_or(0.0);
    let rise = judge.metric("air_rise_max").unwrap_or(0.0);
    let severity = (hover / limits.hover_ms).max(rise / limits.air_rise);
    if severity <= 1.0 {
        return;
    }
    judge.fail(
        "fly",
        severity,
        format!(
            "{} ms in the air without falling (limit {}), rose {} blocks (limit {})",
            round(hover),
            round(limits.hover_ms),
            round(rise),
            round(limits.air_rise)
        ),
        &[
            ("hover_ms_max", hover),
            ("hover_limit_ms", limits.hover_ms),
            ("air_rise_max", rise),
            ("air_rise_limit", limits.air_rise),
        ],
    );
}

fn click_rate(judge: &mut Judge<'_>, limits: &Limits) {
    let cps = judge.metric("cps_max").unwrap_or(0.0);
    let gaps = judge.metric("swing_intervals").unwrap_or(0.0);
    let mean = judge.metric("swing_interval_mean_ms");
    let stddev = judge.metric("swing_interval_stddev_ms");
    let fast = cps / limits.cps;
    let even = match (mean, stddev) {
        (Some(mean), Some(stddev)) if gaps >= limits.click_gaps && mean <= limits.click_mean_ms => {
            limits.click_stddev_ms / stddev.max(1.0)
        }
        _ => 0.0,
    };
    let severity = fast.max(even);
    if severity <= 1.0 {
        return;
    }
    let mut why = Vec::new();
    if fast > 1.0 {
        why.push(format!(
            "{} clicks in a second, limit {}",
            round(cps),
            round(limits.cps)
        ));
    }
    if even > 1.0 {
        why.push(format!(
            "{} clicks {} ms apart varying by {} ms, limit {}",
            round(gaps),
            round(mean.unwrap_or(0.0)),
            round(stddev.unwrap_or(0.0)),
            round(limits.click_stddev_ms)
        ));
    }
    let mut evidence = vec![
        ("cps_max", cps),
        ("cps_limit", limits.cps),
        ("swing_intervals", gaps),
        ("stddev_limit_ms", limits.click_stddev_ms),
    ];
    if let (Some(mean), Some(stddev)) = (mean, stddev) {
        evidence.push(("swing_interval_mean_ms", mean));
        evidence.push(("swing_interval_stddev_ms", stddev));
    }
    judge.fail("click_rate", severity, why.join("; "), &evidence);
}

/// Three decimal places.
fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn sample(metrics: &[(&str, f64)]) -> PlayerSignals {
        PlayerSignals {
            player: Uuid::from_u128(7),
            at_ms: 1_700_000_000_000,
            window_ms: 30_000,
            protocol: Some(777),
            metrics: metrics
                .iter()
                .map(|&(name, value)| (name.to_owned(), value))
                .collect(),
        }
    }

    fn checks(found: &[Violation]) -> Vec<&str> {
        found.iter().map(|v| v.check.as_str()).collect()
    }

    /// What an honest player's windows look like.
    #[test]
    fn honest_windows_pass() {
        let limits = Limits::default();
        let server = sample(&[
            ("moves", 600.0),
            ("horizontal_speed_max", 7.1),
            ("vertical_speed_max", 1.3),
            ("attack_reach_max", 2.9),
            ("cps_max", 9.0),
        ]);
        assert!(server_checks(&server, &limits).is_empty());
        let proxy = sample(&[
            ("cps_max", 11.0),
            ("swing_intervals", 120.0),
            ("swing_interval_mean_ms", 110.0),
            ("swing_interval_stddev_ms", 32.0),
            ("hover_ms_max", 300.0),
            ("air_rise_max", 1.25),
            ("timer_balance_max_ms", 40.0),
            ("ticks", 600.0),
        ]);
        assert!(proxy_checks(&proxy, &limits).is_empty());
        // No measurements at all.
        assert!(proxy_checks(&sample(&[]), &limits).is_empty());
        assert!(server_checks(&sample(&[]), &limits).is_empty());
    }

    #[test]
    fn speed_and_reach() {
        let found = server_checks(
            &sample(&[("horizontal_speed_max", 20.0), ("attack_reach_max", 4.5)]),
            &Limits::default(),
        );
        assert_eq!(checks(&found), ["speed", "reach"]);
        let speed = &found[0];
        assert_eq!(speed.player, Uuid::from_u128(7));
        assert_eq!(speed.at_ms, 1_700_000_000_000);
        assert_eq!(speed.protocol, Some(777));
        assert_eq!(speed.severity, 2.0);
        assert_eq!(speed.summary, "horizontal speed max 20 blocks/s, limit 10");
        assert_eq!(speed.evidence["horizontal_speed_max"], 20.0);
        assert_eq!(speed.evidence["limit"], 10.0);
        assert_eq!(found[1].severity, 1.25);
    }

    #[test]
    fn fly_hovering_or_climbing() {
        let limits = Limits::default();
        let hover = proxy_checks(&sample(&[("hover_ms_max", 5_000.0)]), &limits);
        assert_eq!(checks(&hover), ["fly"]);
        assert_eq!(hover[0].severity, 2.5);
        let climb = proxy_checks(
            &sample(&[("air_rise_max", 7.0), ("hover_ms_max", 900.0)]),
            &limits,
        );
        assert_eq!(climb[0].severity, 2.0);
        assert_eq!(climb[0].evidence["air_rise_max"], 7.0);
    }

    #[test]
    fn click_rate_fast_or_even() {
        let limits = Limits::default();
        let fast = proxy_checks(&sample(&[("cps_max", 30.0)]), &limits);
        assert_eq!(checks(&fast), ["click_rate"]);
        assert_eq!(fast[0].severity, 1.5);
        assert!(fast[0].summary.contains("30 clicks in a second"));

        // Twelve clicks a second, 83 ms apart, varying by 2 ms: a clicker.
        let even = proxy_checks(
            &sample(&[
                ("cps_max", 12.0),
                ("swing_intervals", 300.0),
                ("swing_interval_mean_ms", 83.0),
                ("swing_interval_stddev_ms", 2.0),
            ]),
            &limits,
        );
        assert_eq!(even[0].severity, 5.0);
        assert!(even[0].summary.contains("varying by 2 ms"));

        // Too few gaps, or too slow, to judge evenness.
        let few = sample(&[
            ("swing_intervals", 5.0),
            ("swing_interval_mean_ms", 83.0),
            ("swing_interval_stddev_ms", 0.0),
        ]);
        assert!(proxy_checks(&few, &limits).is_empty());
        let slow = sample(&[
            ("swing_intervals", 50.0),
            ("swing_interval_mean_ms", 400.0),
            ("swing_interval_stddev_ms", 0.0),
        ]);
        assert!(proxy_checks(&slow, &limits).is_empty());
    }

    #[test]
    fn timer() {
        let found = proxy_checks(
            &sample(&[("timer_balance_max_ms", 1_500.0)]),
            &Limits::default(),
        );
        assert_eq!(checks(&found), ["timer"]);
        assert_eq!(found[0].severity, 3.0);
    }

    #[test]
    fn not_a_number_is_no_evidence() {
        let found = server_checks(
            &sample(&[("horizontal_speed_max", f64::NAN)]),
            &Limits::default(),
        );
        assert!(found.is_empty());
    }
}
