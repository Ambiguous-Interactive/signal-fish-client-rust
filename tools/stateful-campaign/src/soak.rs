//! Long-horizon churn probe: 40 cycles of issue-#166-style storms
//! (reconnect-announcement floods, uncoverable departures, gap-range
//! retention floods, generation churn) checking violation surfacing, queue
//! bounds, terminal behavior, and per-frame wall time under every policy.

use std::panic::{catch_unwind, AssertUnwindSafe};

use signal_fish_client::client::ProtocolViolationPolicy;
use signal_fish_client::protocol::{
    DeliveryGap, DeliveryGapReason, DeliveryReportPayload, PlayerId, PlayerInfo, RateLimitInfo,
    ServerMessage,
};

use crate::gen::{self, Ctx};
use crate::run::{run_prefix, Finding};
use crate::script::{Cmd, ConfigKind, Script, Step};

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

/// Run the churn probe under all three policies; returns the failure count.
pub fn soak_probe() -> usize {
    let _self_id = PlayerId::from_u128(0x50A1);
    let peer = PlayerId::from_u128(0x50A2);

    let mut steps: Vec<Step> = Vec::new();
    steps.push(Step::Deliver(
        ServerMessage::Authenticated {
            app_name: "soak".into(),
            organization: None,
            rate_limits: RateLimitInfo {
                per_minute: 1,
                per_hour: 1,
                per_day: 1,
            },
        },
        Default::default(),
    ));
    steps.push(Step::Poll(1));
    let (info, info_meta) = gen::canary_protocol_info();
    steps.push(Step::Deliver(info, info_meta));
    steps.push(Step::Poll(1));
    steps.push(Step::Cmd(Cmd::JoinRoom));
    steps.push(Step::Poll(1));
    steps.push(Step::Deliver(
        gen::canary_room_joined(&Ctx::new_for_canary()),
        Default::default(),
    ));
    steps.push(Step::Poll(1));
    steps.push(Step::Deliver(
        ServerMessage::PlayerJoined {
            player: PlayerInfo {
                id: peer,
                name: "soak-peer".into(),
                is_authority: false,
                is_ready: false,
                connected_at: "2026".into(),
                connection_info: None,
                epoch: Some(1),
                seq: Some(0),
            },
        },
        Default::default(),
    ));
    steps.push(Step::Poll(1));

    // 40 full churn cycles: reconnect-announcement floods + departure floods
    // (each cycle would grow unboundedly without the #166 bounds).
    for cycle in 0..40u32 {
        let base = 2u32.wrapping_add(cycle.wrapping_mul(20));
        for offset in 0..18u32 {
            let epoch = base.wrapping_add(offset);
            steps.push(Step::Deliver(
                ServerMessage::PlayerReconnected {
                    player_id: peer,
                    epoch: Some(epoch),
                },
                Default::default(),
            ));
            steps.push(Step::Deliver(
                ServerMessage::PlayerLeft {
                    player_id: peer,
                    epoch: Some(epoch),
                    final_seq: Some(u64::MAX),
                },
                Default::default(),
            ));
        }
        if cycle % 8 == 0 {
            steps.push(Step::Cmd(Cmd::Ping));
            steps.push(Step::Poll(1));
            steps.push(Step::Deliver(ServerMessage::Pong, Default::default()));
            steps.push(Step::Poll(1));
        }
    }

    // Gap-range flood: 30 reports x 100 singleton ranges with matching
    // cumulative counters (3000 retained ranges without the 1024 bound).
    let mut superseded: u64 = 0;
    for report in 0..30u64 {
        let gaps: Vec<DeliveryGap> = (0..100u64)
            .map(|i| {
                let seq = report.wrapping_mul(100).wrapping_add(i).wrapping_add(1);
                DeliveryGap {
                    from_player: peer,
                    epoch: 1,
                    from_seq: seq,
                    to_seq: seq,
                    reason: DeliveryGapReason::LatestSuperseded,
                }
            })
            .collect();
        superseded = superseded.wrapping_add(100);
        steps.push(Step::Deliver(
            ServerMessage::DeliveryReport(Box::new(DeliveryReportPayload {
                gaps,
                per_class: signal_fish_client::protocol::DeliveryCountersByClass {
                    reliable: Default::default(),
                    latest: signal_fish_client::protocol::LatestDeliveryCounters {
                        superseded,
                        ..Default::default()
                    },
                    volatile: Default::default(),
                },
            })),
            Default::default(),
        ));
        steps.push(Step::Poll(1));
    }

    // Generation churn beyond the 8-superseded fence.
    for k in 0..40u128 {
        steps.push(Step::Deliver(
            ServerMessage::SessionPlan(Box::new(
                signal_fish_client::protocol::SessionPlanPayload {
                    generation: Some(signal_fish_client::protocol::RoomId::from_u128(
                        0x50A4_u128.wrapping_add(k),
                    )),
                    topology: signal_fish_client::protocol::Topology::Relay,
                    transport: signal_fish_client::protocol::TransportKind::Relay,
                    host: None,
                    direct_endpoint: None,
                    peers: vec![],
                    ice_servers: vec![],
                    fallback: signal_fish_client::protocol::TransportKind::Relay,
                },
            )),
            Default::default(),
        ));
        steps.push(Step::Poll(1));
    }

    let mut failures: usize = 0;
    for policy in [
        ProtocolViolationPolicy::Quarantine,
        ProtocolViolationPolicy::Observe,
        ProtocolViolationPolicy::Disconnect,
    ] {
        let script = Script {
            seed: 0,
            index: 9999,
            archetype: "soak",
            config_kind: ConfigKind::V3,
            echo_room_ops: false,
            small_command_capacity: None,
            steps: steps.clone(),
        };
        let started = std::time::Instant::now();
        let outcome =
            match catch_unwind(AssertUnwindSafe(|| run_prefix(&script, policy, usize::MAX))) {
                Ok(outcome) => outcome,
                Err(panic) => {
                    println!("soak {policy:?}: PANIC {}", panic_message(&panic));
                    failures = failures.saturating_add(1);
                    continue;
                }
            };
        let wall = started.elapsed();
        println!(
            "soak {policy:?}: {} frames, {} events, {} violations, terminal={}, {:.1}ms ({:.2}us/frame)",
            outcome.frames_fed,
            outcome.events_seen,
            outcome.violations,
            outcome.terminal,
            wall.as_secs_f32() * 1000.0,
            wall.as_micros() as f64 / usize::max(outcome.frames_fed, 1) as f64
        );
        for finding in &outcome.findings {
            let Finding {
                category,
                detail,
                step_index: _,
            } = finding;
            println!("  FINDING: {category} — {detail}");
        }
        if !outcome.findings.is_empty() {
            failures = failures.saturating_add(1);
        }
        if outcome.violations == 0 {
            println!("  FINDING: soak floods produced zero violations");
            failures = failures.saturating_add(1);
        }
    }
    failures
}
