# Signal Fish Client SDK — Project Context

## Identity

- **Company:** Ambiguous Interactive
- **Product:** Signal Fish Client SDK
- **Crate:** `signal-fish-client`
- **Version:** 0.8.0
- **Edition:** 2021
- **MSRV:** 1.87.0
- **License:** MIT
- **Repository:** <https://github.com/Ambiguous-Interactive/signal-fish-client-rust>
- **Guide (GitHub Pages):** <https://Ambiguous-Interactive.github.io/signal-fish-client-rust/>
- **API Docs (docs.rs):** <https://docs.rs/signal-fish-client>

## Purpose

Transport-agnostic Rust client for the Signal Fish multiplayer signaling protocol. Enables game clients to join rooms, exchange JSON or binary game data, and receive server-pushed events over any bidirectional frame transport.

## Mandatory Workflow

```shell
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features
```

Run this before every commit. All three steps must pass with zero warnings.

## Release Automation

Use the manual **Prepare Release** and **Release** workflows; see
`skills/release-recovery/SKILL.md` and `docs/releasing.md` for fail-closed recovery.

## CI/CD Action Reference Policy

Use `owner/action@vN.N.N` (preferred) or `@vN`, not commit hashes. Exceptions:
`dtolnay/rust-toolchain@stable|nightly|beta` and `mymindstorm/setup-emsdk@vN`.

## Changelog Policy

Only add `CHANGELOG.md` entries for user-visible changes.

- Include: public API, behavior, protocol, feature flags, error-model, MSRV/dependency changes that affect consumers, and contributor-facing environment fixes that unblock using the repository.
- Exclude: internal-only updates such as CI/script/pre-commit automation, refactors, tests, and non-behavioral maintenance.

## Architecture — Core Modules

| File | Purpose |
|------|---------|
| `src/transport.rs` | Object-safe polling `Transport` trait over text/binary `TransportFrame`s |
| `src/protocol.rs` | Wire-compatible protocol types, including v3 delivery/accountability and mesh |
| `src/protocol/binary.rs` | Strict physical MessagePack envelope decoders for v2/v3 binary game data |
| `src/accountability.rs` | Server-0.4.0-derived delivery-accountability state machine |
| `src/signal.rs` | `PeerSignal` — typed, matchbox-compatible WebRTC signal (protocol v3) |
| `src/error_codes.rs` | `ErrorCode` enum — 50 variants from server |
| `src/error.rs` | `SignalFishError` error type |
| `src/event.rs` | `SignalFishEvent` high-level event stream |
| `src/client_core.rs` | Shared command construction, decoding, accountability, state, events, and statistics |
| `src/client_api.rs` | Object-safe `SignalFishClientApi` common synchronous surface |
| `src/client.rs` | Thin async driver + `SignalFishConfig` + `JoinRoomParams` |
| `src/polling_client.rs` | Thin caller-driven polling transport driver (feature: `polling-client`) |
| `src/mesh.rs` | `MeshSession` v3 state tracker (feature: `mesh`) |
| `src/webrtc.rs` | `WebRtcDriver` seam + `MeshController` (feature: `mesh`) |
| `src/transports/websocket.rs` | WebSocket transport (feature: `transport-websocket`) |
| `src/transports/godot_websocket.rs` | Godot 4.5 native/web `WebSocketPeer` transport (feature: `transport-godot`) |

### Transport Trait

```rust,ignore
pub trait Transport {
    fn poll_send(&mut self, cx: &mut Context<'_>, frame: &mut Option<TransportFrame>)
        -> Poll<Result<(), SignalFishError>>;
    fn poll_recv(&mut self, cx: &mut Context<'_>)
        -> Poll<Option<Result<TransportFrame, SignalFishError>>>;
    fn poll_close(&mut self, cx: &mut Context<'_>)
        -> Poll<Result<(), SignalFishError>>;
    fn begin_poll_cycle(&mut self) {}
    fn abort(&mut self) {}
    fn diagnostics(&self) -> TransportDiagnostics { TransportDiagnostics::default() }
    fn is_ready(&self) -> bool { true }
    fn close_info(&self) -> Option<TransportCloseInfo> { None }
}
```

The trait itself has no `Send` bound, so main-thread transports work with the polling client. `SignalFishClient::start` separately requires
`Transport + Send + 'static`. A `poll_send` implementation may take the frame
only when the backend accepts ownership. That transfer is send completion for
admission purposes; it is not peer delivery and never requires the socket-wide
buffered byte count to reach zero. `Pending` before acceptance leaves the
caller's `Option` intact. Close polling is idempotent. See
`skills/transport-abstraction/SKILL.md`.

Godot defaults to adaptive outbound admission: a 50 ms latency target with a
4 KiB floor, 32 KiB ceiling, and a further native-capacity clamp. A successful Godot send
transfers ownership immediately; browser buffering is observed separately.
The blocking Godot workflow builds one official export and runs clean,
seeded-netem impaired, and 3,600-frame soak jobs through Signal Fish Server
0.4.0. It checksum-verifies and builds iproute2 6.6.0 for seeded netem rather
than relying on the runner's older `tc`. A 20-frame Fortress prediction window
leaves recovery headroom while scenario oracles still enforce eight-frame
clean and 12-frame impaired/soak lag bounds. The fixture uses a
peer-independent fixed 18 Hz simulation cadence that preserves elapsed
deadline debt and catches up by at most one frame per rendered callback, plus
a bounded causal relay hold, while the polling-hitch oracle requires forward
gameplay progress. These controls must prove
rollback/resimulation, bounded confirmation lag with zero waits/stalls, exact
state checksum convergence, drained queue age/depth with a non-positive final
eight-sample soak age slope, relay/server conservation, and v3 peer departure.

### Client Usage Pattern

Connect a transport, construct `SignalFishConfig`, and pass both to `SignalFishClient::start`, which returns the handle and event receiver and
queues `Authenticate`. Wait for `Authenticated` before room commands; drain
events continuously and call `shutdown().await` for graceful teardown. The
complete compiling example is `examples/basic_lobby.rs`.

### SignalFishConfig

Required second argument to `SignalFishClient::start`. Only `app_id` is required.
Opt into protocol v3 relay/accountability with `.enable_v3()`. Use
`.enable_mesh()` only when a WebRTC driver is present; it calls `enable_v3()`
and additionally advertises WebRTC mesh/host support.

```rust,ignore
pub struct SignalFishConfig {
    pub app_id: String,
    pub sdk_version: Option<String>,          // defaults to crate version
    pub platform: Option<String>,             // e.g. "unity", "godot", "rust"
    pub game_data_format: Option<GameDataEncoding>,
    pub event_channel_capacity: usize,        // defaults to 256 (buffer before backpressure)
    pub command_channel_capacity: usize,      // defaults to 1024 (bounded send queue)
    pub shutdown_timeout: std::time::Duration, // async shutdown / polling close deadline; 1s
    pub protocol_violation_policy: ProtocolViolationPolicy, // Quarantine
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

Common synchronous commands take `&mut self` through the object-safe
`SignalFishClientApi`. Driver-specific lifecycle stays concrete; both drivers
delegate protocol behavior and state to one `ClientCore`.

```rust,ignore
client.join_room(params: JoinRoomParams) -> Result<()>
client.leave_room() -> Result<()>
client.send_game_data(data: serde_json::Value) -> Result<()>
client.send_game_data_reliable(data).await   // waits for queue space (pacing)
client.send_game_data_with_delivery(data, GameDataDelivery::Latest { key: 7 })
client.send_binary_game_data(payload: Vec<u8>) -> Result<()> // v3 physical binary frame
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
client.snapshot() -> ClientSnapshot // coherent state/token/quarantine view
client.shutdown().await      // async, graceful
```

Sync sends return `SignalFishError::NotConnected` when the transport is closed
and `SignalFishError::SendBufferFull { capacity }` when the bounded queue is
full (message refused, never silently dropped). Events are never dropped either:
a full event channel pauses the transport loop (backpressure); undecodable
frames surface as `DecodeFailed` events; events are missed only on receiver
drop, handle drop without `shutdown()`, or shutdown (abandons ≤1 in-flight).
`SignalFishPollingClient` shares the classified/binary sends, queue bound,
capacity accessors, `stats()`, and coherent `snapshot()`. Its default per-poll
work budget is 64 frames/64 KiB in each direction, and its default close policy
abandons client-owned queued work. Adaptive backpressure and flush-on-close are
explicit opt-ins. Use `polling_stats()` for scheduling/queue diagnostics and
`queue_age_stats()` for sampled current/peak age of client-owned work; reset the
age peak after authentication/setup when measuring gameplay. Backend acceptance
ends queue age but is not peer delivery. Use `transport_diagnostics()` for
backend buffering/admission diagnostics.
Use the polling client's read-only `transport()` accessor for Godot's
zero-expected `admission_watermark_violations()` counter and the separately
accounted `one_frame_escape_bytes()` empty-buffer exception.

## Feature Flags

| Flag | Default | Description |
|------|---------|-------------|
| `transport-websocket` | on | Built-in WebSocket via `tokio-tungstenite` |
| `transport-websocket-emscripten` | off | Emscripten WebSocket transport; enables `polling-client` |
| `transport-godot` | off | Godot 4.5 `WebSocketPeer` transport for native/no-thread web exports; web GDExtensions use `api-custom`; enables `polling-client` |
| `polling-client` | off | `SignalFishPollingClient` — sync, polling-based client for any `Transport` |
| `tokio-runtime` | off (on via `transport-websocket`) | Tokio `rt` + `time` features |
| `mesh` | off | Protocol v3 mesh: `MeshSession` tracker + `WebRtcDriver` seam + `MeshController` |

## Dependencies

| Crate | Purpose |
|-------|---------|
| `tokio` | Async runtime (sync, macros, rt, time features) |
| `serde` + `serde_json` + `serde_bytes` | JSON serialization of protocol messages |
| `rmp` + `rmp-serde` | Strict protocol-v3 MessagePack envelope decoding |
| `uuid` | Player/room IDs matching server format |
| `thiserror` | Derive macro for `SignalFishError` |
| `tracing` | Structured logging and diagnostics |
| `tokio-tungstenite` | WebSocket transport (optional) |
| `futures-util` | Stream/sink utilities for WebSocket (optional) |
| `godot` | Godot 4.5 `WebSocketPeer` bindings for native/web transport (optional) |

`tokio` (full features, for tests) and `tracing-subscriber` (test log output).

## Key Design Decisions

### Transport Agnosticism

The `Transport` trait decouples protocol logic from network I/O. Tests use
in-memory `VecDeque`-backed transports. Production code uses WebSocket. Custom
transports (QUIC, raw TCP, engine WebSockets, etc.) implement three object-safe
polling methods and can preserve structured close metadata.

### Wire Compatibility

`ClientMessage` and `ServerMessage` use adjacently-tagged serde encoding
(`#[serde(tag = "type", content = "data")]`) to match the Signal Fish server
v2 JSON protocol. Never change serde attributes without verifying against
the server spec. See `skills/serde-patterns/SKILL.md` for details.

### Exhaustive Public Types

Public enums and protocol payload structs are exhaustive. `SignalFishEvent`,
`ErrorCode`, `SignalFishError`, and protocol payload types all require explicit
handling of their known variants. Adding variants to these enums is a semver
breaking change.

### Delivery Accountability

Negotiated v3 delivery carries per-sender epoch/sequence stamps. The SDK ports
the server 0.4.0 native reference state machine and validates snapshots,
lifecycle transitions, prior exact gap coverage, cumulative counters, terminal
and reconnect watermarks, and unsupported-format causality. Stale payloads are
suppressed. Violations emit `ProtocolViolation`; policy defaults to quarantine
until a new authoritative room/reconnect snapshot.

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
(`{ "Offer": "..." }`). See `skills/serde-patterns/SKILL.md` for the full wire format,
and `skills/protocol-versioning-and-negotiation/SKILL.md` + `skills/webrtc-mesh-signaling/SKILL.md`
for the v2/v3 deltas.

## `.llm/` Structure

- `.llm/context.md` -- this file (canonical source of truth)
- `.llm/skills/index.md` -- auto-generated human-readable skill catalog (do not edit)
- `.llm/skills/<name>/SKILL.md` -- focused Agent Skill with YAML `name` and
  trigger-focused `description` metadata
- `.llm/skills/<name>/{scripts,references,assets}/` -- optional resources loaded
  only when a skill needs them

Agents discover skills from frontmatter, then load the matching `SKILL.md` only
when its description applies. Resolve relative resource links from the skill's
directory. Skill directory names and frontmatter names must match and use
lowercase hyphen-case.

## Documentation Rendering (MkDocs)

MkDocs Material with pymdownx extensions powers GitHub Pages. A build-time
hook (`hooks/rustdoc_codeblocks.py`) strips rustdoc code-fence annotations
(`rust,ignore`, `rust,no_run`, `rust,compile_fail`) so Pygments highlights
correctly. Mermaid diagrams require `custom_fences` in `mkdocs.yml`. CI runs
`mkdocs build --strict` (`.github/workflows/docs-deploy.yml`). See
`skills/markdown-and-doc-validation/SKILL.md` for full guidance.

## Pre-commit Enforcement

A pre-commit hook enforces:

1. No `.llm/*.md` file exceeds 500 lines (`scripts/pre-commit-llm.py`)
2. Every skill uses `.llm/skills/<name>/SKILL.md`, valid YAML frontmatter, and
   a description that states what the skill does and when to activate it
3. `skills/index.md` is auto-regenerated from skill frontmatter and headings
4. `cargo fmt --all -- --check` passes
5. `cargo clippy --all-targets --all-features -- -D warnings` passes
6. Workflow guard checks pass (`scripts/check-workflows.sh`): explicit step names, MSRV/toolchain policy, fenced-YAML step-key alignment
7. FFI safety check and its script tests pass (`scripts/check-ffi-safety.sh`)
8. Test quality check passes (`scripts/check-test-quality.sh`) — catches `&mut <literal>` temporaries
9. Devcontainer compatibility checks pass (`scripts/check-devcontainer-compat.sh`, plus a Dockerfile `docker buildx build --check` when buildx is available)
10. MkDocs admonition/details titles are well-formed (`scripts/check-admonitions.py`) — no embedded double quotes

`cargo test` runs on push, not every commit (too slow for a blocking hook) —
run it manually before opening a PR.

Install hooks with: `bash scripts/install-hooks.sh`
