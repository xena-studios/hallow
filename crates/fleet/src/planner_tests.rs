//! One or more tests per planner rule. The simulated day is in
//! `tests/simulated_peak.rs`.

use super::*;
use chrono::TimeZone;

const MIN: Duration = Duration::from_secs(60);

fn t(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 15, h, m, 0).unwrap()
}

fn mins(n: u64) -> Duration {
    MIN * n as u32
}

fn burst_cost() -> Cost {
    Cost::hourly(Money::from_micros(50_000), Some(Money::from_units(30)))
}

fn policy() -> Policy {
    Policy {
        headroom_slots: 40,
        queue_wait_target: Duration::from_secs(60),
        scale_up_after: mins(3),
        scale_down_after: mins(10),
        scale_up_cooldown: mins(5),
        scale_down_cooldown: mins(15),
        drain_deadline: mins(20),
        billing_step_margin: mins(5),
        max_nodes: 10,
        monthly_budget: Money::from_units(500),
        daily_cap: Money::from_units(40),
        tighten_from: 0.8,
        floor_nodes: vec![NodeId::from("floor")],
        prefer_burst_on_scale_down: true,
        burst_offers: vec![Offer {
            region: "fsn1".into(),
            machine_type: "cpx41".into(),
            slots: 100,
            cost: burst_cost(),
        }],
    }
}

fn floor(used: u32) -> NodeLoad {
    NodeLoad {
        id: "floor".into(),
        kind: NodeKind::Base,
        region: "fsn1".into(),
        phase: NodePhase::Ready,
        slots: 100,
        used_slots: used,
        players: used,
        cost: Cost::monthly(Money::from_units(40)),
        created_at: Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
        draining_since: None,
    }
}

fn burst(id: &str, phase: NodePhase, used: u32) -> NodeLoad {
    NodeLoad {
        id: id.into(),
        kind: NodeKind::Burst,
        region: "fsn1".into(),
        phase,
        slots: 100,
        used_slots: used,
        players: used,
        cost: burst_cost(),
        created_at: t(10, 0),
        draining_since: (phase == NodePhase::Draining).then(|| t(10, 0)),
    }
}

fn snap(now: DateTime<Utc>, nodes: Vec<NodeLoad>) -> Snapshot {
    Snapshot {
        now,
        nodes,
        queues: vec![],
        spend: Spend::default(),
    }
}

fn buys(plan: &Plan) -> usize {
    plan.decisions
        .iter()
        .filter(|d| matches!(d, Decision::Buy { .. }))
        .count()
}

fn drains(plan: &Plan) -> Vec<&NodeId> {
    plan.decisions
        .iter()
        .filter_map(|d| match d {
            Decision::Drain { node, .. } => Some(node),
            _ => None,
        })
        .collect()
}

fn cancels(plan: &Plan) -> Vec<&NodeId> {
    plan.decisions
        .iter()
        .filter_map(|d| match d {
            Decision::Cancel { node, .. } => Some(node),
            _ => None,
        })
        .collect()
}

fn holds(plan: &Plan) -> Vec<&Reason> {
    plan.decisions
        .iter()
        .filter_map(|d| match d {
            Decision::Hold { reason, .. } => Some(reason),
            _ => None,
        })
        .collect()
}

fn pressured_since(at: DateTime<Utc>) -> Memory {
    Memory {
        pressure_since: Some(at),
        ..Memory::default()
    }
}

fn surplus_since(at: DateTime<Utc>) -> Memory {
    Memory {
        surplus_since: Some(at),
        ..Memory::default()
    }
}

// Scale up.

#[test]
fn buys_after_headroom_stays_low_for_the_sustain_time() {
    let policy = policy();
    let nodes = vec![floor(80)]; // 20 free, target 40
    let p0 = plan(&snap(t(12, 0), nodes.clone()), &policy, &Memory::default());
    assert_eq!(buys(&p0), 0);
    assert!(matches!(holds(&p0)[..], [Reason::WaitingToScaleUp { .. }]));
    assert_eq!(p0.memory.pressure_since, Some(t(12, 0)));

    let p2 = plan(&snap(t(12, 2), nodes.clone()), &policy, &p0.memory);
    assert_eq!(buys(&p2), 0);

    let p3 = plan(&snap(t(12, 3), nodes), &policy, &p2.memory);
    assert_eq!(buys(&p3), 1);
    let Decision::Buy {
        region,
        machine_type,
        slots,
        cost,
        reason,
    } = &p3.decisions[0]
    else {
        panic!("{:?}", p3.decisions);
    };
    assert_eq!(
        (region.as_str(), machine_type.as_str(), *slots),
        ("fsn1", "cpx41", 100)
    );
    assert_eq!(*cost, burst_cost());
    assert_eq!(
        *reason,
        Reason::LowHeadroom {
            free: 20,
            target: 40
        }
    );
    assert_eq!(p3.memory.last_buy, Some(t(12, 3)));
    assert_eq!(
        p3.memory.pressure_since, None,
        "sustain starts over after acting"
    );
}

#[test]
fn buys_enough_nodes_for_the_deficit() {
    let mut policy = policy();
    policy.headroom_slots = 250;
    let p = plan(
        &snap(t(12, 10), vec![floor(100)]),
        &policy,
        &pressured_since(t(12, 0)),
    );
    assert_eq!(buys(&p), 3);
}

#[test]
fn picks_the_cheapest_offer_per_slot() {
    let mut policy = policy();
    policy.burst_offers.push(Offer {
        region: "nbg1".into(),
        machine_type: "cpx51".into(),
        slots: 250,
        cost: Cost::hourly(Money::from_micros(100_000), None), // cheaper per slot
    });
    let p = plan(
        &snap(t(12, 10), vec![floor(90)]),
        &policy,
        &pressured_since(t(12, 0)),
    );
    assert!(
        matches!(&p.decisions[..], [Decision::Buy { machine_type, .. }] if machine_type == "cpx51")
    );
}

#[test]
fn a_long_queue_buys_even_with_headroom() {
    let policy = policy();
    let mut s = snap(t(12, 10), vec![floor(40)]); // 60 free, above target
    s.queues.push(QueueWait {
        game: "skywars".into(),
        wait: Duration::from_secs(90),
    });
    let p = plan(&s, &policy, &pressured_since(t(12, 0)));
    assert_eq!(buys(&p), 1);
    assert!(matches!(
        &p.decisions[0],
        Decision::Buy { reason: Reason::QueueWait { game, .. }, .. } if game == "skywars"
    ));
}

#[test]
fn a_short_spike_buys_nothing() {
    let policy = policy();
    let p0 = plan(
        &snap(t(12, 0), vec![floor(95)]),
        &policy,
        &Memory::default(),
    );
    let p1 = plan(&snap(t(12, 2), vec![floor(95)]), &policy, &p0.memory);
    let p2 = plan(&snap(t(12, 3), vec![floor(30)]), &policy, &p1.memory); // spike over
    assert_eq!(p2.memory.pressure_since, None);
    let p3 = plan(&snap(t(12, 4), vec![floor(95)]), &policy, &p2.memory);
    assert_eq!(p3.memory.pressure_since, Some(t(12, 4)), "sustain restarts");
    assert_eq!(buys(&p0) + buys(&p1) + buys(&p2) + buys(&p3), 0);
}

#[test]
fn cooldown_blocks_another_buy() {
    let policy = policy();
    let memory = Memory {
        pressure_since: Some(t(12, 0)),
        last_buy: Some(t(12, 8)),
        ..Memory::default()
    };
    let p = plan(&snap(t(12, 10), vec![floor(100)]), &policy, &memory);
    assert_eq!(buys(&p), 0);
    assert_eq!(holds(&p), vec![&Reason::Cooldown { until: t(12, 13) }]);
}

#[test]
fn slots_still_setting_up_count_as_free() {
    let policy = policy();
    let p = plan(
        &snap(
            t(12, 10),
            vec![floor(100), burst("b1", NodePhase::SettingUp, 0)],
        ),
        &policy,
        &pressured_since(t(12, 0)),
    );
    assert_eq!(p.free_slots, 100);
    assert!(p.decisions.is_empty(), "{:?}", p.decisions);
}

#[test]
fn reinstates_a_draining_node_instead_of_buying() {
    let policy = policy();
    let nodes = vec![floor(100), burst("b1", NodePhase::Draining, 0)];
    let p = plan(
        &snap(t(10, 58), nodes),
        &policy,
        &pressured_since(t(10, 50)),
    );
    assert_eq!(buys(&p), 0);
    assert!(matches!(&p.decisions[..], [Decision::Reinstate { node, .. }] if node.0 == "b1"));
    assert!(
        cancels(&p).is_empty(),
        "a reinstated node isn't also cancelled"
    );
}

#[test]
fn stops_at_max_nodes() {
    let mut policy = policy();
    policy.max_nodes = 2;
    policy.headroom_slots = 250;
    let nodes = vec![floor(100), burst("b1", NodePhase::Ready, 100)];
    let p = plan(&snap(t(12, 10), nodes), &policy, &pressured_since(t(12, 0)));
    assert_eq!(buys(&p), 0);
    assert_eq!(holds(&p), vec![&Reason::MaxNodes { max: 2 }]);
}

#[test]
fn no_offer_holds() {
    let mut policy = policy();
    policy.burst_offers.clear();
    let p = plan(
        &snap(t(12, 10), vec![floor(100)]),
        &policy,
        &pressured_since(t(12, 0)),
    );
    assert_eq!(holds(&p), vec![&Reason::NoOffer]);
}

// Scale down.

#[test]
fn drains_burst_before_base_then_fewest_players() {
    let policy = policy();
    let mut base = burst("base1", NodePhase::Ready, 5);
    base.kind = NodeKind::Base;
    base.cost = Cost::monthly(Money::from_units(40));
    let nodes = vec![
        floor(10),
        base,
        burst("x", NodePhase::Ready, 30),
        burst("y", NodePhase::Ready, 20),
    ];
    let p = plan(&snap(t(12, 10), nodes), &policy, &surplus_since(t(12, 0)));
    assert_eq!(drains(&p), vec![&NodeId::from("y")]);
    assert_eq!(p.memory.last_drain, Some(t(12, 10)));
    assert!(matches!(
        &p.decisions[0],
        Decision::Drain {
            reason: Reason::Surplus {
                free_after: 235,
                target: 40
            },
            ..
        }
    ));
}

#[test]
fn drains_base_first_when_burst_preference_is_off() {
    let mut policy = policy();
    policy.prefer_burst_on_scale_down = false;
    let mut base = burst("base1", NodePhase::Ready, 5);
    base.kind = NodeKind::Base;
    let nodes = vec![floor(10), base, burst("x", NodePhase::Ready, 30)];
    let p = plan(&snap(t(12, 10), nodes), &policy, &surplus_since(t(12, 0)));
    assert_eq!(
        drains(&p),
        vec![&NodeId::from("base1")],
        "fewest players wins"
    );
}

#[test]
fn surplus_must_hold_before_draining() {
    let policy = policy();
    let nodes = vec![floor(10), burst("x", NodePhase::Ready, 10)];
    let p = plan(&snap(t(12, 0), nodes.clone()), &policy, &Memory::default());
    assert!(drains(&p).is_empty());
    assert!(matches!(holds(&p)[..], [Reason::WaitingToScaleDown { .. }]));
    let p = plan(&snap(t(12, 10), nodes), &policy, &p.memory);
    assert_eq!(drains(&p), vec![&NodeId::from("x")]);
}

#[test]
fn never_drains_capacity_still_needed() {
    let policy = policy();
    // 50 + 70 free; without either burst node only 20 would stay free.
    let nodes = vec![floor(50), burst("x", NodePhase::Ready, 30)];
    let p = plan(&snap(t(12, 10), nodes), &policy, &surplus_since(t(12, 0)));
    assert!(p.decisions.is_empty(), "{:?}", p.decisions);
    assert_eq!(p.memory.surplus_since, None);
}

#[test]
fn never_drains_the_floor() {
    let policy = policy();
    let p = plan(
        &snap(t(3, 0), vec![floor(0)]),
        &policy,
        &surplus_since(t(1, 0)),
    );
    assert!(p.decisions.is_empty(), "{:?}", p.decisions);
}

#[test]
fn scale_down_waits_out_the_cooldown_after_a_buy_or_drain() {
    let policy = policy();
    let nodes = vec![floor(10), burst("x", NodePhase::Ready, 10)];
    let after_buy = Memory {
        surplus_since: Some(t(12, 0)),
        last_buy: Some(t(12, 5)),
        ..Memory::default()
    };
    let p = plan(&snap(t(12, 10), nodes.clone()), &policy, &after_buy);
    assert!(drains(&p).is_empty());
    assert_eq!(holds(&p), vec![&Reason::Cooldown { until: t(12, 20) }]);

    let after_drain = Memory {
        surplus_since: Some(t(12, 0)),
        last_drain: Some(t(12, 1)),
        ..Memory::default()
    };
    let p = plan(&snap(t(12, 10), nodes), &policy, &after_drain);
    assert_eq!(holds(&p), vec![&Reason::Cooldown { until: t(12, 16) }]);
}

// Drain, then cancel.

#[test]
fn keeps_an_empty_burst_node_until_just_before_its_billing_step() {
    let policy = policy();
    let nodes = vec![floor(0), burst("b1", NodePhase::Draining, 0)];
    let p = plan(&snap(t(10, 20), nodes.clone()), &policy, &Memory::default());
    assert!(cancels(&p).is_empty());
    assert_eq!(
        holds(&p),
        vec![&Reason::KeepUntilBillingStep {
            cancel_at: t(10, 55)
        }]
    );

    let p = plan(&snap(t(10, 55), nodes), &policy, &Memory::default());
    assert_eq!(cancels(&p), vec![&NodeId::from("b1")]);
    assert!(matches!(
        &p.decisions[0],
        Decision::Cancel {
            reason: Reason::Empty,
            needs_approval: false,
            ..
        }
    ));
}

#[test]
fn draining_node_with_players_waits_for_its_games_until_the_deadline() {
    let policy = policy();
    let nodes = vec![floor(0), burst("b1", NodePhase::Draining, 3)];
    let p = plan(&snap(t(10, 10), nodes.clone()), &policy, &Memory::default());
    assert!(cancels(&p).is_empty());
    let p = plan(&snap(t(10, 20), nodes), &policy, &Memory::default());
    assert!(matches!(
        &p.decisions[..],
        [Decision::Cancel {
            reason: Reason::DrainDeadline { players: 3 },
            ..
        }]
    ));
}

#[test]
fn cancelling_a_base_node_needs_approval() {
    let policy = policy();
    let mut base = burst("base1", NodePhase::Draining, 0);
    base.kind = NodeKind::Base;
    base.cost = Cost::monthly(Money::from_units(40));
    let p = plan(
        &snap(t(10, 5), vec![floor(0), base]),
        &policy,
        &Memory::default(),
    );
    assert!(matches!(
        &p.decisions[..],
        [Decision::Cancel {
            needs_approval: true,
            ..
        }]
    ));
}

#[test]
fn a_draining_floor_node_is_never_cancelled() {
    let policy = policy();
    let mut f = floor(0);
    f.phase = NodePhase::Draining;
    f.draining_since = Some(t(1, 0));
    let p = plan(&snap(t(12, 0), vec![f]), &policy, &Memory::default());
    assert!(cancels(&p).is_empty());
    assert!(holds(&p).contains(&&Reason::FloorNode));
}

// Budgets.

#[test]
fn the_daily_cap_stops_buying_and_cuts_nothing() {
    let policy = policy();
    let nodes = vec![floor(100), burst("b1", NodePhase::Ready, 100)];
    let mut s = snap(t(12, 10), nodes);
    s.spend.today = Money::from_units(39);
    let p = plan(&s, &policy, &pressured_since(t(12, 0)));
    assert_eq!(buys(&p), 0);
    assert!(drains(&p).is_empty() && cancels(&p).is_empty());
    assert!(
        matches!(holds(&p)[..], [Reason::DailyCap { .. }]),
        "{:?}",
        p.decisions
    );
}

#[test]
fn the_monthly_budget_stops_buying() {
    let policy = policy();
    let mut s = snap(t(12, 10), vec![floor(100)]);
    s.spend.month = Money::from_units(495);
    // Headroom has shrunk to zero by now; a long queue still asks for a node.
    s.queues.push(QueueWait {
        game: "skywars".into(),
        wait: Duration::from_secs(300),
    });
    let p = plan(&s, &policy, &pressured_since(t(12, 0)));
    assert_eq!(p.headroom_target, 0);
    assert_eq!(buys(&p), 0);
    assert!(
        matches!(holds(&p)[..], [Reason::MonthlyBudget { .. }]),
        "{:?}",
        p.decisions
    );
}

#[test]
fn headroom_shrinks_as_the_forecast_nears_the_budget() {
    let policy = policy();
    let mut s = snap(t(12, 0), vec![floor(0)]);
    assert_eq!(plan(&s, &policy, &Memory::default()).headroom_target, 40);

    // 14.5 days into a 30-day month: the trend forecast is spend * 30 / 14.5.
    s.spend.month = Money::from_units(500).scale(0.9 * 14.5 / 30.0);
    let p = plan(&s, &policy, &Memory::default());
    assert!((p.forecast.as_f64() - 450.0).abs() < 0.01, "{}", p.forecast);
    assert_eq!(p.headroom_target, 20);

    s.spend.month = Money::from_units(500).scale(14.5 / 30.0);
    assert_eq!(plan(&s, &policy, &Memory::default()).headroom_target, 0);
}

#[test]
fn forecast_counts_base_nodes_for_the_rest_of_the_month() {
    let policy = policy();
    // 15.5 days left of November: 372 hours of a €40/month node.
    let p = plan(&snap(t(12, 0), vec![floor(0)]), &policy, &Memory::default());
    let expected = Cost::monthly(Money::from_units(40)).per_hour().scale(372.0);
    assert_eq!(p.forecast, expected);
}

#[test]
fn decisions_read_as_audit_log_lines() {
    let d = Decision::Cancel {
        node: "b1".into(),
        reason: Reason::Empty,
        needs_approval: true,
    };
    assert_eq!(d.to_string(), "cancel b1: empty (needs approval)");
    let d = Decision::Hold {
        node: None,
        reason: Reason::DailyCap {
            projected: Money::from_units(41),
            cap: Money::from_units(40),
        },
    };
    assert_eq!(
        d.to_string(),
        "hold: at the daily cap: today would reach 41.0000 of 40.0000"
    );
}
