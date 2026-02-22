---
title: Home
description: "Signal Fish Client SDK — A transport-agnostic Rust client SDK for the Signal Fish multiplayer signaling protocol"
---

<p align="center">
  <img src="assets/logo-banner.svg" alt="Signal Fish Client SDK" width="600">
</p>

**A transport-agnostic Rust client SDK for the Signal Fish multiplayer signaling protocol.**

[![Crates.io](https://img.shields.io/crates/v/signal-fish-client?style=flat-square&logo=rust)](https://crates.io/crates/signal-fish-client)
[![docs.rs](https://img.shields.io/docsrs/signal-fish-client?style=flat-square&logo=docs.rs)](https://docs.rs/signal-fish-client)
[![CI](https://img.shields.io/github/actions/workflow/status/Ambiguous-Interactive/signal-fish-client-rust/ci.yml?branch=main&style=flat-square&logo=github&label=CI)](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/blob/main/LICENSE)
[![MSRV](https://img.shields.io/badge/MSRV-1.75.0-brightgreen?style=flat-square&logo=rust)](https://blog.rust-lang.org/2023/12/28/Rust-1.75.0.html)

---

## Key Features

- :material-swap-horizontal: **Transport-Agnostic** — Plug in any transport that implements the `Transport` trait; swap WebSocket for TCP, QUIC, or a test loopback without changing your game code.
- :material-lightning-bolt: **Async / Await** — Built on Tokio with a fully non-blocking API. Command methods return immediately; events arrive on an async channel.
- :material-message-flash: **Event-Driven Architecture** — All server responses are delivered as strongly-typed `SignalFishEvent` variants on a bounded `mpsc` channel — just `match` in a loop.
- :material-web: **WebSocket Built-In** — `WebSocketTransport` ships out of the box (enabled by default via the `transport-websocket` feature) so you can connect in one line.
- :material-refresh: **Reconnection Support** — Gracefully handle disconnects and reconnect to your session without losing context.
- :material-eye: **Spectator Mode** — Join rooms as a spectator to observe game state without participating.

---

## Quick Start

Add the crate to your project:

```bash
cargo add signal-fish-client
```

Then connect, authenticate, and join a room in just a few lines:

```rust
use signal_fish_client::{
    WebSocketTransport, SignalFishClient, SignalFishConfig,
    JoinRoomParams, SignalFishEvent,
};

#[tokio::main]
async fn main() -> Result<(), signal_fish_client::SignalFishError> {
    let transport = WebSocketTransport::connect("wss://example.com/signal").await?;
    let config = SignalFishConfig::new("mb_app_abc123");
    let (mut client, mut event_rx) = SignalFishClient::start(transport, config);

    while let Some(event) = event_rx.recv().await {
        match event {
            SignalFishEvent::Authenticated { app_name, .. } => {
                println!("Authenticated as {app_name}");
                client.join_room(JoinRoomParams::new("my-game", "Alice"))?;
            }
            SignalFishEvent::RoomJoined { room_code, .. } => {
                println!("Joined room {room_code}");
            }
            SignalFishEvent::Disconnected { .. } => break,
            _ => {}
        }
    }

    client.shutdown().await;
    Ok(())
}
```

!!! tip "Feature flag"
    `WebSocketTransport` requires the **`transport-websocket`** feature, which is enabled by default. If you disabled default features, re-enable it explicitly:
    ```toml
    signal-fish-client = { version = "*", features = ["transport-websocket"] }
    ```

---

## Explore the Docs

<div class="grid cards" markdown>

- :material-rocket-launch:{ .lg .middle } **Getting Started**

    ---

    Install the crate, set up Tokio, and make your first connection in under five minutes.

    [:octicons-arrow-right-24: Getting Started](getting-started.md)

- :material-book-open-variant:{ .lg .middle } **API Reference**

    ---

    Detailed guides for the Client, Transport trait, Events, Errors, and Protocol types.

    [:octicons-arrow-right-24: API Reference](client.md)

- :material-code-tags:{ .lg .middle } **Examples**

    ---

    Walkthroughs of real-world usage patterns — lobby management, custom transports, and more.

    [:octicons-arrow-right-24: Examples](examples.md)

- :material-file-document:{ .lg .middle } **docs.rs**

    ---

    Auto-generated API documentation with full type signatures and doc comments.

    [:octicons-arrow-right-24: docs.rs](https://docs.rs/signal-fish-client)

</div>

---

## Links

| Resource | URL |
|----------|-----|
| **GitHub Repository** | [Ambiguous-Interactive/signal-fish-client-rust](https://github.com/Ambiguous-Interactive/signal-fish-client-rust) |
| **crates.io** | [signal-fish-client](https://crates.io/crates/signal-fish-client) |
| **docs.rs** | [signal-fish-client](https://docs.rs/signal-fish-client) |

---

<p style="text-align: center; opacity: 0.7;">
Built with :heart: by <strong>Ambiguous Interactive</strong>
</p>
