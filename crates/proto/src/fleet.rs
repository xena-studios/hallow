//! Fleet manager settings (docs/FLEET.md): everything the fleet manager
//! decides by, kept in PostgreSQL and edited from the panel. Nothing about
//! when to buy, drain or cancel a machine is hard-coded.
//!
//! Stored settings are read with `#[serde(default)]`, so a setting added
//! later takes its default on networks that saved theirs before it existed,
//! and a `PUT` may leave out settings to keep their defaults.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Field name (`max_nodes`, `providers[0].regions[1]`) to what is wrong with
/// it, for a settings form to show next to each field.
pub type FieldErrors = BTreeMap<String, String>;

/// The longest any duration setting may be: 30 days.
pub const MAX_DURATION_SECS: i64 = 30 * 24 * 3600;

/// Everything the fleet manager decides by. Defaults are in
/// [`FleetSettings::default`] and listed in docs/FLEET.md.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FleetSettings {
    /// Log what would be bought and cancelled without calling any provider.
    /// Default `true`.
    pub dry_run: bool,
    /// Make no decisions at all: nothing is bought, drained or cancelled.
    /// Default `false`.
    pub paused: bool,
    /// IANA time zone, such as `Europe/Berlin`, that decides when "today"
    /// and "this month" start for the daily cap and the monthly budget.
    /// Default `UTC`.
    pub timezone: String,
    /// ISO 4217 code the budgets and provider prices are in. Default `EUR`.
    pub currency: String,
    /// Decimal amount in `currency`, such as `"120.00"`; `null` for no
    /// limit. Default `"100.00"`.
    pub monthly_budget: Option<String>,
    /// Decimal amount in `currency`; `null` for no limit. Default `"10.00"`.
    pub daily_cap: Option<String>,
    /// Nodes of every kind and phase, the floor included. Default `10`.
    pub max_nodes: u32,
    /// Free game slots to keep across all nodes. Default `40`.
    pub headroom_slots: u32,
    /// When the month-end forecast passes this fraction of the monthly
    /// budget, headroom shrinks linearly to zero at the budget. `0` to `1`;
    /// default `0.8`.
    pub tighten_from: f64,
    /// Scale up when any queue waits longer than this. Default `60`.
    pub queue_wait_target_secs: i64,
    /// Pressure must hold this long before buying. Default `180`.
    pub scale_up_after_secs: i64,
    /// Spare capacity must hold this long before draining. Default `600`.
    pub scale_down_after_secs: i64,
    /// After a buy, no further buy for this long. Default `300`.
    pub scale_up_cooldown_secs: i64,
    /// After a buy or a drain, no drain for this long. Default `900`.
    pub scale_down_cooldown_secs: i64,
    /// A draining node is cancelled after this even with players left (they
    /// are moved first). Default `1200`.
    pub drain_deadline_secs: i64,
    /// An empty hourly node is kept until this long before its next billing
    /// step, then cancelled. Default `300`.
    pub billing_step_margin_secs: i64,
    /// Cancelling a draining base (monthly) node waits for a staff member's
    /// approval. Default `true`.
    pub base_drain_needs_approval: bool,
    /// Drain burst nodes before base nodes. Default `true`.
    pub prefer_burst_on_scale_down: bool,
    /// Nodes that are never drained or cancelled: the oldest base nodes.
    /// At least 1; default `1`.
    pub floor_nodes: u32,
    /// Players one Vine proxy should carry. The fleet manager drains proxies
    /// it doesn't need down to `min_proxies`, and gives them back as players
    /// arrive. `0` leaves proxies alone. Default `400`.
    pub players_per_proxy: u32,
    /// Proxies never drained for being idle. At least 1; default `1`.
    pub min_proxies: u32,
    /// Providers the fleet manager may buy from. A provider not listed is
    /// never used. Default: `hetzner-cloud`, enabled, any region and type.
    pub providers: Vec<ProviderSettings>,
}

/// Where and what the fleet manager may buy from one provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSettings {
    /// The provider crate's name, such as `hetzner-cloud`.
    pub name: String,
    pub enabled: bool,
    /// Region or location names, such as `fsn1`; empty for any.
    #[serde(default)]
    pub regions: Vec<String>,
    /// Machine types, such as `cpx41`; empty for any.
    #[serde(default)]
    pub machine_types: Vec<String>,
}

impl ProviderSettings {
    /// Whether this provider may sell `machine_type` in `region`.
    #[must_use]
    pub fn allows(&self, region: &str, machine_type: &str) -> bool {
        self.enabled
            && (self.regions.is_empty() || self.regions.iter().any(|r| r == region))
            && (self.machine_types.is_empty()
                || self.machine_types.iter().any(|m| m == machine_type))
    }
}

impl Default for FleetSettings {
    fn default() -> Self {
        Self {
            dry_run: true,
            paused: false,
            timezone: "UTC".into(),
            currency: "EUR".into(),
            monthly_budget: Some("100.00".into()),
            daily_cap: Some("10.00".into()),
            max_nodes: 10,
            headroom_slots: 40,
            tighten_from: 0.8,
            queue_wait_target_secs: 60,
            scale_up_after_secs: 180,
            scale_down_after_secs: 600,
            scale_up_cooldown_secs: 300,
            scale_down_cooldown_secs: 900,
            drain_deadline_secs: 1200,
            billing_step_margin_secs: 300,
            base_drain_needs_approval: true,
            prefer_burst_on_scale_down: true,
            floor_nodes: 1,
            players_per_proxy: 400,
            min_proxies: 1,
            providers: vec![ProviderSettings {
                name: "hetzner-cloud".into(),
                enabled: true,
                regions: Vec::new(),
                machine_types: Vec::new(),
            }],
        }
    }
}

impl FleetSettings {
    /// Whether `provider` may sell `machine_type` in `region`.
    #[must_use]
    pub fn allows(&self, provider: &str, region: &str, machine_type: &str) -> bool {
        self.providers
            .iter()
            .any(|p| p.name == provider && p.allows(region, machine_type))
    }

    /// Checks every setting; on failure, says what is wrong with each field.
    pub fn validate(&self) -> Result<(), FieldErrors> {
        let mut errors = FieldErrors::new();
        let mut fail = |field: &str, message: &str| {
            errors.insert(field.to_owned(), message.to_owned());
        };

        if self.timezone.parse::<chrono_tz::Tz>().is_err() {
            fail(
                "timezone",
                "must be an IANA time zone, such as UTC or Europe/Berlin",
            );
        }
        if !(self.currency.len() == 3 && self.currency.bytes().all(|b| b.is_ascii_uppercase())) {
            fail(
                "currency",
                "must be a three-letter ISO 4217 code, such as EUR",
            );
        }
        for (field, amount) in [
            ("monthly_budget", &self.monthly_budget),
            ("daily_cap", &self.daily_cap),
        ] {
            if let Some(amount) = amount
                && let Err(message) = check_amount(amount)
            {
                fail(field, message);
            }
        }
        if let (Some(daily), Some(monthly)) = (
            self.daily_cap.as_deref().and_then(parse_amount),
            self.monthly_budget.as_deref().and_then(parse_amount),
        ) && daily > monthly
        {
            fail("daily_cap", "must not be more than the monthly budget");
        }
        if !(0.0..=1.0).contains(&self.tighten_from) {
            fail("tighten_from", "must be between 0 and 1");
        }
        for (field, secs) in [
            ("queue_wait_target_secs", self.queue_wait_target_secs),
            ("scale_up_after_secs", self.scale_up_after_secs),
            ("scale_down_after_secs", self.scale_down_after_secs),
            ("scale_up_cooldown_secs", self.scale_up_cooldown_secs),
            ("scale_down_cooldown_secs", self.scale_down_cooldown_secs),
            ("drain_deadline_secs", self.drain_deadline_secs),
            ("billing_step_margin_secs", self.billing_step_margin_secs),
        ] {
            if secs < 0 {
                fail(field, "must not be negative");
            } else if secs > MAX_DURATION_SECS {
                fail(field, "must be at most 2592000 seconds (30 days)");
            }
        }
        if (0..=MAX_DURATION_SECS).contains(&self.billing_step_margin_secs)
            && self.billing_step_margin_secs >= 3600
        {
            fail(
                "billing_step_margin_secs",
                "must be less than an hour, the shortest billing step",
            );
        }
        if self.floor_nodes < 1 {
            fail("floor_nodes", "must be at least 1");
        }
        if self.max_nodes < self.floor_nodes {
            fail("max_nodes", "must be at least floor_nodes");
        }
        if self.min_proxies < 1 {
            fail("min_proxies", "must be at least 1");
        }

        let mut names: Vec<&str> = Vec::new();
        for (i, provider) in self.providers.iter().enumerate() {
            let at = format!("providers[{i}]");
            if !is_name(&provider.name) {
                fail(
                    &format!("{at}.name"),
                    "must be 1 to 64 letters, digits, '.', '_' or '-'",
                );
            } else if names.contains(&provider.name.as_str()) {
                fail(&format!("{at}.name"), "is listed twice");
            }
            names.push(&provider.name);
            for (list, values) in [
                ("regions", &provider.regions),
                ("machine_types", &provider.machine_types),
            ] {
                for (j, value) in values.iter().enumerate() {
                    if !is_name(value) {
                        fail(
                            &format!("{at}.{list}[{j}]"),
                            "must be 1 to 64 letters, digits, '.', '_' or '-'",
                        );
                    } else if values[..j].contains(value) {
                        fail(&format!("{at}.{list}[{j}]"), "is listed twice");
                    }
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn check_amount(s: &str) -> Result<(), &'static str> {
    if s.trim_start().starts_with('-') {
        return Err("must not be negative");
    }
    parse_amount(s)
        .map(|_| ())
        .ok_or("must be a decimal amount with at most six decimals, such as 120.00")
}

/// A non-negative decimal with at most six decimals, in millionths.
fn parse_amount(s: &str) -> Option<u64> {
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    if whole.is_empty()
        || frac.len() > 6
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let mut micros = whole.parse::<u64>().ok()?.checked_mul(1_000_000)?;
    let mut scale = 100_000;
    for b in frac.bytes() {
        micros = micros.checked_add(u64::from(b - b'0') * scale)?;
        scale /= 10;
    }
    Some(micros)
}

/// `GET` and `PUT /v1/fleet/settings`: the settings in force.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FleetSettingsRecord {
    pub settings: FleetSettings,
    /// Unix milliseconds of the last change; `None` while the defaults
    /// have never been changed.
    pub updated_at: Option<u64>,
    /// Who made the last change; `None` while the defaults have never been
    /// changed.
    pub updated_by: Option<String>,
}

/// `PUT /v1/fleet/settings`: replace every setting.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdateFleetSettings {
    pub settings: FleetSettings,
    /// The staff member making the change, for the audit history.
    pub updated_by: String,
}

/// One change in `GET /v1/fleet/settings/history`, newest first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FleetSettingsChange {
    pub id: i64,
    /// The settings before; `None` for the first change from the defaults.
    pub before: Option<FleetSettings>,
    pub after: FleetSettings,
    /// Unix milliseconds.
    pub updated_at: u64,
    pub updated_by: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_are_valid() {
        assert_eq!(FleetSettings::default().validate(), Ok(()));
    }

    #[test]
    fn missing_fields_take_their_defaults() {
        let settings: FleetSettings =
            serde_json::from_value(json!({"dry_run": false, "max_nodes": 4})).unwrap();
        assert!(!settings.dry_run);
        assert_eq!(settings.max_nodes, 4);
        assert_eq!(settings.timezone, "UTC");
        assert_eq!(settings.providers.len(), 1);
    }

    #[test]
    fn field_names_are_snake_case() {
        let value = serde_json::to_value(FleetSettings::default()).unwrap();
        for key in [
            "dry_run",
            "paused",
            "timezone",
            "currency",
            "monthly_budget",
            "daily_cap",
            "max_nodes",
            "headroom_slots",
            "tighten_from",
            "queue_wait_target_secs",
            "scale_up_after_secs",
            "scale_down_after_secs",
            "scale_up_cooldown_secs",
            "scale_down_cooldown_secs",
            "drain_deadline_secs",
            "billing_step_margin_secs",
            "base_drain_needs_approval",
            "floor_nodes",
            "players_per_proxy",
            "min_proxies",
            "providers",
        ] {
            assert!(value.get(key).is_some(), "{key}");
        }
        assert_eq!(
            value["providers"][0],
            json!({"name": "hetzner-cloud", "enabled": true, "regions": [], "machine_types": []})
        );
    }

    #[test]
    fn every_bad_field_is_named() {
        let settings = FleetSettings {
            timezone: "Mars/Olympus".into(),
            currency: "euro".into(),
            monthly_budget: Some("-5".into()),
            daily_cap: Some("12.5x".into()),
            tighten_from: 1.5,
            scale_up_cooldown_secs: -1,
            billing_step_margin_secs: 3600,
            floor_nodes: 3,
            max_nodes: 2,
            providers: vec![
                ProviderSettings {
                    name: "hetzner-cloud".into(),
                    enabled: true,
                    regions: vec!["fsn1".into(), "fsn1".into()],
                    machine_types: vec![String::new()],
                },
                ProviderSettings {
                    name: "hetzner-cloud".into(),
                    enabled: false,
                    regions: vec![],
                    machine_types: vec![],
                },
            ],
            ..FleetSettings::default()
        };
        let errors = settings.validate().unwrap_err();
        let fields: Vec<&str> = errors.keys().map(String::as_str).collect();
        assert_eq!(
            fields,
            [
                "billing_step_margin_secs",
                "currency",
                "daily_cap",
                "max_nodes",
                "monthly_budget",
                "providers[0].machine_types[0]",
                "providers[0].regions[1]",
                "providers[1].name",
                "scale_up_cooldown_secs",
                "tighten_from",
                "timezone",
            ]
        );
    }

    #[test]
    fn amounts() {
        assert_eq!(parse_amount("120.00"), Some(120_000_000));
        assert_eq!(parse_amount("0.0083"), Some(8_300));
        assert_eq!(parse_amount("7"), Some(7_000_000));
        assert_eq!(parse_amount(".5"), None);
        assert_eq!(parse_amount("1.1234567"), None);
        assert_eq!(check_amount("-1"), Err("must not be negative"));
        let over = FleetSettings {
            daily_cap: Some("200".into()),
            ..FleetSettings::default()
        };
        assert!(over.validate().unwrap_err().contains_key("daily_cap"));
        let unlimited = FleetSettings {
            daily_cap: None,
            monthly_budget: None,
            ..FleetSettings::default()
        };
        assert_eq!(unlimited.validate(), Ok(()));
    }

    #[test]
    fn provider_filters() {
        let settings = FleetSettings {
            providers: vec![ProviderSettings {
                name: "hetzner-cloud".into(),
                enabled: true,
                regions: vec!["fsn1".into()],
                machine_types: vec![],
            }],
            ..FleetSettings::default()
        };
        assert!(settings.allows("hetzner-cloud", "fsn1", "cpx41"));
        assert!(!settings.allows("hetzner-cloud", "nbg1", "cpx41"));
        assert!(!settings.allows("ovh-cloud", "fsn1", "cpx41"));
        let mut off = settings.clone();
        off.providers[0].enabled = false;
        assert!(!off.allows("hetzner-cloud", "fsn1", "cpx41"));
    }
}
