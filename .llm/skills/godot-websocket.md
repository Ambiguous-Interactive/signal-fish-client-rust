# Godot 4.5 WebSocket Transport

Reference for `GodotWebSocketTransport`, the supported native/web networking
path for Godot GDExtension consumers.

## Supported Path

Enable `transport-godot`. It enables `polling-client` and the optional `godot`
0.4.5 dependency with no-thread WASM and lazy function-table support.

```toml
signal-fish-client = {
    version = "0.7.0",
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
- After Godot accepts a send, retain an in-flight marker until
  `get_current_outbound_buffered_amount()` reaches zero. Never take a new frame
  while that marker is set.

The transport ignores async wakers intentionally. The polling client uses a
noop waker and calls it again from the next Godot frame.

## Construction

`GodotWebSocketTransport::connect(url)` constructs a `WebSocketPeer` and calls
`connect_to_url`. `from_peer(peer)` supports advanced setup: configure headers,
subprotocols, buffer sizes, or TLS options, start the connection, then wrap the
peer.

For web pages served over HTTPS, use a `wss://` server URL to avoid browser
mixed-content rejection.

## Testing

Unit tests use the private backend seam, not a fake `Gd`. Cover:

- connecting ownership preservation;
- accepted-send buffering and completion;
- text/binary classification and invalid UTF-8;
- packet errors;
- failed handshakes versus established peer closes;
- clean/unclean close metadata;
- multi-poll idempotent local close.

The `tests/godot-web-smoke` fixture is a standalone GDExtension workspace. It
must remain free of GDScript networking code. Browser automation should assert
its stable `SIGNAL_FISH_SMOKE` log markers and retain browser/server logs on
failure.

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
