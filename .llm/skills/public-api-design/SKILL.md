---
name: public-api-design
description: Design stable public Rust APIs for Signal Fish. Use when changing exported types, exhaustive enums, re-exports, feature flags, MSRV, naming, trait surfaces, or semver-sensitive behavior.
---

# Public API Design

Reference for enum matching policy, semver, re-exports, feature flags, and MSRV in this crate.

## Exhaustive Matching Policy

Public enums in this crate must be matched explicitly. Every public enum and
struct is exhaustive. This includes:

- `SignalFishEvent` — exhaustive; adding variants is a semver breaking change
- `ErrorCode` — exhaustive; adding variants is a semver breaking change
- `SignalFishError` — exhaustive; variant set is controlled by this crate
- `ClientMessage` / `ServerMessage` — wire protocol types; exhaustive
- `SignalFishConfig`, `JoinRoomParams`, `ClientSnapshot` — exhaustive structs
- `GameDataDelivery`, `ProtocolViolationPolicy`, `ProtocolViolationKind` — exhaustive
- Protocol payload structs (`RoomJoinedPayload`, etc.) — exhaustive

### Consumer Impact

Because enums are exhaustive, consumers should avoid wildcard arms when matching
`SignalFishEvent` or `ErrorCode`. Wildcard arms suppress compile-time checks
for newly added variants:

```rust
match event {
    SignalFishEvent::RoomJoined { room_code, .. } => { /* ... */ }
    SignalFishEvent::Disconnected { reason, .. } => { /* ... */ }
    // Avoid wildcard arms in enum matches so missing variants are compile
    // errors during upgrades.
    SignalFishEvent::Connected => {}
    // ...handle remaining variants explicitly...
}
```

Adding any variant to `SignalFishEvent`, `ErrorCode`, or `SignalFishError`
requires a MINOR version bump (breaking change under semver for 0.x crates).

## Public Re-exports

From `src/lib.rs` — these are the primary import surface:

```rust
// Crate root re-exports
pub use client::{
    ClientSnapshot, GameDataDelivery, JoinRoomParams, ProtocolViolationPolicy,
    SignalFishClient, SignalFishConfig,
};
pub use error::SignalFishError;
pub use error_codes::ErrorCode;
pub use event::{ProtocolViolationKind, SignalFishEvent};
pub use protocol::{ClientMessage, DeliveryClass, ServerMessage};
pub use transport::Transport;

// Feature-gated
#[cfg(feature = "transport-websocket")]
pub use transports::WebSocketTransport;
```

Godot types are intentionally absent from core. The lockstep
`signal-fish-client-godot` crate owns and exports `GodotWebSocketTransport`,
`GodotWebSocketOptions`, and `GodotBackpressurePolicy`. Never add a Godot
feature, module, dependency, re-export, or godot-rust type to the core public
surface.

Users can write:

```rust
use signal_fish_client::{
    SignalFishClient, SignalFishConfig, JoinRoomParams,
    GameDataDelivery, SignalFishEvent, SignalFishError, Transport,
};
use signal_fish_client::transport::{TransportCloseInfo, TransportFrame};
```

## Feature Flags

```toml
[features]
default = ["transport-websocket"]
transport-websocket = ["dep:tokio-tungstenite", "dep:futures-util"]
```

### Guarding Feature-Gated Code

```rust
// In source files
#[cfg(feature = "transport-websocket")]
pub mod websocket;

// In tests
#[cfg(feature = "transport-websocket")]
#[tokio::test]
async fn test_websocket_transport() { /* ... */ }
```

### Preventing Dead Code Warnings with Feature-Gated Constructors

When a `#[cfg(feature = "X")]` gate is applied to a constructor but **not**
to the struct definition or its `impl` blocks, `dead_code` warnings appear
when the feature is disabled — the struct and its fields exist but nothing
constructs them.

**The fix:** apply `#[cfg_attr(not(feature = "X"), allow(dead_code))]` to
every item that is only reachable through the gated constructor.

```rust,ignore
// The constructor is only compiled with the feature enabled:
#[cfg(feature = "transport-websocket")]
impl WebSocketState {
    pub fn new(url: &str) -> Self { /* ... */ }
}

// The struct and its fields must suppress dead_code when the feature
// is off, because no constructor exists to create them:
#[cfg_attr(not(feature = "transport-websocket"), allow(dead_code))]
pub(crate) struct WebSocketState {
    url: String,
    connected: bool,
}

// Any inherent impl block used only through the gated constructor
// also needs the attribute:
#[cfg_attr(not(feature = "transport-websocket"), allow(dead_code))]
impl WebSocketState {
    fn internal_helper(&self) { /* ... */ }
}
```

**Checklist when adding `#[cfg(feature = "...")]` to a constructor:**

- [ ] Is the struct itself gated? If not, add `#[cfg_attr(not(feature = "..."), allow(dead_code))]` to the struct.
- [ ] Are there non-gated `impl` blocks for the struct? Add the same `cfg_attr` to each one.
- [ ] Are there helper functions only called from the gated constructor? Gate or annotate those too.
- [ ] Run `cargo clippy --all-targets` **without** the feature to confirm zero warnings.

### Documenting Ungated Import Coupling

When a struct uses types from a dependency that is always available (e.g.,
`tokio::sync::Mutex`) but the struct's constructor is feature-gated, the import
creates a coupling that prevents future refactoring. **Document this coupling
with a comment above the import** explaining:

1. Why the import is not feature-gated
2. That the types are dead code without the feature (suppressed by `cfg_attr`)
3. What would need to change if the coupling needs to be broken

```rust
// tokio/sync is always available (not gated on `tokio-runtime`) because the
// struct uses `mpsc` and `Mutex` unconditionally. Without `tokio-runtime`,
// these types are dead code — suppressed by cfg_attr. If a future refactoring
// needs a different sync primitive, this import and struct fields need gating.
use tokio::sync::{mpsc, Mutex};
```

### docs.rs Configuration

```toml
[package.metadata.docs.rs]
all-features = true
rustdoc-args = ["--cfg", "docsrs"]
```

```rust
#[cfg_attr(docsrs, doc(cfg(feature = "transport-websocket")))]
pub struct WebSocketTransport { /* ... */ }
```

## MSRV Policy

Current MSRV: **Rust 1.87.0**

```toml
[package]
rust-version = "1.87.0"
```

The Godot adapter has a separate Rust 1.94.0 MSRV. Keep these compiler floors
independent: a godot-rust upgrade must not raise core's MSRV.

### Transport and async bounds

`Transport` uses object-safe `poll_send`/`poll_recv`/`poll_close` methods and
has no trait-level `Send` bound. `SignalFishClient::start` adds
`T: Transport + Send + 'static` where `tokio::spawn` requires it;
`SignalFishPollingClient<T>` accepts non-`Send` main-thread transports.

### Testing MSRV in CI

```yaml
- { name: Test on MSRV, uses: dtolnay/rust-toolchain@stable, with: { toolchain: "1.87.0" } }
- run: cargo test --workspace --all-features
```

## Semver and Versioning

This crate is pre-1.0 (0.1.0). Under semver:

- `0.MINOR.PATCH` — MINOR bumps are breaking changes
- `0.1.PATCH` — patches only for bug fixes

### Breaking vs Non-Breaking

| Change | Breaking? |
|--------|-----------|
| Add variant to `SignalFishEvent` | Yes |
| Add variant to `SignalFishError` | Yes |
| Remove public item | Yes |
| Change public function signature | Yes |
| Add required method to `Transport` trait | Yes |
| Change `#[serde(rename)]` or tag values | Yes (wire protocol) |

## Documenting Public API

Every public item should have a doc comment:

```rust
/// Join or create a room with the given parameters.
///
/// # Errors
///
/// Returns [`SignalFishError::NotConnected`] if the transport has closed.
pub fn join_room(&mut self, params: JoinRoomParams) -> Result<()> {
    // ...
}
```

### Doc test conventions

- Use `no_run` for examples requiring network access
- Use `ignore` for examples that can't compile standalone (e.g., in `client.rs`)
- Use `# use ...` to hide boilerplate imports

## Trait Object Safety

The `Transport` trait is object-safe, allowing `Box<dyn Transport>`:

```rust
let transport: Box<dyn Transport> = Box::new(my_transport);
let (client, events) = SignalFishClient::start(transport, config);
```

The polling methods are directly object-safe. `poll_send` receives an
`Option<TransportFrame>` slot: once an implementation takes a frame, it owns and
preserves it across `Pending`. `poll_close` is idempotent, and structured peer
close details come from `Transport::close_info()`.

## Protocol v2/v3 Additions

- **`TransportKind` vs the `Transport` trait.** The protocol data-path enum is
  named `TransportKind` (a wire *value*: `relay`/`direct`/`webrtc`) so it never
  collides with the `Transport` I/O *trait* (the byte channel to the server).
  Both are public and documented to cross-reference each other; never rename the
  trait, and never name the enum `Transport`.
- **Current v3 additions are exhaustive.** Delivery accountability adds
  `DeliveryClass`, gap/counter/report types, `ReplayStatus`, `SenderWatermark`,
  and `MessageTransport`; `ServerMessage`/`SignalFishEvent` add stamped game
  data, `DeliveryReport`, `RelayStats`, and `GoingAway`. `ErrorCode` adds
  `ServerDraining` and `InvalidDeliveryClass`.
- **Client policy/state API.** `GameDataDelivery` makes class/key combinations
  valid by construction; `ProtocolViolationPolicy` defaults to `Quarantine`;
  `ProtocolViolationKind` categorizes the emitted event; `ClientSnapshot`
  synchronously exposes negotiated/session/token/quarantine state on both
  drivers. New exhaustive variants/fields require a MINOR bump for this 0.x crate.
- **The mesh surface is feature-gated** behind `mesh` (and the async controller
  additionally behind `tokio-runtime`), keeping the default build minimal. See
  [webrtc-mesh-signaling](../webrtc-mesh-signaling/SKILL.md).

## Send-Path Conventions (0.6.0)

- **Fail-fast / waiting pairs.** The sync send methods queue on a bounded
  channel and fail fast with `SignalFishError::SendBufferFull { capacity }`
  when full; each high-rate send has an async waiting counterpart named
  `*_reliable` (`send_game_data_reliable`, `send_signal_reliable`). Follow
  this naming and pairing for any future send-style API: congestion must
  surface as an error or as waiting — never as a silent drop or an unbounded
  backlog.
- **Diagnostics are plain accessors.** `send_capacity()`,
  `max_send_capacity()`, `stats()`, and `snapshot()` return plain data on both
  clients; keep the common APIs mirrored. Polling-only scheduling telemetry
  such as `PollingQueueAgeStats` stays on the concrete polling client and is
  exported from the crate root.
- `GameDataDelivery::Reliable` preserves the v2 wire by omitting class/key;
  `Latest { key }` and `Volatile` require negotiated v3.
- Adding `SendBufferFull` to the exhaustive `SignalFishError` is breaking
  (MINOR for 0.x) — the `0.5.0 → 0.6.0` bump.
