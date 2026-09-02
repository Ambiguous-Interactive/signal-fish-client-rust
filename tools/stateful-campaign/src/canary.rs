//! Oracle canaries: known-good and known-bad streams that prove the oracle
//! detects the defect classes it claims to detect.
//!
//! Two families run under `--selftest` and as unit tests:
//!
//! 1. end-to-end scripted journeys with exact event-count assertions,
//! 2. direct-feed oracle-rejection canaries: synthetic event/step sequences
//!    fed straight into the oracle entry points, each of which the strict
//!    oracle must REJECT, plus a broken-oracle sensitivity proof.

use signal_fish_client::client::ProtocolViolationPolicy;
use signal_fish_client::event::SignalFishEvent;
use signal_fish_client::protocol::{PlayerId, RateLimitInfo};

use crate::gen::{self, Ctx};
use crate::run::{run_prefix, run_prefix_verbose, set_oracle_neutered, Oracle};
use crate::script::{Cmd, ConfigKind, Script, Step};

struct DirectFeedVerdict {
    name: &'static str,
    verdict: Result<(), String>,
}

fn fed(name: &'static str, feed: impl FnOnce() -> Result<(), String>) -> DirectFeedVerdict {
    DirectFeedVerdict {
        name,
        verdict: feed(),
    }
}

fn ev_connected() -> SignalFishEvent {
    SignalFishEvent::Connected
}

fn ev_disconnected(reason: &str) -> SignalFishEvent {
    SignalFishEvent::Disconnected {
        reason: Some(reason.to_string()),
        last_server_error: None,
    }
}

fn ev_violation(diagnostic: &str) -> SignalFishEvent {
    SignalFishEvent::ProtocolViolation {
        kind: signal_fish_client::ProtocolViolationKind::Lifecycle,
        diagnostic: diagnostic.to_string(),
    }
}

fn ev_authenticated() -> SignalFishEvent {
    SignalFishEvent::Authenticated {
        app_name: "canary".into(),
        organization: None,
        rate_limits: RateLimitInfo {
            per_minute: 1,
            per_hour: 2,
            per_day: 3,
        },
    }
}

fn ev_protocol_info_v3() -> SignalFishEvent {
    SignalFishEvent::ProtocolInfo(signal_fish_client::protocol::ProtocolInfoPayload {
        platform: None,
        sdk_version: None,
        minimum_version: None,
        recommended_version: None,
        capabilities: vec![],
        notes: None,
        game_data_formats: vec![signal_fish_client::protocol::GameDataEncoding::Json],
        player_name_rules: None,
        protocol_version: Some(3),
        min_protocol_version: Some(2),
        max_protocol_version: Some(3),
        transports: None,
        max_outbound_message_size: None,
    })
}

fn ev_game_data() -> SignalFishEvent {
    SignalFishEvent::GameData {
        from_player: PlayerId::from_u128(0xCA7),
        data: serde_json::json!(null),
        seq: Some(1),
        epoch: Some(1),
        class: Some(signal_fish_client::protocol::DeliveryClass::Reliable),
        key: None,
    }
}

fn ev_room_joined() -> SignalFishEvent {
    SignalFishEvent::RoomJoined {
        room_id: PlayerId::from_u128(0x20),
        room_code: "CANARY".into(),
        player_id: PlayerId::from_u128(0x10),
        game_name: "canary".into(),
        max_players: 4,
        supports_authority: false,
        current_players: vec![],
        is_authority: false,
        lobby_state: signal_fish_client::protocol::LobbyState::Waiting,
        ready_players: vec![],
        relay_type: "websocket".into(),
        current_spectators: vec![],
        ice_servers: vec![],
        reconnection_token: None,
    }
}

fn ev_player_joined() -> SignalFishEvent {
    SignalFishEvent::PlayerJoined {
        player: signal_fish_client::protocol::PlayerInfo {
            id: PlayerId::from_u128(0x11),
            name: "peer".into(),
            is_authority: false,
            is_ready: false,
            connected_at: "2026".into(),
            connection_info: None,
            epoch: Some(1),
            seq: Some(0),
        },
    }
}

fn ev_signal_received() -> SignalFishEvent {
    SignalFishEvent::SignalReceived {
        from: PlayerId::from_u128(0x11),
        generation: None,
        signal: serde_json::json!({ "Offer": "v=0 canary" }),
    }
}

fn snap_in_room(quarantined: bool) -> signal_fish_client::ClientSnapshot {
    signal_fish_client::ClientSnapshot {
        connected: true,
        transport_ready: true,
        authenticated: true,
        room_role: Some(signal_fish_client::RoomRole::Player),
        player_id: Some(PlayerId::from_u128(0x10)),
        room_id: Some(PlayerId::from_u128(0x20)),
        room_code: Some("CANARY".into()),
        quarantined,
        ..Default::default()
    }
}

/// All direct-feed oracle-rejection canaries. Run once under the strict oracle
/// (every verdict must be `Err`) and once under the deliberately-broken oracle
/// (at least one verdict must flip to `Ok`, proving canary sensitivity).
fn run_direct_feed_canaries() -> Vec<DirectFeedVerdict> {
    use signal_fish_client::ClientSnapshot;
    let new_oracle = || Oracle::new(ProtocolViolationPolicy::Quarantine, ConfigKind::V3, false);
    vec![
        // 1. Phase-illegal event: game data before authentication/membership.
        fed("phase_illegal_game_data", || {
            let mut oracle = new_oracle();
            oracle.observe(&ev_connected())?;
            oracle.observe(&ev_game_data())
        }),
        // 2. Phase-illegal event: v3 signal before any SessionPlan.
        fed("phase_illegal_signal_before_plan", || {
            let mut oracle = new_oracle();
            oracle.observe(&ev_connected())?;
            oracle.observe(&ev_authenticated())?;
            oracle.observe(&ev_protocol_info_v3())?;
            oracle.observe(&ev_room_joined())?;
            oracle.observe(&ev_signal_received())
        }),
        // 3. Post-terminal event: a roster update after the terminal
        //    Disconnected must be rejected outright.
        fed("post_terminal_event", || {
            let mut oracle = new_oracle();
            oracle.peer_close_armed = true;
            oracle.observe(&ev_connected())?;
            oracle.observe(&ev_disconnected(
                "closed by server: code=Some(1001), reason=Some(\"server going away\")",
            ))?;
            oracle.observe(&ev_player_joined())
        }),
        // 4. Duplicate Connected event.
        fed("duplicate_connected", || {
            let mut oracle = new_oracle();
            oracle.observe(&ev_connected())?;
            oracle.observe(&ev_connected())
        }),
        // 5. Duplicate Disconnected event.
        fed("duplicate_disconnected", || {
            let mut oracle = new_oracle();
            oracle.peer_close_armed = true;
            oracle.observe(&ev_connected())?;
            oracle.observe(&ev_disconnected(
                "closed by server: code=Some(1001), reason=Some(\"server going away\")",
            ))?;
            oracle.observe(&ev_disconnected(
                "closed by server: code=Some(1001), reason=Some(\"server going away\")",
            ))
        }),
        // 6. Snapshot incoherence: in a room but player id missing.
        fed("snapshot_incoherence_missing_player_id", || {
            let oracle = new_oracle();
            oracle.check_snapshot(&ClientSnapshot {
                connected: true,
                transport_ready: true,
                authenticated: true,
                room_role: Some(signal_fish_client::RoomRole::Player),
                player_id: None,
                room_id: Some(PlayerId::from_u128(0x20)),
                quarantined: false,
                ..Default::default()
            })
        }),
        // 7. Snapshot incoherence: disconnected but transport still ready.
        fed("snapshot_incoherence_disconnected_ready", || {
            let oracle = Oracle::new(ProtocolViolationPolicy::Observe, ConfigKind::V3, false);
            oracle.check_snapshot(&ClientSnapshot {
                connected: false,
                transport_ready: true,
                ..Default::default()
            })
        }),
        // 8. Quarantine latch: a violation under Quarantine policy with a
        //    snapshot that never latched `quarantined` must be a finding (the
        //    "Quarantine behaves like Observe" client bug).
        fed("quarantine_flag_never_latched", || {
            let mut oracle = new_oracle();
            oracle.observe(&ev_connected())?;
            oracle.observe(&ev_authenticated())?;
            oracle.observe(&ev_protocol_info_v3())?;
            oracle.observe(&ev_room_joined())?;
            oracle.observe(&ev_violation("lifecycle violation: canary"))?;
            oracle.check_snapshot(&snap_in_room(false))?;
            // Positive control: the correctly-latched snapshot is accepted.
            oracle.check_snapshot(&snap_in_room(true))
        }),
        // 9. Close-info misattribution: a violation teardown reported with the
        //    server-close reason instead of "protocol violation".
        fed("close_misattribution_violation_as_peer_close", || {
            let mut oracle =
                Oracle::new(ProtocolViolationPolicy::Disconnect, ConfigKind::V3, false);
            oracle.observe(&ev_connected())?;
            oracle.observe(&ev_authenticated())?;
            oracle.observe(&ev_violation("lifecycle violation: canary teardown"))?;
            oracle.observe(&ev_disconnected(
                "closed by server: code=Some(1001), reason=Some(\"server going away\")",
            ))?;
            oracle.verify_close_attribution(false)?;
            // Positive control: the correct cause string is accepted.
            let mut good = Oracle::new(ProtocolViolationPolicy::Disconnect, ConfigKind::V3, false);
            good.observe(&ev_connected())?;
            good.observe(&ev_authenticated())?;
            good.observe(&ev_violation("lifecycle violation: canary teardown"))?;
            good.observe(&ev_disconnected("protocol violation"))?;
            good.verify_close_attribution(false)
        }),
        // 10. Close-info misattribution: a peer close reported as a
        //     protocol-violation cause while the client behaved.
        fed("close_misattribution_peer_close_as_violation", || {
            let mut oracle = new_oracle();
            oracle.peer_close_armed = true;
            oracle.observe(&ev_connected())?;
            oracle.observe(&ev_disconnected("protocol violation"))?;
            oracle.verify_close_attribution(true)
        }),
        // 11. Close-info disagreement: server-close-formatted reason with no
        //     peer close armed in the transport (close_info() == None).
        fed("close_misattribution_close_info_disagrees", || {
            let mut oracle = new_oracle();
            oracle.transport_error_armed = true;
            oracle.expected_transport_reasons =
                vec![crate::transport::RECV_ERROR_DISPLAY.to_string()];
            oracle.observe(&ev_connected())?;
            oracle.observe(&ev_disconnected(
                "closed by server: code=Some(1001), reason=Some(\"server going away\")",
            ))?;
            oracle.verify_close_attribution(false)
        }),
        // 12. Terminal transport error: the Disconnected reason must be the
        //     armed terminal error verbatim.
        fed("transport_error_reason_mismatch", || {
            let mut oracle = new_oracle();
            oracle.arm_transport_error(true, false);
            oracle.observe(&ev_connected())?;
            oracle.observe(&ev_disconnected("some unrelated failure"))?;
            oracle.verify_close_attribution(false)?;
            // Positive control: the exact armed error string is accepted.
            let mut good = new_oracle();
            good.arm_transport_error(true, false);
            good.observe(&ev_connected())?;
            good.observe(&ev_disconnected(crate::transport::RECV_ERROR_DISPLAY))?;
            good.verify_close_attribution(false)
        }),
        // 13. Expectation swallow: a delivered Pong frame must surface its
        //     event; a silent batch leaves the slot pending (detectable), so
        //     the oracle "rejects" the swallow.
        fed("expectation_swallowed_pong", || {
            let mut oracle = crate::run::test_support::fresh_oracle(
                ProtocolViolationPolicy::Quarantine,
                ConfigKind::V3,
                false,
            );
            oracle
                .slots
                .push_back(crate::run::test_support::slot_for("Pong"));
            let mut findings = Vec::new();
            // The poll batch surfaced nothing (the swallow under test).
            oracle.reconcile_batch(&[], 0, 0, &mut findings);
            if !findings.is_empty() {
                return Err("empty batch was misreported as a mismatch".into());
            }
            if oracle.pending_slot_count() != 1 {
                return Err("pending Pong slot vanished without its event".into());
            }
            Err("strict oracle keeps the unresolved Pong slot pending (swallow detectable)".into())
        }),
        // 14. Expectation fabrication: an event with no delivered frame must
        //     be reported; failing to report it accepts the bad stream.
        fed("expectation_fabricated_event", || {
            let mut oracle = crate::run::test_support::fresh_oracle(
                ProtocolViolationPolicy::Quarantine,
                ConfigKind::V3,
                false,
            );
            let mut findings = Vec::new();
            oracle.reconcile_batch(&["Pong"], 0, 0, &mut findings);
            if findings
                .iter()
                .any(|finding| finding.category == "expectation-fabricated")
            {
                return Err("fabrication correctly reported".into());
            }
            Ok(())
        }),
    ]
}

/// Exact-event canaries. Each failure mode prints and counts.
pub fn selftest() -> usize {
    let mut failures: usize = 0;

    // Canary 1: clean v3 journey -> exact event stream, zero violations.
    {
        let mut steps = Vec::new();
        let mut ctx = Ctx::new_for_canary();
        steps.push(Step::Deliver(
            gen::canary_authenticated(),
            Default::default(),
        ));
        steps.push(Step::Poll(1));
        let (info, info_meta) = gen::canary_protocol_info();
        steps.push(Step::Deliver(info, info_meta));
        steps.push(Step::Poll(1));
        // Top-level room responses require an admitted join operation first.
        steps.push(Step::Cmd(Cmd::JoinRoom));
        steps.push(Step::Poll(1));
        steps.push(Step::Deliver(
            gen::canary_room_joined(&ctx),
            Default::default(),
        ));
        steps.push(Step::Poll(1));
        steps.push(Step::Deliver(
            gen::canary_player_joined(&ctx),
            Default::default(),
        ));
        steps.push(Step::Poll(1));
        let (data, data_meta) = gen::canary_game_data(&mut ctx);
        steps.push(Step::Deliver(data, data_meta));
        steps.push(Step::Poll(1));
        let script = Script {
            seed: 0,
            index: 0,
            archetype: "canary_clean",
            config_kind: ConfigKind::V3,
            echo_room_ops: false,
            small_command_capacity: None,
            steps,
        };
        let outcome = run_prefix_verbose(&script, ProtocolViolationPolicy::Quarantine, usize::MAX);
        if !outcome.findings.is_empty() || outcome.violations != 0 {
            println!(
                "canary_clean FAILED: {:?} violations={}",
                outcome.findings, outcome.violations
            );
            failures = failures.saturating_add(1);
        } else if outcome.events_seen != 6 {
            // Connected, Authenticated, ProtocolInfo, RoomJoined, PlayerJoined, GameData.
            println!(
                "canary_clean UNEXPECTED event count: {}",
                outcome.events_seen
            );
            failures = failures.saturating_add(1);
        }
    }

    // Canary 2: duplicate RoomJoined -> exactly one lifecycle violation.
    {
        let mut steps = Vec::new();
        let ctx = Ctx::new_for_canary();
        steps.push(Step::Deliver(
            gen::canary_authenticated(),
            Default::default(),
        ));
        steps.push(Step::Poll(1));
        let (info, info_meta) = gen::canary_protocol_info();
        steps.push(Step::Deliver(info, info_meta));
        steps.push(Step::Poll(1));
        steps.push(Step::Cmd(Cmd::JoinRoom));
        steps.push(Step::Poll(1));
        steps.push(Step::Deliver(
            gen::canary_room_joined(&ctx),
            Default::default(),
        ));
        steps.push(Step::Poll(1));
        // Second RoomJoined after the pending join was released: lifecycle-invalid.
        steps.push(Step::Deliver(
            gen::canary_room_joined(&ctx),
            Default::default(),
        ));
        steps.push(Step::Poll(1));
        let script = Script {
            seed: 0,
            index: 1,
            archetype: "canary_dup_join",
            config_kind: ConfigKind::V3,
            echo_room_ops: false,
            small_command_capacity: None,
            steps,
        };
        let outcome = run_prefix(&script, ProtocolViolationPolicy::Quarantine, usize::MAX);
        if outcome.violations != 1 {
            println!(
                "canary_dup_join FAILED: expected 1 violation, got {}",
                outcome.violations
            );
            failures = failures.saturating_add(1);
        }
        if outcome
            .findings
            .iter()
            .any(|finding| finding.category == "oracle-snapshot")
        {
            println!(
                "canary_dup_join oracle-snapshot noise: {:?}",
                outcome.findings
            );
            failures = failures.saturating_add(1);
        }
    }

    // Canary 3: Disconnect policy + second ProtocolInfo -> violation then Disconnected.
    {
        let mut steps = Vec::new();
        steps.push(Step::Deliver(
            gen::canary_authenticated(),
            Default::default(),
        ));
        steps.push(Step::Poll(1));
        let (info, info_meta) = gen::canary_protocol_info();
        steps.push(Step::Deliver(info.clone(), info_meta));
        steps.push(Step::Poll(1));
        steps.push(Step::Deliver(info, info_meta));
        steps.push(Step::Poll(1));
        let script = Script {
            seed: 0,
            index: 2,
            archetype: "canary_disconnect",
            config_kind: ConfigKind::V3,
            echo_room_ops: false,
            small_command_capacity: None,
            steps,
        };
        let outcome = run_prefix(&script, ProtocolViolationPolicy::Disconnect, usize::MAX);
        if !outcome.terminal || !outcome.violation_teardown || outcome.violations != 1 {
            println!(
                "canary_disconnect FAILED: terminal={} violation_teardown={} violations={}",
                outcome.terminal, outcome.violation_teardown, outcome.violations
            );
            failures = failures.saturating_add(1);
        }
    }

    // Canary 4: PlayerJoined before ProtocolInfo must be refused.
    {
        let mut steps = Vec::new();
        let ctx = Ctx::new_for_canary();
        steps.push(Step::Deliver(
            gen::canary_authenticated(),
            Default::default(),
        ));
        steps.push(Step::Poll(1));
        steps.push(Step::Deliver(
            gen::canary_player_joined(&ctx),
            Default::default(),
        ));
        steps.push(Step::Poll(1));
        let script = Script {
            seed: 0,
            index: 3,
            archetype: "canary_pre_negotiation",
            config_kind: ConfigKind::V3,
            echo_room_ops: false,
            small_command_capacity: None,
            steps,
        };
        let outcome = run_prefix(&script, ProtocolViolationPolicy::Quarantine, usize::MAX);
        if outcome.violations != 1 {
            println!(
                "canary_pre_negotiation FAILED: expected lifecycle violation, got {}",
                outcome.violations
            );
            failures = failures.saturating_add(1);
        }
    }

    // Canary 5 (issue #219): a delivered Pong frame must leave exactly one
    // pending expectation that its event satisfies — a silent batch leaves the
    // slot unresolved (the swallow-detection path).
    {
        let mut oracle = crate::run::test_support::fresh_oracle(
            ProtocolViolationPolicy::Quarantine,
            ConfigKind::V3,
            false,
        );
        let pong = signal_fish_client::protocol::ServerMessage::Pong;
        let expected = oracle.expectation_for(&pong, &Default::default());
        oracle
            .slots
            .push_back(crate::run::test_support::slot_with("Pong", expected));
        let mut findings = Vec::new();
        // The documented outcome: exactly one Pong event satisfies the slot.
        oracle.reconcile_batch(&["Pong"], 0, 7, &mut findings);
        if !findings.is_empty() || oracle.pending_slot_count() != 0 {
            println!(
                "canary_pong_outcome FAILED: findings={findings:?} pending={}",
                oracle.pending_slot_count()
            );
            failures = failures.saturating_add(1);
        }
    }

    // ── Direct-feed oracle-rejection canaries ────────────────────────
    //
    // Strict oracle: every synthetic known-bad stream must be REJECTED
    // (verdict Err). The positive-control feeds inside canaries 8-12 assert
    // the matching known-good variants are accepted.
    let strict = run_direct_feed_canaries();
    let mut strict_accepted: Vec<&str> = Vec::new();
    for canary in &strict {
        if canary.verdict.is_ok() {
            strict_accepted.push(canary.name);
        }
    }
    if !strict_accepted.is_empty() {
        println!(
            "oracle_direct_feed FAILED: strict oracle accepted known-bad streams: {strict_accepted:?}"
        );
        failures = failures.saturating_add(1);
    }

    // Broken-oracle sensitivity proof: neutering the oracle's rejection
    // branches must flip at least one canary from "rejects" to "accepts".
    set_oracle_neutered(true);
    let broken = run_direct_feed_canaries();
    set_oracle_neutered(false);
    let caught: Vec<&str> = broken
        .iter()
        .filter(|canary| canary.verdict.is_ok())
        .map(|canary| canary.name)
        .collect();
    if caught.is_empty() {
        println!(
            "broken_oracle_proof FAILED: the deliberately-broken oracle was not caught \
             by any direct-feed canary"
        );
        failures = failures.saturating_add(1);
    } else {
        println!(
            "broken_oracle_proof: neutered oracle caught by {} canary/ies: {caught:?}",
            caught.len()
        );
    }

    failures
}
