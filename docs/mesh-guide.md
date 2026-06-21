# Mesh (v3) Guide

Protocol v3 adds **WebRTC mesh signaling**: the server can finalize a room into a
peer-to-peer session and ferry the WebRTC handshake between peers. This guide
covers when to use mesh, how to wire a WebRTC backend, and how to let the SDK
drive the whole handshake for you.

!!! info "Requires the `mesh` feature"
    The mesh helpers (`WebRtcDriver`, `MeshController`, `MeshSession`) live behind
    the `mesh` Cargo feature. `MeshController` additionally needs the
    `tokio-runtime` feature.

    ```toml
    signal-fish-client = { version = "0.5.0", features = ["mesh"] }
    ```

---

## When and why to use mesh

By default Signal Fish runs on the **relay floor** (v2): the server relays all
game traffic. That is simple and always works, but every packet makes a round
trip through the server. **Mesh (v3)** lets peers exchange game data directly over
WebRTC data channels — lower latency and less server load — while keeping the
relay as a universal fallback.

Use mesh when you have (or are willing to integrate) a WebRTC stack and want
direct peer-to-peer data. Stay on the relay floor if you want zero extra
dependencies. The choice is per-client: see [Protocol Versioning](protocol-versioning.md).

---

## Signaling-only boundary

**The SDK bundles no WebRTC stack** — no ICE agent, no SDP munging, no TURN, no
media. It ferries WebRTC *signals* (offers, answers, ICE candidates) and
orchestrates the handshake; you bring the WebRTC backend. This keeps the core
crate small and lets you pick the backend that fits your platform.

---

## Enabling mesh

Opt in when building the config:

```rust,ignore
use signal_fish_client::SignalFishConfig;

let config = SignalFishConfig::new("mb_app_abc123").enable_mesh();
```

`enable_mesh()` advertises protocol v3 with the `webrtc`/`relay` transports and
the `mesh`/`host`/`relay` topologies. The **server still chooses** the actual
topology and may keep the room on the relay floor; the client only declares what
it can fulfill.

!!! warning "Don't advertise what you can't fulfill"
    Only call `enable_mesh()` if you actually bridge the resulting signaling
    events to a WebRTC implementation (or use `MeshController`). See
    [Protocol Versioning](protocol-versioning.md#opting-in).

---

## Implementing `WebRtcDriver`

`WebRtcDriver` is the seam between the SDK and your WebRTC backend. It is
**synchronous and poll-based** so it fits the async client, the polling client,
and sans-I/O backends like str0m.

```rust,ignore
use signal_fish_client::protocol::{IceServer, PlayerId};
use signal_fish_client::webrtc::{DriverEvent, WebRtcDriver};
use signal_fish_client::PeerSignal;

pub trait WebRtcDriver {
    fn set_ice_servers(&mut self, servers: &[IceServer]);
    fn connect(&mut self, peer: PlayerId, initiate: bool); // obey `initiate`
    fn on_signal(&mut self, peer: PlayerId, signal: PeerSignal);
    fn send(&mut self, peer: PlayerId, data: &[u8]);
    fn disconnect(&mut self, peer: PlayerId);
    fn poll(&mut self) -> Option<DriverEvent>; // do real I/O here
}
```

| Method | What you do |
|--------|-------------|
| `set_ice_servers` | Configure your peer connections' STUN/TURN servers. |
| `connect` | Begin connecting to `peer`. If `initiate` is `true`, create an offer; otherwise wait for the remote offer. **Obey `initiate` verbatim.** |
| `on_signal` | Apply a remote offer/answer (`set_remote_description`) or add an ICE candidate. |
| `send` | Send application bytes over `peer`'s data channel. |
| `disconnect` | Tear down the connection to `peer`. |
| `poll` | Pump your stack's I/O and return the next `DriverEvent` (see below), or `None` when idle. |

`poll` returns a `DriverEvent`:

| Variant | Meaning |
|---------|---------|
| `Signal { peer, signal }` | A locally-produced offer/answer/ICE to relay to `peer` (the controller forwards it via the server). |
| `Connected { peer }` | The data channel to `peer` opened. |
| `Disconnected { peer }` | The data channel to `peer` closed or failed. |
| `Data { peer, data }` | Application bytes arrived from `peer`. |

!!! danger "Obey `initiate` — never both-offer"
    The server assigns the deterministic WebRTC offerer (lesser-UUID-initiates in
    mesh; clients-initiate-to-host in host topology) and tells each client via the
    `initiate` flag (`SessionPlan.peers[].initiate`) and `you_initiate`
    (`NewPeer`). The client **must copy these verbatim** — never compute who
    offers, never both-offer. This avoids WebRTC "glare".

### The optional `set_ready_waker` latency hook

By default the controller pumps your driver on every signaling event and on a
periodic timer (the [pump interval](#tuning-the-pump-interval)). That means
trickle ICE or inbound data produced *between* signaling events can wait up to one
pump interval before surfacing.

If your backend can signal when it has output ready, implement the optional
`set_ready_waker` method (available with the `tokio-runtime` feature) and call
`MeshWaker::wake()` to have the controller pump on demand — eliminating that
latency:

```rust,ignore
use signal_fish_client::webrtc::{MeshWaker, WebRtcDriver};

impl WebRtcDriver for MyDriver {
    // ... other methods ...

    fn set_ready_waker(&mut self, waker: MeshWaker) {
        // Store it; call `waker.wake()` whenever poll() has new output ready
        // (e.g. a trickled ICE candidate or received data).
        self.waker = Some(waker);
    }
}
```

`wake()` is cheap and safe to call from any thread and as often as you like (extra
wakes at worst cause a redundant, cheap poll). Implementing it is **entirely
optional** — drivers that don't override it simply fall back to the periodic
timer.

---

## Using `MeshController` (batteries-included)

`MeshController` drives the **entire** v3 handshake against your driver on top of
a `SignalFishClient`. On `SessionPlan`/`NewPeer` it calls `connect(peer,
initiate)` and `set_ice_servers`; on a received signal it feeds `on_signal`; it
relays the driver's outbound signals via the client, reports `TransportStatus` on
the 0↔1 connected boundary, tears down peers on re-election / `PlayerLeft` /
`RoomLeft` / `Disconnected`, and surfaces a clean `MeshEvent` stream.

```rust,ignore
use signal_fish_client::webrtc::{MeshController, MeshEvent};
use signal_fish_client::{JoinRoomParams, SignalFishConfig, SignalFishEvent};

// `start` auto-enables mesh if the config didn't.
let mut mesh = MeshController::start(transport, SignalFishConfig::new("app"), my_driver);

while let Some(event) = mesh.recv().await {
    match event {
        MeshEvent::Signaling(sig) => match *sig {
            SignalFishEvent::Authenticated { .. } =>
                mesh.join_room(JoinRoomParams::new("my-game", "Alice"))?,
            SignalFishEvent::LobbyStateChanged { all_ready: true, .. } =>
                mesh.start_game()?,
            _ => {}
        },
        MeshEvent::PeerConnected(peer) => {
            // The data channel to `peer` is open — send a packet.
            mesh.send_to(peer, b"hello peer");
        }
        MeshEvent::PeerDisconnected(peer) => println!("peer {peer} left"),
        MeshEvent::Data { from, data } => {
            println!("{} bytes from {from}", data.len());
        }
    }
}

mesh.shutdown().await;
```

| API | Purpose |
|-----|---------|
| `MeshController::start(transport, config, driver)` | Build the controller; auto-enables mesh if the config didn't. |
| `recv().await -> Option<MeshEvent>` | Drive the handshake and yield the next high-level event. `None` once the transport closes. |
| `send_to(peer, &[u8])` | Send application bytes to a peer over its data channel. |
| `with_pump_interval(Duration)` | Tune the periodic driver pump (default 20 ms). |
| `join_room` / `set_ready` / `start_game` / `leave_room` / `client()` | Room-lifecycle delegations to the inner client. |
| `shutdown().await` | Gracefully stop the controller and its client. |

`MeshEvent` has four variants: `Signaling(Box<SignalFishEvent>)` (every
underlying event, passed through verbatim), `PeerConnected(PlayerId)`,
`PeerDisconnected(PlayerId)`, and `Data { from, data }`.

!!! note "`Send`-ness and `!Send` drivers"
    `MeshController<D>` is `Send` when `D` is, so the `recv()` loop can run on a
    spawned task. A `!Send` driver (e.g. a browser `RTCPeerConnection` wrapper)
    must be driven on the current task instead.

### Tuning the pump interval

Between signaling events the controller pumps the driver on a timer to surface
trickle ICE and inbound data. The default is 20 ms; lower it for snappier trickle
ICE, raise it to reduce idle wakeups:

```rust,ignore
use std::time::Duration;

let mut mesh = MeshController::start(transport, config, my_driver)
    .with_pump_interval(Duration::from_millis(10));
```

If your driver implements [`set_ready_waker`](#the-optional-set_ready_waker-latency-hook),
output surfaces immediately regardless of the pump interval, which then only acts
as a safety net.

---

## ICE pre-gather

To shorten the time-to-connect, the server can deliver STUN/TURN servers
**early** — in the `ice_servers` field on `RoomJoined` / `Reconnected`, during
the lobby wait — so your WebRTC stack can begin gathering candidates before the
`SessionPlan` arrives. Feed these into `set_ice_servers` as soon as you get them
(`MeshController` does this for you). When the `SessionPlan` later carries its own
`ice_servers`, those supersede the pre-gathered set; an empty plan keeps the
pre-gathered ones.

---

## Transport-status reporting

`report_transport_status(transport, connected)` tells the server whether your
WebRTC data path is up. The server fans it out to peers as `PeerTransportStatus`
and uses it for fallback decisions. With `MeshController` this is automatic: it
reports `TransportStatus(WebRtc, true)` on the first peer to connect (the 0→1
edge) and `TransportStatus(WebRtc, false)` when the last peer disconnects (the
1→0 edge) — never one report per peer.

---

## Fallback to relay

The relay is always the floor. Every `SessionPlan` carries a `fallback` field,
which is **always** `TransportKind::Relay`. If WebRTC fails or is never
established, traffic falls back to relaying through the server — the connection
never breaks just because a peer-to-peer path didn't form. Reporting transport
status (above) is what lets the server make these fallback decisions.

---

## Reconnect behavior

After a reconnect, the server rebuilds the mesh session by re-sending a fresh,
**live** `SessionPlan` (so `missed_events` is empty in practice). The client also
defensively folds any mesh events it finds in `Reconnected.missed_events`, so it
stays correct against servers that batch mesh state there instead.

!!! important "Treat each `SessionPlan` as a full replacement"
    A `SessionPlan` **fully replaces** the peer set — replace, never merge. This
    is how host re-election and topology changes work: peers absent from the new
    plan are dropped, survivors keep their existing connections. `MeshSession` and
    `MeshController` handle this for you.

---

## Tracking state by hand: `MeshSession`

If you don't want the full `MeshController`, `MeshSession` is a zero-dependency,
no-I/O state tracker. Fold every event into it with `apply(&event) -> bool` (which
returns whether the view changed) and read the accessors (`topology`,
`transport`, `host`, `peers`, `ice_servers`, `is_p2p`). It handles late joins,
host re-election, `PlayerLeft` removal, and reconnect replay idempotently — but it
contains no WebRTC and does no signaling. You still "obey the server": every
`initiate` flag is copied verbatim.

---

## Integrating a real backend

Map your backend's primitives onto `WebRtcDriver`:

- **str0m** (sans-I/O, native — the recommended backend). Own a `str0m::Rtc` plus
  a UDP socket per peer. `connect(peer, true)` creates an offer via `sdp_api()`
  and emits `DriverEvent::Signal { Offer }`; `on_signal` applies remote SDP or
  `add_remote_candidate`; `poll` does a non-blocking UDP read into
  `rtc.handle_input`, then drains `rtc.poll_output` into `Signal` (trickled ICE),
  `Connected`, `Data`, and `Transmit` (write UDP). `send` writes to the data
  channel.
- **web-sys** (browser/WASM). Wrap `RtcPeerConnection`: `connect(_, true)` →
  `create_offer` → `set_local_description` → emit via `poll`; `on_signal` →
  `set_remote_description` / `add_ice_candidate`; `onicecandidate` callbacks queue
  `DriverEvent::Signal`; `ondatachannel`/`onmessage` queue `Data`. This driver is
  `!Send`, so drive `MeshController::recv()` on the current task.
- **webrtc-rs**. Works via manual signaling, but is Tokio-coupled and heavier;
  prefer str0m for new native drivers.

---

## See also

- [Mesh Session example walkthrough](examples.md#mesh-session-protocol-v3) — a runnable end-to-end demo.
- [Protocol Versioning](protocol-versioning.md) — negotiation, the fail-fast guard, migration.
- [Events: Mesh Events](events.md#mesh-events-protocol-v3) — the four v3 events.
- [Protocol Types](protocol.md#topology-protocol-v3) — `Topology`, `TransportKind`, `SessionPlanPayload`, `PeerSignal`.
