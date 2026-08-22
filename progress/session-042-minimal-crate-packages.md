# Session 042 — Minimal Crate Packages

## Priority and Hosted Audit

The session began from clean `main` at
`00e9d84b0f03eb615532e9f466b2d5733687dad0`, the merge of PR #129. The GitHub
connector found no open or draft pull requests and no dependency pull requests.
Eleven exact-SHA push workflows passed; Godot Web remained in progress with no
failure during implementation. The separate scheduled Repository Policy audit
remains correctly red for the administrative ruleset drift in issue #90.

Issue #128 is the highest-impact correctness item, but the pinned Server 0.7
wire contract has no request identity that can distinguish an old same-kind
response from the current operation. A client-only fix would be a timing or
partial-payload heuristic. Upstream
[signal-fish-server#395](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/395)
now defines the five request/response pairs, negotiation and legacy fallback,
autonomous spectator-exit semantics, and differential stale-response tests.
The dependency is linked from client issue #128.

Issue #130 was therefore the highest-priority independent open issue that could
reach a complete green client PR without inventing a server contract.

## Package Baseline and Result

Before this change, `signal-fish-client` packaged 55 files: 1,933,140 bytes
uncompressed and 356,901 bytes compressed. The archive duplicated 24
repository-only test/fixture files, four examples, and `CHANGELOG.md`. The
Godot adapter was already minimal at seven files and about 16.2 KiB compressed.

The core manifest now permits only `src/**`, `Cargo.toml`, `LICENSE`, and
`README.md`. Cargo's required normalized manifest, lockfile, and VCS metadata
bring the final core archive to 27 files, 1,243,702 bytes uncompressed and
227,829 bytes compressed: 28 fewer files and a 36.2% compressed-size reduction.
`src/**` includes the 1,977-byte token-binding vector consumed by the crate's
unit tests. Exact manifest allowlist tests protect both publishable crates
without a brittle byte ceiling that would penalize legitimate source growth.

The package-facing README now uses durable source/site links and states that
the repository examples require a source checkout. `cargo package` verifies
both interdependent workspace crates from their minimal archives.

## Repository Artifact Audit

The tracked-tree audit found no build output, archives, logs, native binaries,
or unignored untracked files. Executable modes belong only to invoked scripts;
the tracked font and image assets are documented site/evidence inputs. The
stale root `pr-description.md` was a session artifact and was removed, with an
explicit ignore rule preventing recurrence.

The ignore sweep added deterministic outputs produced by coverage, performance,
pytest, and CycloneDX commands. A repository policy test rejects any file that
is simultaneously tracked and ignored except the curated `PLAN.md` and
`progress/` evidence required by this repository's agent workflow, and pins the
known local artifact patterns. Existing ignored local build, fuzz, Godot,
browser, and log trees remain outside the index.

## MSRV Preservation

Trimming `tests/` exposed that a packaged unit test consumed the token-binding
vector through `include_str!("../tests/token-binding/vectors.toml")`. The vector
now lives under `src/testdata/`, so it remains beside the unit test and the
source distribution can compile and execute its own library tests.

The MSRV job compiles every repository core test target and runs all core
library tests at Rust 1.87.0, then independently extracts Cargo's exact package
and repeats the library-test compile and execution with all features at the same
MSRV. Local reproduction passed: all 14 repository test binaries compiled and
all 367 library tests passed from both the repository and extracted archive.

## Main CI Root Cause

The initial Godot Web push attempt on `main` failed only its 3,600-frame soak.
Both peers confirmed the target, matched all 59 checksums, conserved every
delivery, stayed near 18 Hz, drained all queues, proved rollback/resimulation,
and completed teardown with no stalls, drops, malformed frames, desyncs, or
admission violations. One peer reported a 78.59 ms maximum elapsed `poll()`
interval, crossing a hard 50 ms gate around `Instant::now(); client.poll()`.

That elapsed interval includes browser garbage collection and hosted-runner
preemption, so one 50 ms crossing is not a deterministic client-work bound. The
branch instead requires no more than one percent of measured polls to reach 50 ms
and retains a 500 ms emergency maximum. Exact 64-frame/64-KiB poll budgets,
500 ms queue-age and p99 latency bounds, 12–20 Hz simulation cadence, lag and
stall limits, conservation, integrity, rollback, drain, and teardown oracles
remain strict. Negative controls prove the observed 78.59 ms outlier passes in
a healthy distribution, while repeated 50 ms crossings, a 500 ms stall, or
missing timing evidence fail. Failed jobs on the unchanged main run were rerun;
the same 3,600-frame soak and Godot aggregate passed, making all 12 exact-SHA
push workflows green and confirming an intermittent rather than deterministic
client failure.

## Verification and Review

Focused package-policy, repository-artifact, workflow-policy, JavaScript oracle,
package verification, and exact extracted-package MSRV commands passed. The
first adversarial package review caught stale README-link policy and the broken
source-archive unit-test vector; both were corrected and regression-tested.

The frozen local tree passed the mandatory command exactly:

```shell
cargo fmt && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features
```

It also passed the pure JavaScript Godot validators, both `cargo package`
verifications, the extracted Rust 1.87 package tests, and focused archive,
artifact, provenance, README-link, and workflow policies. Two independent
adversarial passes drove fixes for source-archive testability, package-link
policy, hermetic artifact checks, order-independent allowlists, and robust
poll-timing distributions. The repeated frozen-diff audit reported no code or
policy findings after those corrections. Commit, draft-PR identity, hosted
checks, and review disposition are appended after publication.

## Publication

Commit `05386cb044e7bbd878b160f1f1f131a41e54022b` passed all 18 pre-commit and
six pre-push checks and was pushed to `chore/minimize-crate-packages`. Draft
[PR #131](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/pull/131)
targets the audited `00e9d84b0f03eb615532e9f466b2d5733687dad0` main commit, closes issue #130,
and records the main-CI root cause and upstream #128 dependency. Hosted check
and review disposition follow on the final documented PR head.

The first hosted Deep Safety attempt found that mutation testing executes the
suite from an isolated source copy without `.git`. The new index-policy tests
incorrectly treated that intentional environment as a Git failure. They now
retain `.gitignore` content checks everywhere and condition only index queries
on repository metadata being present; ordinary checkouts remain strict.
