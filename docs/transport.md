# Transport Trait & WebSocket

This page covers the `Transport` trait — the networking abstraction at the heart
of the SDK — and the built-in `WebSocketTransport` that ships with the crate.

---

## The `Transport` Trait

Every transport used by `SignalFishClient` must implement the `Transport` trait.
It defines three async methods for bidirectional text messaging:

```rust
#[async_trait]
pub trait Transport: Send + 'static {
    async fn send(&mut self, message: String) -> Result<(), SignalFishError>;
    async fn recv(&mut self) -> Option<Result<String, SignalFishError>>;
    async fn close(&mut self) -> Result<(), SignalFishError>;
}
```

### Trait Bounds

The trait requires **`Send + 'static`** (but *not* `Sync`). This is because
`SignalFishClient::start` moves the transport into a background Tokio task that
runs for the lifetime of the client. `Send` allows the value to cross the thread
boundary into the spawned task; `'static` ensures it owns all its data (no
borrowed references).

The trait also uses `#[async_trait]` from the `async-trait` crate, which
desugars the `async fn` methods into `Pin<Box<dyn Future>>` return types. You
must add `#[async_trait]` to your `impl` block as well.

### Return Type of `recv()`

`recv()` returns `Option<Result<String, SignalFishError>>`. The three possible
outcomes are:

| Return value | Meaning |
|---|---|
| `Some(Ok(text))` | A complete JSON message was received from the server. |
| `Some(Err(e))` | A transport-level error occurred (e.g., `SignalFishError::TransportReceive`). |
| `None` | The connection was closed cleanly by the server. This is **not** an error. |

The client's internal event loop uses `None` to detect a graceful server
shutdown and emit a `SignalFishEvent::Disconnected` event.

### Cancel Safety

!!! warning "recv() must be cancel-safe"
    The `recv()` method is called inside `tokio::select!` in the client's event
    loop. If the `select!` branch is not chosen, the future returned by `recv()`
    is **dropped** before it completes.

    **Cancel-safe** means: if the future is dropped mid-await, calling `recv()`
    again must not lose any data. No message may be partially consumed or
    silently discarded.

    Channel-based implementations (e.g., wrapping `tokio::sync::mpsc::Receiver`)
    are **naturally cancel-safe** because the channel stores messages
    independently of the receive future. The built-in `WebSocketTransport` is
    also cancel-safe.

    If your transport buffers data internally during `recv()`, you must ensure
    that a dropped future does not leave the buffer in an inconsistent state.

### Connection Setup

Connection setup is intentionally **not** part of the trait. Different
transports have fundamentally different connection parameters — URLs for
WebSocket, host:port for TCP, QUIC endpoints, etc. Construct a connected
transport externally, then hand it to `SignalFishClient::start`.

---

## `WebSocketTransport`

The crate ships with a ready-made WebSocket transport behind the
**`transport-websocket`** feature flag (enabled by default).

```rust
use signal_fish_client::WebSocketTransport;
```

It wraps a `tokio-tungstenite` `WebSocketStream` and supports both `ws://` and
`wss://` URLs. TLS is handled transparently.

### `connect(url)`

Establish a new WebSocket connection:

```rust
let transport = WebSocketTransport::connect("wss://example.com/signal").await?;
```

Returns `Result<WebSocketTransport, SignalFishError>`. On failure the error is
`SignalFishError::Io` with the underlying I/O error kind preserved when
available.

### `connect_with_timeout(url, timeout)`

Same as `connect`, but fails with `SignalFishError::Timeout` if the connection
is not established within the given duration:

```rust
use std::time::Duration;

let transport = WebSocketTransport::connect_with_timeout(
    "wss://example.com/signal",
    Duration::from_secs(5),
)
.await?;
```

### `from_stream(stream)`

Wrap an already-established `WebSocketStream` for advanced use cases such as
custom TLS configuration, proxy headers, or authentication cookies:

```rust
use signal_fish_client::transports::websocket::WsStream;

// WsStream is a type alias for:
// tokio_tungstenite::WebSocketStream<
//     tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>
// >

// Construct `my_stream: WsStream` using tokio-tungstenite directly…
let transport = WebSocketTransport::from_stream(my_stream);
```

### Connecting and Starting the Client

A complete example that connects via WebSocket and processes events:

```rust
use signal_fish_client::{
    SignalFishClient, SignalFishConfig, JoinRoomParams,
    SignalFishEvent, WebSocketTransport,
};

#[tokio::main]
async fn main() -> Result<(), signal_fish_client::SignalFishError> {
    // 1. Connect the transport
    let transport = WebSocketTransport::connect("wss://example.com/signal").await?;

    // 2. Build config with your App ID
    let config = SignalFishConfig::new("mb_app_abc123");

    // 3. start() returns (client_handle, event_receiver)
    let (mut client, mut events) = SignalFishClient::start(transport, config);

    // 4. Process events
    while let Some(event) = events.recv().await {
        match event {
            SignalFishEvent::Authenticated { app_name, .. } => {
                println!("Authenticated as {app_name}");
                client.join_room(JoinRoomParams::new("my-room", "Alice"))?;
            }
            SignalFishEvent::RoomJoined { room_code, .. } => {
                println!("Joined room {room_code}");
            }
            SignalFishEvent::Disconnected { .. } => break,
            _ => {}
        }
    }

    // 5. Shut down gracefully
    client.shutdown().await;
    Ok(())
}
```

---

## Implementing a Custom Transport

You can implement `Transport` for any bidirectional text channel — raw TCP,
QUIC, WebRTC data channels, Unix sockets, or even an in-memory loopback for
testing.

!!! tip "When to write a custom transport"
    - **Testing** — unit-test game logic without a real server by using
      in-process channels.
    - **Custom protocols** — adapt a non-WebSocket I/O layer (TCP, QUIC, WebRTC
      data channels).
    - **Unity / FFI interop** — bridge messages from a game engine's networking
      layer into the SDK.

### Step 1: Define the Struct

Use `tokio::sync::mpsc` channels as the backing store. This gives you natural
cancel safety for free:

```rust
use tokio::sync::mpsc;

pub struct LoopbackTransport {
    /// Messages the client sends go here.
    tx: mpsc::UnboundedSender<String>,
    /// Messages the client receives arrive here.
    rx: mpsc::UnboundedReceiver<String>,
}
```

### Step 2: Implement `Transport`

```rust
use async_trait::async_trait;
use signal_fish_client::{SignalFishError, Transport};

#[async_trait]
impl Transport for LoopbackTransport {
    async fn send(&mut self, message: String) -> Result<(), SignalFishError> {
        self.tx
            .send(message)
            .map_err(|e| SignalFishError::TransportSend(e.to_string()))
    }

    async fn recv(&mut self) -> Option<Result<String, SignalFishError>> {
        self.rx.recv().await.map(Ok)
    }

    async fn close(&mut self) -> Result<(), SignalFishError> {
        Ok(())
    }
}
```

Key points:

- **`send`** — push the message into the channel. Map the `SendError` to
  `SignalFishError::TransportSend`.
- **`recv`** — await the next message. `mpsc::UnboundedReceiver::recv()` returns
  `None` when all senders are dropped, which the client interprets as a clean
  close. Wrapping with `.map(Ok)` converts `Option<String>` into the required
  `Option<Result<String, SignalFishError>>`.
- **`close`** — for channels, dropping is sufficient. A no-op `Ok(())` works.

### Step 3: Wire into `SignalFishClient::start()`

```rust
use signal_fish_client::{SignalFishClient, SignalFishConfig, SignalFishEvent};

// Create the loopback pair (client ↔ server channels)
let (client_tx, server_rx) = tokio::sync::mpsc::unbounded_channel();
let (server_tx, client_rx) = tokio::sync::mpsc::unbounded_channel();

let transport = LoopbackTransport {
    tx: client_tx,
    rx: client_rx,
};

let config = SignalFishConfig::new("mb_app_test");
let (mut client, mut events) = SignalFishClient::start(transport, config);

// Process events as usual
while let Some(event) = events.recv().await {
    match event {
        SignalFishEvent::Authenticated { app_name, .. } => {
            println!("Authenticated: {app_name}");
        }
        SignalFishEvent::Disconnected { .. } => break,
        _ => {}
    }
}

client.shutdown().await;
```

The SDK does not care *how* the transport is connected — it only calls `send`,
`recv`, and `close`. Your custom transport is a first-class citizen.
