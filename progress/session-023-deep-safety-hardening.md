# Session 023 — Deep Safety Hardening

Date: 2026-08-16

## Objective

Consolidate issues #78 and #84 into one evidence-backed safety milestone,
repair red or non-actionable analyzer lanes, and advance the stale roadmap from
the now-merged Server 0.7 delivery.

## Baseline Audit

- PR #89 was already merged to `main` as `18a49e6`, issue #86 was closed, and
  no open or draft PR remained. `PLAN.md` and session 022 still described the
  delivery as pending.
- The live default-branch ruleset still lacked review and required-check rules;
  four scheduled Repository Policy runs were red. Issue #90 still required an
  approval on already-merged PR #89, an impossible acceptance criterion.
- Deep Safety was configured as informational. Its latest run selected a musl
  cargo-fuzz target incompatible with ASan/static libc, Miri completed the
  protocol suite and then timed out in repository-policy tests, and mutation
  testing reported a surviving `ErrorCode::description -> "xyzzy"` mutant.
- The binary MessagePack fuzz target existed but neither hosted nor local
  automation ran it; it decoded v3 only. Committed seed directories were also
  passed as libFuzzer's writable corpus and could accumulate generated files.
- Rust compiler lints did not prohibit new unsafe code. Owned production unsafe
  was confined to the target-gated Emscripten C WebSocket binding; the Godot
  adapter had none.
- Blanket Clippy pedantic/nursery/cargo groups produced more than 170 mostly
  stylistic or documentation findings. The only suspicious correctness warning
  was verified as a false positive. Native ASan could not exercise the sole
  owned unsafe implementation because that code is Emscripten-only.
- The standalone Godot fixture Dependabot updater failed at least three weeks
  because its directory-scoped file set could not resolve `../..` path
  dependencies outside the fixture.

## Implementation

- Denied unsafe Rust in the core crate, forbade it in the Godot adapter, and
  documented one module-level Emscripten exception. The required WASM workflow
  now runs the FFI checker and its 25 negative/self-test cases before actual
  Emscripten compilation and Clippy.
- Added an offline source-policy sweep so a second owned `unsafe` site or lint
  exception fails required CI even if a future module tries to override the
  core's deny-level lint or is hidden behind a target cfg.
- Made all Deep Safety analyzers fail closed. Miri now runs only the 125
  production protocol tests; repository-policy subprocess tests remain in
  their appropriate required workflow.
- Selected cargo-fuzz's target from the nightly host triple, ran all three fuzz
  targets, isolated writable corpora under `mktemp`, retained seed directories
  as read-only inputs, uploaded crash artifacts, ignored nested fuzz build
  output, and prevented mutation worktrees from copying ignored artifacts.
- Expanded binary fuzzing to both supported v2/v3 decoders. Every iteration
  exercises raw input plus valid and input-perturbed canonical envelopes.
- Replaced the mutation smoke assertion with an exact actionable-description
  contract, killing the known whole-function survivor.
- Combined the root workspace and standalone Godot fixture in one Dependabot
  Cargo updater file set so it can include the fixture's local path
  dependencies; default-branch execution remains the required proof.
- Updated `.llm/context.md` and `PLAN.md` with the analyzer policy, completed
  Server 0.7 delivery, live governance blocker, current safety work, and next
  token-binding correctness milestone. The published progress claim now says
  tracked history is curated rather than falsely claiming every local ignored
  session is present.
- Re-scoped issue #90 so the next open PR, rather than merged PR #89, proves
  review and required-check enforcement.

## Red-Green Evidence

1. The original binary target reached 74 of 12,870 counters after 1,000 runs
   from a shallow invalid seed. The revised v2/v3 canonical harness reached 202
   counters in 1,000 isolated runs and exited successfully on the ARM host.
2. Running fuzz against `seeds/$target` reproduced repository pollution. The
   temporary first-corpus design left every source seed directory unchanged.
3. The exact mutation scope finished in 1m25s: nine mutants, eight caught, zero
   surviving, zero timeout, and one compile-unviable generic mutation.
4. Miri passed all 125 protocol tests in 17 seconds after compilation.
5. The FFI policy checker and all 25 self-tests passed. Workflow lint, YAML
   lint, shellcheck, action-reference policy, and shell portability passed;
   actionlint was unavailable locally and remains covered by hosted CI.
6. The mandatory pre-commit gate passed with zero warnings or failures:
   `cargo fmt`, workspace/all-target/all-feature Clippy with `-D warnings`, and
   workspace/all-feature tests (including 212 CI-policy and 125 protocol
   integration tests).

## Hosted Work Remaining

- Open one PR, run all required aggregates, dispatch Deep Safety and Protocol
  Sync, and resolve every actionable review finding.
- Confirm the combined Cargo updater succeeds after the configuration reaches
  the default branch.
- Maintainer administration is still required for issue #90: restore the live
  ruleset, resolve the Copilot quota gate, and dispatch a green Repository
  Policy audit. The available connector can inspect but cannot mutate rulesets.
- Close #78 only after hosted Deep Safety is green; close #84 as its duplicate
  milestone at the same point.
