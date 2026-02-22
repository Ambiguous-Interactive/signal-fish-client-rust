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
            // All scripted messages delivered — hang until shutdown
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
