# Session 024 — FFI Lifetime and Dependency Governance

Date: 2026-08-16

## Objective

Audit the post-PR-91 state, fix the highest-severity correctness regression,
repair failed dependency automation, and restore roadmap evidence before
continuing the negotiated token-binding milestone.

## Baseline Audit

- PR #91 had merged as `963a9fc` and closed issues #78/#84, although `PLAN.md`
  and session 023 still described it as blocked and unmerged.
- The live ruleset still lacked review and required-status-check rules. PR #91
  merged with no approval and a quota-failed Copilot check.
- Dependabot Cargo run 31958655375 failed because its temporary file set lacked
  `crates/signal-fish-client-godot/Cargo.toml`. It nevertheless opened PR #92,
  which auto-merged as a zero-file commit while Godot Web was still running;
  its `GITHUB_TOKEN` merge produced no push checks on that `main` SHA. PR #93
  then auto-merged a real Actions update, and its resulting `main` SHA likewise
  had no check/status evidence.
- The Emscripten transport freed its raw `CallbackState` even when
  `emscripten_websocket_delete` failed. Constructor rollback, `poll_close`,
  `abort`, and `Drop` all shared the same possible late-callback
  use-after-free. A logical receive error also set `closed` early enough to
  skip the required native close before deletion.
- The FFI checker verified only that deletion syntax existed. Its 25 passing
  self-tests could not detect unconditional reclamation after delete failure.

## Implementation

- Separated logical connection closure from confirmed native closure.
  Receive errors now still drive the native close/delete cleanup sequence;
  peer-close events remain recognized as already closed by the browser.
- Made callback-state ownership explicit with a dedicated owner around
  `Option<NonNull<CallbackState>>`. Successful deletion yields the typed
  authorization that reclaims the allocation exactly once and clears
  ownership. Failed deletion retains the pointer for retry; final `Drop`
  failure intentionally leaks it instead of risking UAF.
- `poll_close` now reports native close or callback deletion failures rather
  than returning false success. `abort` and `Drop` retain diagnostic results.
- Extended the required FFI checker with typed reclamation-boundary and real
  `poll_close` enforcement plus eighteen negative/positive cases, bringing its
  self-test to 43 cases. The
  repository policy test verifies constructor rollback, the shared cleanup
  helper, close-before-delete ordering, and the terminal leak fallback.
- Added a target-independent cleanup ownership state machine used by the
  Emscripten transport, with executable tests covering every close/delete
  result pair, retries, peer close, close-before-delete authorization, and
  exactly-once reclamation authority. A dedicated owner consumes the private
  authorization and contains the sole raw reclaim site.
- Added the adapter directory explicitly to Dependabot's combined Cargo updater
  so the missing fixture path target is present in its temporary file set.
- Removed the checked-in `GITHUB_TOKEN` Dependabot auto-merge workflow. Reviewed
  dependency merges remain operating policy but are not live enforcement until
  issue #90 is fixed; removing this path prevents it from independently
  bypassing policy and suppressing push CI on the merge SHA.
- Updated `CHANGELOG.md`, `.llm/context.md`, and `PLAN.md` to describe the
  consumer-visible safety fix and the actual merged/failed hosted state.

## Verification

- FFI policy checker: passed.
- FFI checker self-test: 43/43 passed.
- Shell portability: all 25 scripts passed.
- Workflow policy passed locally; `actionlint` was unavailable and remains a
  hosted check.
- The target-gated Emscripten module type-checked through `cargo rustc` with
  the local crate's target cfg overridden; the actual ABI target remains a
  required hosted WASM check.
- The mandatory workflow passed: `cargo fmt`, workspace/all-target/all-feature
  Clippy with `-D warnings`, and workspace/all-feature tests (including 213
  CI-policy tests and 125 protocol tests).

## Hosted Follow-up

- A default-branch Dependabot run must prove that the explicit root, adapter,
  and fixture directory set resolves successfully. Do not treat configuration
  shape alone as proof.
- Issue #90 remains a maintainer-admin blocker: restore the live ruleset,
  remove the quota-broken Copilot gate or fund it, and dispatch Repository
  Policy to green.
- The next PR must prove one approval, zero unresolved threads, all eleven
  required aggregates, strict up-to-date enforcement, and green CI on its
  eventual `main` merge SHA.
