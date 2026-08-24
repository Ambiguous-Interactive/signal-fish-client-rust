#![forbid(unsafe_code)]
// Measurement medians divide by a caller-checked positive operation count.
#![allow(clippy::arithmetic_side_effects)]

use std::time::Instant;

use serde::Serialize;
use signal_fish_client_perf_lab as workloads;

const NANOS_PER_SECOND: u64 = 1_000_000_000;

#[derive(Debug, Serialize)]
struct TimingRecord {
    workload: &'static str,
    logical_operations: u64,
    samples: usize,
    minimum_ns: u64,
    median_ns: u64,
    maximum_ns: u64,
    median_ns_per_operation: u64,
    median_operations_per_second: u64,
    polls: u64,
    inbound_frames: u64,
    inbound_bytes: u64,
    outbound_frames: u64,
    outbound_bytes: u64,
    send_budget_exhaustions: u64,
    receive_budget_exhaustions: u64,
    peak_queue_age_ns: u64,
    protocol_ledger_sha256: [u8; 32],
}

#[derive(Debug, Serialize)]
struct TimingReport {
    schema: u32,
    profile: &'static str,
    records: Vec<TimingRecord>,
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let samples = parse_samples(&args)?;
    let mut records = Vec::with_capacity(workloads::WORKLOADS.len());
    for spec in workloads::WORKLOADS {
        records.push(measure(spec, samples)?);
    }
    let report = TimingReport {
        schema: 1,
        profile: profile_name(),
        records,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&report)
            .map_err(|error| format!("serialize timing report: {error}"))?
    );
    Ok(())
}

fn parse_samples(args: &[String]) -> Result<usize, String> {
    match args {
        [] => Ok(25),
        [flag, value] if flag == "--samples" => {
            let samples = value
                .parse::<usize>()
                .map_err(|error| format!("invalid --samples value {value:?}: {error}"))?;
            if samples == 0 {
                Err("--samples must be positive".to_string())
            } else {
                Ok(samples)
            }
        }
        _ => Err("usage: perf-timing [--samples N]".to_string()),
    }
}

fn measure(spec: workloads::WorkloadSpec, samples: usize) -> Result<TimingRecord, String> {
    std::hint::black_box(workloads::execute_once(spec)?);

    let mut durations = Vec::with_capacity(samples);
    let mut expected_digest = None;
    let mut logical_operations = 0;
    let mut peak_queue_age_ns = 0;
    let mut representative_ledger = None;
    for _ in 0..samples {
        let mut fixture = workloads::prepare_and_warm(spec)?;
        let started = Instant::now();
        let outcome = workloads::run_measured(&mut fixture)?;
        let elapsed = u64::try_from(started.elapsed().as_nanos())
            .map_err(|_| format!("{} duration does not fit u64", spec.id))?;
        let ledger = workloads::finish_and_verify(fixture, outcome)?;
        let digest = workloads::deterministic_ledger_digest(&ledger);
        if let Some(expected) = expected_digest {
            if digest != expected {
                return Err(format!(
                    "{} protocol ledger changed between samples",
                    spec.id
                ));
            }
        } else {
            expected_digest = Some(digest);
        }
        logical_operations = ledger.logical_operations;
        peak_queue_age_ns = peak_queue_age_ns.max(ledger.peak_queue_age_ns);
        representative_ledger = Some(ledger);
        durations.push(elapsed);
    }
    durations.sort_unstable();
    let minimum_ns = durations.first().copied().unwrap_or_default();
    let maximum_ns = durations.last().copied().unwrap_or_default();
    let median_ns = durations
        .get(durations.len() / 2)
        .copied()
        .unwrap_or_default();
    if logical_operations == 0 || median_ns == 0 {
        return Err(format!("{} timing evidence was vacuous", spec.id));
    }
    let median_ns_per_operation = median_ns / logical_operations;
    let median_operations_per_second = logical_operations
        .saturating_mul(NANOS_PER_SECOND)
        .checked_div(median_ns)
        .unwrap_or_default();
    let ledger = representative_ledger
        .ok_or_else(|| format!("{} produced no representative ledger", spec.id))?;
    Ok(TimingRecord {
        workload: spec.id,
        logical_operations,
        samples,
        minimum_ns,
        median_ns,
        maximum_ns,
        median_ns_per_operation,
        median_operations_per_second,
        polls: ledger.polls,
        inbound_frames: ledger.inbound_frames,
        inbound_bytes: ledger.inbound_bytes,
        outbound_frames: ledger.outbound_frames,
        outbound_bytes: ledger.outbound_bytes,
        send_budget_exhaustions: ledger.send_budget_exhaustions,
        receive_budget_exhaustions: ledger.receive_budget_exhaustions,
        peak_queue_age_ns,
        protocol_ledger_sha256: expected_digest.unwrap_or_default(),
    })
}

fn profile_name() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}
