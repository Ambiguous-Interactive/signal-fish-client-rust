# Testing Async Code

Reference for tokio::test, mock transports, and channel-based test patterns as used in this codebase.

## tokio::test Macro

```rust
#[tokio::test]
async fn test_basic() {
    let result = async_function().await;
    assert!(result.is_ok());
}

// With multi-threaded runtime
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_concurrent() {
    // spawned tasks run on real threads
}
```

## Mock Transport Pattern (actual pattern in this codebase)

Tests use a `VecDeque`-based `MockTransport` that replays scripted server
responses. This is the real implementation in `tests/common/mod.rs`:

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
            item   // None = clean close; Some(Ok(s)) = message; Some(Err(e)) = error
        } else {
            // Hang forever — loop stays alive until client.shutdown()
            std::future::pending().await
        }
    }

    async fn close(&mut self) -> Result<(), SignalFishError> {
        self.closed.store(true, Ordering::Relaxed);
        Ok(())
    }
}
```

## Starting a Mock Client

`SignalFishClient::start` always sends `Authenticate` first and emits a
synthetic `Connected` event before processing server responses:

```rust
#[tokio::test]
async fn test_room_join() {
    let (transport, sent, _closed) = MockTransport::new(vec![
        Some(Ok(authenticated_json())),   // server confirms auth
        Some(Ok(room_joined_json())),     // server confirms room join
    ]);

    let config = SignalFishConfig::new("mb_test");
    let (mut client, mut events) = SignalFishClient::start(transport, config);

    // First event is always Connected (synthetic)
    let ev = events.recv().await.unwrap();
    assert!(matches!(ev, SignalFishEvent::Connected));

    // Then Authenticated (from scripted server response)
    let ev = events.recv().await.unwrap();
    assert!(matches!(ev, SignalFishEvent::Authenticated { .. }));

    // Send join_room (synchronous — queues message)
    client.join_room(JoinRoomParams::new("my-game", "Alice")).unwrap();

    // RoomJoined event arrives from scripted response
    let ev = events.recv().await.unwrap();
    assert!(matches!(ev, SignalFishEvent::RoomJoined { .. }));

    client.shutdown().await;
}
```

## Verifying Outgoing Messages

Messages are recorded in `sent` in the order they are dispatched.
`messages[0]` is always the `Authenticate` message sent automatically on start.

```rust
// Give the transport loop time to process the queued command
tokio::time::sleep(std::time::Duration::from_millis(50)).await;

let messages = sent.lock().unwrap();
let join_msg: ClientMessage = serde_json::from_str(&messages[1]).unwrap();
if let ClientMessage::JoinRoom { game_name, player_name, .. } = join_msg {
    assert_eq!(game_name, "my-game");
    assert_eq!(player_name, "Alice");
} else {
    panic!("expected JoinRoom");
}
```

## Scripting Transport Errors and Clean Close

```rust
// Simulate a transport receive error
let (transport, _, _) = MockTransport::new(vec![
    Some(Err(SignalFishError::TransportReceive("network failure".into()))),
]);
// Client emits Disconnected { reason: Some("transport receive error: ...") }

// Simulate a clean server close
let (transport, _, _) = MockTransport::new(vec![
    Some(Ok(authenticated_json())),
    None,   // explicit None = clean transport close
]);
// Client emits Disconnected { reason: None }
```

## Shutdown Timeout State Invariants

When testing shutdown timeout paths (e.g., `transport.close()` hangs and the
task is aborted), always assert client state accessors are reset even if the
`Disconnected` event is not observed:

```rust
client.shutdown().await;
assert!(!client.is_connected());
assert!(!client.is_authenticated());
assert!(client.current_player_id().await.is_none());
assert!(client.current_room_id().await.is_none());
assert!(client.current_room_code().await.is_none());
```

This prevents regressions where aborted tasks skip the normal disconnect event
path and leave stale authenticated/room/player state visible to callers.

## Test Organization

```text
tests/
  client_tests.rs     ← integration tests (public API only)
  common/
    mod.rs            ← MockTransport + JSON helper fns

src/
  client.rs
  #[cfg(test)] mod tests { ... }  ← unit tests (access private items)
```

Helper functions in `tests/common/mod.rs` produce JSON strings for common
server messages: `authenticated_json()`, `room_joined_json()`,
`room_left_json()`, `pong_json()`, `reconnected_json()`, `spectator_joined_json()`,
`spectator_left_json()`, `player_joined_json(name, id)`, `player_left_json(id)`,
`error_json(msg, code)`, `authority_response_json(granted, reason)`,
`game_data_json(player, data)`, `game_data_binary_json(player, enc, bytes)`.

## Cargo Test Commands

```shell
# Run all tests (mandatory workflow)
cargo test --all-features

# Run a specific test by name
cargo test test_join_room

# Run with output visible
cargo test -- --nocapture

# Run integration tests only
cargo test --test client_tests --all-features
```

## Determinism Tips

- All tests use the default `current_thread` runtime (deterministic ordering)
- Avoid `tokio::time::sleep` in tests where possible; prefer scripted responses
- If timing is required, use small sleeps (50ms) to allow the transport loop
  to process queued commands — see the pattern in `client_tests.rs`
- `std::future::pending()` in `MockTransport::recv` keeps the loop alive
  without busy-polling until `shutdown()` is called
