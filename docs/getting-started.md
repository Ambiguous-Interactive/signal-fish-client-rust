# Installation & Quick Start

Get up and running with the Signal Fish Client SDK in minutes.

## Prerequisites

Before you begin, make sure you have:

- **Rust 1.87.0** or newer (`rustup update stable`)
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
| `transport-godot` | No | Godot 4.5 `WebSocketPeer` transport for native and official web exports |
| `transport-websocket-emscripten` | No | Emscripten WebSocket transport for `wasm32-unknown-emscripten` |
| `tokio-runtime` | Yes (via `transport-websocket`) | Tokio runtime integration; disable for WASM targets |

#### With default features (includes WebSocket transport)

```toml
[dependencies]
signal-fish-client = "0.7.0"
```

#### Without default features (bring your own transport)

```toml
[dependencies]
signal-fish-client = { version = "0.7.0", default-features = false }
```

!!! tip
    If you only need the core `Transport` trait to implement a custom backend, disable default features to avoid pulling in `tokio-tungstenite` and `futures-util`.

#### For Godot 4.5 native and web exports

```toml
[dependencies]
signal-fish-client = { version = "0.7.0", default-features = false, features = ["transport-godot"] }
```

!!! tip
    The `transport-godot` feature provides `GodotWebSocketTransport` and `SignalFishPollingClient` — a synchronous, game-loop-driven path using Godot's own `WebSocketPeer`. It works with official no-thread web export templates and requires no GDScript glue. See the [WebAssembly Guide](wasm.md) for complete setup instructions.

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
    signal-fish-client = { version = "0.7.0", default-features = false, features = ["transport-websocket"] }
    ```

## What Happens Under the Hood

When you call `SignalFishClient::start`, the SDK:

1. **Spawns a background task** that drives the transport — reading incoming messages and writing outgoing ones.
2. **Auto-authenticates** by immediately sending an `Authenticate` message with the App ID from your `SignalFishConfig`.
3. **Emits typed events** on a bounded `tokio::sync::mpsc` channel (default capacity **256**, configurable via [`SignalFishConfig::event_channel_capacity`](client.md#signalfishconfig)). Your application consumes these via the `event_rx` receiver returned from `start`.

You interact with the server by calling methods on the `SignalFishClient` handle (e.g., `join_room`, `send_game_data`). These enqueue outgoing messages on a bounded queue that the background task drains over the transport; if you outpace the transport, sends fail fast with `SendBufferFull` instead of silently dropping (see [Core Concepts](concepts.md#non-blocking-command-sending)).

!!! note
    Events are **never dropped on overflow**. If your event-processing loop
    cannot keep up with the server, the transport loop pauses until the channel
    has room — backpressure propagates to the server instead of losing events.
    An event can only be missed if the receiver is dropped, the client handle
    is dropped without calling `shutdown()`, or on
    [`shutdown()`](client.md#shutdown) — which delivers the terminal
    `Disconnected` best-effort and may drop at most one in-flight event. Keep
    your handler responsive so the connection keeps flowing;
    `event_channel_capacity` on your `SignalFishConfig` controls how much
    buffering you get before backpressure kicks in.

## Next Steps

- [Core Concepts](concepts.md) — rooms, players, relays, and the event model
- [Client API](client.md) — full reference for `SignalFishClient` methods
- [Events](events.md) — every `SignalFishEvent` variant explained
- [WebAssembly Guide](wasm.md) — building for `wasm32-unknown-unknown` and `wasm32-unknown-emscripten`, Godot integration
