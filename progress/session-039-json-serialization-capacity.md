# Session 039 — JSON Serialization Capacity

## Priority and Scope

The hosted audit found no open pull requests or dependency PRs. Issue #82 was
reopened after the baseline PR merged, and issue #124 isolates its largest
measured client hotspot: 4 KiB outbound JSON game-data bursts. Issue #90 still
requires maintainer ruleset, quota, and independent-review administration;
issue #80 remains the next lower-impact documentation milestone.

This session optimizes only the measured direct-string JSON path. It does not
change protocol bytes, command admission, frame ownership, backpressure,
delivery accounting, queue budgets, event order, or transport behavior.

## Allocation Attribution

The checked baseline reproduced exactly on Rust 1.96.1/aarch64:

- single 4 KiB JSON send: `1/1/2` allocation/deallocation/reallocation
  operations and `8262/4096/8134` bytes;
- 64-message burst: `64/64/132` operations and
  `537408/262144/529216` bytes;
- single binary controls: exact zero;
- 64-message binary control: four reallocations and 8,640 reallocated bytes.

A temporary same-lockfile Heaptrack profile built `main` and the candidate
head with release debug information. On `main`, three 64-call buffer-growth
groups resolve through `serde_json::ser::to_vec`,
`serde_json::ser::to_string`, and
`SignalFishPollingClient::drive_outbound`; the measured and sacrificial runs
account for the repeated groups. The independent four-call stack resolves
through `VecDeque::push_back` and `queue_command_at`. The candidate profile no
longer contains the game-data Serde growth stacks. Together with the isolated
`stats_alloc` region and binary control, this attributes the measured 132
reallocations to 128 serializer growths (two for each frame) plus four polling
queue growths.

The profile used the exact root `Cargo.lock` in a detached `main` worktree and
these commands:

```shell
CARGO_PROFILE_RELEASE_DEBUG=2 CARGO_INCREMENTAL=0 \
  cargo +1.96.1 build --locked --release \
  -p signal-fish-client-perf-lab --features perf --bin perf-allocations
heaptrack target/release/perf-allocations \
  --child json/out/4096/burst64
heaptrack_print heaptrack.perf-allocations.* > heaptrack.txt
```

The raw profiles were temporary diagnostics rather than repository fixtures;
the exact allocation counters and source attribution are recorded here.

## Hypothesis and Implementation

Serde JSON starts `to_string` with a 128-byte buffer. A direct 4 KiB JSON
string already exposes a constant-time byte-length hint, so both drivers now
share one serializer that reserves that length plus the fixed `GameData`
envelope before asking Serde to write the canonical wire. The optimization is
restricted to direct string payloads of at least 4 KiB. Smaller and structured
values retain `serde_json::to_string`, avoiding unmeasured recursive scans or
over-reservation. Large escaped and multibyte strings still use Serde; an
underestimate can only trigger its safe ordinary growth.

Tests compare optimized bytes with canonical `serde_json::to_string` output
for null, booleans, integer and floating-point extremes, nested structured
values, all delivery shapes, a maximum key, large escaped/control strings,
large multibyte strings, and a non-game-data control message.

## Results

Ten isolated debug samples and ten release samples were identical across all
28 workloads. The changed cells are:

| Workload | Before A/D/R | After A/D/R | Before bytes A/D/R | After bytes A/D/R |
| --- | ---: | ---: | ---: | ---: |
| `json/out/4096/single` | 1/1/2 | 1/1/0 | 8262/4096/8134 | 4226/4096/0 |
| `json/out/4096/burst64` | 64/64/132 | 64/64/4 | 537408/262144/529216 | 279104/262144/8640 |

The burst removes all 128 serialization reallocations and reduces allocated
bytes by 48.1%. Tightened ceilings use the established 10% policy with minimum
operation/byte margins. Both binary single-message exact-zero contracts remain
zero, smaller JSON and classified cells reproduce their prior counters, and
all 28 protocol-ledger digests remain exact. The target burst digest remains
`13e682e87522fb1392dd2d36061c81166600d83e83f5ef0e384847cfda34b84a`.

For paired runtime evidence, the same locked command ran once in each detached
base/head worktree, alternating base/head/head/base on CPU 0:

```shell
taskset -c 0 cargo +1.96.1 run --locked --release \
  -p signal-fish-client-perf-lab --features perf --bin perf-timing \
  -- --samples 25
```

The target burst medians were:

| Run | Median ns | ns/op | ops/s |
| --- | ---: | ---: | ---: |
| base 1 | 202010 | 3156 | 316815 |
| head 1 | 151207 | 2362 | 423260 |
| head 2 | 150108 | 2345 | 426359 |
| base 2 | 199010 | 3109 | 321591 |

That is a stable roughly 25% improvement on the paired, CPU-pinned run. The
complete refreshed 28-cell table is in `tools/perf-lab/README.md`.

## Rejected Alternatives

- Exact recursive JSON sizing was rejected after it more than doubled the
  target timing by rescanning every string before serialization.
- Recursive constant-time-per-string hints were rejected because large
  structured values would receive an extra tree walk without a representative
  workload proving a benefit.
- Applying preallocation to small messages was rejected after it exceeded a
  checked byte ceiling for a structured polling workload.
- Unsafe UTF-8 conversion and assumptions about per-write UTF-8 chunk
  boundaries were rejected. `String::from_utf8` safely reuses the vector
  allocation and makes the impossible encoder violation explicit.
- Queue preallocation was rejected as a different problem: its four growths
  are small, shared by the binary control, and not the dominant measured site.

## Verification

Local verification completed on Rust 1.96.1:

- `cargo fmt`, workspace/all-target/all-feature Clippy with warnings denied,
  and workspace/all-feature tests pass;
- `scripts/check-all.sh`, `scripts/ci-validate.sh`,
  `scripts/check-workflows.sh`, and all 222 `ci_config_tests` pass (optional
  tools absent from the runner are reported as skips by the scripts);
- exact 28-cell smoke and 10-sample debug/release allocation runs pass the
  tightened ceilings, with identical allocation records between profiles;
- the CI-equivalent `cargo llvm-cov` commands report 94.17% line coverage,
  above the 93% gate;
- the wire-parity unit test covers optimized, fallback, escaped, multibyte,
  delivery, maximum-key, numeric-extreme, nested, and control-message cases;
- an independent adversarial audit found no unresolved implementation,
  measurement, baseline, or policy defects.

Hosted pull-request checks and review state are recorded in the pull request;
the separate repository-policy administration blocker remains tracked by
issue #90.
