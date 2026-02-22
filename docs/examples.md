# Example Walkthroughs

Hands-on examples that demonstrate the Signal Fish Client SDK in realistic
scenarios. Each walkthrough covers the **complete** source file, then breaks it
down step by step.

!!! tip "Running the examples"

    Every example lives in the `examples/` directory and can be launched with
    `cargo run --example <name>`.

---

## Basic Lobby

**Source:** `examples/basic_lobby.rs`

Demonstrates the **full WebSocket lifecycle** — connecting to a Signal Fish
server, authenticating, joining a room, reacting to lobby events, and shutting
down gracefully.

### Full source

```rust
use signal_fish_client::{
    JoinRoomParams, SignalFishClient, SignalFishConfig, SignalFishEvent, WebSocketTransport,
};

const DEFAULT_URL: &str = "ws://localhost:3536/ws";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let url = std::env::var("SIGNAL_FISH_URL").unwrap_or_else(|_| DEFAULT_URL.to_string());
    tracing::info!("Connecting to {url}");

    let transport = WebSocketTransport::connect(&url).await?;
    let config = SignalFishConfig::new("mb_app_abc123");
    let (mut client, mut event_rx) = SignalFishClient::start(transport, config);

    loop {
        tokio::select! {
            event = event_rx.recv() => {
                let Some(event) = event else {
                    tracing::info!("Event channel closed, exiting");
                    break;
                };

                match event {
                    SignalFishEvent::Connected => {
                        tracing::info!("Transport connected, awaiting authentication…");
                    }
                    SignalFishEvent::Authenticated { app_name, .. } => {
                        tracing::info!("Authenticated as app: {app_name}");
                        let params = JoinRoomParams::new("example-game", "RustPlayer")
                            .with_max_players(4);
                        client.join_room(params)?;
                        tracing::info!("Join-room request sent");
                    }
                    SignalFishEvent::RoomJoined {
                        room_code, player_id, current_players, ..
                    } => {
                        tracing::info!(
                            "Joined room {room_code} as player {player_id} ({} player(s) present)",
                            current_players.len()
                        );
                        client.set_ready()?;
                        tracing::info!("Set ready");
                    }
                    SignalFishEvent::PlayerJoined { player } => {
                        tracing::info!("Player joined: {} ({})", player.name, player.id);
                    }
                    SignalFishEvent::PlayerLeft { player_id } => {
                        tracing::info!("Player left: {player_id}");
                    }
                    SignalFishEvent::LobbyStateChanged { lobby_state, all_ready, .. } => {
                        tracing::info!("Lobby state → {lobby_state:?} (all_ready={all_ready})");
                    }
                    SignalFishEvent::GameStarting { peer_connections } => {
                        tracing::info!(
                            "Game starting with {} peer connection(s)!",
                            peer_connections.len()
                        );
                    }
                    SignalFishEvent::AuthenticationError { error, error_code } => {
                        tracing::error!("Auth failed [{error_code:?}]: {error}");
                        break;
                    }
                    SignalFishEvent::Error { message, error_code } => {
                        tracing::error!("Server error [{error_code:?}]: {message}");
                    }
                    SignalFishEvent::Disconnected { reason } => {
                        tracing::warn!("Disconnected: {}", reason.as_deref().unwrap_or("unknown"));
                        break;
                    }
                    other => {
                        tracing::debug!("Event: {other:?}");
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Ctrl+C received, shutting down…");
                break;
            }
        }
    }

    client.shutdown().await;
    tracing::info!("Client shut down. Goodbye!");
    Ok(())
}
```

### Step-by-step walkthrough

#### 1. Connect to the server via WebSocket

```rust
let transport = WebSocketTransport::connect(&url).await?;
```

`WebSocketTransport::connect` opens a WebSocket connection to the Signal Fish
server. The URL can be overridden with the **`SIGNAL_FISH_URL`** environment
variable; it defaults to `ws://localhost:3536/ws`.

#### 2. Configure and start the client

```rust
let config = SignalFishConfig::new("mb_app_abc123");
let (mut client, mut event_rx) = SignalFishClient::start(transport, config);
```

`SignalFishConfig` carries your **App ID** (issued by the server).
`SignalFishClient::start` consumes the transport and config, returning:

- a `SignalFishClient` handle for sending commands, and
- an `mpsc::Receiver<SignalFishEvent>` for incoming events.

#### 3. Event loop with `tokio::select!`

```rust
loop {
    tokio::select! {
        event = event_rx.recv() => { /* … */ }
        _ = tokio::signal::ctrl_c() => { break; }
    }
}
```

The outer `loop` + `tokio::select!` pattern lets the example react to **server
events** while also catching **Ctrl+C** for a clean exit.

#### 4. Handle authentication, room join, and lobby events

| Event | Action taken |
|---|---|
| `Connected` | Log that the transport is up; wait for authentication. |
| `Authenticated` | Build `JoinRoomParams` and call `client.join_room(…)`. |
| `RoomJoined` | Log the room code and player list, then call `client.set_ready()`. |
| `PlayerJoined` / `PlayerLeft` | Log the change. |
| `LobbyStateChanged` | Log the new lobby state and whether all players are ready. |
| `GameStarting` | Log the peer connections — the game is about to begin. |
| `AuthenticationError` / `Error` | Log the error (and break on auth failure). |
| `Disconnected` | Log the reason and exit the loop. |

#### 5. Graceful shutdown

```rust
client.shutdown().await;
```

`shutdown()` closes the transport and drains background tasks so the process
exits cleanly.

### Running it

```sh
cargo run --example basic_lobby
```

!!! info "Environment variable"

    Set **`SIGNAL_FISH_URL`** to point at your server if it is not running on
    the default `ws://localhost:3536/ws`.

    ```sh
    SIGNAL_FISH_URL=ws://my-server:3536/ws cargo run --example basic_lobby
    ```

---

## Custom Transport

**Source:** `examples/custom_transport.rs`

Demonstrates how to **implement the `Transport` trait** with a simple in-memory
loopback, then wire it into the SDK — perfect for **unit testing without a
network**.

### Simplified source

!!! note
    The actual [`examples/custom_transport.rs`](https://github.com/Ambiguous-Interactive/signal-fish-client/blob/main/examples/custom_transport.rs) uses the `tracing` crate for structured logging. The version below uses `println!` for simplicity.

```rust
use async_trait::async_trait;
use signal_fish_client::{
    SignalFishClient, SignalFishConfig, SignalFishError, SignalFishEvent, Transport,
};
use tokio::sync::mpsc;

pub struct LoopbackTransport {
    tx: mpsc::UnboundedSender<String>,
    rx: mpsc::UnboundedReceiver<String>,
}

pub struct LoopbackServer {
    pub rx: mpsc::UnboundedReceiver<String>,
    pub tx: mpsc::UnboundedSender<String>,
}

fn loopback_pair() -> (LoopbackTransport, LoopbackServer) {
    let (client_tx, server_rx) = mpsc::unbounded_channel();
    let (server_tx, client_rx) = mpsc::unbounded_channel();
    let transport = LoopbackTransport { tx: client_tx, rx: client_rx };
    let server = LoopbackServer { rx: server_rx, tx: server_tx };
    (transport, server)
}

#[async_trait]
impl Transport for LoopbackTransport {
    async fn send(&mut self, message: String) -> Result<(), SignalFishError> {
        self.tx.send(message).map_err(|e| SignalFishError::TransportSend(e.to_string()))
    }
    async fn recv(&mut self) -> Option<Result<String, SignalFishError>> {
        self.rx.recv().await.map(Ok)
    }
    async fn close(&mut self) -> Result<(), SignalFishError> {
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (transport, mut server) = loopback_pair();
    let config = SignalFishConfig::new("mb_app_test");
    let (mut client, mut event_rx) = SignalFishClient::start(transport, config);

    let auth_msg = server.rx.recv().await.expect("should receive Authenticate");
    println!("Server received: {auth_msg}");

    let auth_response = serde_json::json!({
        "type": "Authenticated",
        "data": {
            "app_name": "Test App",
            "organization": null,
            "rate_limits": { "per_minute": 60, "per_hour": 3600, "per_day": 86400 }
        }
    });
    server.tx.send(auth_response.to_string())?;

    let mut events_seen = 0;
    while let Some(event) = event_rx.recv().await {
        match &event {
            SignalFishEvent::Connected => println!("Event: Connected (synthetic)"),
            SignalFishEvent::Authenticated { app_name, .. } => {
                println!("Event: Authenticated — app_name={app_name}");
            }
            SignalFishEvent::Disconnected { reason } => {
                println!("Event: Disconnected — {}", reason.as_deref().unwrap_or("clean"));
                break;
            }
            _ => println!("Event: {event:?}"),
        }
        events_seen += 1;
        if events_seen >= 2 { break; }
    }

    client.shutdown().await;
    println!("Done — saw {events_seen} event(s). Custom transport works!");
    Ok(())
}
```

### Step-by-step walkthrough

#### 1. Define `LoopbackTransport` with mpsc channels

```rust
pub struct LoopbackTransport {
    tx: mpsc::UnboundedSender<String>,
    rx: mpsc::UnboundedReceiver<String>,
}
```

Two unbounded channels form a **bidirectional pipe**: one for messages the
client sends *to* the server, one for messages the server sends *back*.

The helper `loopback_pair()` wires the channels so that each side's `tx` feeds
the other side's `rx`.

#### 2. Implement the `Transport` trait

```rust
#[async_trait]
impl Transport for LoopbackTransport {
    async fn send(&mut self, message: String) -> Result<(), SignalFishError> {
        self.tx.send(message).map_err(|e| SignalFishError::TransportSend(e.to_string()))
    }
    async fn recv(&mut self) -> Option<Result<String, SignalFishError>> {
        self.rx.recv().await.map(Ok)
    }
    async fn close(&mut self) -> Result<(), SignalFishError> {
        Ok(())
    }
}
```

Only three methods are required:

| Method | Purpose |
|---|---|
| `send` | Push a serialized message toward the server. |
| `recv` | Await the next message from the server (returns `None` on close). |
| `close` | Perform any cleanup (no-op here). |

#### 3. Create a fake server that injects responses

```rust
let auth_msg = server.rx.recv().await.expect("should receive Authenticate");
let auth_response = serde_json::json!({ /* … */ });
server.tx.send(auth_response.to_string())?;
```

The "server" side reads the auto-sent `Authenticate` message, then replies with
a hand-crafted `Authenticated` JSON payload — no network required.

#### 4. Wire into the client and observe events

```rust
let (mut client, mut event_rx) = SignalFishClient::start(transport, config);
```

From the SDK's perspective the loopback behaves identically to a real WebSocket.
The example collects two events (`Connected` and `Authenticated`) before
shutting down.

!!! note "Use case: unit testing without a network"

    This pattern is the recommended way to **test application logic** that
    depends on Signal Fish without needing a live server. Inject whatever
    server messages you like and assert on the resulting events.

### Running it

```sh
cargo run --example custom_transport
```

Expected output:

```text
Server received: {"type":"Authenticate","data":{"app_id":"mb_app_test"}}
Event: Connected (synthetic)
Event: Authenticated — app_name=Test App
Done — saw 2 event(s). Custom transport works!
```
