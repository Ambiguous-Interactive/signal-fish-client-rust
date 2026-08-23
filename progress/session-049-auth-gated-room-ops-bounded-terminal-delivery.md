# Session 049 — Authentication-Gated Room Operations, Bounded Terminal Delivery, Cancellation Pins

## Priority and Audit

The session began from clean `main` at
`6f975d7e343adc5f5911113fb762b198f7d619a9`, the merge of PR #139. Hosted state
showed no open or draft pull requests, all 12 push workflows green on `main`,
and the remaining issue-#126 inventory cells: #140, #141, and #142, plus #90
(maintainer-administration governance blocker). An uncommitted working tree
carried a partially finished #140 implementation; it was verified, completed,
and folded into this session's single PR per the no-stacking rule.

## Issue #140 — Authentication-Gated Room Operations

`ClientCore::validate` now refuses the five admission-fencing operations
(`JoinRoom`, `LeaveRoom`, `Reconnect`, `JoinAsSpectator`, `LeaveSpectator`)
with the new exhaustive `SignalFishError::NotAuthenticated` variant before
the server confirms authentication. The gate sits after the connection check
and before the pending-transition check, mirroring the inbound lifecycle
gates exactly: every room terminal already requires `authenticated && ...`,
so admitting an outbound room command early armed a fence that those gates
would classify as a violation and never release — a permanent
`RoomOperationPending` under non-terminal policies after any premature server
response (most plausibly an auto-reconnect loop calling `reconnect()` on a
fresh transport).

- Both drivers inherit the gate through the shared core; reliable sends
  structurally refuse fencing operations, so only the synchronous surface is
  affected.
- Non-room commands keep their documented pre-authentication behavior
  (`ping` stays available; game data still answers `NotInRoom`).
- Docs updated on all ten method doc-comments (five operations × two
  drivers), plus `docs/errors.md` (new exhaustive-table row + precedence),
  and `docs/client.md` (admission table row + deterministic-precedence
  sentence).
- Test sweep: the change correctly broke every fixture that issued room
  commands pre-authentication. All were migrated to documented usage via a
  shared bounded `wait_for_authentication` helper (`tests/common/mod.rs`),
  event-stream synchronization in the parity harness (some vendored wire
  samples complete before a snapshot poll can observe authentication, so the
  helpers stash events up to and including `Authenticated`), a delivery
  barrier in the parity `TraceMock` that holds trace remainders until the
  admitted command reaches the wire, gated-mock conversions where fixtures
  previously relied on ungated delivery, send-gating support in the unit
  `MockTransport` (permit-consuming semaphore + waker-registered shared
  incoming queue with `IncomingControls`), and a live-join reordering for
  reconnect flows. Positive regression tests pin refusal before
  authentication, fence non-armament, capacity restoration, and immediate
  admission once authenticated in both drivers.

## Issue #141 — Bounded Terminal Disconnect Delivery

`emit_core_disconnected_or_shutdown` now bounds peer-close delivery by the
configured `shutdown_timeout` budget using the same escape machinery as the
send-failure path: on expiry the loop attempts one nonblocking delivery of
the terminal `Disconnected`, closes or aborts the transport under the
remaining budget, and exits. The wedged-consumer task leak — which held
`cmd_rx` forever and parked every waiting reliable sender — is gone.

The new `emit_event_batch` helper routes both the transport loop's
multi-event frame handler and `finish_send_failure`'s drain loop through one
implementation of the documented bound: when shutdown (or the deadline)
preempts a mid-batch delivery, the remaining batch events get one nonblocking
attempt instead of being abandoned sight unseen, keeping "abandons at most
the one in-flight event" honest. Docs updated at the module boundary, the
event-capacity field, `shutdown()`, and `emit_event_or_shutdown`.

Regression tests (all deterministic):

- `peer_close_with_wedged_consumer_terminates_loop_and_releases_parked_senders`
  — peer close, permanently full channel, no shutdown: budget expiry aborts,
  loop exit drops the command receiver, and the parked reserve sender
  resolves with `NotConnected`; delivered prefix stays ordered.
- `terminal_receive_error_aborts_only_after_the_shutdown_budget` — receive
  error against a `poll_close`-that-never-completes transport, so teardown
  provably comes from the budget-expiry abort (verified non-vacuous: a
  deadline-enforcement regression hangs the spin instead of passing).
- `expired_budget_preempts_blocked_batch_delivers_without_corruption` —
  stale-deadline batch emission reports preemption without corrupting
  buffered events.

## Issue #142 — Cancellation Pins and Dequeue-Failure Fence Release

- `ClientCore::dequeue_serialization_failed(message)` releases the operation
  fence (including `pending_reconnects` bookkeeping) when an admitted room
  command fails to serialize at dequeue time, kind-matched so an unrelated
  message can never release someone else's fence. Wired into the async
  driver's serialize-error arm and the polling client's
  `finish_serialization_at`. Unreachable today (every `ClientMessage` field
  serializes infallibly) but eliminates the class rather than asserting it
  away; covered by a data-driven five-operation matrix test including
  mismatched-kind non-release.
- `dropping_a_parked_reliable_send_leaves_no_trace` pins cancel-safety:
  dropping a future parked on queue capacity moves neither stats nor
  snapshot, leaks no permit, never puts the payload on the wire, and an
  identical command still completes afterwards. Doc anchor gains an explicit
  cancel-safety sentence beside the existing `select!`-race hazard note.

## Verification

`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets
--all-features -- -D warnings`, and `cargo test --workspace --all-features`
all pass with zero failures. CHANGELOG carries the two user-visible entries
(breaking `NotAuthenticated` gating under Changed; bounded terminal delivery
under Fixed; #142 is test-only/defensive and intentionally unlisted).

An adversarial sub-agent audit found five must-fix items (stale exhaustive-
error docs, stale client-doc precedence, stale `.llm/context.md` contracts, a
CHANGELOG driver-scope overclaim, and one vacuous budget regression test)
plus four hygiene items; all were fixed and a second adversarial pass
re-verified every item against the code and returned SHIP with zero blocking
findings. The rewritten budget test was specifically confirmed to hang rather
than pass if deadline enforcement regresses.

## Follow-ups Identified

- `.llm/context.md` sits exactly at its 500-line pre-commit limit; the next
  contract addition needs either tightening elsewhere or splitting the file.
- `MockTransport::poll_close` completing instantly made the first draft of
  the budget test vacuous; other close-path tests relying on `closed` as a
  post-budget signal should prefer abort-flag or stream-closure observables.
