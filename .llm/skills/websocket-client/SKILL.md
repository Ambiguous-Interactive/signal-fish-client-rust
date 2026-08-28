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
let transport = WebSocketTransport::connect("ws://signal.example/v2/ws").await?;
let transport = WebSocketTransport::connect_with_timeout(url, timeout).await?;
```

`wss://` needs the optional `tls` feature (see the [TLS](#tls) section).

`from_stream(WsStream)` wraps a stream built with custom TLS, proxy, headers,
or cookies and preserves its caller-selected frame/message limits. New native
connections instead apply `WebSocketConnectOptions::max_inbound_message_size`
to tungstenite's frame and assembled-message limits together. The default is
an inclusive 8 MiB; `None` disables both and `Some(0)` is rejected before
network I/O. This is a protective client policy, not a Server 0.7 protocol
maximum; signal-fish-server#399 tracks the missing outbound contract.
Network-determined connection failures map to `SignalFishError::Io`,
preserving an underlying I/O error kind when possible. Value- and
build-determined rejections — an unparsable URL, a zero inbound-size limit,
or `wss://` without the `tls` feature — are the typed
`SignalFishError::InvalidConfig { field, problem }` instead, decided before
any network I/O.

The opt-in `token-binding` feature adds `TokenBindingMode` to
`WebSocketConnectOptions`. Keep disabled mode on the exact old connect path.
Optional mode may reconnect without an offer only for tungstenite's exact
`NoSubProtocol` result; never downgrade HTTP/TLS/network rejection, unexpected
selection, or a malformed/missing challenge. Required mode maps missing
selection to a typed failure. Consume the first challenge inside connect before
returning the transport, so `Authenticate` cannot race it. `from_stream` cannot
opt in after losing the exact generated handshake key.

## Low-Latency Socket Defaults

`connect` and `connect_with_timeout` disable Nagle's algorithm (`TCP_NODELAY`)
by default via `connect_async_with_config(url, Some(config), /*disable_nagle=*/ true)`.
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

1. If no send is active and the caller slot is empty, return `Ready(Ok(()))`.
2. Otherwise call `poll_ready`; on `Pending`, leave the caller slot untouched.
3. On readiness, prepare an active token-binding wrapper from `frame.as_ref()`;
   do not take the original yet. Disabled mode translates it directly.
4. Call `start_send` once and, on success, take the original, commit the shared
   JSON/binary sequence, and record that a send is active.
5. Poll `poll_flush` until ready; do not take another frame while pending.

This preserves an accepted frame across `Pending` and prevents duplicate
`start_send` calls. If a custom stream rejects the message with
`WriteBufferFull`, restore the exact Text/Binary frame to the caller slot in
disabled mode. In active token-binding mode the original never left the slot;
discard the rejected protected message and do not advance the sequence. Never
restore a protected envelope, which would be wrapped twice on retry.

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

One receive call skips at most 64 Ping, Pong, or defensive raw Frame messages.
After the boundary Ping's automatic Pong is flushed, budget exhaustion calls
`cx.waker().wake_by_ref()` and returns `Pending`; already-buffered application
traffic is resumed in a later poll. Do not manually enqueue a second Pong.

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

Required `Transport::abort` drops the stream and retained send/control state
immediately and is idempotent. Async task cancellation is protected by an
owner guard that invokes abort unless graceful close completed; client drivers
perform no later transport polls.

EOF and terminal receive failures drop the stream, clear retained send and
control state, and fuse the transport. A terminal sink failure rejects later
sends but retains the stream just long enough for `poll_recv` to surface
already-buffered application frames; its first backend `Pending`, receive
failure, EOF, close, or abort then drops the stream. Pre-acceptance token-
binding errors and an exactly restored `WriteBufferFull` remain retryable.
Report the first receive failure as `TransportReceive`; later receives return
`None`, terminal sends return `TransportClosed` without taking their frame, and
close remains idempotent.

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
`SignalFishError::InvalidConfig` (never a panic). Keep TLS features aligned with
`Cargo.toml` rather than duplicating an alternative stack in the transport.
`connect_with_tls_config` accepts caller-controlled roots or mTLS without
retaining or formatting the configuration. When active token binding uses a
custom rustls configuration, a private resolver and signer wrapper hashes the
exact leaf selected with a compatible X.509 signer, emits lowercase SHA-256 DER
hex, and signs those same fingerprint bytes. Raw public keys are not treated as
certificates, and no caller claim is accepted. Offering token binding disables
resumption only on the cloned configuration, including Optional fallback, so
every physical connection exposes certificate selection without mutating the
caller's configuration/cache. Server 0.7 refuses required token binding without
built-in TLS, so its positive E2E must use WSS.

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
- Control-frame budget exhaustion self-wakes before buffered application data.
- Transport send/receive errors map to the matching `SignalFishError` variant.
- EOF and terminal socket errors produce exact fused follow-up behavior.
- A real waker is notified when socket readiness changes.
- The connected TCP socket has `TCP_NODELAY` set by default; `connect_with_options`
  can turn it off.
- Native connect paths apply the same configured inclusive limit to individual
  inbound frames and fragmented-message assembly; boundary, limit+1,
  fragmentation, override/disable, token-binding offer/fallback, and terminal
  error behavior use real tungstenite codecs.
- `from_stream` preserves the caller's WebSocket codec limits.
- Disabled token binding offers no subprotocol and keeps application bytes exact.
- Optional fallback occurs only for `NoSubProtocol`; HTTP rejection is not retried.
- Required negotiation consumes a strict first-message challenge under timeout.
- Preparation, `Pending`, and `WriteBufferFull` preserve the original frame and
  sequence; JSON/binary goldens share one sequence.
- Certificate-capable custom rustls connections bind JSON and binary proofs to
  the actual selected mTLS leaf and pass pinned fingerprint-required Server 0.7
  positive and adversarial E2Es.
- Debug/tracing/errors omit keys, nonces, proofs, signatures, URL credentials,
  and protected payloads.

## Common Errors

| Symptom | Likely cause |
|---|---|
| Binary game data disappears | Binary `Message` was skipped instead of surfaced. |
| Peer times out despite Ping | Auto-Pong was queued but not flushed. |
| Duplicate application message | `start_send` was repeated after `Pending`. |
| Async task never wakes | Blocked sink/stream was not polled with `cx`. |
| Close code lost | Metadata was not copied before returning `None`. |
| ~30-35 ms added per small request/reply | Nagle left enabled; a connect path skipped `disable_nagle` / `TCP_NODELAY`. |
