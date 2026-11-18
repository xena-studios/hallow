//! What nodes have cost so far: the [`Spend`] the planner budgets with,
//! worked out from each node's price and when it ran, the way providers
//! bill.
//!
//! - An hourly node is charged a full step when each hour of its life
//!   starts (created at 10:20, it is charged at 10:20, 11:20, ...), up to
//!   its monthly cap within one month.
//! - A monthly node is charged pro rata by the hour it ran, so a month of
//!   it adds up to its price.
//!
//! Days and months start at midnight in the network's time zone.

use chrono::{DateTime, Datelike, TimeDelta, Utc};
use chrono_tz::Tz;

use crate::money::Money;
use crate::planner::{Spend, start_of_day};
use crate::provider::{BillingStep, Cost};

/// One node's price and life, for billing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Billed {
    pub cost: Cost,
    pub created_at: DateTime<Utc>,
    /// When it was cancelled; `None` while it runs.
    pub ended_at: Option<DateTime<Utc>>,
}

impl Billed {
    /// Charged between `from` and `to`, before any monthly cap.
    fn raw(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Money {
        let end = self.ended_at.map_or(to, |e| e.min(to));
        if end <= from || end <= self.created_at {
            return Money::ZERO;
        }
        match self.cost.step {
            BillingStep::Hour => {
                // Steps start at created + k hours; count those in [from, end).
                let first = if from <= self.created_at {
                    0
                } else {
                    let secs = (from - self.created_at).num_seconds();
                    secs.div_euclid(3600) + i64::from(secs.rem_euclid(3600) != 0)
                };
                let last = (end - self.created_at - TimeDelta::nanoseconds(1))
                    .num_seconds()
                    .div_euclid(3600);
                if last < first {
                    Money::ZERO
                } else {
                    self.cost.amount * (last - first + 1)
                }
            }
            BillingStep::Month => {
                let start = from.max(self.created_at);
                let hours = (end - start).num_seconds().max(0) as f64 / 3600.0;
                self.cost.per_hour().scale(hours)
            }
        }
    }

    /// Charged this month up to `to`, capped at the node's monthly cap.
    fn month_to(&self, month_start: DateTime<Utc>, to: DateTime<Utc>) -> Money {
        let raw = self.raw(month_start, to);
        match self.cost.monthly_cap {
            Some(cap) => raw.min(cap),
            None => raw,
        }
    }
}

/// When the month holding `now` started, and today, in `tz`.
pub fn periods(now: DateTime<Utc>, tz: Tz) -> (DateTime<Utc>, DateTime<Utc>) {
    let today = now.with_timezone(&tz).date_naive();
    let first = today.with_day(1).unwrap_or(today);
    (start_of_day(tz, first), start_of_day(tz, today))
}

/// One node's spend this month so far.
pub fn node_month(node: &Billed, now: DateTime<Utc>, tz: Tz) -> Money {
    let (month_start, _) = periods(now, tz);
    node.month_to(month_start, now)
}

/// Spend today and this month so far, over every node that ran.
pub fn spend(nodes: &[Billed], now: DateTime<Utc>, tz: Tz) -> Spend {
    let (month_start, day_start) = periods(now, tz);
    let mut spend = Spend::default();
    for node in nodes {
        let month = node.month_to(month_start, now);
        let before_today = node.month_to(month_start, day_start.max(month_start));
        spend.month += month;
        spend.today += month - before_today;
    }
    spend
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn at(d: u32, h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, d, h, m, 0).unwrap()
    }

    fn hourly(cap: Option<i64>) -> Cost {
        Cost::hourly(Money::from_cents(5), cap.map(Money::from_cents))
    }

    #[test]
    fn hourly_nodes_pay_each_started_hour() {
        let node = Billed {
            cost: hourly(None),
            created_at: at(15, 10, 20),
            ended_at: None,
        };
        let utc = chrono_tz::UTC;
        assert_eq!(spend(&[node], at(15, 10, 20), utc).today, Money::ZERO);
        assert_eq!(
            spend(&[node], at(15, 10, 21), utc).today,
            Money::from_cents(5)
        );
        assert_eq!(
            spend(&[node], at(15, 11, 20), utc).today,
            Money::from_cents(5)
        );
        assert_eq!(
            spend(&[node], at(15, 11, 21), utc).today,
            Money::from_cents(10)
        );
        let cancelled = Billed {
            ended_at: Some(at(15, 12, 0)),
            ..node
        };
        assert_eq!(
            spend(&[cancelled], at(15, 23, 0), utc).today,
            Money::from_cents(10)
        );
    }

    #[test]
    fn days_split_at_local_midnight() {
        // Runs 22:30 to 01:10 UTC; in Berlin (UTC+1) midnight is 23:00 UTC.
        let node = Billed {
            cost: hourly(None),
            created_at: at(15, 22, 30),
            ended_at: Some(at(16, 1, 10)),
        };
        let utc = spend(&[node], at(16, 2, 0), chrono_tz::UTC);
        assert_eq!(utc.month, Money::from_cents(15));
        // Steps at 22:30, 23:30 and 00:30 UTC; only 00:30 is on the 16th.
        assert_eq!(utc.today, Money::from_cents(5));
        let berlin = spend(&[node], at(16, 2, 0), chrono_tz::Europe::Berlin);
        // 23:30 and 00:30 UTC are both on the 16th in Berlin.
        assert_eq!(berlin.today, Money::from_cents(10));
        assert_eq!(berlin.month, Money::from_cents(15));
    }

    #[test]
    fn the_monthly_cap_holds_within_a_month() {
        let node = Billed {
            cost: hourly(Some(12)),
            created_at: at(15, 0, 0),
            ended_at: None,
        };
        let s = spend(&[node], at(15, 10, 0), chrono_tz::UTC);
        assert_eq!(s.month, Money::from_cents(12));
        assert_eq!(s.today, Money::from_cents(12));
        let s = spend(&[node], at(16, 10, 0), chrono_tz::UTC);
        assert_eq!(s.today, Money::ZERO, "capped since yesterday");
    }

    #[test]
    fn monthly_nodes_pay_pro_rata() {
        let node = Billed {
            cost: Cost::monthly(Money::from_units(73)),
            created_at: Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
            ended_at: None,
        };
        let s = spend(&[node], at(1, 10, 0), chrono_tz::UTC);
        assert_eq!(s.month, Money::from_units(1));
        assert_eq!(s.today, Money::from_units(1));
        let s = spend(&[node], at(2, 10, 0), chrono_tz::UTC);
        assert_eq!(s.month, Money::from_micros(3_400_000)); // 34 hours at 0.10
        assert_eq!(s.today, Money::from_units(1));
    }
}
