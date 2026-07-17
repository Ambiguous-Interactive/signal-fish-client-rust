---
name: godot-websocket
description: Maintain the Godot 4.5 WebSocketPeer transport. Use when changing Godot native or web GDExtension networking, polling integration, feature gates, or smoke tests.
---

# Godot 4.5 WebSocket Transport

Reference for `GodotWebSocketTransport`, the supported native/web networking
path for Godot GDExtension consumers.

## Supported Path

Enable `transport-godot`. It enables `polling-client` and the optional `godot`
0.4.5 dependency with no-thread WASM and lazy function-table support.

```toml
signal-fish-client = {
    version = "0.8.0",
    default-features = false,
    features = ["transport-godot"],
}
```

Use `GodotWebSocketTransport` with `SignalFishPollingClient`. Do not move the
transport to `SignalFishClient::start`: Godot engine objects are main-thread
objects and the async driver requires `Send + 'static`.

## Why This Exists

Official Godot web templates expose networking through Godot's
`WebSocketPeer`. They do not link Emscripten's optional WebSocket JavaScript
library, so raw calls to `<emscripten/websocket.h>` leave unresolved symbols in
standard exports. `GodotWebSocketTransport` avoids that link dependency and
ships no GDScript glue.

`EmscriptenWebSocketTransport` remains for advanced custom hosts that
explicitly link the Emscripten library. It is deprecated as a standard Godot
path for 0.8.

## State Machine

Godot connection setup is non-blocking:

1. `connect_to_url` validates and starts the attempt.
2. Every transport poll calls `WebSocketPeer::poll`.
3. `CONNECTING` returns `Pending` without taking an outbound frame.
4. `OPEN` permits sends and receives and permanently flips `is_ready()` true.
5. `CLOSING` remains `Pending`; keep polling to complete the handshake.
6. `CLOSED` captures code/reason/clean attribution and becomes terminal.

Godot reports `get_close_code() == -1` for an unclean close. Preserve this as
`TransportCloseInfo.clean = Some(false)` and omit the invalid negative code.
A close before the transport was ever open is a receive error, not a clean
peer disconnect.

## Frame and Send Semantics

- `send_text` carries `TransportFrame::Text`.
- `send` with binary write mode carries `TransportFrame::Binary`.
- `get_packet` followed by `was_string_packet` classifies inbound frames.
- Text payloads must pass strict UTF-8 conversion.
- Check `get_packet_error` after retrieving every packet.
- Compute the exact UTF-8 or binary payload byte length with checked arithmetic.
- Before backend transfer, leave the caller frame untouched when the next frame
  would exceed the effective watermark or Godot's native capacity boundary.
- The web backend refuses `buffered + next >= outbound_capacity`; the native
  backend refuses `buffered + next > outbound_capacity`.
- Treat `ERR_OUT_OF_MEMORY` from packet submission as retryable capacity and
  retain the exact frame. Other Godot send errors are terminal.
- Once Godot accepts a packet, take the caller frame and return
  `Ready(Ok(()))` immediately. Acceptance transfers ownership; it is not peer
  delivery and does not wait for socket-wide `bufferedAmount` to reach zero.

The default is adaptive with a 50 ms latency target, 4 KiB floor, 32 KiB
ceiling, and 1/8 EWMA smoothing. Fixed and native-capacity policies are
explicit opt-ins:

```text
watermark = clamp(
  max(EWMA(previous-cycle accepted burst), EWMA(drain bytes/sec) * latency),
  floor,
  min(ceiling, platform-safe native capacity),
)
```

Sample adaptive state once from `begin_poll_cycle`, not per packet. A single
frame larger than the latency watermark may pass only when current buffering is
zero and native capacity still permits it. `NativeCapacity` disables the
latency watermark but never disables capacity-safe preflight.

The transport ignores async wakers intentionally. The polling client uses a
noop waker and calls it again from the next Godot frame.

## Construction

`GodotWebSocketTransport::connect(url)` constructs a `WebSocketPeer` and calls
`connect_to_url`; `connect_with_options` selects fixed, adaptive, or native-only
admission. `from_peer(peer)` and `from_peer_with_options` support advanced
setup: configure headers, subprotocols, buffer sizes, or TLS options, start the
connection, then wrap the peer.

## Close Matrix

| Client policy/state | Required action |
|---|---|
| `Abandon`, client-owned queue | Clear and count queued/unaccepted commands; start Close now. |
| `Flush`, client-owned queue | Transfer FIFO work under normal poll budgets, then start Close. |
| Godot-accepted packets | Call `WebSocketPeer::close` without waiting for zero; WebSocket ordering puts accepted messages before Close. |
| Peer already closing/closed | Preserve peer attribution and drain available inbound packets. |
| Client deadline expires | Count remaining work, invoke `Transport::abort`, and stop reporting closing. |

For web pages served over HTTPS, use a `wss://` server URL to avoid browser
mixed-content rejection.

## Testing

Unit tests use the private backend seam, not a fake `Gd`. Cover:

- connecting ownership preservation;
- sticky nonzero buffering accepting multiple text/binary frames per poll;
- watermark and both native-capacity boundary refusals retaining exact frames;
- capacity recovery preserving FIFO without duplicates;
- retryable `ERR_OUT_OF_MEMORY` and terminal backend errors;
- fixed/adaptive/native bounds and scripted 1/8 EWMA sequences;
- text/binary classification and invalid UTF-8;
- packet errors;
- failed handshakes versus established peer closes;
- clean/unclean close metadata;
- multi-poll idempotent local close.

Primary sources: [Godot WebSocketPeer API](https://docs.godotengine.org/en/stable/classes/class_websocketpeer.html#class-websocketpeer-method-get-current-outbound-buffered-amount),
[Godot 4.5 web implementation](https://github.com/godotengine/godot/blob/4.5-stable/modules/websocket/emws_peer.cpp),
[Godot 4.5 native implementation](https://github.com/godotengine/godot/blob/4.5-stable/modules/websocket/wsl_peer.cpp), and the
[WebSocket `bufferedAmount` definition](https://websockets.spec.whatwg.org/#dom-websocket-bufferedamount).

The `tests/godot-web-smoke` fixture is a standalone GDExtension workspace. It
must remain free of GDScript networking code. Browser automation should assert
its stable `SIGNAL_FISH_SMOKE` and `SIGNAL_FISH_FORTRESS` log markers and
retain browser/server logs on failure. The Fortress scenario launches two
independent Chromium processes, derives stable handles from sorted Signal Fish
UUIDs, and runs one polling cycle per rendered callback against a real server.
Advance simulation on a fixed local cadence that does not consult peer or
network progress, so process scheduling is controlled without hiding genuine
Fortress stalls. Preserve elapsed deadline debt and recover by at most one
simulation frame per rendered callback, preventing permanent process skew
without allowing multi-frame bursts. Before initializing those local cadence
deadlines, use a one-time causal barrier: A observes B at frame zero and B
observes A's subsequent frame one. Retain and validate both roles' release
watermarks and local release frames so launch order cannot masquerade as
gameplay lag. Use causal post-advance watermarks to
bound the relay hold that forces rollback while both games continue advancing,
and require the polling hitch window to contain forward simulation progress. Its impairment must
produce measured rollback/load/resimulation, after which
both peers must match exact game-state checksums, report in-sync health, drain
all queues, conserve relay/server counts, and complete an observable v3
`PlayerLeft` teardown.

The workflow builds/exports once, then runs required clean, seeded bidirectional
netem, and 3,600-confirmed-frame soak jobs in parallel. Impaired profiles include
a six-callback polling hitch while gameplay advances. Pure JavaScript validators
must reject negative controls for checksum, confirmation, conservation, queue
age, lag/stalls, teardown watermarks, and admission diagnostics. Always retain
logs, time series, Prometheus snapshots, summaries, and netem configuration.

Web builds must use `godot/api-custom` so bindgen generates 32-bit interface
types. Point `GODOT4_BIN` at the 4.5 editor, point the target-specific bindgen
arguments at Emscripten's sysroot, build `std` with the pinned nightly, and
link the crate as `SIDE_MODULE=2`. Keep the negative-control fixture: calling
the raw Emscripten transport under the official template must fail on the
undefined `emscripten_websocket_new` symbol.

## Checklist

- [x] `transport-godot` enables `polling-client`.
- [x] Godot remains optional and pinned to the intended 4.5-compatible release.
- [x] Native all-feature Clippy and tests pass.
- [x] The fixture crate checks independently.
- [x] Official Godot 4.5 web templates export the fixture.
- [x] Browser E2E proves connect, authentication, room join, Ping/Pong, relay,
      close attribution, and graceful shutdown.
- [x] Docs recommend Godot transport, not raw Emscripten FFI.
