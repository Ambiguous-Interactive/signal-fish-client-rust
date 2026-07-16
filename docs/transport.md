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
    fn abort(&mut self) {}
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

Text frames carry JSON protocol messages. Binary frames carry opaque
protocol-v3 binary game data. A transport must preserve frame boundaries and
must not silently discard either kind.

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

`begin_poll_cycle` lets adaptive transports sample once per application tick.
`diagnostics` distinguishes backend-owned buffering/admission from the client
queue. `abort` is invoked when the polling close deadline expires; defaulted
hooks preserve existing custom transport implementations. The built-in
WebSocket transports override `abort` to release their socket immediately;
custom transports with owned resources should do the same.

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
`Ready(Ok(()))` on every call after successful completion.

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
ready; the polling client defers its synthetic `Connected` event accordingly.

## Built-in `WebSocketTransport`

The default `transport-websocket` feature provides `WebSocketTransport`, backed
by `tokio-tungstenite` with `ws://` and `wss://` support.

```rust,ignore
let transport = WebSocketTransport::connect("wss://example.com/signal").await?;

let transport = WebSocketTransport::connect_with_timeout(
    "wss://example.com/signal",
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
outbound message to send.

Peer Close code and reason are copied into `TransportCloseInfo`; a bare Close
still records that the peer initiated termination. WebSocket close polling is
idempotent.

## Implementing a channel transport

This complete skeleton passes both text and binary frames through in-process
channels:

```rust,ignore
use std::task::{Context, Poll};
use signal_fish_client::error::SignalFishError;
use signal_fish_client::transport::{Transport, TransportFrame};
use tokio::sync::mpsc;

pub struct LoopbackTransport {
    tx: mpsc::UnboundedSender<TransportFrame>,
    rx: mpsc::UnboundedReceiver<TransportFrame>,
}

impl Transport for LoopbackTransport {
    fn poll_send(
        &mut self,
        _cx: &mut Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> Poll<Result<(), SignalFishError>> {
        let result = match frame.take() {
            Some(frame) => self.tx.send(frame).map_err(|error| {
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
        self.rx.poll_recv(cx).map(|frame| frame.map(Ok))
    }

    fn poll_close(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), SignalFishError>> {
        Poll::Ready(Ok(()))
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
by itself. `polling_stats()` reports client-owned queue depth, work-budget
exhaustion, and close state; `queue_age_stats()` reports current/peak oldest age;
`transport_diagnostics()` reports backend acceptance and buffering. Queued,
backend-accepted, backend-buffered, and peer-delivered are distinct stages.

## Emscripten transport

`EmscriptenWebSocketTransport` implements the same framed polling contract on
`wasm32-unknown-emscripten`. Its browser callbacks buffer readiness, text,
binary, error, and close events; `SignalFishPollingClient::poll` drains them on
the main thread. It exposes structured close metadata and drives idempotent
cleanup through `poll_close`.

It is intended for the polling client, not the Tokio-spawned async client. See
the [WebAssembly guide](wasm.md) for target and linker requirements.

## Custom transport checklist

- Preserve both text and binary frame boundaries.
- Do not take the caller frame before the backend accepts it.
- Retain accepted sends and partial receives across `Pending`; do not wait for
  a socket-wide buffered byte count to reach zero as per-frame completion.
- Register the supplied waker when async progress depends on readiness.
- Make close multi-poll and idempotent.
- Record close code/reason/initiator before returning `None`.
- Keep `is_ready` cheap and monotonic for one physical connection.
- Put connection-specific construction outside the trait.
