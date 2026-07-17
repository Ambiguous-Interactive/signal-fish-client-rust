---
name: testing-async
description: Test asynchronous Signal Fish behavior deterministically. Use when writing Tokio tests, mock transports, channel assertions, backpressure cases, timeouts, protocol samples, or concurrency regressions.
---

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

### tokio::test comes from tokio, not tokio-test

The `#[tokio::test]` attribute macro is provided by the **`tokio`** crate itself (with the `macros` feature). The separate `tokio-test` crate provides different utilities: `assert_ready!`, `assert_pending!`, `task::spawn` (manual poll harness), and `io::Builder` (mock I/O). If your tests only use `#[tokio::test]`, you do not need `tokio-test` as a dev-dependency. Keeping it listed without use will cause `cargo-udeps` failures in CI.

## Mock Transport Pattern (actual pattern in this codebase)

Tests use the `VecDeque`-based `MockTransport` in `tests/common/mod.rs`:

```rust
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use signal_fish_client::transport::TransportFrame;
use signal_fish_client::{SignalFishError, Transport};

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

impl Transport for MockTransport {
    fn poll_send(
        &mut self,
        _cx: &mut Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> Poll<Result<(), SignalFishError>> {
        if let Some(frame) = frame.take() {
            let TransportFrame::Text(message) = frame else {
                panic!("test expected an outbound text frame");
            };
            self.sent.lock().unwrap().push(message);
        }
        Poll::Ready(Ok(()))
    }

    fn poll_recv(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<TransportFrame, SignalFishError>>> {
        if let Some(item) = self.incoming.pop_front() {
            Poll::Ready(item.map(|result| result.map(TransportFrame::Text)))
        } else {
            // No waker: stays pending until shutdown aborts the async driver.
            Poll::Pending
        }
    }

    fn poll_close(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), SignalFishError>> {
        self.closed.store(true, Ordering::Relaxed);
        Poll::Ready(Ok(()))
    }
}
```

For binary-path tests, script and record `TransportFrame` values directly.
A `poll_send` implementation may take the frame only after accepting it;
if it returns `Pending`, retain that frame internally until a later `Ready`.

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

Messages are recorded in `sent`; `messages[0]` is the automatic `Authenticate`.

```rust
tokio::time::timeout(std::time::Duration::from_secs(1), async {
    while sent.lock().unwrap().len() < 2 {
        tokio::task::yield_now().await;
    }
})
.await
.expect("queued command should be sent promptly");
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
let snapshot = client.snapshot();
assert!(!snapshot.connected);
assert!(!snapshot.authenticated);
assert!(snapshot.player_id.is_none());
assert!(snapshot.room_id.is_none());
assert!(snapshot.room_code.is_none());
```

This prevents regressions where aborted tasks skip the normal disconnect event
path and leave stale authenticated/room/player state visible to callers.

## Test Organization

Public-API integration tests live in `tests/client_tests.rs`; their frame-aware
mock and JSON helpers live in `tests/common/mod.rs`. Private unit tests remain
in each source module's `#[cfg(test)]` module. Reuse the common helpers for
authentication, room/spectator lifecycle, errors, and text/binary game data.

## Cargo Test Commands

```shell
# Run all tests (mandatory workflow)
cargo test --workspace --all-features

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
- For outbound mock sends, wait for the mock's `sent` buffer to reach the expected length with a bounded timeout; fixed sleeps are CI-flaky.
- Returning `Poll::Pending` without using `cx.waker()` deliberately never
  wakes the async driver. Use this only for mocks meant to stay idle until
  shutdown; transports with later input or send capacity must register/wake.
- For polling schedulers, use generated transition sequences plus a reference
  ownership model. Persist shrunken failures and include negative models that
  duplicate a frame or impose stop-and-wait so the oracle's sensitivity is
  itself tested.

## Custom Code Scanners in Tests

When writing test functions that scan source files for identifier names (e.g., verifying dependency usage), use word-boundary-aware matching. Simple `line.contains(name)` checks produce false positives when one identifier is a prefix of another (e.g., `tokio` matching `tokio_tungstenite`). See `ci_config_tests.rs::line_references_crate` for the canonical pattern and the [ci-configuration](../ci-configuration/SKILL.md) skill for full guidance.

## Cross-Platform Path Assertions

When testing functions that produce file paths in error messages or output,
never hardcode forward slashes (`/`) in assertions. Windows uses `\` as the
path separator. Use `std::path::Path` to build expected paths so assertions
work on both platforms.

## Test Quality: Prefer `.expect()` Over `.unwrap()`

Use `.expect("descriptive message")` instead of `.unwrap()` in tests. When a
test fails in CI, `.unwrap()` produces only `called 'Option::unwrap()' on a
'None' value` with no context. `.expect()` includes your message, making
failures diagnosable without reproducing locally.

### What Makes a Good Expect Message

Include enough context to identify **what** failed and **where** the data
came from:

```rust
// WRONG — no context on CI failure
let path = find_config().unwrap();

// CORRECT — describes the operation that failed
let path = find_config().expect("find_config should locate Cargo.toml");

// CORRECT — includes runtime context for file operations
let content = std::fs::read_to_string(&path)
    .expect(&format!("failed to read {}", path.display()));

// CORRECT — includes variable context
let value = map.get(key)
    .expect(&format!("map should contain key '{key}'"));
```

### Guidelines for Good Messages

- Describe the operation or expectation, not just the variable name
- For file operations, include the path using `.display()`
- For map/collection lookups, include the key being searched
- For deserialization, mention the type and source
- Phrase as what *should* have been true: `"config should have a [dependencies] section"`

### Exceptions Where `.unwrap()` Is Acceptable

Not every `.unwrap()` needs to become `.expect()`. These cases are acceptable:

- **Mutex locks** (`sent.lock().unwrap()`): A poisoned mutex indicates a
  panic in another thread, which is already a test failure. The `.unwrap()`
  panic message includes the poison error, which is sufficient context.

- **Already-verified Options**: When the value was just checked (e.g.,
  `assert!(opt.is_some()); let val = opt.unwrap();`) or when the `None`
  case is structurally impossible from the preceding logic.

- **Test assertions** (`assert!`, `assert_eq!`, `assert_matches!`): These
  macros produce their own diagnostic output.

- **Infallible conversions**: Operations that cannot fail for the given
  input (e.g., `"valid_utf8".parse::<String>().unwrap()`).

## Protocol v2/v3 Test Patterns

- **Golden-wire conformance**: `tests/wire_golden_tests.rs` round-trips the real
  server samples — see [protocol-wire-conformance](../protocol-wire-conformance/SKILL.md).
- **Negotiation**: script `protocol_info_json(Some(3))` after `authenticated_json()`
  through `MockTransport`; assert the negotiated version, v3 sends, and the
  pre-negotiation error. Assert `supports_mesh()` only with `enable_mesh()`;
  relay-only `enable_v3()` must remain false.
- **Frames/accountability**: keep text and binary frames in one scripted order;
  assert malformed binary becomes `DecodeFailed`, while valid accountability
  failures become `ProtocolViolation` and follow the configured policy.
- **`MeshController`**: test the choreography with a recording `WebRtcDriver` mock
  (see `src/webrtc.rs` tests) — drive the handshake, assert `connect(peer, initiate)`
  obeys the server, signals are relayed, and `PeerConnected`/`PeerDisconnected`
  surface with correct transport-status reports.
