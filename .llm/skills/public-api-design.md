# Public API Design

Reference for #[non_exhaustive], semver, re-exports, feature flags, and MSRV in this crate.

## #[non_exhaustive]

No public types in this crate carry `#[non_exhaustive]`. Every public enum and
struct is exhaustive. This includes:

- `SignalFishEvent` — exhaustive; adding variants is a semver breaking change
- `ErrorCode` — exhaustive; adding variants is a semver breaking change
- `SignalFishError` — exhaustive; variant set is controlled by this crate
- `ClientMessage` / `ServerMessage` — wire protocol types; exhaustive
- `SignalFishConfig`, `JoinRoomParams` — config structs; exhaustive
- Protocol payload structs (`RoomJoinedPayload`, etc.) — exhaustive

### Consumer Impact

Because all enums are exhaustive, consumers matching `SignalFishEvent` or
`ErrorCode` do NOT need a wildcard arm from the compiler's perspective, but
adding one is good practice for forward compatibility at the cost of silencing
exhaustiveness checks:

```rust
match event {
    SignalFishEvent::RoomJoined { room_code, .. } => { /* ... */ }
    SignalFishEvent::Disconnected { reason } => { /* ... */ }
    // Adding a wildcard arm here is valid but means the compiler will not
    // warn if a new variant is added and left unhandled.
    _ => {}
}
```

Adding any variant to `SignalFishEvent`, `ErrorCode`, or `SignalFishError`
requires a MINOR version bump (breaking change under semver for 0.x crates).

## Public Re-exports

From `src/lib.rs` — these are the primary import surface:

```rust
// Crate root re-exports
pub use client::{JoinRoomParams, SignalFishClient, SignalFishConfig};
pub use error::SignalFishError;
pub use error_codes::ErrorCode;
pub use event::SignalFishEvent;
pub use protocol::{ClientMessage, ServerMessage};
pub use transport::Transport;

// Feature-gated
#[cfg(feature = "transport-websocket")]
pub use transports::WebSocketTransport;
```

Users can write:

```rust
use signal_fish_client::{
    SignalFishClient, SignalFishConfig, JoinRoomParams,
    SignalFishEvent, SignalFishError, Transport,
};
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

Current MSRV: **Rust 1.85.0**

```toml
[package]
rust-version = "1.85.0"
```

### MSRV Note on AFIT

Rust 1.75 stabilized async fn in traits (AFIT), but `async-trait` is still
used in this crate for:

- Object safety — AFIT methods are not object-safe without `dyn*` or boxing
- The `Transport` trait uses `async-trait` so that `Box<dyn Transport>` works

### Testing MSRV in CI

```yaml
- name: Test on MSRV
  uses: dtolnay/rust-toolchain@master
  with:
    toolchain: "1.85.0"
- run: cargo test --all-features
```

## Semver and Versioning

This crate is pre-1.0 (0.1.0). Under semver:

- `0.MINOR.PATCH` — MINOR bumps are breaking changes
- `0.1.PATCH` — patches only for bug fixes

### Breaking vs Non-Breaking

| Change | Breaking? |
|--------|-----------|
| Add variant to `SignalFishEvent` (not non-exhaustive) | Yes |
| Add variant to `SignalFishError` (not non-exhaustive) | Yes |
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
pub fn join_room(&self, params: JoinRoomParams) -> Result<()> {
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

`async-trait` makes this work by boxing the returned futures. Without
`async-trait`, `async fn` in traits is not object-safe.
