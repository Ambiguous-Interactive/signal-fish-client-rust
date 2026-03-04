# Client API Reference

The core API surface of the Signal Fish Client SDK consists of three types:
[`SignalFishConfig`](#signalfishconfig) for connection settings,
[`JoinRoomParams`](#joinroomparams) for room entry, and
[`SignalFishClient`](#signalfishclient) — the async client handle itself.
For WebAssembly environments without an async runtime,
[`SignalFishPollingClient`](#signalfishpollingclient) provides a synchronous,
game-loop-driven alternative.

---

## `SignalFishConfig`

Configuration for a `SignalFishClient` connection. The only **required** field is `app_id`; all others have sensible defaults.

### Constructor

```rust,ignore
let config = SignalFishConfig::new("mb_app_abc123");
```

`new()` accepts any type that implements `Into<String>`.

### Fields

| Field | Type | Default | Description |
|---|---|---|---|
| `app_id` | `String` | *(required)* | Public App ID that identifies the game application. |
| `sdk_version` | `Option<String>` | Crate version at compile time | SDK version string sent during authentication. |
| `platform` | `Option<String>` | `None` | Platform identifier (e.g. `"unity"`, `"godot"`, `"rust"`). |
| `game_data_format` | `Option<GameDataEncoding>` | `None` | Preferred game data encoding format (`Json`, `MessagePack`, or `Rkyv`). |
| `event_channel_capacity` | `usize` | `256` | Capacity of the bounded event channel. Values below 1 are clamped to 1. |
| `shutdown_timeout` | `Duration` | `1 second` | Timeout for graceful shutdown of the background transport loop. A zero timeout aborts the loop immediately. |

### Builder Methods

All builder methods are `#[must_use]` — you must chain or assign the return value.

| Method | Parameter Type | Description |
|---|---|---|
| `.with_event_channel_capacity(n)` | `usize` | Set the bounded event channel capacity (default 256). |
| `.with_shutdown_timeout(d)` | `Duration` | Set the graceful shutdown timeout (default 1 second). |

### Full Example

```rust,ignore
use signal_fish_client::{SignalFishConfig, protocol::GameDataEncoding};
use std::time::Duration;

let config = SignalFishConfig::new("mb_app_abc123")
    .with_event_channel_capacity(512)
    .with_shutdown_timeout(Duration::from_secs(5));
```

Or using struct literal syntax with defaults:

```rust,ignore
use signal_fish_client::{SignalFishConfig, protocol::GameDataEncoding};

let config = SignalFishConfig {
    app_id: "mb_app_abc123".into(),
    sdk_version: Some("0.4.0".into()),
    platform: Some("rust".into()),
    game_data_format: Some(GameDataEncoding::Json),
    ..SignalFishConfig::new("mb_app_abc123")
};
```

---

## `JoinRoomParams`

Parameters for joining (or creating) a room, constructed via a builder pattern.
Only `game_name` and `player_name` are required. Leave `room_code` as `None`
for quick-match / auto-create behavior.

### Constructor

```rust,ignore
let params = JoinRoomParams::new("my-game", "Alice");
```

### Builder Methods

All builder methods are `#[must_use]` — you must chain or assign the return value.

| Method | Parameter Type | Description |
|---|---|---|
| `.with_room_code(code)` | `impl Into<String>` | Set an explicit room code to join. |
| `.with_max_players(n)` | `u8` | Set the maximum number of players allowed in the room. |
| `.with_supports_authority(flag)` | `bool` | Enable or disable authority delegation support. |
| `.with_relay_transport(transport)` | `RelayTransport` | Set the preferred relay transport protocol. |

### Full Example

```rust,ignore
use signal_fish_client::{JoinRoomParams, protocol::RelayTransport};

let params = JoinRoomParams::new("my-game", "Alice")
    .with_room_code("ABCD")
    .with_max_players(4)
    .with_supports_authority(true)
    .with_relay_transport(RelayTransport::Udp);
```

---

## `SignalFishClient`

Async client handle for the Signal Fish signaling protocol. Created via
[`SignalFishClient::start`](#creation), which spawns a background transport loop
and returns this handle together with an event receiver.

All command methods serialize a `ClientMessage` and queue it to the transport
loop over an unbounded channel — they return immediately without awaiting a
round-trip.

!!! info "Error convention"
    All `Result<()>` methods return `Err(SignalFishError::NotConnected)` when the
    transport is closed.

!!! info "ID types"
    `PlayerId` and `RoomId` are both type aliases for `uuid::Uuid`.

---

### Creation

#### `start`

Start the client transport loop and return a handle plus event receiver.

```rust,ignore
fn start(
    transport: impl Transport,
    config: SignalFishConfig,
) -> (Self, tokio::sync::mpsc::Receiver<SignalFishEvent>)
```

The return tuple is marked `#[must_use]` — you **must** consume the event
receiver to observe server events.

On start, the client automatically sends an `Authenticate` message using the
provided config. A background Tokio task is spawned to multiplex send/receive
on the transport.

```rust,ignore
use signal_fish_client::{
    SignalFishClient, SignalFishConfig, WebSocketTransport,
};

let transport = WebSocketTransport::connect("ws://localhost:3536/ws").await?;
let config = SignalFishConfig::new("mb_app_abc123");
let (client, mut event_rx) = SignalFishClient::start(transport, config);
```

!!! note
    `WebSocketTransport` requires the `transport-websocket` feature, which is
    enabled by default.

---

### Room Operations

#### `join_room`

Join or create a room with the given parameters.

```rust,ignore
fn join_room(&self, params: JoinRoomParams) -> Result<()>
```

```rust,ignore
client.join_room(
    JoinRoomParams::new("my-game", "Alice")
        .with_max_players(4),
)?;
```

Wait for `SignalFishEvent::RoomJoined` to confirm success.

---

#### `leave_room`

Leave the current room.

```rust,ignore
fn leave_room(&self) -> Result<()>
```

```rust,ignore
client.leave_room()?;
```

The server will broadcast a player-left event to remaining room members.

---

#### `set_ready`

Signal readiness to start the game in the lobby.

```rust,ignore
fn set_ready(&self) -> Result<()>
```

```rust,ignore
client.set_ready()?;
```

When all players in a room are ready, the server transitions the lobby state.

---

#### `join_as_spectator`

Join a room as a read-only spectator.

```rust,ignore
fn join_as_spectator(
    &self,
    game_name: String,
    room_code: String,
    spectator_name: String,
) -> Result<()>
```

```rust,ignore
client.join_as_spectator(
    "my-game".into(),
    "ABCD".into(),
    "Watcher".into(),
)?;
```

Spectators receive game events but cannot send game data or affect room state.

---

#### `leave_spectator`

Leave spectator mode.

```rust,ignore
fn leave_spectator(&self) -> Result<()>
```

```rust,ignore
client.leave_spectator()?;
```

---

### Game Data

#### `send_game_data`

Send arbitrary JSON game data to other players in the room.

```rust,ignore
fn send_game_data(&self, data: serde_json::Value) -> Result<()>
```

```rust,ignore
client.send_game_data(serde_json::json!({
    "action": "move",
    "x": 10,
    "y": 20,
}))?;
```

The data is forwarded to all other players (and spectators) in the room.

---

### Authority

#### `request_authority`

Request to become (or relinquish) the room authority.

```rust,ignore
fn request_authority(&self, become_authority: bool) -> Result<()>
```

```rust,ignore
// Claim authority
client.request_authority(true)?;

// Release authority
client.request_authority(false)?;
```

Authority delegation must be enabled when creating the room
(see `JoinRoomParams::with_supports_authority`).

---

### Connection Management

#### `provide_connection_info`

Provide P2P connection information to the server for relay/direct connection establishment.

```rust,ignore
fn provide_connection_info(
    &self,
    connection_info: ConnectionInfo,
) -> Result<()>
```

```rust,ignore
use signal_fish_client::protocol::ConnectionInfo;

client.provide_connection_info(ConnectionInfo::Direct {
    host: "192.168.1.10".into(),
    port: 7777,
})?;
```

The `ConnectionInfo` enum supports `Direct`, `UnityRelay`, `Relay`, `WebRTC`,
and `Custom` variants.

---

#### `reconnect`

Reconnect to a previous session after a disconnection.

```rust,ignore
fn reconnect(
    &self,
    player_id: PlayerId,
    room_id: RoomId,
    auth_token: String,
) -> Result<()>
```

```rust,ignore
client.reconnect(player_id, room_id, auth_token)?;
```

Use the `player_id` and `room_id` from the original `SignalFishEvent::RoomJoined`
event, along with the `auth_token` provided by your application server.

---

#### `ping`

Send a heartbeat ping to the server.

```rust,ignore
fn ping(&self) -> Result<()>
```

```rust,ignore
client.ping()?;
```

Useful for keeping the connection alive through proxies or load balancers.

---

### State Accessors

Synchronous accessors use atomics; async accessors acquire an internal mutex.

| Method | Signature | Description |
|---|---|---|
| `is_connected()` | `fn is_connected(&self) -> bool` | Returns `true` if the transport is believed to be connected. |
| `is_authenticated()` | `fn is_authenticated(&self) -> bool` | Returns `true` if the server has confirmed authentication. |
| `current_room_id()` | `async fn current_room_id(&self) -> Option<RoomId>` | Returns the current room ID, if in a room. |
| `current_player_id()` | `async fn current_player_id(&self) -> Option<PlayerId>` | Returns the current player ID, if assigned by the server. |
| `current_room_code()` | `async fn current_room_code(&self) -> Option<String>` | Returns the current room code, if in a room. |

```rust,ignore
if client.is_connected() && client.is_authenticated() {
    if let Some(room_id) = client.current_room_id().await {
        println!("In room: {room_id}");
    }
}
```

---

### Lifecycle

#### `shutdown`

Gracefully shut down the client.

```rust,ignore
async fn shutdown(&mut self)
```

```rust,ignore
client.shutdown().await;
```

Shutdown proceeds in three stages:

1. Sends a oneshot signal to the background transport loop.
2. Awaits the loop task with a configurable timeout (default **1 second**,
   set via [`SignalFishConfig::shutdown_timeout`](#signalfishconfig)).
3. If the timeout expires, the task is logged as unresponsive and aborted.
   The `Disconnected` event may not be delivered in this case.
4. Regardless of whether `Disconnected` is delivered, connection/session state
   is cleared (`is_connected() == false`, `is_authenticated() == false`, and
   room/player accessors return `None`).

!!! warning "Drop fallback"
    If `shutdown()` is never called, the `Drop` implementation **aborts** the
    background task immediately. Always prefer an explicit `shutdown().await` for
    a clean disconnect.

---

## `SignalFishPollingClient`

Synchronous, polling-based client for environments without an async runtime.
Originally created for WebAssembly targets (specifically
`wasm32-unknown-emscripten` and Godot 4.5 web exports via gdext), but usable
in any single-threaded context with any `Transport` implementation.

!!! note "Feature gate"
    `SignalFishPollingClient` requires the `polling-client` feature.
    This feature is automatically enabled by `transport-websocket-emscripten`.

Unlike `SignalFishClient`, the polling client does **not** spawn background
tasks. Instead, the caller drives the protocol by calling
[`poll()`](#poll) once per frame from the game loop. All state is owned
directly — no `Arc`, `Mutex`, or atomics.

---

### Creation

#### `new`

Create a new polling client with a connected transport and config.

```rust,ignore
fn new(transport: impl Transport, config: SignalFishConfig) -> Self
```

```rust,ignore
use signal_fish_client::{
    EmscriptenWebSocketTransport, SignalFishPollingClient, SignalFishConfig,
};

let transport = EmscriptenWebSocketTransport::connect("wss://server/ws")
    .expect("connection failed");
let config = SignalFishConfig::new("mb_app_abc123");
let mut client = SignalFishPollingClient::new(transport, config);
```

On construction, the client immediately queues an `Authenticate` message
(just like `SignalFishClient::start`). The message is flushed on the first
call to `poll()`.

---

### Game Loop Integration

#### `poll`

Drain incoming messages, flush outgoing commands, and return all events
generated this frame.

```rust,ignore
fn poll(&mut self) -> Vec<SignalFishEvent>
```

```rust,ignore
// In your game loop (_process in Godot, Update in Unity, etc.)
let events = client.poll();
for event in events {
    match event {
        SignalFishEvent::Authenticated { app_name, .. } => {
            // Safe to join a room now.
        }
        SignalFishEvent::RoomJoined { room_code, .. } => {
            // You are in the room.
        }
        _ => {}
    }
}
```

`poll()` performs three steps internally:

1. **Flush** — sends all queued outgoing messages via `transport.send()`.
2. **Drain** — calls `transport.recv()` in a loop (using a noop waker) until
   no more messages are buffered.
3. **Parse** — deserializes each received JSON message into a
   `ServerMessage`, updates internal state, and converts it to a
   `SignalFishEvent`.

!!! tip "Call frequency"
    Call `poll()` once per frame. It is designed to be cheap when idle
    (no messages buffered = no work done). Calling it more often than once
    per frame is harmless but unnecessary.

---

### Command Methods

All command methods are synchronous. They queue an outgoing message that is
flushed on the next `poll()` call. All return `Result<(), SignalFishError>`.

| Method | Description |
|---|---|
| `join_room(params: JoinRoomParams)` | Join or create a room. |
| `leave_room()` | Leave the current room. |
| `set_ready()` | Signal readiness in the lobby. |
| `send_game_data(data: serde_json::Value)` | Send arbitrary JSON game data. |
| `request_authority(become: bool)` | Request or release room authority. |
| `provide_connection_info(info: ConnectionInfo)` | Provide P2P connection information. |
| `reconnect(player_id, room_id, auth_token)` | Reconnect to a previous session. |
| `ping()` | Send a heartbeat ping. |
| `join_as_spectator(game, room, name)` | Join a room as a spectator. |
| `leave_spectator()` | Leave spectator mode. |

All methods return `Err(SignalFishError::NotConnected)` if the transport has
closed.

---

### State Accessors

All accessors are **synchronous** (no async, no mutex):

| Method | Returns | Description |
|---|---|---|
| `is_connected()` | `bool` | Whether the transport is believed connected. |
| `is_authenticated()` | `bool` | Whether the server confirmed authentication. |
| `current_player_id()` | `Option<PlayerId>` | Current player ID, if assigned. |
| `current_room_id()` | `Option<RoomId>` | Current room ID, if in a room. |
| `current_room_code()` | `Option<&str>` | Current room code, if in a room. |

!!! note "No async accessors"
    Unlike `SignalFishClient`, all `SignalFishPollingClient` accessors are
    plain `&self` methods — no `.await` needed. This is because the polling
    client is single-threaded and owns its state directly.

---

### Lifecycle

#### `close`

Gracefully shut down the transport.

```rust,ignore
fn close(&mut self)
```

```rust,ignore
client.close();
```

Calls `transport.close()` via a single noop-waker poll and clears session
state. If the transport's `close()` future returns `Pending`, the result is
silently discarded — only transports whose `close()` resolves to `Ready`
immediately are guaranteed a clean shutdown. The primary transport
(`EmscriptenWebSocketTransport`) always completes `close()` synchronously.
After calling `close()`, `is_connected()` returns `false` and all command
methods return `Err(SignalFishError::NotConnected)`.

!!! warning "No Drop fallback"
    Unlike `SignalFishClient`, the polling client does **not** abort a
    background task on drop (there is no background task). However, the
    underlying transport's `Drop` implementation will still clean up
    resources (e.g., `EmscriptenWebSocketTransport` calls
    `emscripten_websocket_close` and `emscripten_websocket_delete`).
