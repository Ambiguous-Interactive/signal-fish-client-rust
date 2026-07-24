---
name: websocket-client
description: Maintain the built-in tokio-tungstenite transport state machine. Use when changing WebSocket connection setup, polling, buffering, close metadata, TLS features, or transport tests.
---

# WebSocket Client

Reference for the built-in `tokio-tungstenite` transport and its polling state
machine.

## Feature and Construction

`WebSocketTransport` is behind `transport-websocket` (enabled by default).
Connection setup remains outside `Transport`:

```rust,ignore
let transport = WebSocketTransport::connect("ws://signal.example/ws").await?;
let transport = WebSocketTransport::connect_with_timeout(url, timeout).await?;
```

`wss://` needs the optional `tls` feature (see the [TLS](#tls) section).

`from_stream(WsStream)` wraps a stream built with custom TLS, proxy, headers,
or cookies. Connection failures map to `SignalFishError::Io`, preserving an
underlying I/O error kind when possible.

## Low-Latency Socket Defaults

`connect` and `connect_with_timeout` disable Nagle's algorithm (`TCP_NODELAY`)
by default via `connect_async_with_config(url, None, /*disable_nagle=*/ true)`.
Small, latency-sensitive game messages are then sent without waiting on TCP's
delayed-ACK timer — the Nagle + delayed-ACK stall costs tens of milliseconds per
round trip. The flag is applied to the raw socket *before* any TLS handshake, so
it covers both `ws://` and `wss://`.

Callers opt out with
`connect_with_options(url, WebSocketConnectOptions::new().with_disable_nagle(false))`
(e.g. for bulk/throughput links). `from_stream` leaves all socket options to the
caller.

Never route a new connection through the bare `connect_async(url)` — it leaves
Nagle enabled. Any new connect entry point must go through
`connect_async_with_config(..)` (or set `TCP_NODELAY` on the socket directly).
See the class-level rule in the `transport-abstraction` skill.

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

`ws://` is always available and unencrypted. `wss://` requires the optional
`tls` feature, which enables `tokio-tungstenite/rustls-tls-webpki-roots` and a
direct `rustls` dependency with the **ring** provider. `connect_with_options`
installs ring as the process-default provider once (idempotent; yields to any
provider the application already installed) so tokio-tungstenite's
`ClientConfig::builder()` never hits rustls' ambiguous feature auto-detection —
which panics when both `ring` and `aws_lc_rs` are in the dependency graph.
Without the `tls` feature, a `wss://` connect fails cleanly with
`SignalFishError::Io` (never a panic). Keep TLS features aligned with
`Cargo.toml` rather than duplicating an alternative stack in the transport.

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
- The connected TCP socket has `TCP_NODELAY` set by default; `connect_with_options`
  can turn it off.

## Common Errors

| Symptom | Likely cause |
|---|---|
| Binary game data disappears | Binary `Message` was skipped instead of surfaced. |
| Peer times out despite Ping | Auto-Pong was queued but not flushed. |
| Duplicate application message | `start_send` was repeated after `Pending`. |
| Async task never wakes | Blocked sink/stream was not polled with `cx`. |
| Close code lost | Metadata was not copied before returning `None`. |
| ~30-35 ms added per small request/reply | Nagle left enabled; a connect path skipped `disable_nagle` / `TCP_NODELAY`. |
