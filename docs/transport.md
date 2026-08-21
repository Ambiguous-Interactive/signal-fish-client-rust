# Transport Trait & WebSocket

`Transport` is the framed networking boundary between the client protocol and
an I/O backend. The same object-safe polling contract works in both the Tokio
client and the game-loop-driven polling client.

## The `Transport` contract

```rust,ignore
use std::task::{Context, Poll};

pub trait Transport {
    fn poll_send(
        &mut self,
        cx: &mut Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> Poll<Result<(), SignalFishError>>;

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<TransportFrame, SignalFishError>>>;

    fn poll_close(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), SignalFishError>>;

    fn begin_poll_cycle(&mut self) {}
    fn abort(&mut self);
    fn diagnostics(&self) -> TransportDiagnostics { TransportDiagnostics::default() }
    fn is_ready(&self) -> bool { true }
    fn close_info(&self) -> Option<TransportCloseInfo> { None }
}
```

There is no `async-trait` macro and no trait-level `Send` bound. The trait is
object-safe, so `Box<dyn Transport>` is valid.

`SignalFishClient::start` moves its transport into a spawned Tokio task and
therefore requires `Transport + Send + 'static`. `SignalFishPollingClient`
does not spawn a task and accepts non-`Send`, main-thread-only transports.

Connection setup is intentionally outside the trait. Construct or connect the
backend first, then give it to a client.

## Text and binary frames

```rust,ignore
pub enum TransportFrame {
    Text(String),
    Binary(Vec<u8>),
}
```

Text frames carry JSON protocol messages. Inbound binary frames can decode
protocol-v2 or protocol-v3 game-data envelopes; the public physical binary-send
APIs are gated on negotiated v3 plus MessagePack. A transport treats binary
payloads as opaque bytes, preserves frame boundaries, and must not silently
discard either kind.

## Datagram and raw-stream scope

`Transport` begins at one **complete, ordered text/binary signaling-frame
stream bound to the intended server**. It is not a byte-stream codec, a
datagram protocol, or a server-authentication mechanism. `TransportFrame` carries
no source address or peer identity, so the client attributes every yielded
frame to that server. The built-in transports connect to the server's
WebSocket endpoint, and the pinned Server 0.7
[AsyncAPI contract](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/3f7f43d4cd4b3cc7f8fb893220dc35c9b1fad333/spec/signal-fish-protocol.asyncapi.yaml)
defines one bidirectional WebSocket channel for signaling and relayed
`GameData`. Its room service
[accepts but ignores `JoinRoom.relay_transport`](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/3f7f43d4cd4b3cc7f8fb893220dc35c9b1fad333/src/server/room_service.rs#L213-L222),
and its relay policy states that Server 0.7
[contains no separate relay server](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/3f7f43d4cd4b3cc7f8fb893220dc35c9b1fad333/src/server/relay_policy.rs#L5-L20).

A custom TCP or QUIC-stream adapter must delimit messages before returning a
frame. A custom datagram adapter would likewise need an external protocol that
defines, at minimum, versioning, maximum message size, server trust/source
binding (which may deliberately provide no cryptographic identity),
text-versus-binary classification, truncation and fragmentation, duplicate and
reorder handling, loss signaling/recovery, and terminal/error behavior. It
must yield one ordered frame stream for the intended signaling server, or
report a transport error instead of fabricating or silently skipping a frame.
Only after that layer produces one complete `TransportFrame` does this SDK's
JSON/MessagePack decoding, lifecycle validation, and v3 delivery
accountability apply.

These upper layers validate representation, lifecycle, and sequence
consistency; they do not authenticate frame origin. If the backend's
trust/source-binding policy provides no cryptographic identity, the SDK offers
no separate spoof protection.

The SDK intentionally provides no raw UDP backend or datagram envelope. It
therefore makes no claim that arbitrary, truncated, duplicated, reordered, or
spoofed datagrams are safe protocol input. Adding parser fuzzing or loopback
UDP tests here would test a nonexistent wire contract. Datagram behavior stays
with the component that owns the actual data path:

- an engine/networking integration consumes self-declared
  `ConnectionInfo::Direct`/`ConnectionInfo::Relay` metadata and applies its
  trust/credential rules;
- a `WebRtcDriver` implementation owns ICE/DTLS/SCTP and its underlying UDP
  sockets, while `MeshController` sees assembled data-channel messages;
- any future server/client datagram transport must define and test its envelope
  and trust boundary before adapting complete frames into `Transport` (or use a
  separate abstraction if its semantics are not connection-oriented).

`RelayTransport::Udp` is only a legacy wire label. Signal Fish Server 0.7
ignores it when selected in `JoinRoomParams`; that selection neither creates an
executable UDP path nor reconfigures the signaling transport, opens a UDP
socket, or bypasses the complete-frame requirement.

## Sending and ownership across `Pending`

The `Option<TransportFrame>` argument is an ownership slot shared by the caller
and transport:

1. Before the transport takes the value, the caller still owns it.
2. A transport that cannot accept it yet returns `Pending` and leaves the slot
   unchanged.
3. Once the transport calls `frame.take()`, it has accepted responsibility for
   that exact frame.
4. If it then returns `Pending`, it must retain the accepted frame/write state
   internally and continue it on the next poll.
5. It may return `Ready(Ok(()))` as soon as the backend accepts ownership.
   This does not mean peer delivery or that socket-wide buffering is empty.

Never take a frame, forget it on `Pending`, and ask the caller to retry. Never
repeat a partially completed write: either mistake can lose or duplicate an
application message.

While `is_ready()` is false, `poll_send` must return `Pending` without taking
the frame. This lets both clients admit FIFO commands during an asynchronous
handshake without transferring them prematurely.

`begin_poll_cycle` lets adaptive transports sample once per application tick.
`diagnostics` distinguishes backend-owned buffering/admission from the client
queue. `abort` is required and is invoked when graceful close errors, either
client's close deadline expires, or an owner is dropped before close completes.
It must promptly release or safely detach backend-owned work, discard retained
accepted sends, return without blocking or panicking, and be idempotent.
Completed cleanup is not repeated, while failed cleanup may be retried safely.
Afterwards, client drivers make no further polling calls: only repeated
`abort`, `is_ready`, `close_info`, `diagnostics`, and drop are allowed. The
built-in WebSocket transports and Godot adapter also fuse later polls to
terminal results as a stronger convenience.

### Migrating custom transports for 0.11

`abort` no longer has a default implementation. Every custom transport must
make an explicit resource-lifetime decision:

```rust,ignore
fn abort(&mut self) {
    if self.aborted {
        return;
    }
    self.aborted = true;
    self.retained_send = None;
    self.shared_backend.unregister(self.connection_id);
    self.socket = None;
    self.waker = None;
}
```

The exact fields vary by backend. Clear retained frames and wakers, revoke this
transport's participation in any shared backend, and release owned handles
without blocking. If the implementation owns no live resource, retained work,
callback, or shared registration, an explicit no-op is valid:

```rust,ignore
fn abort(&mut self) {}
```

Do not wait for a peer handshake in `abort`; that belongs to `poll_close`.
Because `abort` can run from `Drop` during unwinding, it must never panic. If an
external cleanup API fails, keep callback backing storage alive rather than
risk use-after-free, and make later `abort` or `Drop` retries safe.
`examples/custom_transport.rs` shows a complete channel-backed implementation.

## Receiving

| Result | Meaning |
|---|---|
| `Pending` | No complete frame is available yet. |
| `Ready(Some(Ok(frame)))` | One complete text or binary frame arrived. |
| `Ready(Some(Err(error)))` | The transport failed while receiving. |
| `Ready(None)` | The connection reached a terminal clean/peer close. |

If an implementation consumes partial input before returning `Pending`, it
must retain that partial input. A future poll continues from the saved state.

When an async-runtime waker is supplied, the transport must register or forward
it so readiness wakes the client task. The polling client supplies a noop waker
and polls again on the next application tick.

## Closing and close metadata

`poll_close` may need multiple calls. It is idempotent: it starts at most one
close handshake, retains progress across `Pending`, and returns
`Ready(Ok(()))` on every call after successful completion. On error, logical
I/O terminates and both clients immediately call `abort`; fallible backend
cleanup may remain safely retryable.

After a peer close, `close_info()` may return:

```rust,ignore
pub struct TransportCloseInfo {
    pub code: Option<u16>,
    pub reason: Option<String>,
    pub clean: Option<bool>,
    pub initiated_by_peer: bool,
}
```

Capture this metadata before `poll_recv` returns `Ready(None)`. The clients use
it to attribute `SignalFishEvent::Disconnected`.

`is_ready()` defaults to `true`, which is correct for transports connected by
their constructor. An asynchronous-handshake transport returns `false` until
ready; both clients defer their synthetic `Connected` event accordingly. The
value must be cheap and monotonic for one physical connection. When readiness
changes while the async client is blocked, the transport must wake a waker
registered by `poll_send` or `poll_recv`; `is_ready()` itself cannot register
one. Before readiness, `poll_send` retains caller ownership and `poll_recv`
must not return a complete protocol frame.

## Built-in `WebSocketTransport`

The default `transport-websocket` feature provides `WebSocketTransport`, backed
by `tokio-tungstenite`. `ws://` is always available; `wss://` requires the
optional `tls` feature (rustls with bundled webpki roots). Connections disable
Nagle's algorithm (`TCP_NODELAY`) by default; see `WebSocketConnectOptions` to
override.

```rust,ignore
let transport = WebSocketTransport::connect("ws://example.com/signal").await?;

let transport = WebSocketTransport::connect_with_timeout(
    "ws://example.com/signal",
    std::time::Duration::from_secs(5),
)
.await?;
```

`from_stream` wraps an already-established `WsStream` for custom TLS, proxy,
headers, or cookie setup.

The WebSocket mapping is direct:

| WebSocket frame | SDK frame/outcome |
|---|---|
| Text | `TransportFrame::Text` |
| Binary | `TransportFrame::Binary` |
| Close | `Ready(None)` and structured `close_info` |
| Ping/Pong | Transparent control traffic |

Outbound frames are accepted with `poll_ready`/`start_send` and retained until
`poll_flush` completes. Inbound binary messages are application traffic, not
ignored frames.

Tungstenite automatically queues a Pong while reading Ping. The transport
explicitly drives `poll_flush` before reading further frames, ensuring that the
automatic RFC 6455 response reaches the peer even when the application has no
outbound message to send. Each receive poll skips at most 64 control frames; if
that budget is exhausted, it schedules another poll so buffered application
traffic cannot be hidden behind unbounded control-frame work.

Peer Close code and reason are copied into `TransportCloseInfo`; a bare Close
still records that the peer initiated termination. WebSocket close polling is
idempotent. EOF and terminal socket errors release the stream and fuse the
transport: the first receive error remains observable, later receives return
`None`, sends fail with `TransportClosed`, and repeated close calls succeed.

## Implementing a channel transport

This complete skeleton passes both text and binary frames through in-process
channels:

```rust,ignore
use std::task::{Context, Poll};
use signal_fish_client::error::SignalFishError;
use signal_fish_client::transport::{Transport, TransportFrame};
use tokio::sync::mpsc;

pub struct LoopbackTransport {
    tx: Option<mpsc::UnboundedSender<TransportFrame>>,
    rx: mpsc::UnboundedReceiver<TransportFrame>,
    closed: bool,
}

impl Transport for LoopbackTransport {
    fn poll_send(
        &mut self,
        _cx: &mut Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> Poll<Result<(), SignalFishError>> {
        let Some(tx) = self.tx.as_ref() else {
            return Poll::Ready(Err(SignalFishError::TransportClosed));
        };
        let result = match frame.take() {
            Some(frame) => tx.send(frame).map_err(|error| {
                SignalFishError::TransportSend(error.to_string())
            }),
            None => Ok(()),
        };
        Poll::Ready(result)
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<TransportFrame, SignalFishError>>> {
        if self.closed {
            return Poll::Ready(None);
        }
        self.rx.poll_recv(cx).map(|frame| frame.map(Ok))
    }

    fn poll_close(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), SignalFishError>> {
        self.closed = true;
        self.tx = None;
        self.rx.close();
        Poll::Ready(Ok(()))
    }

    fn abort(&mut self) {
        self.closed = true;
        self.tx = None;
        self.rx.close();
    }
}
```

The channel send completes synchronously, so it can take the frame and return
`Ready` in the same call. A socket that remains pending after acceptance needs
an internal outbound slot or equivalent state machine.

Use it with the async client only when the transport is `Send + 'static`:

```rust,ignore
let (mut client, mut events) = SignalFishClient::start(transport, config);
while let Some(event) = events.recv().await {
    // Handle events.
}
client.shutdown().await;
```

Or use any `Transport`, including a non-`Send` one, with the polling client:

```rust,ignore
let mut client = SignalFishPollingClient::new(transport, config);
for event in client.poll() {
    // Handle this tick's events.
}
```

`poll()` defaults to at most 64 frames/64 KiB in each direction. Configure
`PollingClientOptions` for other budgets or `PollingClosePolicy::Flush`. Zero
budgets clamp to one, and one individually oversized frame can consume a poll
by itself. `polling_stats()` reports client-owned queue/budget/close state;
`queue_age_stats()` reports the sampled current and peak age of the oldest
client-owned item, and `reset_queue_age_peak()` excludes earlier setup peaks;
`transport_diagnostics()` reports backend acceptance and buffering. Queued,
backend-accepted, backend-buffered, and peer-delivered are distinct stages.

## Emscripten transport

`EmscriptenWebSocketTransport` implements the same framed polling contract on
`wasm32-unknown-emscripten`. Its browser callbacks buffer readiness, text,
binary, error, and close events; `SignalFishPollingClient::poll` drains them on
the main thread. It exposes structured close metadata and drives idempotent
cleanup through `poll_close`; `abort` applies the same close-before-release
ordering when close errors, times out, or the polling owner is dropped.

It is intended for the polling client, not the Tokio-spawned async client. See
the [WebAssembly guide](wasm.md) for target and linker requirements.

## Custom transport checklist

- Preserve both text and binary frame boundaries.
- Delimit raw stream/datagram input and apply the backend's signaling-server
  trust/source-binding policy before returning a frame; preserve the intended
  signaling server's ordering and surface unrecoverable loss/corruption as an
  error rather than passing partial, concatenated, duplicated, or reordered
  bytes through.
- Do not take the caller frame before the backend accepts it.
- Retain accepted sends and partial receives across `Pending`; do not wait for
  a socket-wide buffered byte count to reach zero as per-frame completion.
- Register the supplied waker when async progress depends on readiness.
- Make close multi-poll and idempotent.
- Implement prompt, non-blocking, non-panicking, idempotent `abort` cleanup;
  clear retained work and make failed backend cleanup safe to retry.
- Record close code/reason/initiator before returning `None`.
- Keep `is_ready` cheap and monotonic for one physical connection.
- Put connection-specific construction outside the trait.
