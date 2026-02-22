# Signal Fish Client SDK — Project Context

## Identity

- **Company:** Ambiguous Interactive
- **Product:** Signal Fish Client SDK
- **Crate:** `signal-fish-client`
- **Version:** 0.1.0
- **Edition:** 2021
- **MSRV:** 1.85.0
- **License:** MIT
- **Repository:** <https://github.com/ambiguous-interactive/signal-fish-client-rust>

## Purpose

Transport-agnostic Rust client for the Signal Fish multiplayer signaling protocol. Enables game clients to join rooms, exchange game data, and receive server-pushed events over any bidirectional text transport.

## Mandatory Workflow

```shell
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features
```

Run this before every commit. All three steps must pass with zero warnings.

## Architecture — 7 Core Modules

| File | Purpose |
|------|---------|
| `src/transport.rs` | `Transport` trait — async bidirectional text messages |
| `src/protocol.rs` | Wire-compatible protocol types (`ClientMessage`, `ServerMessage`) |
| `src/error_codes.rs` | `ErrorCode` enum — 40 variants from server |
| `src/error.rs` | `SignalFishError` error type |
| `src/event.rs` | `SignalFishEvent` high-level event stream |
| `src/client.rs` | `SignalFishClient` async client + `SignalFishConfig` + `JoinRoomParams` |
| `src/transports/websocket.rs` | WebSocket transport (feature: `transport-websocket`) |

### Transport Trait

```rust
#[async_trait]
pub trait Transport: Send + 'static {
    async fn send(&mut self, message: String) -> Result<(), SignalFishError>;
    async fn recv(&mut self) -> Option<Result<String, SignalFishError>>;
    async fn close(&mut self) -> Result<(), SignalFishError>;
}
```

Note the bound is `Send + 'static` (not `Sync`). The `recv` method returns
`Option<Result<...>>` — `None` signals a clean server close, not an error.
`recv` MUST be cancel-safe because it is used inside `tokio::select!`.

### Client Usage Pattern

```rust
use signal_fish_client::{
    SignalFishClient, SignalFishConfig, JoinRoomParams, SignalFishEvent,
    WebSocketTransport,
};

#[tokio::main]
async fn main() -> Result<(), signal_fish_client::SignalFishError> {
    // 1. Connect transport
    let transport = WebSocketTransport::connect("wss://example.com/signal").await?;

    // 2. Build config with your App ID
    let config = SignalFishConfig::new("mb_app_abc123");

    // 3. start() returns (client_handle, event_receiver)
    //    Authenticate is sent automatically on start.
    let (mut client, mut events) = SignalFishClient::start(transport, config);

    // 4. Process events
    while let Some(event) = events.recv().await {
        match event {
            SignalFishEvent::Authenticated { app_name, .. } => {
                // Now safe to join a room
                client.join_room(JoinRoomParams::new("my-game", "Alice"))?;
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

### SignalFishConfig

Required second argument to `SignalFishClient::start`. Only `app_id` is required.

```rust
pub struct SignalFishConfig {
    pub app_id: String,
    pub sdk_version: Option<String>,   // defaults to crate version
    pub platform: Option<String>,      // e.g. "unity", "godot", "rust"
    pub game_data_format: Option<GameDataEncoding>,
}

let config = SignalFishConfig::new("mb_app_abc123");
```

### JoinRoomParams

Builder for `client.join_room(...)`.

```rust
let params = JoinRoomParams::new("my-game", "Alice")
    .with_room_code("ABC123")   // omit for quick-match
    .with_max_players(4)
    .with_supports_authority(true);
client.join_room(params)?;
```

### Key Client Methods

All methods except `shutdown` are synchronous (they queue a message, no round-trip):

```rust
client.join_room(params: JoinRoomParams) -> Result<()>
client.leave_room() -> Result<()>
client.send_game_data(data: serde_json::Value) -> Result<()>
client.set_ready() -> Result<()>
client.request_authority(become_authority: bool) -> Result<()>
client.provide_connection_info(info: ConnectionInfo) -> Result<()>
client.reconnect(player_id, room_id, auth_token) -> Result<()>
client.join_as_spectator(game_name, room_code, spectator_name) -> Result<()>
client.leave_spectator() -> Result<()>
client.ping() -> Result<()>
client.shutdown().await      // async, graceful
```

All `Result<()>` methods return `Err(SignalFishError::NotConnected)` when the
transport is closed.

## Feature Flags

| Flag | Default | Description |
|------|---------|-------------|
| `transport-websocket` | on | Built-in WebSocket via `tokio-tungstenite` |

## Dependencies

### Runtime

| Crate | Purpose |
|-------|---------|
| `tokio` | Async runtime (sync, macros, rt, time features) |
| `async-trait` | Async methods in traits (pre-AFIT, MSRV 1.75) |
| `serde` + `serde_json` + `serde_bytes` | JSON serialization of protocol messages |
| `uuid` | Player/room IDs matching server format |
| `thiserror` | Derive macro for `SignalFishError` |
| `tracing` | Structured logging and diagnostics |
| `tokio-tungstenite` | WebSocket transport (optional) |
| `futures-util` | Stream/sink utilities for WebSocket (optional) |

### Dev

| Crate | Purpose |
|-------|---------|
| `tokio` (full) | Full runtime for tests |
| `tracing-subscriber` | Log output during tests |

## Key Design Decisions

### Transport Agnosticism

The `Transport` trait decouples protocol logic from network I/O. Tests use
in-memory `VecDeque`-backed transports. Production code uses WebSocket. Custom
transports (QUIC, raw TCP, etc.) need only implement three async methods.

### Wire Compatibility

`ClientMessage` and `ServerMessage` use adjacently-tagged serde encoding
(`#[serde(tag = "type", content = "data")]`) to match the Signal Fish server
v2 JSON protocol. Never change serde attributes without verifying against
the server spec. See `skills/serde-patterns.md` for details.

### `#[non_exhaustive]`

No public enums in this crate carry `#[non_exhaustive]`. `SignalFishEvent`,
`ErrorCode`, `SignalFishError`, and all protocol payload structs are exhaustive.
Adding variants to any of these enums is a semver breaking change.

### No Heavy Dependencies

- No `chrono` — timestamps remain `String` from the server
- No `bytes` — binary payloads are `Vec<u8>` with `serde_bytes`
- No `reqwest` — HTTP is out of scope

### UUID Convention

Player IDs and room IDs are `uuid::Uuid`, serialized as lowercase hyphenated
strings to match server expectations.

### Connection / Auth Flow

1. `SignalFishClient::start(transport, config)` queues `ClientMessage::Authenticate`
   immediately before spawning the transport loop.
2. Server responds with `ServerMessage::Authenticated` → `SignalFishEvent::Authenticated`.
3. Client may then call `join_room`, etc.
4. The transport loop emits a synthetic `SignalFishEvent::Connected` at startup
   and `SignalFishEvent::Disconnected` when the transport closes.

## Protocol Overview

Both `ClientMessage` and `ServerMessage` use adjacent tagging:

```json
{ "type": "JoinRoom", "data": { "game_name": "my-game", ... } }
{ "type": "RoomJoined", "data": { "room_id": "...", ... } }
```

Variant names are PascalCase in JSON (serde default for adjacently-tagged enums
with no `rename_all`). See `skills/serde-patterns.md` for the full wire format
reference.

## `.llm/` Structure

```text
.llm/
  context.md          ← you are here (canonical source of truth)
  skills/
    index.md          ← auto-regenerated by pre-commit hook (do not edit)
    async-rust-patterns.md
    ci-configuration.md
    crate-publishing.md
    error-handling.md
    markdown-and-doc-validation.md
    public-api-design.md
    serde-patterns.md
    testing-async.md
    tracing-instrumentation.md
    transport-abstraction.md
    websocket-client.md
```

Skills are focused reference guides for common tasks in this codebase. See
`skills/index.md` for a summary of each.

## Pre-commit Enforcement

A pre-commit hook enforces:

1. No `.llm/*.md` file exceeds 300 lines (`scripts/pre-commit-llm.py`)
2. `skills/index.md` is auto-regenerated from skill file headings
3. `cargo fmt --all -- --check` passes
4. `cargo clippy --all-targets --all-features -- -D warnings` passes

`cargo test` is part of the mandatory workflow but runs on push, not every
commit, because it is too slow for a blocking hook. Run it manually before
opening a PR.

Install hooks with: `bash scripts/install-hooks.sh`
