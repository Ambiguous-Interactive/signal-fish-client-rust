<p align="center">
  <img src="docs/assets/logo-banner.svg" alt="Signal Fish Client SDK" width="600">
</p>

<p align="center">
  <a href="https://Ambiguous-Interactive.github.io/signal-fish-client-rust/">
    <img src="https://img.shields.io/badge/docs-GitHub%20Pages-blue?logo=github" alt="Documentation">
  </a>
  <a href="https://crates.io/crates/signal-fish-client">
    <img src="https://img.shields.io/crates/v/signal-fish-client.svg" alt="Crates.io">
  </a>
  <a href="https://docs.rs/signal-fish-client">
    <img src="https://img.shields.io/docsrs/signal-fish-client" alt="docs.rs">
  </a>
  <a href="https://github.com/Ambiguous-Interactive/signal-fish-client-rust/actions/workflows/ci.yml">
    <img src="https://github.com/Ambiguous-Interactive/signal-fish-client-rust/actions/workflows/ci.yml/badge.svg" alt="CI">
  </a>
  <a href="https://doc.rust-lang.org/stable/releases.html#version-1870-2025-05-15">
    <img src="https://img.shields.io/badge/MSRV-1.87.0-blue.svg" alt="MSRV">
  </a>
  <a href="LICENSE">
    <img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT">
  </a>
</p>

Transport-agnostic async Rust client for the **Signal Fish** multiplayer signaling protocol. Connect to a Signal Fish server over any bidirectional transport, authenticate, join rooms, and receive strongly-typed events — all through a simple channel-based API.

---

> **🤖 AI Disclosure**
>
> This project was developed with **substantial AI assistance**. The protocol
> design and core technology concepts were created entirely by humans, but the
> vast majority of the code, documentation, and tests were written with the
> help of **Claude Opus 4.6** and **Codex 5.3**. Human oversight covered code
> review and architectural decisions, but day-to-day implementation was
> primarily AI-driven. This transparency is provided so users can make informed
> decisions about using this crate.

---

## Features

- **Transport-agnostic** — implement the `Transport` trait for any backend (WebSocket, TCP, QUIC, WebRTC data channels, etc.)
- **Wire-compatible** — protocol types are conformance-tested against the server's published wire samples and error-code registry; undecodable frames surface as a typed `DecodeFailed` event
- **Protocol support: v2 relay + server 0.4.0 v3** — opt-in v3 adds classified delivery, accountability, binary frames, reconnect tokens, graceful drain, and WebRTC mesh signaling; the default remains byte-identical to v2. Use `enable_v3()` for relay-only support or `enable_mesh()` with a WebRTC driver.
- **Feature-gated WebSocket transport** — the default `transport-websocket` feature provides a ready-to-use `WebSocketTransport`
- **Event-driven** — receive typed `SignalFishEvent`s via a Tokio MPSC channel
- **Structured errors** — typed client errors, server error codes, decode failures, and categorized protocol-accountability violations
- **Full protocol coverage** — typed v2/v3 messages and events, including strict physical MessagePack envelopes
- **No silent loss** — events are delivered with backpressure (never dropped), and the bounded send queue surfaces congestion as `SignalFishError::SendBufferFull` instead of buffering without bound; `send_game_data_reliable` / `send_signal_reliable` wait for capacity, and `stats()` counters make relay-path loss observable
- **Configurable** — tune event channel capacity, command queue capacity, shutdown timeout, and more via `SignalFishConfig` builder methods
- **WebAssembly ready** — compiles to `wasm32-unknown-unknown` and `wasm32-unknown-emscripten` with zero unsafe panics
- **Godot 4.5 native + web transport** — the `transport-godot` feature wraps Godot's own `WebSocketPeer`, including official no-thread web exports
- **Advanced Emscripten transport** — `transport-websocket-emscripten` remains available for custom hosts that explicitly link Emscripten's WebSocket library
- **Polling client** — `SignalFishPollingClient` drives the protocol from a game loop without an async runtime, ideal for frame-driven engines and wasm targets (e.g. Godot 4.5 web exports)

## Installation

```toml
[dependencies]
signal-fish-client = "0.8.0"
```

Without the built-in WebSocket transport (bring your own):

```toml
[dependencies]
signal-fish-client = { version = "0.8.0", default-features = false }
```

## Quick Start

```rust,no_run
use signal_fish_client::{
    WebSocketTransport, SignalFishClient, SignalFishConfig,
    JoinRoomParams, SignalFishEvent,
};

#[tokio::main]
async fn main() -> Result<(), signal_fish_client::SignalFishError> {
    // 1. Connect a WebSocket transport to the signaling server.
    let transport = WebSocketTransport::connect("ws://localhost:3536/ws").await?;

    // 2. Build a client config with your application ID.
    let config = SignalFishConfig::new("mb_app_abc123");

    // 3. Start the client — returns a handle and an event receiver.
    //    The client automatically sends Authenticate on start.
    let (mut client, mut event_rx) = SignalFishClient::start(transport, config);

    // 4. Process events — wait for Authenticated before joining a room.
    while let Some(event) = event_rx.recv().await {
        match event {
            SignalFishEvent::Authenticated { app_name, .. } => {
                println!("Authenticated as {app_name}");
                // Now it's safe to join a room.
                client.join_room(JoinRoomParams::new("my-game", "Alice"))?;
            }
            SignalFishEvent::RoomJoined { room_code, .. } => {
                println!("Joined room {room_code}");
            }
            SignalFishEvent::Disconnected { .. } => break,
            _ => {}
        }
    }

    // 5. Shut down gracefully.
    client.shutdown().await;
    Ok(())
}
```

## Feature Flags

| Feature               | Default | Description                                                             |
| --------------------- | ------- | ----------------------------------------------------------------------- |
| `transport-websocket` | **yes** | Built-in WebSocket transport via `tokio-tungstenite` and `futures-util` |
| `transport-godot` | no | Godot 4.5 `WebSocketPeer` transport for native and official web exports; enables `polling-client` |
| `transport-websocket-emscripten` | no | Emscripten WebSocket transport via raw FFI to `<emscripten/websocket.h>` |
| `tokio-runtime` | **yes** (via `transport-websocket`) | Tokio runtime integration (`rt`, `time`); disable for pure WASM targets |

## Architecture

| Module        | Purpose                                                           |
| ------------- | ----------------------------------------------------------------- |
| `client`      | `SignalFishClient` handle, `SignalFishConfig`, `JoinRoomParams`   |
| `event`       | Typed application, transport, delivery, and violation events      |
| `protocol`    | Wire-compatible v2/v3 client and server message types             |
| `error`       | `SignalFishError` unified client/transport error type             |
| `error_codes` | Typed server error-code registry                                  |
| `transport`   | `Transport` trait for pluggable backends                          |
| `transports`  | Built-in Tokio, Godot, and advanced Emscripten WebSocket transports |
| `polling_client` | `SignalFishPollingClient` — synchronous, game-loop-driven client |
| `mesh`        | `MeshSession` — zero-dep v3 mesh state tracker (`mesh` feature)    |
| `webrtc`      | `WebRtcDriver` seam + `MeshController` v3 orchestrator (`mesh` feature) |

## Examples

### Basic Lobby

Full lifecycle: connect, authenticate, join a room, handle events, and shut down gracefully with Ctrl+C support.

```sh
cargo run --example basic_lobby

# Override the server URL:
SIGNAL_FISH_URL=ws://my-server:3536/ws cargo run --example basic_lobby
```

See [`examples/basic_lobby.rs`](examples/basic_lobby.rs).

### Custom Transport

Implement a channel-based loopback transport, wire it into the client, and verify events flow correctly — no network required.

```sh
cargo run --example custom_transport
```

See [`examples/custom_transport.rs`](examples/custom_transport.rs).

### WebAssembly

The SDK compiles to WebAssembly. See the [WebAssembly Guide](docs/wasm.md) for Godot gdext integration examples and build instructions.

## Custom Transport

Implement the `Transport` trait to plug in any I/O backend:

```rust,ignore
use std::task::{Context, Poll};
use signal_fish_client::transport::TransportFrame;
use signal_fish_client::{SignalFishError, Transport};

struct MyTransport { /* … */ }

impl Transport for MyTransport {
    fn poll_send(
        &mut self,
        cx: &mut Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> Poll<Result<(), SignalFishError>> {
        // Leave `frame` in place until accepted. If you take it and return
        // Pending, retain that exact send internally until it completes.
        todo!()
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<TransportFrame, SignalFishError>>> {
        todo!()
    }

    fn poll_close(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), SignalFishError>> {
        // Drive one idempotent close handshake across as many polls as needed.
        todo!()
    }
}
```

Key requirements:

- Preserve both `TransportFrame::Text` and `TransportFrame::Binary` boundaries
- Retain accepted outbound frames and partial receives across `Poll::Pending`
- Register the supplied waker when async progress becomes possible
- Make `poll_close` idempotent and expose structured peer metadata via `close_info`
- Connection setup happens *before* constructing the transport — the trait only covers message I/O
- The trait has no `Send` bound; only `SignalFishClient::start` requires
  `Send + 'static`, while `SignalFishPollingClient` accepts non-`Send` transports

## WebAssembly Support

The SDK supports two WASM targets:

| Target | Use Case | Transport | Client |
| --- | --- | --- | --- |
| `wasm32-unknown-unknown` | Browser apps (wasm-pack, wasm-bindgen) | Bring your own | `SignalFishPollingClient` (with `polling-client` feature) |
| `wasm32-unknown-emscripten` | Godot native/web exports | `GodotWebSocketTransport`; Emscripten transport only with custom link-enabled templates | `SignalFishPollingClient` |

The async `SignalFishClient` needs a *driven* tokio runtime (its transport loop runs under `tokio::spawn`); manually "ticking" a runtime once per frame starves it. Frame-driven or single-threaded environments — game loops on native as well as wasm — should use `SignalFishPollingClient` (feature `polling-client`), a synchronous pump you call once per frame.

### Godot 4.5 Quick Start

`GodotWebSocketTransport` uses Godot's own `WebSocketPeer`, so the same Rust
code works in native builds and official no-thread web export templates.

```rust,ignore
use signal_fish_client::{
    GodotWebSocketTransport, SignalFishPollingClient,
    SignalFishConfig, JoinRoomParams, SignalFishEvent,
};

// 1. Connect (synchronous — no .await needed).
let transport = GodotWebSocketTransport::connect("wss://server/ws")
    .expect("WebSocket creation failed");

// 2. Create the polling client (auto-sends Authenticate).
let config = SignalFishConfig::new("mb_app_abc123");
let mut client = SignalFishPollingClient::new(transport, config);

// Reset after authentication/setup, then monitor both queue depth and age.
client.reset_queue_age_peak();
let queue_age = client.queue_age_stats();
assert!(queue_age.current_oldest_queue_age <= queue_age.peak_oldest_queue_age);

// Optional: tune the bounded per-frame work and flush queued commands on close
// with SignalFishPollingClient::new_with_options(...).
// Godot admission defaults to an adaptive 50 ms target in the 4-32 KiB range,
// further limited by the native backend; use connect_with_options to override it.

// 3. Each frame, poll and handle events.
for event in client.poll() {
    match event {
        SignalFishEvent::Authenticated { app_name, .. } => {
            println!("Authenticated as {app_name}");
            client.join_room(JoinRoomParams::new("my-game", "Alice")).ok();
        }
        SignalFishEvent::RoomJoined { room_code, .. } => {
            println!("Joined room {room_code}");
        }
        _ => {}
    }
}
```

Enable it in a Godot GDExtension crate with:

```toml
godot = { version = "0.4.5", features = ["api-custom", "experimental-wasm", "experimental-wasm-nothreads", "lazy-function-tables"] }
signal-fish-client = { version = "0.8.0", default-features = false, features = ["transport-godot"] }
```

The custom Godot API binding is required for the 32-bit Emscripten ABI. Set
`GODOT4_BIN` to the Godot 4.5 editor when compiling the web extension.

For rollback games, the [Godot + Fortress guide](docs/fortress.md) documents
the bounded binary relay and exact frame order exercised by the real
two-process browser test in CI.

`EmscriptenWebSocketTransport` is retained for advanced custom-template
integrations. It requires the final host to link Emscripten's WebSocket
JavaScript library; official Godot templates do not.

### Building the Advanced Emscripten Transport

```sh
# Install prerequisites
rustup toolchain install nightly
rustup component add rust-src --toolchain nightly

# Build
cargo +nightly build -Zbuild-std \
    --target wasm32-unknown-emscripten \
    --no-default-features \
    --features transport-websocket-emscripten
```

See the [WebAssembly Guide](docs/wasm.md) for the full reference including Godot integration examples and toolchain setup.

## Development

### Run CI Locally

A unified script runs all CI checks locally:

```sh
# Run all checks (matches CI exactly)
bash scripts/check-all.sh

# Quick mode: fmt + clippy + test only
bash scripts/check-all.sh --quick
```

### Mandatory baseline

```sh
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features
```

### Additional quality checks

| Command | CI Workflow | Install |
| ------- | ----------- | ------- |
| `cargo deny check` | ci.yml | `cargo install cargo-deny` |
| `cargo audit` | security-supply-chain.yml | `cargo install cargo-audit` |
| `bash scripts/check-no-panics.sh` | no-panics.yml | (built-in) |
| `typos` | ci.yml | `cargo install typos-cli` |
| `markdownlint-cli2 "**/*.md"` | docs-validation.yml | `npm install -g markdownlint-cli2` |
| `lychee --config .lychee.toml "**/*.md"` | docs-validation.yml | `cargo install lychee` |
| `cargo machete` | unused-deps.yml | `cargo install cargo-machete` |
| `cargo semver-checks check-release` | semver-checks.yml | `cargo install cargo-semver-checks` |
| `bash scripts/check-workflows.sh` | workflow-lint.yml | (built-in) |
| `cargo +nightly miri test --test protocol_tests` | deep-safety.yml | `rustup component add miri --toolchain nightly` |
| `cd fuzz && cargo +nightly fuzz run ...` | deep-safety.yml | `cargo install cargo-fuzz` |
| `cargo mutants --file src/protocol.rs ...` | deep-safety.yml | `cargo install cargo-mutants` |
| `cargo llvm-cov --all-features --summary-only` | coverage.yml | `cargo install cargo-llvm-cov` + `rustup component add llvm-tools-preview` |

Release operators should follow the [release runbook](docs/releasing.md) for
the reviewed preparation workflow, protected crates.io publication, and
fail-closed recovery procedure.

## Minimum Supported Rust Version (MSRV)

<!-- markdownlint-disable-next-line MD036 -->
**1.87.0**

Tested against the latest stable Rust and the declared MSRV. Bumping the MSRV is considered a minor version change.

## License

[MIT](LICENSE) — Copyright (c) 2025-2026 Ambiguous Interactive

---

📖 **[Full guide on GitHub Pages](https://Ambiguous-Interactive.github.io/signal-fish-client-rust/)** | 📚 **[API reference on docs.rs](https://docs.rs/signal-fish-client)**
