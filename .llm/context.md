# Signal Fish Client SDK — Project Context

## Identity

- **Company:** Ambiguous Interactive
- **Product:** Signal Fish Client SDK
- **Crate:** `signal-fish-client`
- **Version:** 0.7.0
- **Edition:** 2021
- **MSRV:** 1.85.0
- **License:** MIT
- **Repository:** <https://github.com/Ambiguous-Interactive/signal-fish-client-rust>
- **Guide (GitHub Pages):** <https://Ambiguous-Interactive.github.io/signal-fish-client-rust/>
- **API Docs (docs.rs):** <https://docs.rs/signal-fish-client>

## Purpose

Transport-agnostic Rust client for the Signal Fish multiplayer signaling protocol. Enables game clients to join rooms, exchange game data, and receive server-pushed events over any bidirectional text transport.

## Mandatory Workflow

```shell
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features
```

Run this before every commit. All three steps must pass with zero warnings.

## CI/CD Action Reference Policy

Use version tags in workflow `uses:` references, not commit hashes.

- Prefer: `owner/action@vN.N.N` (immutable tag); `owner/action@vN` acceptable
  (mutable major pin, flagged by Phase 7 warning)
- Exceptions: `dtolnay/rust-toolchain@stable|nightly|beta`,
  `mymindstorm/setup-emsdk@vN` (no patch releases available)
- Avoid commit-SHA refs unless a workflow has an explicit unavoidable requirement

## Changelog Policy

Only add `CHANGELOG.md` entries for user-visible changes.

- Include: public API, behavior, protocol, feature flags, error-model, MSRV/dependency changes that affect consumers, and contributor-facing environment fixes that unblock using the repository.
- Exclude: internal-only updates such as CI/script/pre-commit automation, refactors, tests, and non-behavioral maintenance.

## Architecture — Core Modules

| File | Purpose |
|------|---------|
| `src/transport.rs` | `Transport` trait — async bidirectional text messages |
| `src/protocol.rs` | Wire-compatible protocol types (`ClientMessage`, `ServerMessage`, v3 `Topology`/`TransportKind`/`SessionPlanPayload`) |
| `src/signal.rs` | `PeerSignal` — typed, matchbox-compatible WebRTC signal (protocol v3) |
| `src/error_codes.rs` | `ErrorCode` enum — 50 variants from server |
| `src/error.rs` | `SignalFishError` error type |
| `src/event.rs` | `SignalFishEvent` high-level event stream |
| `src/client.rs` | `SignalFishClient` async client + `SignalFishConfig` + `JoinRoomParams` |
| `src/mesh.rs` | `MeshSession` v3 state tracker (feature: `mesh`) |
| `src/webrtc.rs` | `WebRtcDriver` seam + `MeshController` (feature: `mesh`) |
| `src/transports/websocket.rs` | WebSocket transport (feature: `transport-websocket`) |

### Transport Trait

```rust,ignore
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
    // 3. start() returns (client, events); Authenticate is sent automatically
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
Opt into the protocol v3 mesh with `.enable_mesh()` (advertises `protocol_version`/
`supported_transports`/`supported_topologies`); see `skills/webrtc-mesh-signaling.md`.

```rust,ignore
pub struct SignalFishConfig {
    pub app_id: String,
    pub sdk_version: Option<String>,          // defaults to crate version
    pub platform: Option<String>,             // e.g. "unity", "godot", "rust"
    pub game_data_format: Option<GameDataEncoding>,
    pub event_channel_capacity: usize,        // defaults to 256 (buffer before backpressure)
    pub command_channel_capacity: usize,      // defaults to 1024 (bounded send queue)
    pub shutdown_timeout: std::time::Duration, // defaults to 1 second
}

let config = SignalFishConfig::new("mb_app_abc123")
    .with_event_channel_capacity(512)
    .with_command_channel_capacity(2048)
    .with_shutdown_timeout(std::time::Duration::from_secs(5));
```

### JoinRoomParams

Builder for `client.join_room(...)`.

```rust,ignore
let params = JoinRoomParams::new("my-game", "Alice")
    .with_room_code("ABC123")   // omit for quick-match
    .with_max_players(4)
    .with_supports_authority(true);
client.join_room(params)?;
```

### Key Client Methods

All methods except `shutdown` and the `*_reliable` sends are synchronous (they queue a message on the bounded command channel, no round-trip):

```rust,ignore
client.join_room(params: JoinRoomParams) -> Result<()>
client.leave_room() -> Result<()>
client.send_game_data(data: serde_json::Value) -> Result<()>
client.send_game_data_reliable(data).await   // waits for queue space (pacing)
client.set_ready() -> Result<()>
client.start_game() -> Result<()>           // protocol v2: explicit game start
client.request_authority(become_authority: bool) -> Result<()>
client.provide_connection_info(info: ConnectionInfo) -> Result<()>
client.reconnect(player_id, room_id, auth_token) -> Result<()>
client.join_as_spectator(game_name, room_code, spectator_name) -> Result<()>
client.leave_spectator() -> Result<()>
client.ping() -> Result<()>
client.send_signal_reliable(to, signal).await // v3 only; waiting send_signal
client.send_capacity() / client.max_send_capacity() -> usize // queue diagnostics
client.stats() -> ClientStats  // cumulative game_data_sent/received counters
client.shutdown().await      // async, graceful
```

Sync sends return `SignalFishError::NotConnected` when the transport is closed
and `SignalFishError::SendBufferFull { capacity }` when the bounded queue is
full (message refused, never silently dropped). Events are also never dropped:
a full event channel pauses the transport loop (backpressure); events are
missed only on receiver drop, shutdown-timeout abort, or handle drop without
`shutdown()`.
`SignalFishPollingClient` shares the queue bound, capacity accessors, and `stats()`.

## Feature Flags

| Flag | Default | Description |
|------|---------|-------------|
| `transport-websocket` | on | Built-in WebSocket via `tokio-tungstenite` |
| `transport-websocket-emscripten` | off | Emscripten WebSocket transport; enables `polling-client` |
| `polling-client` | off | `SignalFishPollingClient` — sync, polling-based client for any `Transport` |
| `tokio-runtime` | off (on via `transport-websocket`) | Tokio `rt` + `time` features |
| `mesh` | off | Protocol v3 mesh: `MeshSession` tracker + `WebRtcDriver` seam + `MeshController` |

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

`tokio` (full features, for tests) and `tracing-subscriber` (test log output).

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

### Exhaustive Public Types

Public enums and protocol payload structs are exhaustive. `SignalFishEvent`,
`ErrorCode`, `SignalFishError`, and protocol payload types all require explicit
handling of their known variants. Adding variants to these enums is a semver
breaking change.

### No Heavy Dependencies

No `chrono` (timestamps remain `String` from the server), no `bytes` (binary
payloads are `Vec<u8>` with `serde_bytes`), no `reqwest` (HTTP is out of scope).

### UUID Convention

Player IDs and room IDs are `uuid::Uuid`, serialized as lowercase hyphenated
strings to match server expectations.

### Connection / Auth Flow

1. `SignalFishClient::start(transport, config)` queues `ClientMessage::Authenticate`
   immediately before spawning the transport loop.
2. Server responds with `ServerMessage::Authenticated` → `SignalFishEvent::Authenticated`.
3. Client may then call `join_room`, etc.
4. Both clients emit a synthetic `SignalFishEvent::Connected` once the transport
   is ready (`SignalFishClient`: at the start of the transport loop;
   `SignalFishPollingClient`: once `Transport::is_ready()` returns `true`).
   `SignalFishEvent::Disconnected` is emitted when the transport closes
   (best-effort; missed only if the receiver is dropped, shutdown times out,
   or the handle is dropped without `shutdown()`).

## Protocol Overview

Both `ClientMessage` and `ServerMessage` use adjacent tagging:

```json
{ "type": "JoinRoom", "data": { "game_name": "my-game", ... } }
{ "type": "RoomJoined", "data": { "room_id": "...", ... } }
```

Variant names are PascalCase in JSON (serde default for adjacently-tagged enums
with no `rename_all`). Protocol v3 adds the additive, opt-in mesh (the default
stays a byte-identical-to-v2 "relay floor"); WebRTC signals are externally tagged
(`{ "Offer": "..." }`). See `skills/serde-patterns.md` for the full wire format,
and `skills/protocol-versioning-and-negotiation.md` + `skills/webrtc-mesh-signaling.md`
for the v2/v3 deltas.

## `.llm/` Structure

- `.llm/context.md` -- this file (canonical source of truth)
- `.llm/skills/index.md` -- auto-regenerated skill index (do not edit); summarizes each skill
- `.llm/skills/*.md` -- focused reference guides for common tasks

## Documentation Rendering (MkDocs)

MkDocs Material with pymdownx extensions powers GitHub Pages. A build-time
hook (`hooks/rustdoc_codeblocks.py`) strips rustdoc code-fence annotations
(`rust,ignore`, `rust,no_run`, `rust,compile_fail`) so Pygments highlights
correctly. Mermaid diagrams require `custom_fences` in `mkdocs.yml`. CI runs
`mkdocs build --strict` (`.github/workflows/docs-deploy.yml`). See
`skills/markdown-and-doc-validation.md` for full guidance.

## Pre-commit Enforcement

A pre-commit hook enforces:

1. No `.llm/*.md` file exceeds 300 lines (`scripts/pre-commit-llm.py`)
2. `skills/index.md` is auto-regenerated from skill file headings
3. `cargo fmt --all -- --check` passes
4. `cargo clippy --all-targets --all-features -- -D warnings` passes
5. Workflow guard checks pass (`scripts/check-workflows.sh`): explicit step names, MSRV/toolchain policy, fenced-YAML step-key alignment
6. FFI safety check and its script tests pass (`scripts/check-ffi-safety.sh`)
7. Test quality check passes (`scripts/check-test-quality.sh`) — catches `&mut <literal>` temporaries
8. Devcontainer compatibility checks pass (`scripts/check-devcontainer-compat.sh`, plus a Dockerfile `docker buildx build --check` when buildx is available)
9. MkDocs admonition/details titles are well-formed (`scripts/check-admonitions.py`) — no embedded double quotes

`cargo test` runs on push, not every commit (too slow for a blocking hook) —
run it manually before opening a PR.

Install hooks with: `bash scripts/install-hooks.sh`
