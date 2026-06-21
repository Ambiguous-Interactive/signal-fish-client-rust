# Protocol Versioning and Capability Negotiation

Reference for the relay-floor backward-compatibility guarantee, v2/v3 capability negotiation, and the "client obeys the server" model in the Signal Fish protocol.

## The Relay Floor Guarantee

The single most important compatibility invariant: a client that opts into nothing
behaves **byte-identically to the old v2 client**. Its `Authenticate` carries none
of the v3 negotiation fields, the server emits no v3 messages, and every existing
flow is unchanged.

- `SignalFishConfig::new(app)` leaves `protocol_version` / `supported_transports` /
  `supported_topologies` as `None`. All three are `#[serde(skip_serializing_if = "Option::is_none")]`,
  so they vanish from the wire — the `Authenticate` bytes equal v2.
- This is pinned by `authenticate_relay_floor_omits_v3_fields` (protocol tests),
  the client-produced-path tests in `client.rs`/`polling_client.rs`, and
  `v2_authenticate_sample_carries_no_v3_fields` against the real server sample.

**Never regress the relay floor.** Any new optional field on an existing message
MUST be `Option` + `skip_serializing_if` (or `Vec` + `default` + `skip_if-empty`).

## Capability Negotiation Flow

1. The client advertises what it can fulfill in `Authenticate`:
   `protocol_version: Option<u16>`, `supported_transports: Option<Vec<TransportKind>>`,
   `supported_topologies: Option<Vec<Topology>>`.
2. The server clamps to its own `[min, max]` and echoes the result in `ProtocolInfo`
   (`protocol_version` / `min_protocol_version` / `max_protocol_version`, all
   `Option<u16>`, omitted for a v2 negotiation so v2 bytes stay identical).
3. If the negotiation is < v3, or transports/topologies were omitted, the server
   keeps the room on the relay floor and emits no v3 messages.

The client tracks the negotiated version from `ProtocolInfo`:
`SignalFishClient::negotiated_protocol_version()` and `supports_mesh()` (true when
the negotiated version is ≥ 3).

## Never Advertise What You Cannot Fulfill

The SDK is **signaling-only** — it bundles no WebRTC. Advertising `webrtc`/`mesh`
when no driver is wired would make the server build a `SessionPlan` the client
can't honor. So:

- `SignalFishConfig::new(app)` advertises nothing (relay floor).
- `SignalFishConfig::enable_mesh()` is the opt-in for consumers who HAVE a WebRTC
  stack (or use `MeshController`). It advertises v3 + `[webrtc, relay]` +
  `[mesh, host, relay]`.

## The Fail-Fast Guard

v3-only sends (`send_signal`/`send_offer`/`send_answer`/`send_ice_candidate`/
`send_raw_signal`/`report_transport_status`) call `ensure_v3()` and return
`SignalFishError::ProtocolUnsupported { mode }` when the connection has not
negotiated v3 — fail-fast at the call site instead of an async, unattributed
`Error` event. `mode` is `"relay-only"` (authenticated, not v3) or
`"pre-negotiation"` (no `ProtocolInfo` yet). `start_game` is **not** guarded — it
is the universal v2 change.

The guard threshold is `>= 3` (the version that introduced mesh signaling), NOT
`>= PROTOCOL_VERSION` — a future SDK version bump must not reject a v3-negotiated
connection.

## v2 Delta: Explicit StartGame

Game start is now **explicit**: `ClientMessage::StartGame` (unit variant,
`{"type":"StartGame"}`) finalizes the lobby. It is accepted only when every
current player is ready; if the room has an authority, only the authority may
start. Errors: `GameStartNotReady` (`GAME_START_NOT_READY`), `GameStartForbidden`
(`GAME_START_FORBIDDEN`). **Migration:** consumers that relied on the game
auto-starting on readiness must now call `start_game()`.

## v3 Delta: Additive Mesh

Purely additive on v2. New wire types (`Topology`, `TransportKind`, `IceServer`,
`SessionPeer`, `SessionPlanPayload`, `PeerSignal`), new messages (`Signal`,
`TransportStatus`, `NewPeer`, `SessionPlan`, `PeerTransportStatus`), optional
`ice_servers` on `RoomJoined`/`Reconnected`, and six new *signaling/lifecycle*
error codes (the `ErrorCode` (8 new) figure in
[public-api-design](public-api-design.md) also counts the two v2 `GameStart`
codes). See
[webrtc-mesh-signaling](webrtc-mesh-signaling.md).

## Client Obeys the Server

The server is the brain: it selects topology/transport and assigns the
deterministic WebRTC offerer via the `initiate` (in `SessionPlan.peers`) and
`you_initiate` (in `NewPeer`) flags. The client **copies these verbatim** and
never computes who offers. See [public-api-design](public-api-design.md) for the
exhaustive-variant semver rules and the `TransportKind`-vs-`Transport`-trait
naming rule, and [crate-publishing](crate-publishing.md) for the version bump.

## Forward Compatibility

Protocol types must tolerate unknown (additive) server fields — `deny_unknown_fields`
is forbidden in the protocol layer (enforced by `no_protocol_type_uses_deny_unknown_fields`).
An unknown `type` value fails to deserialize and the transport loop logs + skips
it; there is intentionally no catch-all `Unknown` variant.
