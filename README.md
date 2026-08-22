<p align="center">
  <img src="https://raw.githubusercontent.com/Ambiguous-Interactive/signal-fish-client-rust/main/docs/assets/logo-banner.svg" alt="Signal Fish Client SDK" width="640">
</p>

<p align="center">
  <a href="https://Ambiguous-Interactive.github.io/signal-fish-client-rust/"><img src="https://img.shields.io/badge/docs-GitHub%20Pages-blue?logo=github" alt="Documentation"></a>
  <a href="https://crates.io/crates/signal-fish-client"><img src="https://img.shields.io/crates/v/signal-fish-client.svg" alt="Crates.io"></a>
  <a href="https://docs.rs/signal-fish-client"><img src="https://img.shields.io/docsrs/signal-fish-client" alt="docs.rs"></a>
  <a href="https://github.com/Ambiguous-Interactive/signal-fish-client-rust/actions/workflows/ci.yml"><img src="https://github.com/Ambiguous-Interactive/signal-fish-client-rust/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://doc.rust-lang.org/stable/releases.html#version-1870-2025-05-15"><img src="https://img.shields.io/badge/MSRV-1.87.0-blue.svg" alt="MSRV"></a>
  <a href="https://github.com/Ambiguous-Interactive/signal-fish-client-rust/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
</p>

Signal Fish is a multiplayer signaling service. This Rust SDK connects a game
to a Signal Fish server, joins players to rooms, relays game data, and exposes
server activity as typed events. It supports ordinary Tokio applications and
frame-driven engines such as Godot.

The SDK does not provide a game engine, rollback implementation, or WebRTC
backend. It handles the Signal Fish protocol while your game owns simulation
and peer networking.

## Start here

Need a local server or app ID? Follow the server's [five-minute quick
start](https://ambiguous-interactive.github.io/signal-fish-server/quickstart/).
Its development setup accepts a test app ID. The App ID is a public application
label, not a secret; production servers use the operator's configured policy.

Add the client and Tokio:

```toml
[dependencies]
signal-fish-client = "0.10.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Connect, wait for authentication, and join a room:

```rust
use signal_fish_client::{
    JoinRoomParams, SignalFishClient, SignalFishConfig, SignalFishEvent,
    WebSocketTransport,
};

#[tokio::main]
async fn main() -> Result<(), signal_fish_client::SignalFishError> {
    let transport = WebSocketTransport::connect("ws://localhost:3536/v2/ws").await?;
    let config = SignalFishConfig::new("mb_app_abc123");
    let (mut client, mut events) = SignalFishClient::start(transport, config);

    while let Some(event) = events.recv().await {
        match event {
            SignalFishEvent::Authenticated { .. } => {
                client.join_room(JoinRoomParams::new("my-game", "Alice"))?;
            }
            SignalFishEvent::RoomJoined { room_code, .. } => {
                println!("joined {room_code}");
            }
            SignalFishEvent::Disconnected { .. } => break,
            _ => {}
        }
    }

    client.shutdown().await;
    Ok(())
}
```

In a clone of the source repository, run the complete example, which also
handles readiness, game start, reconnection state, errors, and Ctrl+C:

```sh
cargo run --example basic_lobby
```

Set `SIGNAL_FISH_URL` to use a server other than
`ws://localhost:3536/v2/ws`.

The published `0.10.0` crate is the stable release. This branch also documents
unreleased changes planned for 0.11. Use the [0.10.0 API
docs](https://docs.rs/signal-fish-client/0.10.0/) for the published surface, or
see the
[changelog](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/blob/main/CHANGELOG.md)
before depending on `main`.

## Choose your integration

| Your application | Use |
| --- | --- |
| Tokio application | `SignalFishClient` with the built-in `WebSocketTransport` |
| Native or web Godot 4.5 game | `SignalFishPollingClient` with `signal-fish-client-godot` |
| Browser or another frame-driven host | `SignalFishPollingClient` with your own `Transport` |
| Custom async network stack | `SignalFishClient` with your own `Transport + Send` |

Start with protocol v2 relay unless you need Server 0.7 delivery
accountability or WebRTC mesh signaling. Opt into those features deliberately;
the [protocol versioning guide](https://Ambiguous-Interactive.github.io/signal-fish-client-rust/protocol-versioning/)
explains the
choice.

## Documentation

- [Installation and first connection](https://Ambiguous-Interactive.github.io/signal-fish-client-rust/getting-started/)
- [Basic lobby walkthrough](https://Ambiguous-Interactive.github.io/signal-fish-client-rust/examples/)
- [Client commands and configuration](https://Ambiguous-Interactive.github.io/signal-fish-client-rust/client/)
- [Events](https://Ambiguous-Interactive.github.io/signal-fish-client-rust/events/)
  and [errors](https://Ambiguous-Interactive.github.io/signal-fish-client-rust/errors/)
- [Godot and WebAssembly](https://Ambiguous-Interactive.github.io/signal-fish-client-rust/wasm/)
- [Protocol v2, v3, and mesh](https://Ambiguous-Interactive.github.io/signal-fish-client-rust/protocol-versioning/)
- [Custom transports](https://Ambiguous-Interactive.github.io/signal-fish-client-rust/transport/)
- [API reference](https://docs.rs/signal-fish-client)

The [full guide](https://Ambiguous-Interactive.github.io/signal-fish-client-rust/)
keeps advanced protocol, delivery, transport, and migration material separate
from the onboarding path.

## Before production

- Enable the `tls` feature and use `wss://` outside a trusted local environment.
- Drain events continuously; a full event channel intentionally backpressures
  the transport loop.
- Treat `SendBufferFull` as congestion and retry according to your game policy.
- Call `shutdown().await` when possible so the transport can close cleanly.
- Allow the exact HTTPS origin serving a browser or Godot web build in the
  Signal Fish server configuration.

## Project notes

The core crate supports Rust 1.87.0 and newer. The Godot adapter requires Rust
1.94.0 because of its godot-rust dependency.

<details>
<summary>AI disclosure</summary>

This project was developed with substantial AI assistance. Humans created the
protocol and core technology concepts and retained responsibility for
architecture and review; AI tools assisted heavily with implementation,
documentation, and tests.

</details>

Signal Fish Client SDK is available under the
[MIT License](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/blob/main/LICENSE).
Brand asset provenance and bundled font licenses are recorded in the
[documentation attribution page](https://Ambiguous-Interactive.github.io/signal-fish-client-rust/attributions/).
