#![forbid(unsafe_code)]

use std::alloc::System;
use std::process::Command;

use serde::{Deserialize, Serialize};
use stats_alloc::{Region, Stats, StatsAlloc, INSTRUMENTED_SYSTEM};

use signal_fish_client_perf_lab as workloads;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
struct AllocationStats {
    allocations: u64,
    deallocations: u64,
    reallocations: u64,
    bytes_allocated: u64,
    bytes_deallocated: u64,
    bytes_reallocated: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AllocationRecord {
    workload: String,
    stats: AllocationStats,
    protocol_ledger_sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AllocationReport {
    schema: u32,
    profile: String,
    samples: usize,
    records: Vec<AllocationRecord>,
}

#[derive(Debug, Deserialize)]
struct AllocationBaselines {
    schema: u32,
    toolchain: String,
    validated_profiles: Vec<String>,
    records: Vec<AllocationBaseline>,
}

#[derive(Debug, Deserialize)]
struct AllocationBaseline {
    workload: String,
    ceiling: AllocationStats,
    exact_zero: bool,
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--child") {
        run_child(args.get(1).map(String::as_str))?;
        return Ok(());
    }

    let samples = parse_samples(&args)?;
    let expected = spawn_child(None)?;
    for sample in 1..samples {
        let observed = spawn_child(None)?;
        if observed != expected {
            return Err(format!(
                "allocation sample {} differed from sample 1",
                sample.saturating_add(1)
            ));
        }
    }
    validate_baselines(&expected)?;
    let report = AllocationReport {
        schema: 1,
        profile: profile_name().to_string(),
        samples,
        records: expected,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&report)
            .map_err(|error| format!("serialize allocation report: {error}"))?
    );
    Ok(())
}

fn validate_baselines(records: &[AllocationRecord]) -> Result<(), String> {
    let baselines: AllocationBaselines =
        serde_json::from_str(include_str!("../../allocation-baselines.json"))
            .map_err(|error| format!("decode allocation baselines: {error}"))?;
    if baselines.schema != 1 {
        return Err(format!(
            "unsupported allocation baseline schema {}",
            baselines.schema
        ));
    }
    if baselines.toolchain != "1.96.1" {
        return Err(format!(
            "allocation baseline toolchain must be 1.96.1, found {}",
            baselines.toolchain
        ));
    }
    if !baselines
        .validated_profiles
        .iter()
        .any(|profile| profile == profile_name())
    {
        return Err(format!(
            "allocation baselines do not cover {} profile",
            profile_name()
        ));
    }
    if baselines.records.len() != records.len() {
        return Err(format!(
            "allocation baseline workload count: expected {}, found {}",
            records.len(),
            baselines.records.len()
        ));
    }
    for record in records {
        let baseline = baselines
            .records
            .iter()
            .find(|baseline| baseline.workload == record.workload)
            .ok_or_else(|| format!("missing allocation baseline for {}", record.workload))?;
        if baseline.exact_zero && record.stats != AllocationStats::default() {
            return Err(format!(
                "{} must remain allocation-free, found {:?}",
                record.workload, record.stats
            ));
        }
        require_at_or_below(record, baseline.ceiling)?;
    }
    for baseline in &baselines.records {
        if !records
            .iter()
            .any(|record| record.workload == baseline.workload)
        {
            return Err(format!(
                "stale allocation baseline for {}",
                baseline.workload
            ));
        }
    }
    Ok(())
}

fn require_at_or_below(record: &AllocationRecord, ceiling: AllocationStats) -> Result<(), String> {
    let observed = record.stats;
    let within_ceiling = observed.allocations <= ceiling.allocations
        && observed.deallocations <= ceiling.deallocations
        && observed.reallocations <= ceiling.reallocations
        && observed.bytes_allocated <= ceiling.bytes_allocated
        && observed.bytes_deallocated <= ceiling.bytes_deallocated
        && observed.bytes_reallocated <= ceiling.bytes_reallocated;
    if within_ceiling {
        Ok(())
    } else {
        Err(format!(
            "{} exceeded allocation ceiling: observed {observed:?}, ceiling {ceiling:?}",
            record.workload
        ))
    }
}

fn parse_samples(args: &[String]) -> Result<usize, String> {
    match args {
        [] => Ok(10),
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
        _ => Err("usage: perf-allocations [--samples N]".to_string()),
    }
}

fn spawn_child(workload: Option<&str>) -> Result<Vec<AllocationRecord>, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("resolve allocation executable: {error}"))?;
    let mut command = Command::new(executable);
    command.arg("--child");
    if let Some(workload) = workload {
        command.arg(workload);
    }
    let output = command
        .output()
        .map_err(|error| format!("run isolated allocation sample: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "isolated allocation sample failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("decode isolated allocation sample: {error}"))
}

fn run_child(workload: Option<&str>) -> Result<(), String> {
    validate_planted_controls()?;
    let records = if let Some(id) = workload {
        let spec = workloads::WORKLOADS
            .iter()
            .copied()
            .find(|spec| spec.id == id)
            .ok_or_else(|| format!("unknown workload id {id:?}"))?;
        vec![measure(spec)?]
    } else {
        workloads::WORKLOADS
            .into_iter()
            .map(measure)
            .collect::<Result<Vec<_>, _>>()?
    };
    println!(
        "{}",
        serde_json::to_string(&records)
            .map_err(|error| format!("serialize child allocation records: {error}"))?
    );
    Ok(())
}

fn measure(spec: workloads::WorkloadSpec) -> Result<AllocationRecord, String> {
    std::hint::black_box(workloads::execute_once(spec)?);

    let mut fixture = workloads::prepare_and_warm(spec)?;
    let region = Region::new(GLOBAL);
    let outcome = workloads::run_measured(&mut fixture)?;
    let stats = convert_stats(region.change())?;
    let ledger = workloads::finish_and_verify(fixture, outcome)?;
    Ok(AllocationRecord {
        workload: spec.id.to_string(),
        stats,
        protocol_ledger_sha256: workloads::deterministic_ledger_digest(&ledger),
    })
}

fn validate_planted_controls() -> Result<(), String> {
    let region = Region::new(GLOBAL);
    let allocated = Vec::<u8>::with_capacity(4_096);
    std::hint::black_box(&allocated);
    let allocation = convert_stats(region.change())?;
    require_stats(
        "allocation",
        allocation,
        AllocationStats {
            allocations: 1,
            deallocations: 0,
            reallocations: 0,
            bytes_allocated: 4_096,
            bytes_deallocated: 0,
            bytes_reallocated: 0,
        },
    )?;
    drop(allocated);

    let deallocated = vec![0u8; 4_096];
    std::hint::black_box(&deallocated);
    let region = Region::new(GLOBAL);
    drop(deallocated);
    let deallocation = convert_stats(region.change())?;
    require_stats(
        "deallocation",
        deallocation,
        AllocationStats {
            allocations: 0,
            deallocations: 1,
            reallocations: 0,
            bytes_allocated: 0,
            bytes_deallocated: 4_096,
            bytes_reallocated: 0,
        },
    )?;

    let mut growing = Vec::with_capacity(1);
    growing.push(1u8);
    std::hint::black_box(&growing);
    let region = Region::new(GLOBAL);
    growing.reserve_exact(4_096);
    std::hint::black_box(&growing);
    let reallocation = convert_stats(region.change())?;
    require_stats(
        "reallocation",
        reallocation,
        AllocationStats {
            allocations: 0,
            deallocations: 0,
            reallocations: 1,
            bytes_allocated: 4_096,
            bytes_deallocated: 0,
            bytes_reallocated: 4_096,
        },
    )?;
    drop(growing);

    if require_nonzero_control("disconnected", AllocationStats::default()).is_ok() {
        return Err("disconnected allocator control unexpectedly passed".to_string());
    }
    require_nonzero_control("connected", allocation)
}

fn require_stats(
    name: &str,
    observed: AllocationStats,
    expected: AllocationStats,
) -> Result<(), String> {
    if observed == expected {
        Ok(())
    } else {
        Err(format!(
            "planted {name} control mismatch: expected {expected:?}, found {observed:?}"
        ))
    }
}

fn require_nonzero_control(name: &str, stats: AllocationStats) -> Result<(), String> {
    if stats == AllocationStats::default() {
        Err(format!("{name} allocator accounting is disconnected"))
    } else {
        Ok(())
    }
}

fn convert_stats(stats: Stats) -> Result<AllocationStats, String> {
    Ok(AllocationStats {
        allocations: u64::try_from(stats.allocations)
            .map_err(|_| "allocation count does not fit u64".to_string())?,
        deallocations: u64::try_from(stats.deallocations)
            .map_err(|_| "deallocation count does not fit u64".to_string())?,
        reallocations: u64::try_from(stats.reallocations)
            .map_err(|_| "reallocation count does not fit u64".to_string())?,
        bytes_allocated: u64::try_from(stats.bytes_allocated)
            .map_err(|_| "allocated bytes do not fit u64".to_string())?,
        bytes_deallocated: u64::try_from(stats.bytes_deallocated)
            .map_err(|_| "deallocated bytes do not fit u64".to_string())?,
        bytes_reallocated: i64::try_from(stats.bytes_reallocated)
            .map_err(|_| "reallocated bytes do not fit i64".to_string())?,
    })
}

fn profile_name() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ceiling_verifier_rejects_each_allocation_counter_class() {
        let ceiling = AllocationStats::default();
        for stats in [
            AllocationStats {
                allocations: 1,
                ..AllocationStats::default()
            },
            AllocationStats {
                deallocations: 1,
                ..AllocationStats::default()
            },
            AllocationStats {
                reallocations: 1,
                ..AllocationStats::default()
            },
            AllocationStats {
                bytes_allocated: 1,
                ..AllocationStats::default()
            },
            AllocationStats {
                bytes_deallocated: 1,
                ..AllocationStats::default()
            },
            AllocationStats {
                bytes_reallocated: 1,
                ..AllocationStats::default()
            },
        ] {
            let record = AllocationRecord {
                workload: "control".to_string(),
                stats,
                protocol_ledger_sha256: [0; 32],
            };
            assert!(require_at_or_below(&record, ceiling).is_err());
        }
    }
}
