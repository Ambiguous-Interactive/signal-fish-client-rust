# Session 050 — Deep-Safety Gate Re-Run, Perf-Lab Evidence, and Post-#143 Follow-Ups

## Priority and Audit

The session began from a local `main` that carried one pre-#143 duplicate
commit (`10f7bdc`) and an unresolved merge against `origin/main`. The merge
tree was byte-identical to the merged PR #143 squash (`2dc0b42`), so the
working tree was reset to `origin/main` — nothing was lost; the duplicate and
its one-line CHANGELOG wording difference ("without enqueuing it") are already
merged upstream. Hosted state: no open or draft pull requests; all 12 push
workflows green on `main` at `2dc0b42` (CI run #407: Success). Open issues:
#126 (this milestone's umbrella), #90 (maintainer-administration governance
blocker), #144 (post-session-049 follow-ups — addressed below).

With the issue-#126 correctness slices finished through PR #143, the remaining
milestone work was PLAN items 4–5: re-run the established gates and refresh
measured evidence against the 28-cell laboratory. All lanes were executed
locally on this aarch64 Linux host (Rust 1.97.1 stable) and passed.

## Gate Re-Run Evidence (PLAN item 4)

| Gate | Tool / command | Result |
| --- | --- | --- |
| Mandatory workflow | `cargo fmt && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features` | Clean before any change; all workspace tests pass |
| No-panic policy | `scripts/check-no-panics.sh` | PASS (both phases; tests have explicit opt-in) |
| Unused dependencies | `cargo machete` | No unused dependencies |
| Advisory audit | `cargo audit` (upgraded 0.21.0 → CI-pinned 0.22.1) | 0 advisories across 166 locked dependencies |
| Miri | nightly 1.100.0-nightly, `MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test --test protocol_tests --all-features` | 127 passed, no UB reports |
| Fuzz smoke | cargo-fuzz 0.13.1, 3 × `-max_total_time=30` | `fuzz_server_message` 1,165,090; `fuzz_client_message` 1,857,280; `fuzz_binary_game_data` 1,681,212 execs — no crashes |
| FFI safety | `scripts/check-ffi-safety.sh` | PASS |
| Unsafe inventory | grep sweep + manifest policy | Every owned `unsafe` block remains confined to `src/transports/emscripten_websocket.rs`; core `unsafe_code = "deny"`, Godot adapter `forbid` |

Tooling note: the locally installed cargo-audit 0.21.0 can no longer parse the
RustSec advisory database (CVSS-4.0 entries such as RUSTSEC-2026-0209), so it
was upgraded to the CI-pinned 0.22.1. No repository change is required; CI
already pins the working version.

Mutation testing was left to its scheduled Deep Safety lane per PLAN item 4,
which lists unsafe-inventory/Miri/fuzz/dependency/no-panic for this re-run.

## Perf-Lab Evidence (PLAN item 5)

All four measurement modes ran against unchanged checked-in baselines:

- `perf-smoke`: verified all 28 deterministic workload cells.
- `perf-timing` (release, 25 samples/cell): every median within ±16% of the
  README baseline column (mostly faster on this host; timing is diagnostic).
  Slowest ratio vs baseline: `json/in/4096/burst64` at 1.01–1.03 on repeat
  runs — inside run-to-run variance, no regression signal.
- `perf-allocations` (debug and release): every cell reproduced at or under
  every one of the six counters' ceilings; both exact-zero binary outbound
  cells stayed exactly zero in both profiles.

No optimization is proposed: with no repeatable regression measured, the
acceptance rule (accept complexity only for repeatable, protocol-neutral gain)
keeps the implementation unchanged. Raw outputs: local-run artifacts
(`timing.json`, allocation JSONs) were captured out-of-tree; the durable
summary is this file.

## Issue #144 — Follow-Ups

1. **`.llm/context.md` line budget.** The canonical transport-trait code block
   duplicated the verbatim pin that `skills/transport-abstraction/SKILL.md`
   already carries. context.md now names the required/defaulted methods and
   points to the skill: 500 → 489 lines, restoring headroom without dropping
   any contract sentence.
2. **Close-path test observables.** Added "Close-Path Tests Need a
   Pending-Close Mock" to `skills/testing-async/SKILL.md`: instant-close mocks
   (`MockTransport::closed`) flip concurrently with a wedged delivery and pass
   vacuously; deadline enforcement must be observed through the abort path of
   a forever-pending-close mock (`HangingCloseTransport`'s `close_called` /
   `abort_called` / `dropped` flags).

## Stale In-Progress Work

- Remote branch `agent/fortress-rollback-throughput-e2e` (base `df9934b`,
  PR #62 era): its five commits' concepts landed in stronger form through
  #64/#65/#66 and later work — e.g. branch-tip `reset_queue_age_peak` exists
  on `main` at `src/polling_client.rs:912` with the stronger never-lower-than-
  current invariant, and the Fortress relay E2E now runs as
  `scripts/run-godot-fortress-e2e.mjs` plus the clean/impaired/soak jobs in
  `godot-web.yml`. Superseded; deletion recommended in the follow-up issue.
- Local stashes: `stash@{0}` holds old PR-description/goal scaffolding from
  the `dev/wallstop/harden-decode-path` era; `stash@{1}` holds action-ref bump
  edits long since merged as pinned versions. Both are disposable; recorded
  here rather than dropped unilaterally.

## Outcome

One coherent PR carries: the two #144 fixes, refreshed gate/lab evidence
(this file), and PLAN.md brought current. No production code changed; no
CHANGELOG entry per policy (internal-only). Follow-up issue opened for the
superseded branch/stash cleanup decision.
