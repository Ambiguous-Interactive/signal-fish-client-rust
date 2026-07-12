# Migrating from 0.7 to 0.8

Version 0.8 unifies the async and polling clients behind one protocol state
machine and a common object-safe API. Wire behavior remains unchanged: the
default client still speaks the protocol-v2 relay floor, while v3 remains an
explicit opt-in.

## Mutable synchronous commands

Common synchronous commands now take `&mut self` on both drivers. Declare the
client binding mutable and pass a mutable reference to helpers:

```rust,ignore
// 0.7
fn join(client: &SignalFishClient) -> Result<(), SignalFishError> {
    client.join_room(JoinRoomParams::new("game", "Alice"))
}

// 0.8
fn join(client: &mut SignalFishClient) -> Result<(), SignalFishError> {
    client.join_room(JoinRoomParams::new("game", "Alice"))
}
```

The async waiting sends (`*_reliable`) still take `&self`; `shutdown` already
takes `&mut self`. Code that previously shared an async handle through `Arc`
must provide exclusive access for synchronous commands, for example with a
mutex, or route commands through an application-owned task.

`MeshController::join_room`, `set_ready`, `start_game`, and `leave_room` also
take `&mut self`. Use the new `MeshController::client_mut()` accessor for other
synchronous commands; `client()` remains available for read-only access.

## Driver-independent application logic

Use `SignalFishClientApi` when the same room and signaling logic should work
with either driver:

```rust,ignore
use signal_fish_client::{
    JoinRoomParams, SignalFishClientApi, SignalFishError,
};

fn enter_lobby(client: &mut dyn SignalFishClientApi) -> Result<(), SignalFishError> {
    client.join_room(JoinRoomParams::new("game", "Alice"))?;
    client.set_ready()
}
```

The trait contains synchronous commands, queue capacity, statistics, and
`snapshot()`. Driver-specific lifecycle stays on the concrete types:

- `SignalFishClient`: waiting sends and `shutdown().await`.
- `SignalFishPollingClient`: `poll()`, `close()`, and `is_closing()`.

The trait uses owned concrete arguments (`PeerSignal`, `String`, and
`serde_json::Value`) so it remains object-safe. The concrete clients retain
their ergonomic `impl Into` signal helpers.

## Transport and protocol changes

The frame-capable `Transport` boundary, binary game data, v3 delivery classes,
reconnection tokens, and accountability policy were introduced in the same
0.8 development cycle. See the [Transport](transport.md),
[Protocol Versioning](protocol-versioning.md), and
[Delivery Contract](delivery.md) guides for those migrations.
