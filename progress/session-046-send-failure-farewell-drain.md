# Session 046 — Send-Failure Farewell Drain

## Priority and Audit

The session began from clean `main` at
`c1fbdb4141295d7fadea99a138db2aa276fca1b2`, the merge of PR #136. The GitHub
connector found no open pull requests. Open client work was the #126
correctness/performance audit, focused issues #134 and #135, and the separate
maintainer-only repository-policy blocker #90. This session takes #134 first,
the roadmap's next disconnect-adjacent correctness slice.

Both drivers previously treated an outbound `poll_send` error as immediately
terminal. The async combined I/O poll returned without polling receive, and the
polling client returned before its receive drain. A complete server
`Error(SlowConsumer)` already ready at that duplex boundary was therefore lost,
so `Disconnected.last_server_error` could not attribute the server farewell.
The polling driver also prefixed already-formatted send and receive transport
errors a second time.

## Design

A driver-internal `ReadyFrameDrain` now owns common frame/byte accounting and
returns at most one immediately available complete frame per call. The frame
that reaches or crosses a byte bound is processed before the drain stops, so an
oversized first frame remains valid and no later frame is consumed without
normal `ClientCore` processing. `Pending`, EOF, receive error, the bounded work
limit, deadline, and protocol-directed disconnect all stop the drain.

On async send failure a mutex-serialized bit freezes both fail-fast and waiting
admission, including a reliable sender that already reserved a Tokio channel
permit. The command receiver then closes before the drain. One absolute
terminal deadline covers ready-frame processing, backpressured event delivery,
`Disconnected`, and transport cleanup. Shutdown can preempt a blocked terminal
event without replacing the already-established send cause. The native
WebSocket backend rejects later sends but retains buffered read state until the
first non-ready receive boundary; retriable tungstenite `WriteBufferFull`
refusals retain their exact-frame and token-binding sequence behavior.

The polling driver immediately abandons the failed caller-owned frame and all
queued commands. It uses the smaller of its configured receive budget and the
shared 64-frame/64-KiB terminal safety cap, then marks receive terminal so the
close phase cannot poll and discard frames beyond the chosen stop. Both drivers
prefer only peer-initiated `TransportCloseInfo` over the original send error;
bare EOF, receive error, local close metadata, bounds, and deadlines do not
replace it.

## Regression Evidence

One shared transport drives the principal async/polling regression:
authentication succeeds, the first queued `Ping` fails without transferring
its frame, a complete slow-consumer farewell and EOF are immediately ready,
and a second queued `Ping` must never be offered. Both clients emit exact
`Error` then `Disconnected` order, retain `SlowConsumer` in
`last_server_error`, preserve the single-prefix send reason, freeze subsequent
admission, and close without retrying queued work.

The same transport covers `Pending`, EOF, receive error, peer and non-peer
close metadata, capacity-one event backpressure, shutdown preemption, a paused
terminal deadline, protocol-directed stop, shared and caller frame/byte limits,
oversized-first processing, prefetched polling input, statistics, and
close-phase no-repoll behavior. Native WebSocket and multithreaded reserved-
permit regressions cover the backend and admission boundaries independently.

## Adversarial Review

The first parallel reviews rejected a draft that pre-collected every terminal
event before async channel delivery because it weakened event backpressure.
They also found an over-budget frame could be consumed without processing, a
peer-close shortcut bypassed the farewell drain, shutdown could overwrite the
send cause, polling close could resume receive after a terminal stop, and each
phase started a fresh timeout. The final design above resolves the entire
class: one-frame scheduling, crossing-frame processing, one absolute deadline,
cause preservation, and a persistent polling receive stop.

The frozen-diff reviews then found that closing a Tokio receiver does not revoke
an outstanding permit, native WebSocket send errors dropped buffered reads,
and the draft lacked protocol-stop, byte-budget, prefetched-frame,
deterministic-deadline, and EOF-only metadata cases. They also identified
misplaced driver policy and duplicate accounting in `transport.rs`. The final
revision adds the serialized admission freeze, split WebSocket send/receive
terminal state, all missing acceptance cases, and a small `terminal_drain`
driver module with one shared budget, payload counter, and close formatter.

## Verification and Hosted Disposition

The frozen implementation passes the mandatory local chain:

- `cargo fmt`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features` (389 core unit tests, 48 polling
  parity tests, all workspace integration and doc tests; 11 live-server tests
  remain ignored by their explicit environment contract)

The no-default-feature Clippy build, LLM pre-commit/context limit, panic-policy,
test-quality, target-gated doc-link, format, and diff checks also pass. MkDocs
rendering is the sole local skip because MkDocs is not installed. Three
independent frozen-diff reviews report zero actionable correctness, test/docs,
or simplicity findings after the fix loop.

Hosted PR and CI disposition will be appended after the branch is published.
