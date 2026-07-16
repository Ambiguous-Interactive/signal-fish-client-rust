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
| `event_channel_capacity` | `usize` | `256` | Capacity of the bounded event channel. Events are never dropped on overflow — a full channel pauses the transport loop (backpressure), so this only controls buffering before backpressure kicks in. Values below 1 are clamped to 1. |
| `command_channel_capacity` | `usize` | `1024` | Capacity of the bounded outgoing command queue. When full, the synchronous send methods fail fast with [`SignalFishError::SendBufferFull`](errors.md#handling-sendbufferfull); the `*_reliable` variants wait for a slot instead. Values below 1 are clamped to 1. |
| `shutdown_timeout` | `Duration` | `1 second` | Deadline for async shutdown and polling-client close (including optional queued-work flush). A zero timeout aborts immediately. |
| `protocol_violation_policy` | `ProtocolViolationPolicy` | `Quarantine` | Response to invalid v3 delivery-accountability state: quarantine room data, disconnect, or observe. |

### Builder Methods

All builder methods are `#[must_use]` — you must chain or assign the return value.

| Method | Parameter Type | Description |
|---|---|---|
| `.with_event_channel_capacity(n)` | `usize` | Set the bounded event channel capacity (default 256). |
| `.with_command_channel_capacity(n)` | `usize` | Set the bounded outgoing command queue capacity (default 1024). |
| `.with_shutdown_timeout(d)` | `Duration` | Set the graceful shutdown timeout (default 1 second). |
| `.enable_v3()` | — | Advertise protocol v3 relay/accountability support without opting into WebRTC. |
| `.enable_mesh()` | — | Enable v3 and advertise WebRTC mesh/host support. Only use when a WebRTC driver is available. |
| `.with_protocol_violation_policy(policy)` | `ProtocolViolationPolicy` | Select `Quarantine` (default), `Disconnect`, or `Observe`. |

### Full Example

```rust,ignore
use signal_fish_client::{SignalFishConfig, protocol::GameDataEncoding};
use std::time::Duration;

let config = SignalFishConfig::new("mb_app_abc123")
    .with_event_channel_capacity(512)
    .with_command_channel_capacity(2048)
    .with_shutdown_timeout(Duration::from_secs(5));
```

Or using struct literal syntax with defaults:

```rust,ignore
use signal_fish_client::{SignalFishConfig, protocol::GameDataEncoding};

let config = SignalFishConfig {
    app_id: "mb_app_abc123".into(),
    sdk_version: Some("0.8.0".into()),
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
loop over a **bounded** channel (default 1024, via
[`SignalFishConfig::command_channel_capacity`](#fields)) — they return
immediately without awaiting a round-trip.

!!! info "Error convention"
    All synchronous `Result<()>` methods return
    `Err(SignalFishError::NotConnected)` when the transport is closed, and
    `Err(SignalFishError::SendBufferFull { capacity })` when the outgoing
    command queue is full (the message is **not** queued; nothing is silently
    dropped). The async `*_reliable` variants
    ([`send_game_data_reliable`](#send_game_data_reliable),
    [`send_signal_reliable`](mesh-guide.md)) wait for queue capacity instead
    of failing fast.

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
let (mut client, mut event_rx) = SignalFishClient::start(transport, config);
```

!!! note
    `WebSocketTransport` requires the `transport-websocket` feature, which is
    enabled by default.

---

### Room Operations

#### `join_room`

Join or create a room with the given parameters.

```rust,ignore
fn join_room(&mut self, params: JoinRoomParams) -> Result<()>
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
fn leave_room(&mut self) -> Result<()>
```

```rust,ignore
client.leave_room()?;
```

The server will broadcast a player-left event to remaining room members.

---

#### `set_ready`

Signal readiness to start the game in the lobby.

```rust,ignore
fn set_ready(&mut self) -> Result<()>
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
    &mut self,
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
fn leave_spectator(&mut self) -> Result<()>
```

```rust,ignore
client.leave_spectator()?;
```

---

### Game Data

#### `send_game_data`

Send arbitrary JSON game data to other players in the room.

```rust,ignore
fn send_game_data(&mut self, data: serde_json::Value) -> Result<()>
```

```rust,ignore
client.send_game_data(serde_json::json!({
    "action": "move",
    "x": 10,
    "y": 20,
}))?;
```

The data is forwarded to all other players (and spectators) in the room.

`send_game_data` returns as soon as the message is queued; when the bounded
command queue is full it fails fast with
`SignalFishError::SendBufferFull` — the message is not queued.

---

#### `send_game_data_reliable`

Send arbitrary JSON game data, waiting for space in the outgoing command
queue when it is full.

```rust,ignore
async fn send_game_data_reliable(&self, data: serde_json::Value) -> Result<()>
```

```rust,ignore
client.send_game_data_reliable(serde_json::json!({
    "input": { "frame": 1042, "buttons": 0b0110 },
})).await?;
```

The backpressure-aware counterpart to [`send_game_data`](#send_game_data):
instead of failing fast with `SendBufferFull`, it pauses until the transport
drains a slot, pacing the caller to actual transport throughput. This is the
recommended way to stream high-rate payloads (rollback input packets, state
sync) without guessing at sleep durations. It only errors with
`NotConnected` when the transport has closed.

!!! warning "Keep draining events"
    The command queue only drains while the transport loop runs, and the
    loop pauses whenever the *event* channel is full (events are never
    dropped). A task that awaits this method while it is also the only
    consumer of the event receiver can deadlock under simultaneous
    send + receive pressure — drain events from a separate task rather than
    strictly sequentially. (Do **not** race this send against the event
    receiver in a `tokio::select!`: a cancelled send discards its payload.)

The WebRTC-signaling counterpart is `send_signal_reliable(to, signal)`
(protocol v3 only — see the [Mesh Guide](mesh-guide.md)); a lost
offer/answer/ICE candidate stalls a handshake, so waiting beats failing when
the queue is congested.

#### Classified JSON delivery (protocol v3)

`send_game_data_with_delivery(data, delivery)` selects an explicit relay
delivery class:

```rust,ignore
use signal_fish_client::GameDataDelivery;

client.send_game_data_with_delivery(
    serde_json::json!({ "position": [12, 8] }),
    GameDataDelivery::Latest { key: 7 },
)?;
client.send_game_data_with_delivery(
    serde_json::json!({ "spark": true }),
    GameDataDelivery::Volatile,
)?;
```

`GameDataDelivery::Reliable` preserves the existing v2-compatible wire shape.
`Latest` and `Volatile` require a negotiated v3 connection and otherwise
return `ProtocolUnsupported`. The async
`send_game_data_with_delivery_reliable` counterpart waits for command-queue
capacity; “reliable” in that method name describes local queue admission, not
the selected server delivery class.

#### Binary game data (protocol v3)

`send_binary_game_data(payload)` queues a physical WebSocket binary
frame; `send_binary_game_data_reliable` waits for local queue capacity. Binary
frames use the protocol-reliable delivery path and require v3 negotiation.
They also require a binary `game_data_format`; the default/JSON format returns
`BinaryFormatNotNegotiated` before anything is queued. If the server reports an
unsupported requested format and falls back to JSON, subsequent binary sends
fail the same way.
Inbound envelopes are decoded strictly; malformed maps, duplicate or missing
fields, invalid UUID representation, zero stamps, and trailing bytes surface as
bounded `DecodeFailed` events.

---

### Send Queue and Traffic Stats

Synchronous diagnostics for the outgoing command queue and game-data traffic:

| Method | Signature | Description |
|---|---|---|
| `send_capacity()` | `fn send_capacity(&self) -> usize` | Messages that can currently be queued before the fail-fast sends return `SendBufferFull`. A shrinking value is the congestion signal; `0` means the next fail-fast send is refused. |
| `max_send_capacity()` | `fn max_send_capacity(&self) -> usize` | Configured capacity of the outgoing command queue (`command_channel_capacity`). |
| `stats()` | `fn stats(&self) -> ClientStats` | Cumulative game-data traffic counters. |

`ClientStats` (re-exported at the crate root) carries `game_data_sent`
(`GameData` messages written to the transport), `game_data_received`
(`GameData`/`GameDataBinary` messages read off the transport and parsed —
counted at **receipt**, not at delivery to your event loop, so a consumer
that stops draining events cannot masquerade as relay loss; in steady state
the two are identical because events are not dropped on overflow), and
`messages_undecodable` (inbound frames that failed to decode — each also
surfaces as a [`DecodeFailed`](events.md#decodefailed) event; steady growth
means protocol drift or a corrupting middlebox). The counters are
cumulative for the lifetime of the client — they survive room changes and
disconnects.

Because the client itself never drops game data (events are delivered with
backpressure; refused sends return `SendBufferFull`), these counters make
loss *elsewhere* observable: exchange or log them across peers, and a
persistent sent-vs-received deficit points at the relay path or a peer — not
at this client.

```rust,ignore
let stats = client.stats();
println!(
    "sent {} / received {} (queue {}/{} free)",
    stats.game_data_sent,
    stats.game_data_received,
    client.send_capacity(),
    client.max_send_capacity(),
);
```

---

### Authority

#### `request_authority`

Request to become (or relinquish) the room authority.

```rust,ignore
fn request_authority(&mut self, become_authority: bool) -> Result<()>
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
    &mut self,
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
    &mut self,
    player_id: PlayerId,
    room_id: RoomId,
    auth_token: String,
) -> Result<()>
```

```rust,ignore
client.reconnect(player_id, room_id, auth_token)?;
```

Use the `player_id` and `room_id` from the original `RoomJoined` event and the
server-issued token from `client.snapshot().reconnection_token`. A successful
`Reconnected` response rotates the token; read and persist the replacement
snapshot before another unexpected disconnect. Tokens are connection secrets:
do not log them.

---

#### `ping`

Send a heartbeat ping to the server.

```rust,ignore
fn ping(&mut self) -> Result<()>
```

```rust,ignore
client.ping()?;
```

Useful for keeping the connection alive through proxies or load balancers.

---

### State Accessors

`snapshot()` synchronously returns one coherent `ClientSnapshot`, including
connection/authentication state, room/player IDs, room code, the latest
reconnection token, negotiated protocol version, and whether delivery is
quarantined. Prefer it whenever multiple fields must describe the same instant.

Synchronous accessors use atomics; async accessors acquire an internal mutex.

| Method | Signature | Description |
|---|---|---|
| `is_connected()` | `fn is_connected(&self) -> bool` | Returns `true` if the transport is believed to be connected. |
| `is_authenticated()` | `fn is_authenticated(&self) -> bool` | Returns `true` if the server has confirmed authentication. |
| `snapshot()` | `fn snapshot(&self) -> ClientSnapshot` | Returns coherent session, reconnect-token, negotiation, and quarantine state. |
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
Originally created for WebAssembly targets (including Godot 4.5 native and
web exports via gdext), but usable in any single-threaded context with any
`Transport` implementation.

This is the right client whenever your application is **frame-driven** —
native game loops as much as wasm. The async `SignalFishClient` only makes
progress while its tokio runtime is being driven; manually "ticking" a
runtime once per frame starves its transport loop (see
[Driving the Client](concepts.md#driving-the-client-runtime-contract)). The
polling client has no background task and no runtime — you pump it yourself.

!!! note "Feature gate"
    `SignalFishPollingClient` requires the `polling-client` feature.
    This feature is automatically enabled by `transport-godot` and
    `transport-websocket-emscripten`.

Unlike `SignalFishClient`, the polling client does **not** spawn background
tasks. Instead, the caller drives the protocol by calling
[`poll()`](#poll) once per frame from the game loop. All state is owned
directly — no `Arc`, `Mutex`, or atomics.

---

### Creation

#### `new` and `new_with_options`

Create a new polling client with a connected transport and config.

```rust,ignore
fn new(transport: impl Transport, config: SignalFishConfig) -> Self
fn new_with_options(
    transport: impl Transport,
    config: SignalFishConfig,
    options: PollingClientOptions,
) -> Self
```

```rust,ignore
use signal_fish_client::{
    GodotWebSocketTransport, SignalFishPollingClient, SignalFishConfig,
};

let transport = GodotWebSocketTransport::connect("wss://server/ws")
    .expect("connection failed");
let config = SignalFishConfig::new("mb_app_abc123");
let mut client = SignalFishPollingClient::new(transport, config);
```

On construction, the client immediately queues an `Authenticate` message
(just like `SignalFishClient::start`). The message is offered on the first
call to `poll()`. `new` uses fixed defaults of 64 frames/64 KiB in each
direction and the `Abandon` close policy. Use `new_with_options` to tune these
limits or opt in to `Flush`.

---

### Game Loop Integration

#### `poll`

Transfer bounded outgoing work, process bounded incoming work, and return the
events generated this frame.

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

`poll()` offers commands through `transport.poll_send`, then calls
`transport.poll_recv`; each loop stops on `Pending` or at either its frame or
byte budget. Remaining frames retain FIFO order for later polls. Zero limits
clamp to one, and one individually oversized frame may consume a poll by
itself. A successful send means backend ownership transfer, not peer delivery
or a socket-wide drain.

!!! tip "Call frequency"
    Call `poll()` once per frame. It is designed to be cheap when idle
    (no messages buffered = no work done). Calling it more often than once
    per frame is harmless but unnecessary.

---

### Command Methods

All command methods are synchronous. They queue an outgoing message that is
offered on subsequent `poll()` calls as readiness and work budgets allow. All
return `Result<(), SignalFishError>`.

The outgoing queue is bounded by the same
[`SignalFishConfig::command_channel_capacity`](#fields) (default 1024): if
the transport stalls long enough for the queue to fill, further queuing
methods return `SignalFishError::SendBufferFull` (the message is not
queued). `send_capacity()` / `max_send_capacity()` report the remaining and
configured capacity.

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
| `send_capacity()` | `usize` | Messages that can still be queued before `SendBufferFull`. |
| `max_send_capacity()` | `usize` | Configured command-queue capacity. |
| `stats()` | `ClientStats` | Cumulative `game_data_sent` / `game_data_received` / `messages_undecodable` counters (see [Send Queue and Traffic Stats](#send-queue-and-traffic-stats)). |
| `polling_stats()` | `PollingStats` | Client-owned queue depth, budget exhaustion, abandoned-command, and deadline counters. |
| `transport_diagnostics()` | `TransportDiagnostics` | Backend acceptance, buffering, watermark, and capacity counters. |

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

New commands are rejected immediately and session state is cleared. The
default `Abandon` policy discards queued/unaccepted work and starts close;
`Flush` first transfers existing work under the normal per-poll budget.
Backend-accepted data remains ordered before Close. Subsequent `poll()` calls
drive the lifecycle while `is_closing()` is true. If
`SignalFishConfig::shutdown_timeout` expires, remaining work is counted as
abandoned, the transport is aborted, and `is_closing()` becomes false.
After calling `close()`, `is_connected()` returns `false` and all command
methods return `Err(SignalFishError::NotConnected)`.

!!! warning "No Drop fallback"
    Unlike `SignalFishClient`, the polling client does **not** abort a
    background task on drop (there is no background task). However, the
    underlying transport's `Drop` implementation will still clean up
    resources. Call `close()` and continue polling while `is_closing()` for a
    graceful WebSocket close handshake.
