# Session 043 — Native WebSocket Size Limits

## Priority and Hosted Audit

The session began from clean `main` at
`c7790e8feb4090383cef9635e09847118e88b099`, the merge of PR #131. The GitHub
connector found no open or draft pull requests and no dependency pull requests.
All 12 push workflows on that exact commit are green. Docs Validation attempt 1
timed out in its Playwright accessibility watchdog; the unchanged failed-jobs
retry passed in under one minute, so no deterministic product failure remained.

Issue #128 remains the highest-impact correctness item and remains legitimately
blocked on the negotiated operation identifier in signal-fish-server#395.
Issue #126 therefore supplied the next independent correctness slice: prevent a
grossly oversized native WebSocket message from being assembled before it can
reach `ClientCore`.

## Authority and Limit Selection

The audit used the pinned Signal Fish Server 0.7.0 source at commit
`3f7f43d4cd4b3cc7f8fb893220dc35c9b1fad333`. Its default inbound application
limit is 64 KiB and its server-side WebSocket decoder allows twice that value,
but neither value bounds messages sent by the server. A legal server message
can aggregate many separately accepted values: the default deployment's
configurable 100-player ceiling (room default 8) and near-cap per-player
metadata can approach roughly 6.25 MiB before snapshot envelope or replay
overhead. Spectator rosters are not room-bounded, and replayable spectator
events contain complete snapshots.

The client therefore uses an inclusive 8 MiB protective default for both an
individual inbound frame and a fragmented message's assembled payload. It is a
resource policy, not a protocol maximum. Callers can raise it or choose `None`
when a trusted layer provides another bound. `Some(0)` is rejected before
network I/O. Detailed upstream issue
[signal-fish-server#399](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/399)
tracks a discoverable and enforced outbound-message contract.

## Implementation and Contract

Every SDK-owned native connection path supplies the same `WebSocketConfig` to
tungstenite: ordinary disabled mode, token-binding offer and Optional fallback,
and custom-rustls disabled and selected paths. `WebSocketConnectOptions` exposes
`max_inbound_message_size` plus a matching builder. Oversize codec errors map to
the existing one-shot `TransportReceive` outcome and terminalize the transport;
later receive, send, and close behavior remains fused. Token-binding feature
errors retain precedence over an independently invalid size option.

`WebSocketTransport::from_stream` is deliberately different. Its caller owns
the completed handshake and codec, so wrapping the stream preserves the
caller's frame and message limits. Browser, Emscripten, Godot, and custom
transport size policies likewise remain their platform/implementor boundary and
are not silently represented as covered by the native option.

## Consolidated Audit Matrix

This is the living issue #126 inventory. “Pinned” means a source authority and
repeatable regression exist; “open” names the next evidence needed rather than
implying a defect.

| Surface | Authority and current regression evidence | Status / next gap |
|---|---|---|
| JSON and binary protocol codecs | Server 0.7 fixture, protocol tests, wire goldens, error-code conformance | Pinned; extend only for newly negotiated wire fields |
| `ClientCore` transitions and admission | Public lifecycle contract, shared-core policy tests, client integration traces | Pinned for room-operation kind correlation; same-kind identity blocked by #128/#395 |
| Async and polling drivers | Shared-core architecture, client tests, 39-trace parity suite | Pinned for current lifecycle/ownership surface; disconnect-adjacent parity remains open |
| Native WebSocket setup and receive bounds | tungstenite 0.30 codec contract plus Server 0.7 aggregation audit; real socket boundary/overflow tests | Pinned for frame, fragmentation, override, token-binding, and terminal behavior in this session |
| Emscripten WebSocket | Emscripten API boundary, target build and FFI policy tests | Platform owns assembly; explicit browser size-policy evidence remains open |
| Godot native/web adapter | Godot 4.5 `WebSocketPeer`, 35 fake-backend tests, native/web real-server scenarios and soak | Engine owns assembly; explicit engine size-policy evidence remains open |
| Token binding v2 | Server 0.7 contract, canonical vectors, unit/conformance and required-WSS E2E | Pinned; size cap now covered on selected and fallback physical connections |
| Mesh/WebRTC generations | Server 0.7 plan/generation wire contract, controller unit/integration/parity tests | Pinned for stale-generation fencing; fresh race/adversarial sweep remains open |
| Setup, configuration, recovery, and misuse | Public API docs, compile/policy tests, lifecycle and timeout regressions | Partially pinned; continue cancellation, queue saturation, and reconnect race cells |
| Safety and performance gates | unsafe inventory, no-panic, Miri/fuzz/mutation CI, 28-cell semantic/performance laboratory | Established; rerun fresh analyzers and profile before accepting new performance complexity |

## Regression Evidence

Real tungstenite codecs prove that the exact configured boundary is accepted,
one unfragmented byte beyond it is rejected, and individually valid fragments
whose aggregate exceeds it are rejected. Data-driven cases cover a larger
custom limit and `None`. Separate paths prove Required token binding and
Optional fallback retain the cap, while a caller-configured `from_stream` cap
is not replaced. The oversize regression also proves one error followed by
exact fused receive/send/close semantics and retained caller frame ownership.

The focused 47-test WebSocket suite passes with all features. A no-default,
WebSocket-only run proves unavailable token binding still reports
`FeatureDisabled` before size validation. All-feature Clippy passes with
warnings denied. Final mandatory and adversarial evidence is recorded below.
Publication and hosted disposition belong in the PR conversation and the
following session record because appending them here would create a different
head.

## Final Local Verification and Review

The first adversarial design review rejected a symmetric 128 KiB limit after
finding valid multi-megabyte Server 0.7 aggregates. Its 8 MiB configurable
policy, all-path wiring, caller-owned `from_stream`, error precedence, and
changelog recommendations were implemented. A frozen-diff review then found no
production defect but identified weak `None`, fragmented-boundary, explicit
rustls-constructor, and public zero-limit coverage. An independent repair pass
made those tests behaviorally discriminating without adding a TLS fixture or
dependency, and the final adversarial and GOAL/evidence audits reported no
remaining finding.

The exact mandatory command passed on the final local tree: formatting was
unchanged, workspace/all-target/all-feature Clippy emitted zero warnings, and
the complete all-feature workspace test suite passed. Focused no-token-feature
tests separately retain feature-error precedence over the invalid size option.

## Remaining Issue #126 Work

Disconnect-adjacent transition parity is the next independent client-owned
correctness slice. Browser/engine receive-limit ownership needs explicit
authority before adding SDK policy, and the fresh analyzer/performance phases
remain after the correctness inventory. Same-kind room response identity must
wait for the negotiated server contract rather than use timing heuristics.
