# Transport Abstraction

Reference for the `Transport` trait design, writing mock transports for tests, and implementing custom transports.

## The Transport Trait

Defined in `src/transport.rs`:

```rust
use async_trait::async_trait;
use crate::error::SignalFishError;

#[async_trait]
pub trait Transport: Send + 'static {
    /// Send a JSON text message to the server.
    async fn send(&mut self, message: String) -> Result<(), SignalFishError>;

    /// Receive the next JSON text message from the server.
    /// Returns None when the connection is closed cleanly by the server.
    async fn recv(&mut self) -> Option<Result<String, SignalFishError>>;

    /// Close the transport connection gracefully.
    async fn close(&mut self) -> Result<(), SignalFishError>;
}
```

Key points:

- The trait bound is `Send + 'static`, NOT `Sync`.
- `recv` returns `Option<Result<...>>`, not `Result<Option<...>>`. `None` is
  a clean close; `Some(Err(e))` is a transport error.
- `recv` MUST be cancel-safe — the transport loop uses `tokio::select!` and
  may cancel an in-progress `recv` call. Channel-based implementations are
  naturally cancel-safe.
- The trait is object-safe: `Box<dyn Transport>` works for dynamic dispatch.
- `SignalFishClient::start` accepts `impl Transport` (monomorphized).

## Using Transport with SignalFishClient

```rust
use signal_fish_client::{SignalFishClient, SignalFishConfig};

// start() takes ownership of the transport.
// It returns (client_handle, event_receiver) — not a builder.
let config = SignalFishConfig::new("mb_app_abc123");
let (mut client, mut events) = SignalFishClient::start(transport, config);

while let Some(event) = events.recv().await {
    // handle events
}
client.shutdown().await;
```

## VecDeque-Based Mock Transport (actual pattern in this codebase)

The real mock transport used in tests scripts responses with a `VecDeque`.
See `tests/common/mod.rs` for the canonical implementation:

```rust
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use async_trait::async_trait;
use signal_fish_client::{Transport, SignalFishError};

pub struct MockTransport {
    incoming: VecDeque<Option<Result<String, SignalFishError>>>,
    pub sent: Arc<Mutex<Vec<String>>>,
    pub closed: Arc<AtomicBool>,
}

impl MockTransport {
    pub fn new(
        incoming: Vec<Option<Result<String, SignalFishError>>>,
    ) -> (Self, Arc<Mutex<Vec<String>>>, Arc<AtomicBool>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let transport = Self {
            incoming: VecDeque::from(incoming),
            sent: Arc::clone(&sent),
            closed: Arc::clone(&closed),
        };
        (transport, sent, closed)
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn send(&mut self, message: String) -> Result<(), SignalFishError> {
        self.sent.lock().unwrap().push(message);
        Ok(())
    }

    async fn recv(&mut self) -> Option<Result<String, SignalFishError>> {
        if let Some(item) = self.incoming.pop_front() {
            item  // None entry = clean close; Some(result) = message or error
        } else {
            // All scripted responses consumed — pending() never
            // completes (yields `Poll::Pending` without registering a
            // waker). The tokio runtime keeps this task alive until
            // `client.shutdown()` aborts it. Missing mock responses
            // surface as test timeouts rather than silent successes.
            std::future::pending().await
        }
    }

    async fn close(&mut self) -> Result<(), SignalFishError> {
        self.closed.store(true, Ordering::Relaxed);
        Ok(())
    }
}
```

### Test Example

```rust
#[tokio::test]
async fn test_join_room_sends_correct_message() {
    let (transport, sent, _closed) = MockTransport::new(vec![
        Some(Ok(authenticated_json())),   // server Authenticated response
    ]);

    let config = SignalFishConfig::new("mb_test");
    let (mut client, mut events) = SignalFishClient::start(transport, config);

    let _ = events.recv().await; // Connected (synthetic)
    let _ = events.recv().await; // Authenticated

    client.join_room(JoinRoomParams::new("my-game", "Alice")).unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let messages = sent.lock().unwrap();
    // messages[0] = Authenticate (sent automatically on start)
    // messages[1] = JoinRoom
    let join_msg: ClientMessage = serde_json::from_str(&messages[1]).unwrap();
    assert!(matches!(join_msg, ClientMessage::JoinRoom { .. }));

    client.shutdown().await;
}
```

## Implementing a Custom Transport

```rust
use async_trait::async_trait;
use signal_fish_client::{Transport, SignalFishError};

struct MyTransport { /* fields */ }

#[async_trait]
impl Transport for MyTransport {
    async fn send(&mut self, message: String) -> Result<(), SignalFishError> {
        // Write the JSON text over your transport layer
        todo!()
    }

    async fn recv(&mut self) -> Option<Result<String, SignalFishError>> {
        // Return Some(Ok(text)) for a message
        // Return Some(Err(e)) for a transport error
        // Return None for clean server close
        // MUST be cancel-safe
        todo!()
    }

    async fn close(&mut self) -> Result<(), SignalFishError> {
        // Graceful shutdown; release resources even if handshake fails
        todo!()
    }
}
```

## Design Constraints

- Transport must be `Send + 'static` — required for `tokio::spawn`
- `recv` returning `None` signals clean shutdown (not an error)
- `recv` MUST be cancel-safe for use in `tokio::select!`
- `close` should be idempotent — calling it twice must not panic
- Do not buffer messages inside the transport; the client layer handles ordering

## `is_ready()` — Connection Readiness

Default returns `true` (correct for transports connected at construction).
Override to return `false` until the handshake completes for async-handshake
transports. `SignalFishPollingClient` defers `Connected` until `is_ready()`
returns `true`. Contract: cheap, non-blocking, monotonic (once true, stays true).

## close() and the Polling Client

`SignalFishPollingClient::close()` polls the transport's `close()` future
exactly once with a noop waker and discards the `Poll` result. If `close()`
returns `Pending`, the shutdown is silently incomplete. Only transports whose
`close()` resolves to `Ready` immediately are guaranteed a clean shutdown
via the polling client.

The `EmscriptenWebSocketTransport` always returns `Ready(Ok(()))` from
`close()`, so this is safe for the primary use case. Custom transports
targeting the polling client must ensure `close()` completes synchronously.

Document this contract in the `close()` method's doc comment and in
`docs/client.md` whenever the polling client's close behavior is described.

## `std::future::pending()` in Transport Implementations

### When to use

`std::future::pending()` is appropriate in `recv` when there are no more
messages to deliver and the transport should block indefinitely:

- **Mock transports** (e.g., `MockTransport`): after all scripted responses
  are consumed, `recv` returns `pending().await` to keep the task alive until
  `shutdown()` aborts it.
- **Polling-only transports** (e.g., emscripten WebSocket): the transport is
  polled with a noop waker and never awaited in a real async runtime. Returning
  `pending().await` signals "no data yet" to the polling loop.

### Caller contract

Each call to `recv` must create a **new** `pending()` future. The future
registers no waker and will never wake — re-polling the same future is
pointless. The transport loop in `SignalFishClient` naturally satisfies this
because `tokio::select!` drops and recreates the `recv` future each iteration.

### Noop-waker polling vs real async runtime

- **Emscripten transport**: polled via `Future::poll` with a noop waker from
  a browser event loop. `Pending` means "nothing ready this tick" and the
  browser calls back later. No tokio runtime is involved.
- **Tokio mock transport**: the task is spawned on a tokio runtime. `Pending`
  with no waker means the task will never be woken — it stays alive only
  because tokio keeps spawned tasks until they are aborted by `shutdown()`.
  Missing mock responses surface as test timeouts.

### Documentation requirement

Any use of `std::future::pending().await` **must** include a comment explaining:
(1) the future never wakes (no waker registered), (2) callers must create a
new future per call, and (3) which runtime/polling model makes this safe.

### Debug-build misuse detection for polling-only transports

Transports designed exclusively for noop-waker polling (like
`EmscriptenWebSocketTransport`) should include a `cfg(debug_assertions)`
guard that detects when a real async runtime waker is provided. This
prevents silent hangs that are extremely difficult to diagnose.

Pattern: Create a `NoopWakerPending` future that checks
`cx.waker().will_wake(Waker::noop())` in debug builds:

```rust
struct NoopWakerPending;

impl Future for NoopWakerPending {
    type Output = std::convert::Infallible;
    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        #[cfg(debug_assertions)]
        {
            let noop = std::task::Waker::noop();
            if !_cx.waker().will_wake(noop) {
                tracing::error!(
                    "transport polled with real waker; use SignalFishPollingClient"
                );
            }
        }
        Poll::Pending
    }
}
```

This replaces `std::future::pending().await` and makes misuse immediately
visible during development.

### Awaiting futures with uninhabited output types

`NoopWakerPending` has `Output = Infallible` (an uninhabited type). Nightly
Rust may change how `()` vs `Infallible` interacts with `.await`. Use the
`match expr.await {}` pattern to handle any uninhabited output without
binding the result:

```rust
// WRONG — breaks if nightly changes the inferred type
NoopWakerPending.await

// CORRECT — works for any uninhabited type (Infallible, !, etc.)
match NoopWakerPending.await {}
```

The `match {}` with no arms is exhaustive for uninhabited types and the
expression diverges (has type `!`), so it works in any position.
