---
name: webrtc-mesh-signaling
description: Maintain matchbox-compatible WebRTC mesh signaling. Use when changing PeerSignal, WebRtcDriver, MeshController choreography, offerer selection, ICE handling, or mesh backend integration.
---

# WebRTC Mesh Signaling

Reference for the matchbox-compatible `PeerSignal` shape, the `WebRtcDriver` seam, the `MeshController` choreography, and integrating a real WebRTC backend (str0m / web-sys). Enabled by the `mesh` feature.

## Signaling-Only Boundary

This crate ferries WebRTC *signals*; it bundles **no** WebRTC stack — no ICE
agent, no SDP munging, no TURN, no media. The consumer brings a WebRTC backend
and the SDK orchestrates the handshake against it. Keep this boundary: never add
a WebRTC implementation to the core crate.

## PeerSignal Shape

`PeerSignal` is **externally tagged** (serde default — no `tag`/`content`/`rename_all`),
byte-identical to `matchbox_socket::PeerSignal`:

```json
{ "Offer": "<sdp>" }
{ "Answer": "<sdp>" }
{ "IceCandidate": "<candidate>" }
```

The wire field on `ClientMessage::Signal` / `ServerMessage::Signal` is
`serde_json::Value` (so an unknown future signal shape never breaks deserialization).
`PeerSignal` is the typed convenience with an infallible `From<PeerSignal> for Value`
— it constructs the externally-tagged object **directly** (`json!({ "Offer": sdp })`),
structurally infallible with no lossy `Null` fallback — and a fallible
`TryFrom<&Value>`. See [serde-patterns](../serde-patterns/SKILL.md).

## Designated Offerer / Initiate Obedience

The server assigns the offerer deterministically (lesser-UUID-initiates in mesh;
clients-initiate-to-host in host topology) and tells each client via the
`initiate` flag (`SessionPlan.peers[].initiate`) and `you_initiate` (`NewPeer`).
The client **must obey verbatim** — never compute who offers, never both-offer.
This avoids glare without perfect-negotiation rollback.

Because there is no rollback, when the server *reassigns* the offerer — a host
re-election or topology change flips `initiate`/`you_initiate` for a peer that
survives the re-plan — `MeshController` restarts that peer's handshake in the new
role (`disconnect` then `connect`) instead of leaving the driver in the stale one,
which would otherwise glare (both offer) or stall (both wait). A survivor whose
role is unchanged keeps its live connection only within the same generation.
Every generation change rebuilds all retained physical pairs, drops buffered
signals, and rejects late driver output from the prior generation.

## MeshSession Tracker (no WebRTC)

`MeshSession` folds the v3 events into a consistent view (`topology`/`transport`/
`generation`/`host`/`direct_endpoint`/`peers`/`ice_servers`, each peer with
`initiate` + selected-path `connected`). A peer status mutates liveness only
when its transport matches the plan; generation, transport, or offerer-role
changes clear stale liveness. Plans replace peers and ICE wholesale.

## WebRtcDriver Seam

Implement this sync, poll-based trait against your backend:

```rust
pub trait WebRtcDriver {
    fn set_ice_servers(&mut self, servers: &[IceServer]);
    fn connect(&mut self, peer: PlayerId, generation: Option<SessionGeneration>, initiate: bool);
    fn on_signal(&mut self, peer: PlayerId, generation: Option<SessionGeneration>, signal: PeerSignal);
    fn send(&mut self, peer: PlayerId, data: &[u8]);
    fn disconnect(&mut self, peer: PlayerId);
    fn poll(&mut self) -> Option<DriverEvent>; // do real I/O here
}
```

`poll` returns generation-bearing
`DriverEvent::{Signal, Connected, Disconnected, Data}`. It is
sync + poll-based so it fits the async client, the WASM/polling client, and
sans-I/O backends like str0m.

## MeshController (batteries-included)

`MeshController::start(transport, config, driver)` drives the whole handshake:
on WebRTC `SessionPlan`/`NewPeer` it calls
`connect(peer, generation, initiate)` and `set_ice_servers`;
on `SignalReceived` it feeds `on_signal`; it relays the driver's outbound `Signal`s
via the client, reports `TransportStatus` on the 0↔1 connected boundary, tears down
peers on re-election/`PlayerLeft`/`RoomLeft`/`Disconnected`, and surfaces a
`MeshEvent` stream (`Signaling`, `PeerConnected`, `PeerDisconnected`, `Data`).
`start` ensures v3, WebRTC, and a P2P topology even for a preselected v3 config;
compatible explicit lists and future versions remain intact. `MeshController<D>`
is `Send` when `D` is; drive a `!Send` driver on the current task.

Direct and Relay plans never drive `WebRtcDriver`; they clear controller WebRTC
state. Applications may implement Direct separately from the validated
`MeshSession::direct_endpoint()` while retaining Relay fallback.

## Integrating a Real Backend

The SDK never bundles WebRTC; map your backend's primitives onto `WebRtcDriver`.

**str0m** (sans-I/O, native; the recommended backend — matchbox's data plane is
coupled to its own signaling and cannot consume our external signals):

- Own a `str0m::Rtc` + a UDP socket per peer.
- `connect(peer, true)`: `rtc.sdp_api()` → create offer → emit `DriverEvent::Signal{Offer}`.
- `on_signal`: apply the remote SDP via `sdp_api`, or `rtc.add_remote_candidate` for ICE.
- `poll`: non-blocking UDP read → `rtc.handle_input`, then drain `rtc.poll_output`
  into `DriverEvent::Signal` (trickled ICE), `Connected`, `Data`, and `Transmit`
  (write UDP). Honor the `Output::Timeout` cadence with the controller's pump interval.
- `send`: write to the str0m data channel.

**web-sys** (browser/WASM): wrap `RtcPeerConnection`. `connect(_, true)` →
`create_offer` → `set_local_description` → emit via `poll`; `on_signal` →
`set_remote_description` / `add_ice_candidate`; `onicecandidate` callbacks queue
`DriverEvent::Signal`; `ondatachannel`/`onmessage` queue `Data`. This driver is
`!Send`, so drive `MeshController::recv()` on the current task.

**webrtc-rs**: works via manual signaling (`create_offer`/`set_*_description`/
`add_ice_candidate`/`on_ice_candidate`/`on_data_channel`) but is Tokio-coupled and
heavier; prefer str0m for new native drivers.

See [protocol-versioning-and-negotiation](../protocol-versioning-and-negotiation/SKILL.md)
for the relay-floor guarantee and the fail-fast guard.
