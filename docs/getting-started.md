# Installation & Quick Start

This guide connects one Rust client, authenticates it, and joins a room. You
need Rust **1.87.0** or newer, a Signal Fish server URL, and an app ID accepted
by that server.

If you do not have a server yet, follow the Signal Fish Server [five-minute
quick start](https://ambiguous-interactive.github.io/signal-fish-server/quickstart/).
Its development setup accepts a test app ID. The App ID is a public application
label, not a secret. For production, use a label allowed by the server
operator's policy.

## Install the SDK

Add the client and the Tokio features used by this example:

```sh
cargo add signal-fish-client
cargo add tokio --features macros,rt-multi-thread
```

The equivalent manifest entries are:

```toml
[dependencies]
signal-fish-client = "0.12.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

`signal-fish-client` enables its built-in WebSocket transport by default.

!!! note "Published release and main"
    **0.12.0** is the current crates.io release. This guide follows unreleased
    `main`, which may include breaking APIs that have not reached a release
    yet; the
    [changelog](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/blob/main/CHANGELOG.md)
    says which release added them. Use the [0.12.0
    docs.rs pages](https://docs.rs/signal-fish-client/0.12.0/) with the versioned
    dependency above. To evaluate unreleased changes, use:

    ```toml
    signal-fish-client = { git = "https://github.com/Ambiguous-Interactive/signal-fish-client-rust" }
    ```

## Connect and join a room

Create `src/main.rs`:

```rust
use signal_fish_client::{
    JoinRoomParams, SignalFishClient, SignalFishConfig, SignalFishEvent,
    WebSocketTransport,
};

#[tokio::main]
async fn main() -> Result<(), signal_fish_client::SignalFishError> {
    let url = std::env::var("SIGNAL_FISH_URL")
        .unwrap_or_else(|_| "ws://localhost:3536/v2/ws".to_owned());

    let transport = WebSocketTransport::connect(&url).await?;
    let config = SignalFishConfig::new("mb_app_abc123");
    let (mut client, mut events) = SignalFishClient::start(transport, config);

    while let Some(event) = events.recv().await {
        match event {
            SignalFishEvent::Connected => println!("transport connected"),
            SignalFishEvent::Authenticated { app_name, .. } => {
                println!("authenticated as {app_name}");
                client.join_room(JoinRoomParams::new("my-game", "Alice"))?;
            }
            SignalFishEvent::RoomJoined { room_code, .. } => {
                println!("joined room {room_code}");
            }
            SignalFishEvent::AuthenticationError { error, .. } => {
                eprintln!("authentication failed: {error}");
                break;
            }
            SignalFishEvent::Disconnected { reason, .. } => {
                eprintln!("disconnected: {}", reason.as_deref().unwrap_or("unknown"));
                break;
            }
            _ => {}
        }
    }

    client.shutdown().await;
    Ok(())
}
```

Replace `mb_app_abc123` with your app ID, then run:

```sh
SIGNAL_FISH_URL=ws://localhost:3536/v2/ws cargo run
```

Authentication is queued when the client starts. Wait for `Authenticated`
before sending room commands, and keep receiving events for as long as the
client is active. A full event channel pauses protocol progress instead of
silently dropping events.

## Add the game lifecycle

Most games continue with these events and commands:

1. On `RoomJoined`, call `set_ready()` once when the local player is ready
   (the wire message toggles readiness, so a second call would un-ready you).
2. Observe `LobbyStateChanged` and authority events.
3. Call `start_game()` once the server's room rules allow it.
4. Exchange JSON game data after `GameStarting`. Binary frames require
   [protocol v3](protocol-versioning.md#opting-in) and an effectively negotiated
   binary format; see [Game Data](client.md#game-data).
5. Handle reconnect or disconnect events and call `shutdown().await` on exit.

The [`basic_lobby` example](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/blob/main/examples/basic_lobby.rs)
implements this flow without hiding authority changes or reconnect state. Run
the repository copy with:

```sh
cargo run --example basic_lobby
```

## Choose a different runtime

The async client runs a Tokio background task. Use the polling client when the
host gives your code one callback per frame and does not continuously drive a
Tokio runtime.

| Environment | Client and transport |
| --- | --- |
| Tokio native application | `SignalFishClient` + `WebSocketTransport` |
| Godot 4.5 native or web | `SignalFishPollingClient` + `GodotWebSocketTransport` |
| Browser with a custom binding | `SignalFishPollingClient` + your `Transport` |
| Custom async backend | `SignalFishClient` + your `Transport + Send` |

See [WebAssembly](wasm.md) for Godot and browser setup, or [Transport](transport.md)
to implement a backend.

## Optional capabilities

The default feature set is enough for the example above. Add features only for
the capability you need:

| Feature | Purpose |
| --- | --- |
| `transport-websocket` | Built-in native WebSocket transport; enabled by default |
| `tokio-runtime` | Async driver task and timing support; enabled by the default transport |
| `tls` | Native `wss://` connections |
| `polling-client` | Caller-driven client for game loops |
| `mesh` | Protocol-v3 WebRTC mesh state and controller APIs |
| `token-binding` | Native Server 0.8 token-binding negotiation |
| `transport-websocket-emscripten` | Advanced custom Emscripten hosts |

The default `transport-websocket` feature also enables `tokio-runtime`, which
provides the task and timing support required by `SignalFishClient`. If you
disable default features, select both capabilities explicitly.

## Next steps

- [Basic Lobby Walkthrough](examples.md) for the complete lobby flow
- [Client API Reference](client.md) for commands and configuration
- [Events Reference](events.md) and [Error Handling](errors.md) for event loops
- [Protocol Versioning](protocol-versioning.md) before enabling v3 or mesh
- [Delivery Contract & Backpressure](delivery.md) before tuning queue capacity
