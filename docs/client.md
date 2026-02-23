# Client API Reference

The core API surface of the Signal Fish Client SDK consists of three types:
[`SignalFishConfig`](#signalfishconfig) for connection settings,
[`JoinRoomParams`](#joinroomparams) for room entry, and
[`SignalFishClient`](#signalfishclient) — the async client handle itself.

---

## `SignalFishConfig`

Configuration for a `SignalFishClient` connection. The only **required** field is `app_id`; all others have sensible defaults.

### Constructor

```rust
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
| `shutdown_timeout` | `Duration` | `1 second` | Timeout for the graceful shutdown of the background transport loop. A zero timeout abandons the loop immediately. |

### Builder Methods

All builder methods are `#[must_use]` — you must chain or assign the return value.

| Method | Parameter Type | Description |
|---|---|---|
| `.with_event_channel_capacity(n)` | `usize` | Set the bounded event channel capacity (default 256). |
| `.with_shutdown_timeout(d)` | `Duration` | Set the graceful shutdown timeout (default 1 second). |

### Full Example

```rust
use signal_fish_client::{SignalFishConfig, protocol::GameDataEncoding};
use std::time::Duration;

let config = SignalFishConfig::new("mb_app_abc123")
    .with_event_channel_capacity(512)
    .with_shutdown_timeout(Duration::from_secs(5));
```

Or using struct literal syntax with defaults:

```rust
use signal_fish_client::{SignalFishConfig, protocol::GameDataEncoding};

let config = SignalFishConfig {
    app_id: "mb_app_abc123".into(),
    sdk_version: Some("0.1.0".into()),
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

```rust
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

```rust
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

```rust
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

```rust
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

```rust
fn join_room(&self, params: JoinRoomParams) -> Result<()>
```

```rust
client.join_room(
    JoinRoomParams::new("my-game", "Alice")
        .with_max_players(4),
)?;
```

Wait for `SignalFishEvent::RoomJoined` to confirm success.

---

#### `leave_room`

Leave the current room.

```rust
fn leave_room(&self) -> Result<()>
```

```rust
client.leave_room()?;
```

The server will broadcast a player-left event to remaining room members.

---

#### `set_ready`

Signal readiness to start the game in the lobby.

```rust
fn set_ready(&self) -> Result<()>
```

```rust
client.set_ready()?;
```

When all players in a room are ready, the server transitions the lobby state.

---

#### `join_as_spectator`

Join a room as a read-only spectator.

```rust
fn join_as_spectator(
    &self,
    game_name: String,
    room_code: String,
    spectator_name: String,
) -> Result<()>
```

```rust
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

```rust
fn leave_spectator(&self) -> Result<()>
```

```rust
client.leave_spectator()?;
```

---

### Game Data

#### `send_game_data`

Send arbitrary JSON game data to other players in the room.

```rust
fn send_game_data(&self, data: serde_json::Value) -> Result<()>
```

```rust
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

```rust
fn request_authority(&self, become_authority: bool) -> Result<()>
```

```rust
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

```rust
fn provide_connection_info(
    &self,
    connection_info: ConnectionInfo,
) -> Result<()>
```

```rust
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

```rust
fn reconnect(
    &self,
    player_id: PlayerId,
    room_id: RoomId,
    auth_token: String,
) -> Result<()>
```

```rust
client.reconnect(player_id, room_id, auth_token)?;
```

Use the `player_id` and `room_id` from the original `SignalFishEvent::RoomJoined`
event, along with the `auth_token` provided by your application server.

---

#### `ping`

Send a heartbeat ping to the server.

```rust
fn ping(&self) -> Result<()>
```

```rust
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

```rust
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

```rust
async fn shutdown(&mut self)
```

```rust
client.shutdown().await;
```

Shutdown proceeds in three stages:

1. Sends a oneshot signal to the background transport loop.
2. Awaits the loop task with a configurable timeout (default **1 second**,
   set via [`SignalFishConfig::shutdown_timeout`](#signalfishconfig)).
3. If the timeout expires, the task is logged as unresponsive and abandoned
   (it continues running to completion in the background).

!!! warning "Drop fallback"
    If `shutdown()` is never called, the `Drop` implementation **aborts** the
    background task immediately. Always prefer an explicit `shutdown().await` for
    a clean disconnect.
