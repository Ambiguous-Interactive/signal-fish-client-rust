# Session 038 — Allocation and Performance Baselines

## Priority and Scope

The hosted audit found no open pull requests or dependency PRs. Open issues
were #90 (repository administration), #82 (performance evidence), and #80
(documentation design). The exact head of PR #122 had all 12 workflows green
before merge to `main` commit `1acc211`. Issue #90 still requires maintainer
ruleset, Copilot-quota, and independent-review administration unavailable to
this session, so #82 was the highest-impact actionable work.

## Deterministic Laboratory

An unpublished, opt-in `tools/perf-lab` workspace package now exercises the
public polling client and therefore shared `ClientCore`. Its 28-cell registry
covers 2/8/16-player lobbies; 256-byte and 4 KiB JSON and MessagePack relay in
both directions as single messages and 64-message bursts; Latest and Volatile
delivery; one server-authorized Latest gap; 2/8/16-player reconnect replay; and
polling frame, byte, receive-byte, and pre-acceptance `Pending` boundaries.

A dedicated deterministic transport separates setup and measured ingress and
scripts `Pending` without decoding measured wire data. Corpus construction,
serialization, warm-up, digesting, event inspection, state snapshots, and
ledger verification stay outside the measured region. Every successful run
proves exact frame and byte traffic, event counts and payload fingerprints,
wire digests, roster/replay/watermark/gap evidence, client statistics,
send/receive budget exhaustion, final queue depth and age, and terminal state.
Mutation tests prove traffic, event, state, queue, and digest corruption is
rejected.

## Allocation and Timing Evidence

`stats_alloc` records allocation, deallocation, and reallocation counts and
bytes around only `run_measured`. Each sample runs in an isolated child process
after a sacrificial workload. Planted allocate/deallocate/reallocate controls
must report their exact operation and byte counts, while a disconnected zero
control must fail. Ten debug and ten release samples produced identical exact
results for all 28 cells on Rust 1.96.1. The single 256-byte and 4 KiB binary
outbound cells perform zero measured allocations, deallocations, or
reallocations and are pinned as exact-zero contracts.

The release timing report records 25-sample min/median/max latency,
per-operation latency, throughput, queue age, traffic, polls, and budget
exhaustion. The largest measured hotspot is the 4 KiB JSON outbound burst:
182,510 ns median, 350,665 operations/second, 64 allocations, 132
reallocations, and 537,408 allocated bytes. This session establishes evidence
and ceilings; it intentionally makes no production optimization without a
separate measured hypothesis.

## Regression Contract

Checked-in allocation ceilings cover both debug and release. Non-contract
cells receive a narrow 10% margin with a minimum two operations or 256 bytes;
the deliberate allocation-free cells remain exact zero. Required CI uses
pinned Ubuntu 24.04 ARM64 and Rust 1.96.1 to run the ledger smoke, ten isolated
samples in each profile, and a five-sample diagnostic timing capture, then
uploads all JSON reports. The `CI Required` aggregate includes this job.

The package has no default features, all dependencies are optional behind
`perf`, and every binary requires that feature. Default and no-default-feature
workspace gates therefore do not acquire the polling client or measurement
dependencies. The release workspace plan continues to include only the two
publishable lockstep crates.

## Verification and Adversarial Review

The final 28-cell smoke passed, and fresh isolated ten-sample debug and release
allocation reports both passed their checked-in ceilings. A five-sample release
timing report also completed. The exact cargo-llvm-cov 0.8.4 commands used by
CI passed end to end with the lab excluded from production coverage and 94.15%
line coverage, above the 93% floor.

Two independent adversarial passes converged to zero findings. The measurement
review mutated traffic, event payloads, nested reconnect replay, watermarks,
snapshots, queues, and digests and confirmed that serialization and validation
remain outside `run_measured`. The CI review exercised workflow commands,
aggregate gating, ARM64 provenance, feature and Rust-version isolation,
packaging scope, context limits, and changelog classification. Findings during
the loops added pinned protocol ledgers, full event/snapshot fingerprints, an
explicit semantically checked regeneration path, and compatible coverage
report arguments before the zero-finding verdicts.

## Rejected Candidates

- The stale Fortress branch was not cherry-picked; its superseded system
  fixture is outside this client-local measurement boundary.
- DHAT was rejected because its public statistics do not independently expose
  the required deallocation and reallocation operation counts.
- The shared test mock was rejected because parsing measured frames to decide
  scripted behavior would charge harness allocations to the client.
- Runtime thresholds were rejected because host scheduling makes them noisy;
  CI captures timing diagnostics but gates only deterministic allocation
  ceilings and exact protocol evidence.

## Documentation and Changelog

`tools/perf-lab/README.md` records commands, methodology, environment, ceiling
policy, and the complete 28-cell timing/queue/allocation baseline. `PLAN.md`
removes the completed #82 milestone and advances #80. `.llm/context.md` records
the permanent performance contract. No `CHANGELOG.md` entry is warranted:
this is unpublished internal measurement, tests, CI, and contributor evidence
with no public API, runtime behavior, feature, dependency, or consumer-facing
change.
