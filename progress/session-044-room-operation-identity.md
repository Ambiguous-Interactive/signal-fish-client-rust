# Session 044 — Negotiated Room-Operation Identity

## Priority and Authority

The session began from clean `main` at `c2b22b2`, the merge of PR #132. The
GitHub connector found client issues #128, #126, and #90 open and no open pull
requests. Signal Fish Server issue #395 had closed through PR #398, removing the
external blocker for client issue #128. The exact post-0.7 protocol authority is
server commit `2d7c3836edf64bb734482b7fbb2b3db3f88fea8b`; released runtime
compatibility remains separately bound to Server 0.7.0 commit
`3f7f43d4cd4b3cc7f8fb893220dc35c9b1fad333` and Server 0.4.0.

## Negotiation and Wire Contract

Configurations that can negotiate v3 request the exact
`room_operation_ids` capability in `Authenticate`. Default and explicit-v2
configurations omit `requested_capabilities`, preserving the relay-floor bytes.
The shared core enables correlation only after the exact token is echoed in a
valid v3 `ProtocolInfo`; unknown tokens are tolerated, missing echoes retain
legacy behavior, unsolicited echoes are violations, and an echo below v3 is
invalid.

After activation, all five directed room operations use a `RoomOperation`
envelope with a fresh client-generated UUID. The matching terminal
`RoomOperationResult` must echo the exact ID and carry a result allowed for that
operation kind. Human-readable IDs accept only lowercase hyphenated UUID text.
The ID is a correlation fence, not an idempotency key; the server only echoes
it. Physical disconnect clears the negotiated mode and pending scope.

An operation admitted before `ProtocolInfo` retains a legacy `None` ID for its
complete lifetime even if capability negotiation finishes while it is pending.
This closes the handshake race without delaying established join-on-auth flows.
New operations admitted after the echo are correlated. Queue refusal never
records the generated admission, and the frame ID is asserted equal to the
recorded pending ID.

## Shared-Core Safety

Wrong, stale, duplicate, unknown, malformed, wrong-kind, and unwrapped directed
results produce a protocol violation without mutating membership or consuming
the current fence under Observe, Quarantine, or Disconnect. A top-level `Error`
remains uncorrelated and cannot clear a pending operation. Exact
`OperationFailed` releases only its matching fence and reconnect metadata and
emits the distinct `RoomOperationFailed` event; it does not set
`last_server_error` or enter the delivery-accountability special case for
top-level `Error`.

Autonomous spectator removal, disconnection, and room closure remain valid
top-level `SpectatorLeft` messages. Correlated spectator-leave results accept
only the voluntary/omitted reason shape, preventing an operation result from
impersonating an autonomous server action. Correlated envelopes remain
forbidden inside reconnect `missed_events` by the existing replay allowlist.

## Regression and Provenance Evidence

The server's complete room-operation golden suite now runs in the client. It
pins exact JSON and MessagePack bytes for five requests, eight operation-specific
results, `OperationFailed`, canonical-ID rejection, additive-field tolerance,
and variant-only secret-redacting `Debug`. The four JSONL files and AsyncAPI
spec were copied verbatim from the authority commit, with refreshed SHA-256
markers. V2 artifact hashes are unchanged.

Core regressions cover request/echo/downgrade state, the pre-echo temporal race,
fresh and exact IDs, the complete result-kind matrix, stale and wrong IDs,
malformed frames, top-level errors, attributed failure cleanup, and all
violation policies. A dedicated async/polling parity transport holds operation B
under send backpressure, injects delayed result A while B is pending, proves B
remains fenced and unsent, then releases and completes B exactly once.

## Adversarial Review

The initial design review found and corrected four material hazards: global
post-echo rejection of a valid legacy pending response, duplicated capability
intent, correlated command construction that could discard its admission, and
payload-bearing derived `Debug`. A second integration review found that mapping
`OperationFailed` to top-level `Error` would corrupt disconnect attribution and
delivery accountability; the distinct event preserves those semantics. The
backpressure parity race was added after review rejected tests that injected a
stale result only after the current send had already completed.
A frozen integration review then found an avoidable heap allocation on every
inbound message in the normalization path. The internal outcome now carries
ordinary server messages directly through a standard conversion result,
preserving the hot path while retaining the special correlated-failure branch.
A final integration run caught the stale deterministic performance pins before
commit. All 28 protocol digests were reviewed and refreshed because v3
authentication bytes changed and the event ledger gained a slot. Measurement
also showed that the added `Authenticate` field widens each polling command
cell by 24 bytes: 64-command queue growth moves 1,440 more bytes without adding
an allocation or reallocation. The four exceeded byte ceilings and affected
README rows now record that intentional cost; debug and release allocation
records remain identical across ten isolated samples.
The existing v3 Rkyv-request end-to-end test also passed against a locally
built server at the exact `2d7c3836` authority commit, exercising capability
request/echo, correlated player join and result normalization, and public
`RoomJoined` delivery through a real WebSocket connection.

## Verification and Hosted Disposition

Focused wire, lifecycle, parity, policy, performance, docs.rs, and no-panic
checks passed during implementation. The final mandatory local chain passed:
`cargo fmt`, warnings-as-errors all-target/all-feature workspace Clippy, and the
complete all-feature workspace test suite. The warnings-as-errors no-default
feature build also passed. MkDocs rendering was unavailable in the container
because `mkdocs` is not installed, and only the native aarch64 target is
installed; hosted CI owns those matrix checks. Commit, PR, review, and hosted
check evidence are recorded after the hosted tree is immutable.
