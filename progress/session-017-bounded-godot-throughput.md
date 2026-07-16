# Session 017 — Bounded Godot Throughput

## Objective

Implement Issue #61: remove Godot's socket-wide zero-drain send completion,
add capacity-safe fixed/adaptive admission, bound polling-client work and close,
publish scheduling/transport diagnostics, and make the official Godot browser
throughput regression merge-blocking.

## Progress

- Added defaulted `Transport` poll-cycle, abort, and diagnostics hooks.
- Added polling work-budget, close-policy, scheduling-statistics, and options
  APIs while preserving `SignalFishPollingClient::new`.
- Added Godot fixed/adaptive/native-capacity options and immediate backend
  ownership transfer with exact frame retention on capacity refusal.
- Added deterministic sticky-buffer, capacity, EWMA, work-budget, FIFO, close
  policy, zero-timeout, deadline-overflow, force-abort, and injected-clock tests.
- Updated public docs, changelog, canonical context, focused skills, and the
  generated skill index.
- Expanded the official browser fixture to a four-frame one-callback proof and
  a two-client 136 frames/s, 16-second timestamped load phase with JSON/CSV and
  Prometheus artifacts.
- Completed an adversarial production/docs/E2E pass. It found and drove fixes
  for unlimited native capacity, force-close semantics, accepted-Pending close
  ordering, deadline overflow, EWMA overflow, reversed adaptive bounds, stale
  terminal diagnostics, marker ambiguity, metrics timing, and failure-artifact
  preservation.
- Completed a second adversarial loop and targeted re-reviews after every
  finding; production, documentation/tests, and browser-E2E reviewers all
  reported zero remaining issues.
- Opened PR #62 and repaired the first CI pass by restoring the Godot 0.4.5
  binding required by Rust 1.87 and removing a Rust 1.97 rustdoc warning.
- Addressed all three Cursor Bugbot findings with built-in socket abort
  teardown, bounded inbound draining during close, a Godot buffered-packet
  close guard, and a 32-command per-client load-producer cap.
- Addressed the second Cursor pass by flushing a backend-owned in-flight send
  before peer-disconnect close and counting receive-budget exhaustion only
  when another frame is actually retained for a later poll.
- Addressed the third Cursor pass by applying Godot's buffered-inbound close
  guard both before and after a locally initiated close advances the peer to
  `Closed`.
- Diagnosed the first browser workflow failure as a punctuation boundary in
  the expected raw-Emscripten linker marker and corrected the exact marker.

## Evidence

- Pre-fix focused regression: the legacy Godot model accepted one frame and
  returned `Pending` while `bufferedAmount` remained at 7, preventing the next
  queued frame from transferring in the same client poll. The new combined
  sticky-buffer client regression accepts Authenticate plus three Pings in one
  poll with the nonzero buffer retained.
- Issue #61 baseline: receiver capacity was 135.8 messages/s while browser
  acceptance was capped at 62.5 sends/s by one-frame-per-rendered-callback
  completion.
- `cargo fmt && cargo clippy --all-targets --all-features -- -D warnings &&
  cargo test --all-features` — final post-review run passed: 285 library unit
  tests plus all integration/doc suites (3 live-server tests remained
  intentionally environment-gated).
- Focused revised suites — 102 polling tests and 30 Godot transport tests passed.
- `cargo check --all-features` — passed.
- Godot 4.5 fixture `cargo check` and `cargo clippy -- -D warnings` with the
  pinned local Godot executable — passed.
- `node --check scripts/run-godot-web-smoke.mjs` — passed.
- `python3 scripts/pre-commit-llm.py` — passed and regenerated the skill index.
- `uv run --with pytest pytest -q scripts/test_pre_commit_llm.py` — 111 passed.
- The first full browser workflow reached the intended raw-Emscripten linker
  rejection but its marker assertion rejected the following period; the fixed
  workflow is pending a new-head CI run.

## Next

- Run the complete final gate, push the review/CI corrections, and repeat CI
  and automated review until every required check is green with no unresolved
  findings.
