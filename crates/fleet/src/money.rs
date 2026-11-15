//! Money as an integer, so budgets add up exactly.

use std::fmt;
use std::iter::Sum;
use std::ops::{Add, AddAssign, Mul, Sub, SubAssign};

use serde::{Deserialize, Serialize};

/// An amount in millionths of the network's currency (one currency per
/// network; Hetzner bills in EUR). Hourly cloud prices have more than two
/// decimals (€0.0083/h), so cents aren't fine enough.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Money(i64);

impl Money {
    pub const ZERO: Money = Money(0);
    const SCALE: i64 = 1_000_000;

    pub const fn from_micros(micros: i64) -> Money {
        Money(micros)
    }

    pub const fn from_cents(cents: i64) -> Money {
        Money(cents * 10_000)
    }

    pub const fn from_units(units: i64) -> Money {
        Money(units * Self::SCALE)
    }

    pub const fn micros(self) -> i64 {
        self.0
    }

    /// Parses a decimal string such as Hetzner's `"0.0060000000"`. Digits past
    /// the sixth decimal are dropped.
    pub fn parse(s: &str) -> Option<Money> {
        let s = s.trim();
        let (negative, s) = match s.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, s),
        };
        let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
        if whole.is_empty() && frac.is_empty() {
            return None;
        }
        if !whole.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let whole: i64 = if whole.is_empty() {
            0
        } else {
            whole.parse().ok()?
        };
        let mut micros: i64 = 0;
        let mut scale = Self::SCALE / 10;
        for b in frac.bytes().take(6) {
            micros += i64::from(b - b'0') * scale;
            scale /= 10;
        }
        let value = whole.checked_mul(Self::SCALE)?.checked_add(micros)?;
        Some(Money(if negative { -value } else { value }))
    }

    /// Multiplies by a non-integer factor (hours, fractions), rounding to the
    /// nearest micro.
    pub fn scale(self, factor: f64) -> Money {
        Money((self.0 as f64 * factor).round() as i64)
    }

    pub fn as_f64(self) -> f64 {
        self.0 as f64 / Self::SCALE as f64
    }
}

impl fmt::Display for Money {
    /// Four decimals: enough for hourly prices, readable for budgets.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = self.0.unsigned_abs();
        let whole = abs / Self::SCALE as u64;
        let frac = (abs % Self::SCALE as u64) / 100;
        write!(f, "{sign}{whole}.{frac:04}")
    }
}

impl Add for Money {
    type Output = Money;
    fn add(self, rhs: Money) -> Money {
        Money(self.0 + rhs.0)
    }
}

impl AddAssign for Money {
    fn add_assign(&mut self, rhs: Money) {
        self.0 += rhs.0;
    }
}

impl Sub for Money {
    type Output = Money;
    fn sub(self, rhs: Money) -> Money {
        Money(self.0 - rhs.0)
    }
}

impl SubAssign for Money {
    fn sub_assign(&mut self, rhs: Money) {
        self.0 -= rhs.0;
    }
}

impl Mul<i64> for Money {
    type Output = Money;
    fn mul(self, rhs: i64) -> Money {
        Money(self.0 * rhs)
    }
}

impl Sum for Money {
    fn sum<I: Iterator<Item = Money>>(iter: I) -> Money {
        iter.fold(Money::ZERO, Add::add)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_provider_decimals() {
        assert_eq!(
            Money::parse("0.0060000000"),
            Some(Money::from_micros(6_000))
        );
        assert_eq!(Money::parse("3.7900"), Some(Money::from_micros(3_790_000)));
        assert_eq!(Money::parse("12"), Some(Money::from_units(12)));
        assert_eq!(
            Money::parse("0.0083333333"),
            Some(Money::from_micros(8_333))
        );
        assert_eq!(Money::parse("-1.5"), Some(Money::from_micros(-1_500_000)));
        assert_eq!(Money::parse(""), None);
        assert_eq!(Money::parse("1.2.3"), None);
        assert_eq!(Money::parse("abc"), None);
    }

    #[test]
    fn displays_four_decimals() {
        assert_eq!(Money::from_micros(6_000).to_string(), "0.0060");
        assert_eq!(Money::from_units(150).to_string(), "150.0000");
        assert_eq!(Money::from_micros(-1_500_000).to_string(), "-1.5000");
    }
}
