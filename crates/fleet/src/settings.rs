//! From the fleet settings the panel edits ([`FleetSettings`], kept in
//! PostgreSQL by the control plane) to the planner's [`Policy`].
//!
//! `dry_run` and `paused` aren't planner inputs: the loop that runs the
//! planner reads them. With `paused` it doesn't call [`plan`](crate::plan);
//! with `dry_run` it carries out decisions through a [`DryRun`](crate::DryRun)
//! provider.

use std::time::Duration;

use chrono_tz::Tz;
use hallow_proto::fleet::{FieldErrors, FleetSettings};

use crate::money::Money;
use crate::planner::{NodeLoad, Offer, Policy};
use crate::provider::{NodeId, NodeKind};

/// Settings that don't pass [`FleetSettings::validate`], field by field.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid fleet settings: {}", .0.iter().map(|(f, m)| format!("{f} {m}")).collect::<Vec<_>>().join("; "))]
pub struct InvalidSettings(pub FieldErrors);

impl Policy {
    /// The policy for `settings`, with the floor's node ids (see
    /// [`floor_nodes`]) and the machines that may be bought (see
    /// [`allowed_offers`]).
    pub fn from_settings(
        settings: &FleetSettings,
        floor_nodes: Vec<NodeId>,
        burst_offers: Vec<Offer>,
    ) -> Result<Policy, InvalidSettings> {
        settings.validate().map_err(InvalidSettings)?;
        let invalid = |field: &str, message: &str| {
            InvalidSettings(FieldErrors::from([(field.to_owned(), message.to_owned())]))
        };
        let timezone: Tz = settings
            .timezone
            .parse()
            .map_err(|_| invalid("timezone", "isn't an IANA time zone"))?;
        let money = |field: &str, amount: &Option<String>| -> Result<_, InvalidSettings> {
            amount
                .as_deref()
                .map(|a| Money::parse(a).ok_or_else(|| invalid(field, "isn't an amount")))
                .transpose()
        };
        let secs = |s: i64| Duration::from_secs(u64::try_from(s).unwrap_or(0));
        Ok(Policy {
            timezone,
            headroom_slots: settings.headroom_slots,
            queue_wait_target: secs(settings.queue_wait_target_secs),
            scale_up_after: secs(settings.scale_up_after_secs),
            scale_down_after: secs(settings.scale_down_after_secs),
            scale_up_cooldown: secs(settings.scale_up_cooldown_secs),
            scale_down_cooldown: secs(settings.scale_down_cooldown_secs),
            drain_deadline: secs(settings.drain_deadline_secs),
            billing_step_margin: secs(settings.billing_step_margin_secs),
            max_nodes: settings.max_nodes,
            monthly_budget: money("monthly_budget", &settings.monthly_budget)?,
            daily_cap: money("daily_cap", &settings.daily_cap)?,
            tighten_from: settings.tighten_from,
            floor_nodes,
            prefer_burst_on_scale_down: settings.prefer_burst_on_scale_down,
            base_drain_needs_approval: settings.base_drain_needs_approval,
            burst_offers,
        })
    }
}

/// The offers of `provider` the settings allow: the provider is listed and
/// enabled, and the region and machine type are in its lists (an empty list
/// allows any).
pub fn allowed_offers(
    settings: &FleetSettings,
    provider: &str,
    offers: impl IntoIterator<Item = Offer>,
) -> Vec<Offer> {
    offers
        .into_iter()
        .filter(|o| settings.allows(provider, &o.region, &o.machine_type))
        .collect()
}

/// The floor: the `count` oldest base nodes, then the oldest others if
/// there aren't enough base nodes.
pub fn floor_nodes(count: u32, nodes: &[NodeLoad]) -> Vec<NodeId> {
    let mut sorted: Vec<&NodeLoad> = nodes.iter().collect();
    sorted.sort_by_key(|n| (n.kind != NodeKind::Base, n.created_at, n.id.clone()));
    sorted
        .into_iter()
        .take(count as usize)
        .map(|n| n.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use hallow_proto::fleet::ProviderSettings;

    use super::*;
    use crate::planner::NodePhase;
    use crate::provider::Cost;

    fn offer(region: &str, machine_type: &str) -> Offer {
        Offer {
            region: region.into(),
            machine_type: machine_type.into(),
            slots: 100,
            cost: Cost::hourly(Money::from_micros(50_000), None),
        }
    }

    #[test]
    fn defaults_make_a_policy() {
        let settings = FleetSettings::default();
        let policy = Policy::from_settings(&settings, vec!["floor".into()], vec![]).unwrap();
        assert_eq!(policy.timezone, chrono_tz::UTC);
        assert_eq!(policy.monthly_budget, Some(Money::from_units(100)));
        assert_eq!(policy.daily_cap, Some(Money::from_units(10)));
        assert_eq!(policy.scale_down_cooldown, Duration::from_secs(900));
        assert_eq!(policy.max_nodes, 10);
        assert!(policy.base_drain_needs_approval);
    }

    #[test]
    fn settings_carry_over() {
        let settings = FleetSettings {
            timezone: "Europe/Berlin".into(),
            monthly_budget: None,
            daily_cap: Some("12.50".into()),
            queue_wait_target_secs: 30,
            base_drain_needs_approval: false,
            ..FleetSettings::default()
        };
        let policy = Policy::from_settings(&settings, vec![], vec![]).unwrap();
        assert_eq!(policy.timezone, chrono_tz::Europe::Berlin);
        assert_eq!(policy.monthly_budget, None);
        assert_eq!(policy.daily_cap, Some(Money::from_cents(1250)));
        assert_eq!(policy.queue_wait_target, Duration::from_secs(30));
        assert!(!policy.base_drain_needs_approval);
    }

    #[test]
    fn invalid_settings_are_refused() {
        let settings = FleetSettings {
            timezone: "Atlantis/Capital".into(),
            ..FleetSettings::default()
        };
        let err = Policy::from_settings(&settings, vec![], vec![]).unwrap_err();
        assert!(err.0.contains_key("timezone"));
    }

    #[test]
    fn offers_are_filtered_by_provider_region_and_type() {
        let settings = FleetSettings {
            providers: vec![
                ProviderSettings {
                    name: "hetzner-cloud".into(),
                    enabled: true,
                    regions: vec!["fsn1".into(), "nbg1".into()],
                    machine_types: vec!["cpx41".into()],
                },
                ProviderSettings {
                    name: "ovh-cloud".into(),
                    enabled: false,
                    regions: vec![],
                    machine_types: vec![],
                },
            ],
            ..FleetSettings::default()
        };
        let offers = vec![
            offer("fsn1", "cpx41"),
            offer("fsn1", "cpx51"),
            offer("hel1", "cpx41"),
            offer("nbg1", "cpx41"),
        ];
        let allowed = allowed_offers(&settings, "hetzner-cloud", offers.clone());
        assert_eq!(allowed, [offer("fsn1", "cpx41"), offer("nbg1", "cpx41")]);
        assert!(allowed_offers(&settings, "ovh-cloud", offers.clone()).is_empty());
        assert!(allowed_offers(&settings, "unlisted", offers).is_empty());
    }

    #[test]
    fn the_floor_is_the_oldest_base_nodes() {
        let node = |id: &str, kind, day| NodeLoad {
            id: id.into(),
            kind,
            region: "fsn1".into(),
            phase: NodePhase::Ready,
            slots: 100,
            used_slots: 0,
            players: 0,
            cost: Cost::monthly(Money::from_units(40)),
            created_at: Utc.with_ymd_and_hms(2026, 10, day, 0, 0, 0).unwrap(),
            draining_since: None,
        };
        let nodes = vec![
            node("burst-old", NodeKind::Burst, 1),
            node("base-new", NodeKind::Base, 9),
            node("base-old", NodeKind::Base, 2),
        ];
        assert_eq!(floor_nodes(1, &nodes), [NodeId::from("base-old")]);
        assert_eq!(
            floor_nodes(3, &nodes),
            [
                NodeId::from("base-old"),
                NodeId::from("base-new"),
                NodeId::from("burst-old")
            ]
        );
    }
}
