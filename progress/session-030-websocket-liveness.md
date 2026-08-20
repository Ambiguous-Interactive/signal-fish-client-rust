# Session 030 — WebSocket Liveness

## Scope

Issue #105 bounds peer-controlled WebSocket receive work and makes every native
socket terminal outcome leave one coherent, fused transport state.

## Invariant → Source → Evidence

| Invariant | Source | Executable evidence |
| --- | --- | --- |
| One receive call skips at most 64 Ping, Pong, or defensive raw Frame messages. A boundary Ping flushes its automatic Pong before the transport self-wakes and yields. | `MAX_SKIPPED_CONTROL_FRAMES_PER_POLL`; `WebSocketState::poll_recv`. | `websocket::tests::control_frame_budget_self_wakes_before_buffered_application_data`; `ping_auto_pong_is_flushed_before_later_application_reads`. |
| Partial input and sink backpressure retain their exact state and real wakers. An accepted first send completes before a second caller frame is consumed. | `WebSocketState::{poll_recv, poll_send}`. | `partial_frame_registers_and_notifies_the_real_waker`; `pending_flush_retains_the_second_frame_and_preserves_fifo`. |
| A custom tungstenite write-buffer refusal does not consume a frame the sink rejected. | `WebSocketState::poll_send` restores `WriteBufferFull` Text/Binary frames. | `rejected_write_buffer_full_restores_the_exact_caller_frame`. |
| EOF, peer Close, and terminal receive/send errors drop the stream, clear retained poll state, and fuse later operations. The first receive failure remains observable. | `WebSocketState::mark_terminal`; receive/send/close terminal branches. | `raw_eof_fuses_receive_send_and_close_operations`; `socket_receive_error_is_reported_once_then_transport_is_terminal`; `socket_send_error_maps_to_transport_send_and_becomes_terminal`; `recv_returns_none_on_close_frame`; `recv_after_local_close_returns_exact_terminal_none`. |
| Existing frame, metadata, TLS, and socket-latency behavior remains intact. | `WebSocketTransport` constructors and frame mapping. | Exact text/binary FIFO round trip; peer-close metadata/response tests; TLS provider test; default/override `TCP_NODELAY` tests. |

## Same-Class Sweep

- The Godot adapter processes at most one backend packet per receive poll.
- The Emscripten transport consumes at most its one synthetic Open event before
  returning an application or terminal result. Neither has a peer-controlled
  ignored-control loop, so the native WebSocket fix does not apply there.
- The FFI reclamation scanner exempts only tungstenite's exact
  `WebSocketStream::from_raw_socket` constructor while retaining its broad
  ownership-reclamation matches; negative alias/comment-split cases and a new
  longer ownership-API fixture protect both sides.

## Review and Verification

- Three independent production, test, and adversarial audits agreed on the
  bounded-work, flush-ordering, terminal-state, ownership, and evidence gaps.
- The mandatory `cargo fmt && cargo clippy --workspace --all-targets
  --all-features -- -D warnings && cargo test --workspace --all-features` gate
  passes, including 332 core library tests, 31 driver-parity tests, 126 protocol
  tests, and 35 Godot adapter tests; six live-server tests remain intentionally
  ignored outside their pinned jobs.
- The 29-test focused WebSocket suite, no-default and isolated WebSocket builds,
  stable all-feature rustdoc, nightly docs.rs simulation, workflow policy, FFI
  policy plus its 50-case self-test, LLM validation, and `git diff --check` pass.
- The first hosted Semver run exposed `cargo-semver-checks` 0.46's rustdoc-v57
  ceiling against stable's v60 output. Semver and publish workflows now share
  the upstream 0.50 pin (v60/v61 support), guarded by a repository policy test.
- Hosted spelling validation also caught a partial-frame byte-string fragment;
  the fixture now expresses those remaining bytes individually.
- PR #113's code head `f05cf14` passed all 11 blocking aggregates: CI,
  Coverage, Docs Validation, Examples Validation, Godot Web, No Panics,
  Security, Semver Checks, Unused Deps, WASM, and Workflow Lint. An
  evidence-only follow-up repeated all 11 successfully.
- Cursor Bugbot classified the change as medium risk without a finding.
  Copilot could not review because its requester quota is exhausted (the known
  governance limitation tracked by #90); no actionable comment or unresolved
  inline thread remains.
