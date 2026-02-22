# Installation & Quick Start

Get up and running with the Signal Fish Client SDK in minutes.

## Prerequisites

Before you begin, make sure you have:

- **Rust 1.75.0** or newer (`rustup update stable`)
- A **tokio** async runtime (the SDK is async-first)
- A running **Signal Fish server** URL (e.g., `ws://localhost:3536/ws`)
- An **App ID** registered with your Signal Fish server

## Installation

Add the crate to your project:

```sh
cargo add signal-fish-client
```

### Feature Flags

| Feature                | Default | Description                                      |
|------------------------|---------|--------------------------------------------------|
| `transport-websocket`  | Yes     | WebSocket transport via `tokio-tungstenite`       |

#### With default features (includes WebSocket transport)

```toml
[dependencies]
signal-fish-client = "0.1"
```

#### Without default features (bring your own transport)

```toml
[dependencies]
signal-fish-client = { version = "0.1", default-features = false }
```

!!! tip
    If you only need the core `Transport` trait to implement a custom backend, disable default features to avoid pulling in `tokio-tungstenite` and `futures-util`.

## Minimal Example

Below is a complete working example that connects to a Signal Fish server, authenticates, joins a room, and shuts down gracefully.

```rust
use signal_fish_client::{
    JoinRoomParams, SignalFishClient, SignalFishConfig, SignalFishEvent, WebSocketTransport,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Read the server URL from the environment, or fall back to localhost.
    let url = std::env::var("SIGNAL_FISH_URL")
        .unwrap_or_else(|_| "ws://localhost:3536/ws".to_string());

    // 1. Connect a WebSocket transport to the signaling server.
    let transport = WebSocketTransport::connect(&url).await?;

    // 2. Build a client config with your application ID.
    let config = SignalFishConfig::new("your-app-id");

    // 3. Start the client — returns a handle and an event receiver.
    let (mut client, mut event_rx) = SignalFishClient::start(transport, config);

    // 4. Drive the event loop.
    loop {
        tokio::select! {
            event = event_rx.recv() => {
                let Some(event) = event else {
                    // Channel closed — background task exited.
                    break;
                };

                match event {
                    SignalFishEvent::Connected => {
                        println!("Transport connected");
                    }
                    SignalFishEvent::Authenticated { app_name, .. } => {
                        println!("Authenticated as {app_name}");
                        // Safe to join a room now.
                        client.join_room(JoinRoomParams::new("my-game", "Alice"))?;
                    }
                    SignalFishEvent::RoomJoined { room_code, player_id, .. } => {
                        println!("Joined room {room_code} as {player_id}");
                    }
                    SignalFishEvent::Disconnected { .. } => break,
                    _ => {}
                }
            }
            // Graceful shutdown on Ctrl+C.
            _ = tokio::signal::ctrl_c() => {
                println!("Shutting down...");
                break;
            }
        }
    }

    // 5. Shut down gracefully.
    client.shutdown().await;
    Ok(())
}
```

!!! note
    `WebSocketTransport` requires the `transport-websocket` feature, which is enabled by default. If you disabled default features you will need to re-enable it explicitly:
    ```toml
    signal-fish-client = { version = "0.1", default-features = false, features = ["transport-websocket"] }
    ```

## What Happens Under the Hood

When you call `SignalFishClient::start`, the SDK:

1. **Spawns a background task** that drives the transport — reading incoming messages and writing outgoing ones.
2. **Auto-authenticates** by immediately sending an `Authenticate` message with the App ID from your `SignalFishConfig`.
3. **Emits typed events** on a bounded `tokio::sync::mpsc` channel (capacity **256**). Your application consumes these via the `event_rx` receiver returned from `start`.

You interact with the server by calling methods on the `SignalFishClient` handle (e.g., `join_room`, `send_game_data`). These enqueue outgoing messages that the background task sends over the transport.

!!! warning
    If your event-processing loop cannot keep up with the server, events will be
    **dropped** (with a warning logged) to avoid blocking the transport loop. The
    `Disconnected` event is always delivered. Design your handler to stay responsive.

## Next Steps

- [Core Concepts](concepts.md) — rooms, players, relays, and the event model
- [Client API](client.md) — full reference for `SignalFishClient` methods
- [Events](events.md) — every `SignalFishEvent` variant explained
