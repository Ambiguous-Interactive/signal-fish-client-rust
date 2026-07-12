# WebSocket Client

Reference for the built-in `tokio-tungstenite` transport and its polling state
machine.

## Feature and Construction

`WebSocketTransport` is behind `transport-websocket` (enabled by default).
Connection setup remains outside `Transport`:

```rust,ignore
let transport = WebSocketTransport::connect("wss://signal.example/ws").await?;
let transport = WebSocketTransport::connect_with_timeout(url, timeout).await?;
```

`from_stream(WsStream)` wraps a stream built with custom TLS, proxy, headers,
or cookies. Connection failures map to `SignalFishError::Io`, preserving an
underlying I/O error kind when possible.

## Frame Mapping

The transport passes application frames through without loss:

| WebSocket message | `Transport` result |
|---|---|
| `Text` | `TransportFrame::Text(String)` |
| `Binary` | `TransportFrame::Binary(Vec<u8>)` |
| `Close` | `poll_recv -> Ready(None)` plus `close_info()` |
| `Ping` | transparent; tungstenite queues Pong and transport flushes it |
| `Pong` | transparent |

Binary frames are protocol-v3 application traffic. Never log-and-skip them.
Raw `Message::Frame` is not expected from the read half and is ignored.

## Outbound State Machine

`poll_send` uses the `Sink` primitives directly:

1. If no send is active, call `poll_ready`.
2. On `Pending`, leave the caller's frame slot untouched.
3. On readiness, take exactly one `TransportFrame` and translate it to
   `Message::Text` or `Message::Binary`.
4. Call `start_send` once and record that a send is active.
5. Poll `poll_flush` until ready; do not take another frame while pending.

This preserves an accepted frame across `Pending` and prevents duplicate
`start_send` calls.

```rust,ignore
match frame {
    TransportFrame::Text(text) => Message::Text(text.into()),
    TransportFrame::Binary(bytes) => Message::Binary(bytes.into()),
}
```

The async client wraps the poll method in `poll_fn`; the polling client invokes
the same method once per tick.

## Inbound Control Flush

Tungstenite automatically queues a Pong when reading Ping, but queued control
output still needs a flush. After Ping, `WebSocketTransport` sets a
`control_flush_pending` flag and drives `poll_flush` before reading another
application frame. If flushing is pending, it preserves the flag and returns
`Pending` with the sink's waker registered.

Do not manually enqueue a second Pong. Do not continue reading indefinitely
without flushing the automatically queued reply.

## Close Metadata and Idempotency

On peer Close, preserve structured metadata before returning `None`:

```rust,ignore
TransportCloseInfo {
    code: Some(frame.code.into()),
    reason: (!frame.reason.is_empty()).then(|| frame.reason.to_string()),
    clean: None,
    initiated_by_peer: true,
}
```

A bare peer close still records `initiated_by_peer: true`. The tungstenite API
does not supply a separate clean-handshake boolean here, so `clean` remains
`None`.

`poll_close` calls the sink's `poll_close`, retains progress in the stream, and
marks the transport closed on either terminal success or error. Once closed,
later calls return `Ready(Ok(()))` without another close frame.

## Wakers

Always pass `cx` to `poll_ready`, `poll_flush`, `poll_next`, and `poll_close`.
Those primitives register the runtime waker. Returning `Pending` without
polling the blocked primitive can strand the async driver.

## TLS

The crate uses Tokio Tungstenite with Rustls roots for `wss://`; `ws://` is
unencrypted. Keep TLS features aligned with `Cargo.toml` rather than duplicating
an alternative stack in the transport.

## Reconnection

A closed `WebSocketTransport` is terminal. Reconnection creates a new transport
and client physical connection. Protocol reconnection then uses the latest
server-issued room token through the client API; do not attempt to reuse the
closed WebSocket object.

## Test Checklist

- Text and binary frames pass through exactly.
- A pending flush does not consume a second caller frame.
- Peer close code/reason and initiator are retained.
- Bare peer close is distinguishable from missing metadata where applicable.
- Repeated `poll_close` after completion is harmless.
- Ping causes the automatically queued Pong to be flushed.
- Transport send/receive errors map to the matching `SignalFishError` variant.
- A real waker is notified when socket readiness changes.

## Common Errors

| Symptom | Likely cause |
|---|---|
| Binary game data disappears | Binary `Message` was skipped instead of surfaced. |
| Peer times out despite Ping | Auto-Pong was queued but not flushed. |
| Duplicate application message | `start_send` was repeated after `Pending`. |
| Async task never wakes | Blocked sink/stream was not polled with `cx`. |
| Close code lost | Metadata was not copied before returning `None`. |
