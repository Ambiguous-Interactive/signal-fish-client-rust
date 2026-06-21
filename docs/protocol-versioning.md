# Protocol Versioning

The Signal Fish Client SDK speaks two generations of the signaling protocol:
**v2 relay** and **v3 mesh**. v3 is **additive, opt-in, and backward-compatible**
— a client that opts into nothing behaves exactly like the old v2 client. This
page explains what's new, how negotiation works, and how to migrate.

!!! tip "Just want peer-to-peer?"
    If you have a WebRTC stack and want full mesh, jump to the
    [Mesh Guide](mesh-guide.md). This page covers the versioning model that makes
    it safe.

---

## The relay-floor guarantee

The single most important compatibility invariant:

> A client that opts into nothing behaves **byte-identically** to the old v2
> client.

`SignalFishConfig::new("app")` leaves the v3 negotiation fields unset. Because
each is `Option` (skipped when `None`), they vanish from the wire and the
`Authenticate` bytes equal v2. The server relays all traffic through itself
(the "relay floor"), emits no v3 messages, and every existing flow is unchanged.

```rust,ignore
// v2 relay floor — the default. Byte-identical to the old client.
let config = SignalFishConfig::new("mb_app_abc123");
```

This is why upgrading the SDK is safe: existing code keeps working with no
changes at all.

---

## What's new in v3

v3 is **purely additive** on v2. It introduces:

- **New wire types:** [`Topology`](protocol.md#topology-protocol-v3),
  [`TransportKind`](protocol.md#transportkind-protocol-v3),
  [`IceServer`](protocol.md#iceserver-protocol-v3),
  [`SessionPeer`](protocol.md#sessionpeer-protocol-v3),
  [`SessionPlanPayload`](protocol.md#sessionplanpayload-protocol-v3), and
  [`PeerSignal`](protocol.md#peersignal-protocol-v3).
- **New client messages:** `Signal`, `TransportStatus`.
- **New server messages:** `Signal`, `NewPeer`, `SessionPlan`,
  `PeerTransportStatus`.
- **New events:** [`SignalReceived`, `NewPeer`, `SessionPlan`, `PeerTransportStatus`](events.md#mesh-events-protocol-v3).
- **New optional fields** on existing messages: `Authenticate`
  (`protocol_version` / `supported_transports` / `supported_topologies`),
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

Opt into v3 with one call:

```rust,ignore
// v3 mesh — advertises webrtc/relay transports and mesh/host/relay topologies.
let config = SignalFishConfig::new("mb_app_abc123").enable_mesh();
```

`enable_mesh()` sets the protocol version to [`PROTOCOL_VERSION`] and advertises
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
   (`protocol_version`, `supported_transports`, `supported_topologies`).
2. The server **clamps** to its own `[min, max]` range and echoes the negotiated
   `protocol_version` (plus min/max) back in `ProtocolInfo`. A v2 negotiation
   omits these fields entirely, so v2 bytes stay identical.
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
    // negotiated >= 3: send_signal / report_transport_status are available.
}
```

| Accessor | Returns |
|----------|---------|
| `negotiated_protocol_version()` | `Option<u16>` — `None` before `ProtocolInfo`, or for a v2 negotiation. |
| `supports_mesh()` | `bool` — `true` once the negotiated version is ≥ 3. |

---

## The fail-fast guard

The v3-only send methods — `send_signal`, `send_offer`, `send_answer`,
`send_ice_candidate`, `send_raw_signal`, and `report_transport_status` — check
the negotiated version **before** sending. If v3 has not been negotiated, they
return [`SignalFishError::ProtocolUnsupported`](errors.md) immediately rather
than letting the server reject the message asynchronously (an unattributed
`Error` event would be much harder to debug).

The `mode` field tells you why:

| `mode` | Meaning |
|--------|---------|
| `"pre-negotiation"` | No `ProtocolInfo` has arrived yet — negotiation is still in flight; retry once it completes. |
| `"relay-only"` | A `ProtocolInfo` arrived but negotiated v2 (the relay floor) — waiting will not help; enable the mesh and reconnect. |

```rust,ignore
match client.send_offer(peer, sdp) {
    Ok(()) => {}
    Err(SignalFishError::ProtocolUnsupported { mode }) => {
        eprintln!("not in mesh mode ({mode}) — did you enable_mesh()?");
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

Migration is **purely additive** — there is nothing you must change:

- **Existing v2 code keeps working unchanged.** Don't call `enable_mesh()` and
  you stay on the byte-identical relay floor.
- **One v2 behavior change:** the game now starts **explicitly**. If you relied
  on the game auto-starting when everyone was ready, call `client.start_game()`
  (typically on `LobbyStateChanged { all_ready: true, .. }`). Rejections surface
  as `GameStartNotReady` / `GameStartForbidden` error codes.
- **To adopt mesh,** add `.enable_mesh()`, wire a
  [`WebRtcDriver`](mesh-guide.md) (or use `MeshController`), and handle the four
  [mesh events](events.md#mesh-events-protocol-v3).

[`PROTOCOL_VERSION`]: https://docs.rs/signal-fish-client/latest/signal_fish_client/constant.PROTOCOL_VERSION.html

---

## See also

- [Mesh Guide](mesh-guide.md) — implementing WebRTC mesh end to end.
- [Core Concepts](concepts.md#protocol-versioning--topology) — the conceptual overview.
- [Protocol Types](protocol.md) — the v3 wire types in detail.
- [Events](events.md#mesh-events-protocol-v3) — the v3 events.
- [Errors](errors.md) — `ProtocolUnsupported` and the v3 error codes.
