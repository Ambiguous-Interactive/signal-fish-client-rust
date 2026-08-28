# Protocol Versioning

The Signal Fish Client SDK speaks two generations of the signaling protocol:
**v2 relay** and **v3 delivery/mesh**. v3 negotiation is **additive, opt-in,
and backward-compatible** — a client that opts into nothing sends the same v2
authentication bytes and remains on the relay floor. Version 0.8 separately
made protocol-v2 game start explicit. This page explains both changes.

!!! info "Server 0.7 token binding is a transport negotiation"
    The opt-in native `token-binding` feature supports disabled, optional, and
    required `signalfish.tokenbinding.v2` modes. It is negotiated and completed
    before either protocol driver sends `Authenticate`, so it is separate from
    v2/v3 application capability negotiation. See
    [WebSocket Token Binding](token-binding.md).

!!! tip "Just want peer-to-peer?"
    If you have a WebRTC stack and want full mesh, jump to the
    [Mesh Guide](mesh-guide.md). This page covers the versioning model that makes
    it safe.

---

## The relay-floor guarantee

The single most important compatibility invariant:

> A client's default `Authenticate` message is **byte-identical** to the old
> v2 client.

`SignalFishConfig::new("app")` leaves the v3 negotiation fields unset. Because
each is `Option` (skipped when `None`), they vanish from the wire and the
`Authenticate` bytes equal v2. The server relays all traffic through itself
(the "relay floor"), emits no v3 messages, and every existing flow is unchanged.

```rust,ignore
// v2 relay floor — the default. Byte-identical to the old client.
let config = SignalFishConfig::new("mb_app_abc123");
```

This protects relay-floor negotiation from accidental v3 opt-in. It does not
remove the 0.8 game-start migration: applications that relied on readiness
auto-starting the game must call `start_game()`.

---

## What's new in v3

v3 is **purely additive** on v2. It introduces:

- **New wire types:** [`Topology`](protocol.md#topology-protocol-v3),
  [`TransportKind`](protocol.md#transportkind-protocol-v3),
  [`IceServer`](protocol.md#iceserver-protocol-v3),
  [`SessionPeer`](protocol.md#sessionpeer-protocol-v3),
  [`SessionPlanPayload`](protocol.md#sessionplanpayload-protocol-v3),
  [`PeerSignal`](protocol.md#peersignal-protocol-v3), `RoomOperationId`,
  `RoomOperationRequest`, and `RoomOperationResult`.
- **New client messages:** `Signal`, `TransportStatus`, and negotiated
  `RoomOperation`.
- **New server messages:** `Signal`, `NewPeer`, `SessionPlan`,
  `PeerTransportStatus`, `DeliveryReport`, `RelayStats`, `GoingAway`, and
  negotiated `RoomOperationResult`.
- **Classified relay delivery:** reliable, keyed-latest, and volatile JSON
  messages with exact gap accountability; strict physical binary envelopes.
- **Reconnect accountability:** rotating tokens, replay status, sender
  watermarks, and lifecycle epoch/sequence metadata.
- **New events:** mesh events plus typed delivery reports, relay statistics,
  graceful-drain advisories, categorized protocol violations, and attributed
  `RoomOperationFailed` outcomes.
- **New optional fields** on existing messages: `Authenticate`
  (`protocol_version` / `supported_transports` / `supported_topologies` /
  `requested_capabilities`),
  `ProtocolInfo` (`protocol_version` / `min_protocol_version` /
  `max_protocol_version`), and `ice_servers` on `RoomJoined` / `Reconnected`
  (ICE "pre-gather").
- **Six new signaling/lifecycle error codes** plus two v2 `GameStart` codes —
  see [Errors](errors.md).

A v2 client safely **ignores** any v3 field it doesn't recognize: the protocol
types never use `deny_unknown_fields`, so unknown additive fields deserialize
without error.

---

## Opting in

Opt into the portion of v3 you can fulfill:

```rust,ignore
// v3 relay/accountability, without claiming WebRTC support.
let relay_config = SignalFishConfig::new("mb_app_abc123").enable_v3();

// v3 mesh — use only with a WebRTC driver.
let mesh_config = SignalFishConfig::new("mb_app_abc123").enable_mesh();
```

`enable_v3()` sets the protocol version and advertises relay-only capability.
`enable_mesh()` calls it and additionally advertises
the `[WebRtc, Relay]` transports and `[Mesh, Host, Relay]` topologies.

!!! warning "Never advertise what you can't fulfill"
    The SDK is **signaling-only** — it bundles no WebRTC stack. Only call
    `enable_mesh()` when you actually bridge the resulting signaling events to a
    WebRTC implementation (or use [`MeshController`](mesh-guide.md)). Advertising
    `webrtc`/`mesh` with no driver would make the server build a `SessionPlan`
    you can't honor.

Power-user escape hatches exist for finer control:
`with_protocol_version(v)`, `with_transports([...])`, `with_topologies([...])`.

---

## Capability negotiation

Negotiation is a single round trip layered onto the existing handshake:

1. The client **advertises** what it can fulfill in `Authenticate`
   (`protocol_version`, `supported_transports`, `supported_topologies`, and
   additive `requested_capabilities`).
2. The server caps the advertised version at its configured maximum and echoes
   the negotiated `protocol_version` (plus min/max) back in `ProtocolInfo`. If
   the client's maximum is below the server minimum, authentication fails with
   `UNSUPPORTED_PROTOCOL_VERSION`; the server never upgrades the client above
   what it advertised. A v2 negotiation omits these fields entirely.
3. If the negotiation is below v3 (or transports/topologies were omitted), the
   server keeps the room on the relay floor and emits no v3 messages.

Read the result on the client:

```rust,ignore
// After ProtocolInfo has arrived:
match client.negotiated_protocol_version() {
    Some(v) => println!("negotiated protocol v{v}"),
    None => println!("relay floor (v2) — or not negotiated yet"),
}

if client.supports_mesh() {
    // This client negotiated v3 after advertising WebRTC plus Host or Mesh.
    // The server may still select an explicit Relay plan.
}

let snapshot = client.snapshot();
println!("selected path: {:?} / {:?}", snapshot.session_topology, snapshot.session_transport);
```

| Accessor | Returns |
|----------|---------|
| `negotiated_protocol_version()` | `Option<u16>` — `None` before `ProtocolInfo`, or for a v2 negotiation. |
| `supports_mesh()` | `bool` — negotiated local capability: v3 plus advertised WebRTC and at least one P2P topology (`Host` or `Mesh`). It does not mean the active plan is P2P. |
| `session_topology()` / `session_transport()` | The topology and transport selected by the latest authoritative plan, or `None` outside a plan. Read both from `snapshot()` when they must describe one instant. |
| `is_p2p_active()` | `bool` — whether the selected topology is currently `Host` or `Mesh`, independent of local capability. |

### 0.11 migration: capability versus active plan

`supports_mesh()` is retained for source compatibility, but its meaning is now
precise: it requires advertised WebRTC, an advertised `Host` or `Mesh` topology,
and negotiated v3. A custom WebRTC + relay-only configuration therefore changes
from `true` to `false`. It never guarantees that the server selected P2P.

`ClientSnapshot` is exhaustive and gains `session_topology` and
`session_transport`, so 0.11 consumers constructing snapshots must initialize
both fields. Routing code should read those fields from one `snapshot()` (or use
`is_p2p_active()` for a one-value query); do not substitute `supports_mesh()`.
For the release's other error-surface changes, see
[Migrating 0.10 to 0.11](migration-0.11.md).

---

## The fail-fast guard

After connection and room-role validation, the v3-only send methods —
classified non-reliable JSON sends, binary sends,
`send_signal`, `send_offer`, `send_answer`,
`send_ice_candidate`, `send_raw_signal`, and `report_transport_status` — check
the negotiated version **before** sending. If v3 has not been negotiated, they
return [`SignalFishError::ProtocolUnsupported`](errors.md) immediately rather
than letting the server reject the message asynchronously (an unattributed
`Error` event would be much harder to debug).

This ordering is intentional: a player-only command attempted outside a room
returns `NotInRoom` (or `WrongRoomRole` for a spectator) even if negotiation is
still in flight. Once player membership is valid, `ProtocolUnsupported`
describes the remaining version failure precisely.

Signaling has a second guard: every send requires an authoritative WebRTC
`SessionPlan` that authorizes its target. Otherwise it returns
`SignalFishError::SessionPlanUnavailable` without queuing a frame. This covers
no plan, a non-WebRTC plan, self, and peers absent from either the authoritative
session peer set (the replacement plan plus valid compatibility `NewPeer`
updates) or current room roster. Accepted signals use the plan's generation.

The `mode` field tells you why:

| `mode` | Meaning |
|--------|---------|
| `"pre-negotiation"` | No `ProtocolInfo` has arrived yet — negotiation is still in flight; retry once it completes. |
| `"relay-only"` | A `ProtocolInfo` arrived but negotiated v2 (the relay floor) — waiting will not help; enable v3 and reconnect. |

```rust,ignore
match client.send_offer(peer, sdp) {
    Ok(()) => {}
    Err(SignalFishError::ProtocolUnsupported { mode }) => {
        eprintln!("protocol v3 was not negotiated ({mode})");
    }
    Err(e) => eprintln!("send failed: {e}"),
}
```

!!! note "`start_game()` is not guarded"
    Explicit game start is the one **universal v2** change (the game no longer
    auto-starts on readiness). `client.start_game()` works on every connection
    and is not gated behind the mesh opt-in.

---

## Migrating from v2 to v3

Adopting v3 capabilities is additive, but upgrading an older SDK still requires
the explicit-start audit:

- **Relay negotiation stays v2 by default.** Don't call `enable_v3()` or
  `enable_mesh()` and the authentication bytes stay on the relay floor.
- **One v2 behavior change:** the game now starts **explicitly**. If you relied
  on the game auto-starting when everyone was ready, call `client.start_game()`
  (typically on `LobbyStateChanged { all_ready: true, .. }`). Rejections surface
  as `GameStartNotReady` / `GameStartForbidden` error codes. Use a one-shot
  latch and, in authority rooms, require current authority; see
  [Migrating 0.7 to 0.8](migration-0.8.md#explicit-game-start).
- **To adopt mesh,** add `.enable_mesh()`, wire a
  [`WebRtcDriver`](mesh-guide.md) (or use `MeshController`), and handle the four
  [mesh events](events.md#mesh-events-protocol-v3).
- **To adopt v3 relay only,** add `.enable_v3()`, choose delivery classes with
  `GameDataDelivery`, persist `snapshot().reconnection_token`, and handle
  `ProtocolViolation` according to your recovery policy. See
  [Delivery Contract](delivery.md#protocol-v3-delivery-classes-and-accountability).

---

## See also

- [Mesh Guide](mesh-guide.md) — implementing WebRTC mesh end to end.
- [Core Concepts](concepts.md#protocol-versioning-and-topology) — the conceptual overview.
- [Protocol Types](protocol.md) — the v3 wire types in detail.
- [Events](events.md#mesh-events-protocol-v3) — the v3 events.
- [Errors](errors.md) — `ProtocolUnsupported` and the v3 error codes.
