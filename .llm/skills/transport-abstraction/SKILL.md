---
name: transport-abstraction
description: Implement the frame-capable polling Transport contract. Use when changing transport ownership, send or receive polling, readiness, close behavior, custom backends, or transport mocks.
---

# Transport Abstraction

Reference for the frame-capable polling [`Transport`](../../../src/transport.rs)
contract, custom transports, and test mocks.

## Public Contract

```rust
use std::task::{Context, Poll};
use signal_fish_client::error::SignalFishError;
use signal_fish_client::transport::{
    Transport, TransportCloseInfo, TransportFrame,
};

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

`TransportFrame` has two variants:

- `Text(String)` for JSON protocol messages.
- `Binary(Vec<u8>)` for opaque protocol-v2/v3 binary game data.

The trait is object-safe and deliberately has no `Send`, `Sync`, or `'static`
supertrait. `Box<dyn Transport>` works. The async driver places
`Send + 'static` on `SignalFishClient::start` because it moves the transport
into a Tokio task. `SignalFishPollingClient` accepts non-`Send`, main-thread
transports.

## Outbound Ownership

The caller owns `frame: Option<TransportFrame>` until the transport takes it.

- Return `Pending` without taking the frame when the transport has not accepted
  it.
- Once the transport calls `frame.take()`, the backend owns the frame. This is
  successful ownership transfer, not peer delivery or a socket-wide drain.
- Client-owned queue-age telemetry ends at that `frame.take()` boundary. A
  refusal that leaves the slot intact must preserve the frame's original
  enqueue timestamp along with its FIFO identity.
- A transport may return `Ready(Ok(()))` immediately after backend acceptance.
  Never use a socket-wide buffered amount reaching zero as per-frame completion.
- If a transport returns `Pending` after taking a frame, it must retain all
  state needed to finish that exact accepted operation across later polls.
- Repeated polls while that send is pending must not accept a replacement frame,
  restart the write, or duplicate bytes.
- `None` means no new frame. If no retained write exists, return `Ready(Ok(()))`.

For a buffered sink, the usual state machine is:

1. `poll_ready`; if pending, leave the caller's slot untouched.
2. Take the frame and call `start_send` once.
3. Return success once the backend contract defines ownership as transferred.
   A sink that requires `poll_flush` to preserve accepted bytes retains that
   state; a browser/native API that copies or queues the packet can return at
   once.

This replaces the old cancel-safe async `send` requirement with explicit,
persistent polling state.

## Receive Outcomes

`poll_recv` returns:

| Value | Meaning |
|---|---|
| `Pending` | No complete frame is available yet. |
| `Ready(Some(Ok(frame)))` | One complete text or binary frame. |
| `Ready(Some(Err(error)))` | Transport receive failure. |
| `Ready(None)` | Terminal clean/peer close. |

If `poll_recv` consumes partial input before returning `Pending`, that partial
input must remain in the transport. A later poll must continue it rather than
lose it. When a real async waker is supplied, register or forward it so the
async client wakes when progress becomes possible. A frame-loop polling client
uses a noop waker and simply polls again next tick.

## Close Contract

`poll_close` may return `Pending`; both drivers poll it again. It must be
idempotent:

- Start the close handshake at most once.
- Retain close progress across polls.
- After `Ready(Ok(()))`, every later call returns `Ready(Ok(()))` without
  emitting another close.
- Release local resources even when the handshake fails.

`close_info()` returns structured terminal metadata when available:

```rust
pub struct TransportCloseInfo {
    pub code: Option<u16>,
    pub reason: Option<String>,
    pub clean: Option<bool>,
    pub initiated_by_peer: bool,
}
```

Capture peer close metadata before returning `Ready(None)` from `poll_recv`.

## Readiness

`is_ready()` is cheap and non-blocking. The default `true` fits transports
whose constructor completes the handshake. Async-handshake transports return
`false` until ready; the polling client defers its synthetic `Connected` event.
Once true, readiness should remain true for that physical connection.

## Minimal Channel Transport

```rust,ignore
use std::task::{Context, Poll};
use signal_fish_client::error::SignalFishError;
use signal_fish_client::transport::{Transport, TransportFrame};
use tokio::sync::mpsc;

struct LoopbackTransport {
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

This immediate channel send may take the frame because it also returns `Ready`.
A socket that returns `Pending` after acceptance needs an internal outbound
slot/state machine. A backend that reports capacity without accepting must
leave the caller frame untouched so it can be retried exactly once and in order.

## Canonical Test Mock

Tests usually keep incoming JSON as `String` for concise fixtures and map it at
the boundary:

```rust,ignore
fn poll_recv(
    &mut self,
    _cx: &mut Context<'_>,
) -> Poll<Option<Result<TransportFrame, SignalFishError>>> {
    match self.incoming.pop_front() {
        Some(item) => Poll::Ready(
            item.map(|result| result.map(TransportFrame::Text)),
        ),
        None => Poll::Pending,
    }
}
```

Use `Poll::Pending` directly; do not construct an async `pending()` future.
Mocks that deliberately gate a send must either leave the caller's frame in its
slot or retain the accepted frame internally. If they need wake-driven async
progress, retain and poll the readiness future rather than recreating and
dropping it each call.

## Built-in WebSocket Requirements

`WebSocketTransport`:

- maps WebSocket text and binary messages without discarding either;
- retains an accepted outbound message until `poll_flush` completes;
- records close code/reason in `TransportCloseInfo`;
- drives `poll_close` idempotently;
- flushes tungstenite's automatically queued Pong before reading again;
- disables Nagle's algorithm (`TCP_NODELAY`) by default on the socket it owns.

Do not treat Ping/Pong as application frames. Do not skip binary application
frames.

### Low-latency socket options (class-level rule)

A transport that **owns its TCP socket** — the built-in WebSocket transport, or a
future QUIC / raw-TCP backend — must disable Nagle's algorithm (`TCP_NODELAY`) so
small, latency-sensitive game frames are not held back by TCP's delayed-ACK timer
(a stall worth tens of milliseconds per round trip). A transport that **delegates
its socket** to a browser or game engine — the Emscripten and Godot
`WebSocketPeer` backends — cannot reach the socket and need not: the platform
owns that tuning. When adding any socket-owning transport, apply this rule (and
add a regression test asserting the option) rather than rediscovering the stall
in integration testing. The `websocket-client` skill has the concrete
`WebSocketTransport` implementation and its caller opt-out.

## Checklist

- [ ] Text and binary frames round-trip.
- [ ] A frame is neither lost nor duplicated across `Pending`.
- [ ] Real wakers are registered/forwarded for async progress.
- [ ] `poll_recv` retains partial input across `Pending`.
- [ ] Peer close returns `None` and preserves structured metadata.
- [ ] `poll_close` is multi-poll and idempotent.
- [ ] `is_ready` matches handshake state.
- [ ] Non-`Send` transports compile with the polling client.
- [ ] `Send + 'static` is imposed only by async-client construction.
- [ ] A transport that owns its TCP socket disables Nagle (`TCP_NODELAY`);
      browser/engine-delegated backends are exempt.
