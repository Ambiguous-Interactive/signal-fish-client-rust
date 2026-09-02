//! Campaign CLI: drives deterministic hostility campaigns and diagnostics.
//!
//! Modes:
//! - default: seeds × scripts × policies campaign with a budget watchdog
//! - `--selftest`: oracle canaries (known-good/known-bad sensitivity)
//! - `--soak`: long-horizon churn probe
//! - `--repro SEED SCRIPT POLICY[:PREFIX]`: verbose single-script replay

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use signal_fish_client::client::ProtocolViolationPolicy;

use signal_fish_client_stateful_campaign::run::{
    render_prefix, run_prefix_verbose, run_script, set_oracle_neutered, ReducedFailure,
    CURRENT_LABEL, DELIVERED, HEARTBEAT,
};
use signal_fish_client_stateful_campaign::script::Script;
use signal_fish_client_stateful_campaign::transport::lock;
use signal_fish_client_stateful_campaign::{canary, gen, soak};

const ALL_POLICIES: [ProtocolViolationPolicy; 3] = [
    ProtocolViolationPolicy::Quarantine,
    ProtocolViolationPolicy::Observe,
    ProtocolViolationPolicy::Disconnect,
];

struct Stats {
    sequences: u64,
    frames: u64,
    events: u64,
    commands: u64,
    refused: u64,
    violations: u64,
    failures: u64,
}

static TOTAL_FRAMES: AtomicU64 = AtomicU64::new(0);

fn main() -> std::process::ExitCode {
    let mut seeds = 24u64;
    let mut scripts_per_seed = 30usize;
    let mut start_seed = 1u64;
    let mut budget_secs = 570u64;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--seeds" => {
                seeds = args.next().and_then(|v| v.parse().ok()).unwrap_or(seeds);
            }
            "--scripts" => {
                scripts_per_seed = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(scripts_per_seed);
            }
            "--start-seed" => {
                start_seed = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(start_seed);
            }
            "--budget-secs" => {
                budget_secs = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(budget_secs);
            }
            "--selftest" => {
                let failures = canary::selftest();
                if failures == 0 {
                    println!("selftest: all canaries passed (oracle detects known-good and known-bad streams)");
                    return std::process::ExitCode::SUCCESS;
                }
                println!("selftest: {failures} canary failure(s)");
                return std::process::ExitCode::from(3);
            }
            "--soak" => {
                let failures = soak::soak_probe();
                if failures == 0 {
                    println!("soak: clean");
                    return std::process::ExitCode::SUCCESS;
                }
                println!("soak: {failures} failure(s)");
                return std::process::ExitCode::from(4);
            }
            "--repro" => {
                // --repro SEED SCRIPT POLICY_PREFIX_QUOTED (policy: Q|O|D, prefix: steps)
                let seed: u64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                let index: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                let rest = args.next().unwrap_or_default();
                let mut parts = rest.split(':');
                let policy = match parts.next().unwrap_or("Q") {
                    "O" => ProtocolViolationPolicy::Observe,
                    "D" => ProtocolViolationPolicy::Disconnect,
                    _ => ProtocolViolationPolicy::Quarantine,
                };
                let prefix: usize = parts
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(usize::MAX);
                repro(seed, index, policy, prefix);
                return std::process::ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown argument: {other}");
                eprintln!(
                    "usage: stateful-campaign [--seeds N] [--scripts N] [--start-seed S] \
                     [--budget-secs S] [--selftest] [--soak] [--repro SEED SCRIPT POLICY[:PREFIX]]"
                );
                return std::process::ExitCode::from(2);
            }
        }
    }

    // The oracle-rejection-neutering toggle for sensitivity demonstrations.
    if std::env::var("STATEFUL_CAMPAIGN_BREAK_ORACLE").is_ok_and(|v| v == "1") {
        set_oracle_neutered(true);
        eprintln!("!! oracle deliberately broken via STATEFUL_CAMPAIGN_BREAK_ORACLE=1");
    }

    spawn_watchdog(budget_secs);

    let started = Instant::now();
    let mut stats = Stats {
        sequences: 0,
        frames: 0,
        events: 0,
        commands: 0,
        refused: 0,
        violations: 0,
        failures: 0,
    };
    let mut failure_report = String::new();

    for seed in start_seed..start_seed.saturating_add(seeds) {
        for index in 0..scripts_per_seed {
            let script = gen::generate_script(seed, index);
            for policy in ALL_POLICIES {
                stats.sequences = stats.sequences.saturating_add(1);
                match run_script(&script, policy) {
                    Ok(outcome) => {
                        stats.frames = stats.frames.saturating_add(outcome.frames_fed as u64);
                        stats.events = stats.events.saturating_add(outcome.events_seen as u64);
                        stats.commands = stats
                            .commands
                            .saturating_add(outcome.commands_accepted as u64);
                        stats.refused = stats
                            .refused
                            .saturating_add(outcome.commands_refused as u64);
                        stats.violations =
                            stats.violations.saturating_add(outcome.violations as u64);
                        TOTAL_FRAMES.fetch_add(outcome.frames_fed as u64, Ordering::Relaxed);
                    }
                    Err(failure) => {
                        stats.failures = stats.failures.saturating_add(1);
                        failure_report.push_str(&format_failure(&script, policy, &failure));
                        // Keep going: policy-differential evidence is valuable.
                    }
                }
            }
        }
        eprintln!(
            "[{:>6.1}s] seed {} done (sequences={}, failures={})",
            started.elapsed().as_secs_f32(),
            seed,
            stats.sequences,
            stats.failures
        );
    }
    print_summary(started, &stats, &failure_report);
    if stats.failures == 0 {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::from(5)
    }
}

fn format_failure(
    script: &Script,
    policy: ProtocolViolationPolicy,
    failure: &ReducedFailure,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "\n=== FINDING | seed={} script={} archetype={} config={} echo_room_op_ids={} policy={policy:?} ===\n",
        script.seed,
        script.index,
        script.archetype,
        script.config_kind.name(),
        script.echo_room_ops,
    ));
    for finding in &failure.findings {
        out.push_str(&format!(
            "  category: {}\n  detail:   {}\n",
            finding.category, finding.detail
        ));
    }
    out.push_str(&format!(
        "  reduced failing prefix: {} of {} steps (repro: --seeds 1 --start-seed {})\n",
        failure.prefix_len,
        script.steps.len(),
        script.seed
    ));
    out.push_str("  steps:\n");
    out.push_str(&render_prefix(script, failure.prefix_len, 60));
    out.push('\n');
    out
}

fn print_summary(started: Instant, stats: &Stats, failure_report: &str) {
    let wall = started.elapsed();
    println!("\n================ CAMPAIGN SUMMARY ================");
    println!(
        "wall time: {:.1}s (budget watchdog: 570s default)",
        wall.as_secs_f32()
    );
    println!("sequences (script × policy runs): {}", stats.sequences);
    println!("server frames fed:                {}", stats.frames);
    println!("client events observed:           {}", stats.events);
    println!(
        "client commands accepted/refused: {}/{}",
        stats.commands, stats.refused
    );
    println!("ProtocolViolation diagnostics:    {}", stats.violations);
    println!("FAILURES:                         {}", stats.failures);
    if !failure_report.is_empty() {
        println!("\n---------------- FINDINGS ----------------");
        print!("{failure_report}");
    } else {
        println!("\nzero findings.");
    }
    let delivered = lock(&DELIVERED);
    let all_variants = [
        "Authenticated",
        "ProtocolInfo",
        "AuthenticationError",
        "RoomJoined",
        "RoomJoinFailed",
        "RoomLeft",
        "PlayerJoined",
        "PlayerLeft",
        "GameData",
        "GameDataBinary",
        "AuthorityChanged",
        "AuthorityResponse",
        "LobbyStateChanged",
        "GameStarting",
        "Pong",
        "Reconnected",
        "ReconnectionFailed",
        "PlayerReconnected",
        "SpectatorJoined",
        "SpectatorJoinFailed",
        "SpectatorLeft",
        "RoomOperationResult",
        "NewSpectatorJoined",
        "SpectatorDisconnected",
        "Error",
        "Signal",
        "NewPeer",
        "SessionPlan",
        "PeerTransportStatus",
        "RelayStats",
        "GoingAway",
        "DeliveryReport",
    ];
    let never: Vec<&str> = all_variants
        .iter()
        .copied()
        .filter(|variant| !delivered.contains(*variant))
        .collect();
    println!("\n---------------- WIRE COVERAGE ----------------");
    println!(
        "delivered ServerMessage variants: {}/{}",
        all_variants.len().saturating_sub(never.len()),
        all_variants.len()
    );
    if never.is_empty() {
        println!("every ServerMessage variant was delivered at least once.");
    } else {
        println!("NEVER delivered: {never:?}");
    }
    let echo_kinds = [
        "RoomOperationResult::RoomJoined",
        "RoomOperationResult::RoomJoinFailed",
        "RoomOperationResult::RoomLeft",
        "RoomOperationResult::Reconnected",
        "RoomOperationResult::ReconnectionFailed",
        "RoomOperationResult::SpectatorJoined",
        "RoomOperationResult::SpectatorJoinFailed",
        "RoomOperationResult::SpectatorLeft",
        "RoomOperationResult::OperationFailed",
    ];
    let missing_echo: Vec<String> = echo_kinds
        .iter()
        .map(|kind| (*kind).to_string())
        .filter(|kind| !delivered.contains(kind.as_str()))
        .collect();
    if missing_echo.is_empty() {
        println!("every RoomOperationResult sub-variant was delivered at least once.");
    } else {
        println!("NEVER delivered RoomOperationResult sub-variants: {missing_echo:?}");
    }
    if !delivered.contains("<physical binary frame>") {
        println!("NOTE: no physical binary frame was delivered.");
    }
    if !delivered.contains("<raw schema-invalid frame>") {
        println!("NOTE: no raw schema-invalid frame was delivered.");
    }
    println!("==================================================");
}

/// Verbose single-script replay: prints every event as it is observed.
fn repro(seed: u64, index: usize, policy: ProtocolViolationPolicy, prefix: usize) {
    let script = gen::generate_script(seed, index);
    println!(
        "repro seed={seed} script={index} archetype={} config={} echo_room_ops={} policy={policy:?} prefix={prefix}",
        script.archetype,
        script.config_kind.name(),
        script.echo_room_ops,
    );
    let outcome = run_prefix_verbose(&script, policy, prefix);
    for finding in &outcome.findings {
        println!(
            "FINDING at step {:?}: {} — {}",
            finding.step_index, finding.category, finding.detail
        );
    }
    println!(
        "frames={} events={} violations={} refused={}",
        outcome.frames_fed, outcome.events_seen, outcome.violations, outcome.commands_refused
    );
}

/// Watchdog: detects library stalls (no step progress) and the global budget.
///
/// The scoped `exit` allowance is load-bearing: a hung library call can never
/// reach a cooperative stop flag, and a CI lane that hangs forever is worse
/// than one that fails loudly with the stall label and frame count.
#[allow(clippy::exit)]
fn spawn_watchdog(budget_secs: u64) {
    std::thread::spawn(move || {
        let started = Instant::now();
        let mut last_hb = HEARTBEAT.load(Ordering::Relaxed);
        let mut last_change = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(200));
            let hb = HEARTBEAT.load(Ordering::Relaxed);
            if hb != last_hb {
                last_hb = hb;
                last_change = Instant::now();
            } else if last_change.elapsed() > Duration::from_secs(60) {
                let label = lock(&CURRENT_LABEL).clone();
                eprintln!("\n!!! WATCHDOG STALL (60s without progress) at {label}");
                eprintln!(
                    "frames fed so far: {}",
                    TOTAL_FRAMES.load(Ordering::Relaxed)
                );
                std::process::exit(111);
            }
            if started.elapsed() > Duration::from_secs(budget_secs) {
                let label = lock(&CURRENT_LABEL).clone();
                eprintln!("\n!!! GLOBAL BUDGET EXCEEDED ({budget_secs}s) at {label}");
                eprintln!(
                    "frames fed so far: {}",
                    TOTAL_FRAMES.load(Ordering::Relaxed)
                );
                std::process::exit(112);
            }
        }
    });
}
