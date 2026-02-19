# Claude AI — Repository Guidelines

## Project Identity

- **Company:** Ambiguous Interactive
- **Product:** Signal Fish Client SDK
- **Crate:** `signal-fish-client`

## Mandatory Workflow

```shell
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features
```

## Architecture

This is a transport-agnostic Rust client for the Signal Fish multiplayer signaling protocol.

- `src/transport.rs` — `Transport` trait (async bidirectional text messages)
- `src/protocol.rs` — Wire-compatible protocol types (`ClientMessage`, `ServerMessage`)
- `src/error_codes.rs` — `ErrorCode` enum (39 variants from server)
- `src/error.rs` — `SignalFishError` error type
- `src/event.rs` — `SignalFishEvent` high-level event stream
- `src/client.rs` — `SignalFishClient` async client implementation
- `src/transports/websocket.rs` — WebSocket transport (feature: `transport-websocket`)

## Key Decisions

- Transport-agnostic via `Transport` trait
- Wire-compatible with Signal Fish server v2 protocol
- `#[non_exhaustive]` on all public enums and payload structs
- Timestamps as `String` (no `chrono` dependency)
- Binary payloads as `Vec<u8>` (no `bytes` dependency)
- UUIDs for player/room IDs (matching server)
