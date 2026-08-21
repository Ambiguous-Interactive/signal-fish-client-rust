#![forbid(unsafe_code)]

use serde::Serialize;
use signal_fish_client_perf_lab as workloads;

#[derive(Serialize)]
struct ProtocolBaselines {
    schema: u32,
    records: Vec<ProtocolBaseline>,
}

#[derive(Serialize)]
struct ProtocolBaseline {
    workload: &'static str,
    sha256: [u8; 32],
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.as_slice() == ["--emit-protocol-baselines"] {
        return emit_protocol_baselines();
    }
    if !args.is_empty() {
        return Err("usage: perf-smoke [--emit-protocol-baselines]".to_string());
    }
    for spec in workloads::WORKLOADS {
        std::hint::black_box(workloads::execute_once(spec)?);
    }
    println!(
        "verified {} deterministic workload cells",
        workloads::WORKLOADS.len()
    );
    Ok(())
}

fn emit_protocol_baselines() -> Result<(), String> {
    let records = workloads::WORKLOADS
        .into_iter()
        .map(|spec| {
            let ledger = workloads::execute_once_without_protocol_pin(spec)?;
            Ok(ProtocolBaseline {
                workload: spec.id,
                sha256: workloads::deterministic_ledger_digest(&ledger),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let baselines = ProtocolBaselines { schema: 1, records };
    println!(
        "{}",
        serde_json::to_string_pretty(&baselines)
            .map_err(|error| format!("serialize protocol baselines: {error}"))?
    );
    Ok(())
}
