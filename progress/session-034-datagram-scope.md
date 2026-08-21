# Session 034 — Datagram Scope Decision

## Scope and Priority

Issue #107 was the highest gameplay-impacting actionable issue after PR #116
merged issue #106. The hosted audit found no open, draft, or dependency PRs and
no pending review feedback. Main matched local commit `64045da`; ten of eleven
required aggregates were green while the long Godot Web push run was still in
progress. Issue #90 remains a separate maintainer-administration governance
blocker.

## Decision

Raw UDP/datagram transport remains out of scope for this SDK. `Transport`
begins at one complete, ordered text/binary signaling-frame stream bound to one
intended server. It contains no source address or peer identity, so the core
attributes every yielded frame to that connection. A raw stream/datagram
backend must own framing, maximum size, signaling-server trust/source binding,
fragmentation/reassembly, loss/duplicate/reorder behavior, and error/close
semantics before it yields a `TransportFrame`. If its trust policy provides no
cryptographic identity, the SDK provides no additional spoof protection.

The core validates JSON/MessagePack representation, lifecycle, and v3 delivery
consistency only after frame admission. Those checks are not datagram source
authentication. A backend that cannot repair or reject raw transport damage
must report a transport error rather than fabricate, reorder, or silently skip
frames.

## Authoritative Server Evidence

All server evidence is pinned to Signal Fish Server 0.7 commit
`3f7f43d4cd4b3cc7f8fb893220dc35c9b1fad333`.

| Question | Evidence | Conclusion |
| --- | --- | --- |
| What message endpoint exists? | Vendored AsyncAPI defines one bidirectional WebSocket channel; `ProtocolInfo` advertises only `websocket`. | Signaling and relayed `GameData` use the existing WebSocket connection. |
| Does `JoinRoom.relay_transport` activate TCP/UDP? | `room_service.rs` binds it as `_relay_transport` and never reads it. | It is ignored legacy wire metadata in Server 0.7. |
| Does Server 0.7 own a separate relay endpoint? | `relay_policy.rs` states that the server includes no relay server and uses `relay_type` only for protocol labeling. | `relay_type`/`RelayTransport` are not evidence of a physical data path. |
| Is `ConnectionInfo::Relay` authoritative? | `ProvideConnectionInfo` accepts self-declared legacy metadata and forwards it to peers; only `Direct` has a narrow v3 host-validation use. | The application/engine owns endpoint trust and networking behavior. |
| Who owns WebRTC UDP? | `WebRtcDriver` is consumer-implemented; the integration guide places UDP sockets and WebRTC polling inside the backend. | ICE/DTLS/SCTP, authentication, fragmentation, ordering, and reliability precede `DriverEvent::Data`. |

## Issue #107 Acceptance Matrix

| Acceptance criterion | Resolution |
| --- | --- |
| Decide whether UDP belongs in this SDK. | Out of scope: no UDP signaling endpoint, SDK datagram envelope, or owned UDP backend exists. |
| Define datagram envelope/version/size/authentication/fragmentation/error policy if in scope. | Not applicable here; documented as prerequisites for the component that owns any future datagram protocol. |
| Map ownership/backpressure into `Transport` or add an abstraction. | `Transport` remains connection-oriented. Only an adapter that reconstructs its complete ordered signaling-server-bound frame contract can implement it; otherwise use a separate abstraction. |
| Add parser fuzzing and malformed datagram corpora. | Deliberately not added. Existing binary fuzzing begins with assembled MessagePack envelopes and is not UDP-resilience evidence. |
| Add UDP loopback evidence. | Deliberately not added because no wire contract exists. Existing real-server evidence covers the supported WebSocket endpoint. |
| Document protections and exclusions. | Public Rust docs, README, guides, canonical context, skill, and changelog now state the exact boundary and correct misleading relay labels. |
| Close with owning component/reference. | The planned client PR will use `Closes #107`; server issue #393 tracks clarification or retirement of the ignored wire surface. |

## Non-Bloating Implementation

No socket, dependency, feature flag, parser, fuzz target, error variant, or
`Transport` method was added. The legacy serde shape remains byte-compatible.
Documentation now distinguishes `Transport`, `MessageTransport`,
`TransportKind`, `RelayTransport`, self-declared `ConnectionInfo`, and
consumer-owned WebRTC behavior without inventing runtime guarantees.

Server issue #393 records the upstream AsyncAPI/implementation drift and its
own acceptance criteria.

## Verification

- The mandatory `cargo fmt && cargo clippy --workspace --all-targets
  --all-features -- -D warnings && cargo test --workspace --all-features`
  workflow passes.
- Strict workspace Rustdoc passes with `RUSTDOCFLAGS='-D warnings'`.
- `scripts/validate-docs.sh` passes; its optional spelling check reports the
  pre-existing local absence of `typos`.
- An isolated `requirements-docs.txt` virtual environment passes all 17
  `scripts/check-docs-rendering.sh` checks, including `mkdocs build --strict`,
  rendered fences/Mermaid, links, and exact published `llms.txt` output.
- Three independent audits agreed on the out-of-scope decision and found the
  pinned Server 0.7 ignored-field/self-declared-metadata distinctions that were
  incorporated into the final wording.

Hosted PR evidence is not yet available before the initial branch push; the
final-head run links and review state will be attached to the PR after CI.
