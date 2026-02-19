# Signal Fish Client

Transport-agnostic Rust client for the **Signal Fish** multiplayer signaling protocol.

## Features

- **Transport-agnostic** — implement the `Transport` trait for any backend (WebSocket, WebRTC data channel, custom TCP, etc.)
- **Wire-compatible** — all protocol types match the Signal Fish server's v2 format exactly
- **Feature-gated WebSocket transport** — the default `transport-websocket` feature provides a ready-to-use `WebSocketTransport`
- **Event-driven** — receive typed `SignalFishEvent`s via a Tokio channel
- **Non-exhaustive** — all public enums and payload structs are `#[non_exhaustive]` for forward compatibility

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
signal-fish-client = "0.1"
```

To use without the built-in WebSocket transport (bring your own):

```toml
[dependencies]
signal-fish-client = { version = "0.1", default-features = false }
```

## Quick Start

```rust,no_run
use signal_fish_client::{SignalFishClient, SignalFishEvent};

#[tokio::main]
async fn main() {
    // Full usage examples coming in Phase 6+
    println!("Signal Fish Client scaffold ready!");
}
```

## Feature Flags

| Feature               | Default | Description                                          |
| --------------------- | ------- | ---------------------------------------------------- |
| `transport-websocket` | **yes** | Built-in WebSocket transport via `tokio-tungstenite` |

## Minimum Supported Rust Version (MSRV)

**1.75.0**

This crate is tested against the latest stable Rust and the declared MSRV. Bumping the MSRV is considered a minor version change.

## Architecture

| Module                  | Purpose                                                 |
| ----------------------- | ------------------------------------------------------- |
| `transport`             | `Transport` trait for pluggable backends                |
| `protocol`              | Wire-compatible `ClientMessage` / `ServerMessage` types |
| `error_codes`           | `ErrorCode` enum (39 server error variants)             |
| `error`                 | `SignalFishError` unified error type                    |
| `event`                 | `SignalFishEvent` high-level event stream               |
| `client`                | `SignalFishClient` async client implementation          |
| `transports::websocket` | Built-in WebSocket transport                            |

## License

[MIT](LICENSE) — Copyright (c) 2025-2026 Ambiguous Interactive
