# Client API Reference

The core API surface of the Signal Fish Client SDK consists of three types:
[`SignalFishConfig`](#signalfishconfig) for connection settings,
[`JoinRoomParams`](#joinroomparams) for room entry, and
[`SignalFishClient`](#signalfishclient) — the async client handle itself.
For WebAssembly environments without an async runtime,
[`SignalFishPollingClient`](#signalfishpollingclient) provides a synchronous,
game-loop-driven alternative.

!!! note "Published crate versus this guide"
    The stable crates.io release is **0.12.0**. This guide tracks the current
    `main` branch, which may include additions that have not reached a release
    yet; the [changelog](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/blob/main/CHANGELOG.md)
    lists what is new. Use the
    [0.12.0 API docs](https://docs.rs/signal-fish-client/0.12.0/) for the
    published surface, or a `git` dependency on `main` for the unreleased APIs.

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
| `game_data_format` | `Option<GameDataEncoding>` | `None` | Requested game-data encoding (`Json`, `MessagePack`, or reserved `Rkyv`). The effective wire format is resolved from the first authoritative `ProtocolInfo`; omission and unsupported requests resolve to JSON on Server 0.8. |
| `protocol_version` | `Option<u16>` | `None` | Highest signaling protocol version advertised. `None` preserves the v2 relay floor. Prefer `enable_v3()` or `enable_mesh()` over setting this alone. |
| `supported_transports` | `Option<Vec<TransportKind>>` | `None` | Protocol-v3 data-path transports the application can actually fulfill. |
| `supported_topologies` | `Option<Vec<Topology>>` | `None` | Protocol-v3 session topologies the application can participate in. |
| `event_channel_capacity` | `usize` | `256` | Capacity of the bounded event channel. Events are never dropped on overflow — a full channel pauses the transport loop (backpressure), so this only controls buffering before backpressure kicks in. Values below 1 are clamped to 1; values above tokio's semaphore permit ceiling (`usize::MAX >> 3`) are clamped to that ceiling. |
| `command_channel_capacity` | `usize` | `1024` | Capacity of the bounded outgoing command queue. When full, the synchronous send methods fail fast with [`SignalFishError::SendBufferFull`](errors.md#handling-sendbufferfull); the `*_reliable` variants wait for a slot instead. Values below 1 are clamped to 1; values above tokio's semaphore permit ceiling (`usize::MAX >> 3`) are clamped to that ceiling. |
| `shutdown_timeout` | `Duration` | `1 second` | Deadline for async shutdown and polling-client close (including optional queued-work flush). A zero timeout aborts immediately. |
| `protocol_violation_policy` | `ProtocolViolationPolicy` | `Quarantine` | Response to invalid v3 delivery-accountability state: quarantine room data, disconnect, or observe. |
| `reconnect_policy` | `Option<ReconnectPolicy>` | `None` | Opt the async client into automatic reconnection with backoff (`None` keeps recovery fully manual). See [Automatic reconnection](#automatic-reconnection-opt-in). |

### Builder Methods

All builder methods are `#[must_use]` — you must chain or assign the return value.

| Method | Parameter Type | Description |
|---|---|---|
| `.with_event_channel_capacity(n)` | `usize` | Set the bounded event channel capacity (default 256). |
| `.with_command_channel_capacity(n)` | `usize` | Set the bounded outgoing command queue capacity (default 1024). |
| `.with_shutdown_timeout(d)` | `Duration` | Set the graceful shutdown timeout (default 1 second). |
| `.enable_v3()` | — | Advertise protocol v3 relay/accountability support without opting into WebRTC. |
| `.enable_mesh()` | — | Enable v3 and advertise WebRTC mesh/host support. Only use when a WebRTC driver is available. |
| `.with_protocol_version(v)` | `u16` | Set the advertised protocol ceiling without selecting transports or topologies. Power-user API. |
| `.with_transports(values)` | `impl IntoIterator<Item = TransportKind>` | Advertise data-path transports the application can fulfill. Power-user API. |
| `.with_topologies(values)` | `impl IntoIterator<Item = Topology>` | Advertise supported session topologies. Power-user API. |
| `.with_protocol_violation_policy(policy)` | `ProtocolViolationPolicy` | Select `Quarantine` (default), `Disconnect`, or `Observe`. |
| `.with_reconnect_policy(policy)` | `ReconnectPolicy` | Automate fresh transports, backoff, re-authentication, and player-room reconnects after retryable disconnects. Async client only. |

### Full Example

```rust,ignore
use signal_fish_client::SignalFishConfig;
use std::time::Duration;

let config = SignalFishConfig::new("mb_app_abc123")
    .with_event_channel_capacity(512)
    .with_command_channel_capacity(2048)
    .with_shutdown_timeout(Duration::from_secs(5));
```

Or using struct literal syntax with defaults:

```rust,ignore
use signal_fish_client::{GameDataEncoding, SignalFishConfig};

let config = SignalFishConfig {
    app_id: "mb_app_abc123".into(),
    sdk_version: Some("0.12.0".into()),
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
| `.with_max_players(n)` | `u8` | Set the maximum number of players allowed in the room. The field is a `u8`, so values above 255 cannot be expressed; rooms larger than that are outside this SDK's model. |
| `.with_supports_authority(flag)` | `bool` | Enable or disable authority delegation support. |
| `.with_relay_transport(transport)` | `RelayTransport` | Set legacy relay metadata retained for wire compatibility; Server 0.8 ignores it and it does not reconfigure signaling. |

### Full Example

```rust,ignore
use signal_fish_client::{JoinRoomParams, protocol::RelayTransport};

let params = JoinRoomParams::new("my-game", "Alice")
    .with_room_code("ABCD")
    .with_max_players(4)
    .with_supports_authority(true)
    .with_relay_transport(RelayTransport::Udp);
```

`RelayTransport::Udp` is legacy descriptor metadata, not a raw UDP
implementation in this SDK. Signal Fish Server 0.8 accepts but ignores this
`JoinRoom` field, and continues to relay `GameData` over the WebSocket
connection. An application/engine owns any self-declared relay metadata carried
through `ConnectionInfo`; see the
[datagram scope](transport.md#datagram-and-raw-stream-scope).

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
    transport: impl Transport + Send + 'static,
    config: SignalFishConfig,
) -> (Self, tokio::sync::mpsc::Receiver<SignalFishEvent>)
```

The return tuple is marked `#[must_use]` — you **must** consume the event
receiver to observe server events. Both the handle and the event receiver are
`Send + Sync`: move the receiver into a `tokio::spawn` drain loop and share the
handle via `Arc<Mutex<_>>` across tasks.

On start, the client automatically sends an `Authenticate` message using the
provided config. A background Tokio task is spawned to multiplex send/receive
on the transport.

```rust,ignore
use signal_fish_client::{
    SignalFishClient, SignalFishConfig, WebSocketTransport,
};

let transport = WebSocketTransport::connect("ws://localhost:3536/v2/ws").await?;
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

Signal readiness to start the game in the lobby. Each call **toggles**
readiness, so call it once per room membership — a repeated call flips the
player back to not-ready.

```rust,ignore
fn set_ready(&mut self) -> Result<()>
```

```rust,ignore
client.set_ready()?;
```

Readiness does **not** start the game. Once a
`LobbyStateChanged { all_ready: true, .. }` event arrives, one eligible client
must explicitly call [`start_game()`](#start_game). In an authority-enabled
room, only the current authority is eligible.

---

#### `start_game`

Request explicit protocol-v2 game start.

```rust,ignore
fn start_game(&mut self) -> Result<()>
```

```rust,ignore
if all_ready && (!supports_authority || is_authority) && !start_request_sent {
    client.start_game()?;
    start_request_sent = true;
}
```

The server accepts the request only after every current player is ready. In an
authority-enabled room, it accepts the request only from the authority.
Rejected requests arrive as `GameStartNotReady` or `GameStartForbidden` server
errors. Applications that previously relied on readiness auto-starting the
game must add this call; see [Migrating from 0.7 to 0.8](migration-0.8.md#explicit-game-start).

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

JSON game data is bounded at **128 nested containers** (arrays/objects),
matching `serde_json`'s own deserialization recursion limit, so every
payload the client could receive is also sendable. A deeper payload is
refused at the call site with `SignalFishError::PayloadTooDeep` instead of
being serialized recursively on a driver thread, where a sufficiently deep
value would abort the whole process with a stack overflow. Flatten deeply
nested payloads, or send compact binary game data
(`send_binary_game_data`, protocol v3).

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
sync) without guessing at sleep durations. It returns the same membership
and payload-shape errors as [`send_game_data`](#send_game_data) — including
`SignalFishError::PayloadTooDeep`, checked synchronously before it waits —
plus `NotConnected` if the transport closes while waiting.

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
the selected server delivery class. The 128-container nesting bound applies
to every JSON delivery class equally.

!!! note "Cost of the depth check"
    The bound is enforced by a fast, allocation-free walk of the payload on
    the calling thread before queuing — no wire bytes, allocation counts,
    or pinned performance ledgers change.

#### Binary game data (protocol v3)

`send_binary_game_data(payload)` queues a physical WebSocket binary
frame; `send_binary_game_data_reliable` waits for local queue capacity. Binary
frames use the protocol-reliable delivery path and require v3 negotiation.
They also require an effectively negotiated binary format; the default/JSON
format returns `BinaryFormatNotNegotiated` before anything is queued. The
client resolves this from `ProtocolInfo.game_data_formats`, not from the
earlier advisory `UnsupportedGameDataFormat` error. An unsupported request
(including Server 0.8's reserved `Rkyv`) resolves to JSON, so binary sends fail
locally even when that advisory arrives before `Authenticated` and
`ProtocolInfo`.
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
| `transport_diagnostics()` | `fn transport_diagnostics(&self) -> TransportDiagnostics` | Backend acceptance, buffering, watermark, and capacity counters, as last sampled by the driver loop (the polling driver reads its transport synchronously). |

`ClientStats` (re-exported at the crate root) carries `game_data_sent`
(`GameData` messages counted when the transport takes frame ownership, even if
backend completion later fails), `game_data_received`
(`GameData`/`GameDataBinary` messages counted immediately after successful
protocol decode, before message validation, sequence accountability,
quarantine, or event suppression), and
`messages_undecodable` (inbound frames that failed to decode — each also
surfaces as a [`DecodeFailed`](events.md#decodefailed) event; steady growth
means protocol drift or a corrupting middlebox). The counters are
cumulative for the lifetime of the client — they survive room changes and
disconnects.

Physical binary frames rejected by lifecycle or negotiated-representation
policy before logical decoding are outside the counter. Once decoded,
`game_data_received` includes stale and quarantined messages;
those explain differences between receipt and application events rather than a
sent-versus-received deficit. Malformed frames are excluded from it. Cross-peer
counter equality is not guaranteed: transport acceptance is not server receipt,
broadcast fanout can increase aggregate receives, and server delivery classes,
rejection, terminal unread work, or an accepted-then-failed send can reduce it.
WebRTC data-channel traffic and non-game protocol messages are outside these
counters. Use them as boundary-specific diagnostics, not delivery receipts.

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
use signal_fish_client::ConnectionInfo;

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
do not log them. Terminal reconnect responses must match the player, room, and
credential of an admitted `Reconnect` command. Under v3, the client accepts a
reconnect baseline only when it contains replay status, a nonempty rotated
token, exact player stamps, and sender watermarks covering the complete
current-player snapshot. Malformed
baselines are never applied, including under `ProtocolViolationPolicy::Observe`.

Reconnect is also a hard session-plan boundary. The old generation and peer
set are fenced immediately, and Server 0.8 follows `Reconnected` with a fresh
live `SessionPlan` for a finalized room. `ProtocolInfo`, `SessionPlan`, signals,
and game data are not legal `missed_events` replay entries.

##### End-to-end recovery policy

The reconnect flow spans a disconnect and a fresh connection, so it is worth
writing down as one procedure:

1. **Persist after every `RoomJoined` / `Reconnected`:** `player_id`, `room_id`,
   and `snapshot().reconnection_token` (a connection secret — never log it).
2. **On an unexpected `Disconnected`:** build a fresh transport and client,
   then wait for `Authenticated` before recovering (directed room operations
   refuse until the server confirms authentication).
3. **Call `reconnect(player_id, room_id, token)`** with the persisted triple.
4. **On `Reconnected`:** fold `missed_events` into your game state, adopt the
   fresh live `SessionPlan` that follows, and persist the rotated
   `reconnection_token` from the new snapshot.
5. **On `ReconnectionFailed`:** decide by `error_code`:
   - `ReconnectionExpired` or `ReconnectionTokenInvalid` — the server no
     longer knows this session: fall back to a normal `join_room`.
   - `PlayerAlreadyConnected` — another live connection still holds the seat;
     wait for it to exit (or drop it) before retrying.
   - Anything else — retry with backoff while the room is worth rejoining.

Peers stay fenced against your signals until the post-reconnect
`SessionPlan` arrives, so gate mesh/relay work on it as on a first join.

##### Automatic reconnection (opt-in)

The procedure above is fully manual by default. The async client can automate
the transport-and-authentication core of it with a
`ReconnectPolicy` on
`SignalFishConfig::with_reconnect_policy`:

```rust,ignore
let config = SignalFishConfig::new("mb_app_abc123").with_reconnect_policy(
    ReconnectPolicy::new(|| Box::new(my_transport_factory.spawn())),
);

let (mut client, events) = SignalFishClient::start(transport, config);
```

With a policy configured, a terminal disconnect **other than** shutdown, a
dropped handle, or a `ProtocolViolationPolicy::Disconnect` teardown (a
protocol violation is a correctness signal, never masked) becomes a
retryable edge:

1. The usual bounded teardown delivers `Disconnected`, exactly as without a
   policy.
2. The loop emits `Reconnecting { attempt, next_backoff }`, waits the
   deterministic exponential delay (`min(initial_backoff * 2^(n-1),
   max_backoff)`, tokio-timed so paused clocks advance it in tests), then
   opens a fresh transport from the policy's factory.
3. The fresh connection runs the normal `Connected` → `Authenticated`
   sequence, and queued-but-unsent commands of the dead connection stay
   discarded with it.
4. If the client was a **player** in a room when the connection ended, the
   retained `player_id`/`room_id`/token context is consumed automatically:
   the client issues the same directed `reconnect` as step 3 of the manual
   procedure, and the server answers with `Reconnected` or
   `ReconnectionFailed`. The context refreshes from every
   `RoomJoined`/`Reconnected` baseline and is discarded by a voluntary
   `leave_room`, so a policy never rejoins a room you chose to leave. One
   deliberate gap: if the connection dies again *before* the automatic
   reconnect is answered, that attempt is lost with the round and the next
   round is connection-only — recovery then continues manually.

The `Reconnecting`/`ReconnectAbandoned` deliveries share the terminal
farewell's bounded budget: a consumer that stops draining delays each round
by at most `shutdown_timeout` instead of parking the loop, and an undelivered
scheduling event falls back to one nonblocking attempt (so a wedged consumer
may miss a `Reconnecting` marker while reconnection itself continues).

On `ReconnectionFailed` the connection stays up but room recovery stops —
apply the same `error_code` decision tree as the manual flow (fall back to
`join_room`, wait out `PlayerAlreadyConnected`, or give up). When the attempt
budget (`max_attempts`, reset whenever a connection reaches `Authenticated`)
runs out, the client emits `ReconnectAbandoned { attempts, last_reason }` and
the event stream ends.

The polling client is caller-driven by design and ignores this option —
recover it by constructing a new client inside your game loop. There is
deliberately **no jitter**: the SDK pins determinism and carries no RNG
dependency; de-synchronize retry storms by varying `initial_backoff` per
client or add jitter inside your factory.

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
connection ownership/readiness/authentication state, room role/participant ID,
room code, the latest
reconnection token, requested and effective game-data formats, negotiated
protocol version, current session generation, and whether delivery is
quarantined. It also carries the latest selected `session_topology` and
`session_transport`, plus the negotiated server outbound-message cap
(`server_max_outbound_message_size`). Prefer it whenever multiple fields must
describe the same instant.

Synchronous snapshot and negotiation accessors briefly lock the shared core;
the async room-ID accessors acquire the same internal mutex.

| Method | Signature | Description |
|---|---|---|
| `is_connected()` | `fn is_connected(&self) -> bool` | Returns `true` while the client owns a nonterminal transport attempt, including its connecting phase. |
| `is_transport_ready()` | `fn is_transport_ready(&self) -> bool` | Returns `true` after the driver observes a completed transport handshake and before terminal teardown. |
| `is_authenticated()` | `fn is_authenticated(&self) -> bool` | Returns `true` if the server has confirmed authentication. |
| `snapshot()` | `fn snapshot(&self) -> ClientSnapshot` | Returns coherent session, reconnect-token, negotiation, and quarantine state. |
| `room_role()` | `fn room_role(&self) -> Option<RoomRole>` | Server-confirmed `Player` or `Spectator` role; `None` outside a room. |
| `requested_game_data_format()` | `fn requested_game_data_format(&self) -> Option<GameDataEncoding>` | Exact preference supplied in `SignalFishConfig`, preserving omission. |
| `effective_game_data_format()` | `fn effective_game_data_format(&self) -> Option<GameDataEncoding>` | Server-selected format; `None` before valid `ProtocolInfo` or after disconnect. |
| `supports_mesh()` | `fn supports_mesh(&self) -> bool` | Negotiated local capability: v3 plus advertised WebRTC and a Host or Mesh topology. This does not describe the active plan. |
| `session_topology()` | `fn session_topology(&self) -> Option<Topology>` | Topology selected by the latest authoritative plan. |
| `session_transport()` | `fn session_transport(&self) -> Option<TransportKind>` | Transport selected by the latest authoritative plan. |
| `is_p2p_active()` | `fn is_p2p_active(&self) -> bool` | Whether the selected plan uses a Host or Mesh topology. |
| `current_room_id()` | `async fn current_room_id(&self) -> Option<RoomId>` | Returns the current room ID, if in a room. |
| `current_player_id()` | `async fn current_player_id(&self) -> Option<PlayerId>` | Legacy name for the local room participant ID (player or spectator). Interpret it with `room_role()`. |
| `current_room_code()` | `async fn current_room_code(&self) -> Option<String>` | Returns the current room code, if in a room. |

The snapshot fields form these nested connection phases:

| Phase | `connected` | `transport_ready` | `authenticated` | `room_role` |
|---|---:|---:|---:|---|
| Connecting / client-owned | `true` | `false` | `false` | `None` |
| Transport ready | `true` | `true` | `false` | `None` |
| Authenticated, outside a room | `true` | `true` | `true` | `None` |
| In a room | `true` | `true` | `true` | player or spectator |
| Terminal | `false` | `false` | `false` | `None` |

`transport_ready` is the driver's sticky observation for the current physical
connection, not a fresh call to the backend. `SignalFishEvent::Connected`
corresponds to that transition. Commands may be queued during the connecting
phase; a conforming transport leaves their frames caller-owned until ready.

```rust,ignore
let state = client.snapshot();
match (state.room_role, state.player_id, state.room_id.as_ref()) {
    (Some(RoomRole::Player), Some(player_id), Some(room_id)) => {
        println!("Player {player_id} is in room {room_id}");
    }
    (Some(RoomRole::Spectator), Some(spectator_id), Some(room_id)) => {
        println!("Spectator {spectator_id} is watching room {room_id}");
    }
    (None, None, None) => println!("Outside a room"),
    _ => unreachable!("ClientSnapshot preserves the membership invariant"),
}
```

Read these fields from one `snapshot()` as above; separate accessor calls can
observe different instants while the background task applies a transition.
`room_role`, `player_id`, `room_id`, and `room_code` form one invariant: all
four are absent outside a room, and a confirmed player or spectator membership
sets all four. A confirmed exit clears all four. Admission of a join, leave, or
reconnect fences later room commands until a matching typed success or failure
response, preventing FIFO commands from running against obsolete membership.
An uncorrelated generic server error or an absent response cannot safely release
that fence; the client stays fail-closed until transport teardown, after which
a new connection may retry.

Configurations that can negotiate v3 automatically request the
`room_operation_ids` capability. Once the server echoes it in `ProtocolInfo`,
each directed room operation carries a fresh UUID and only the terminal result
with that exact UUID can release the fence. Delayed, duplicate, wrong-kind,
malformed, or legacy unwrapped results are reported as `ProtocolViolation`
without changing membership. If the server does not echo the capability, the
connection continues with the legacy Server 0.7 wire behavior. Operations
admitted before the first `ProtocolInfo` also remain legacy for their complete
request/response lifetime, even if negotiation finishes while one is pending.
Legacy results carry no correlation identity on the wire, so a duplicated
kind-compatible legacy reply can consume the next same-kind operation's fence;
correlated UUID results are individually identifiable and immune to this.

Room command admission is role-specific, and the five directed room operations
additionally require server-confirmed authentication:

| Local state | Allowed room operations |
|---|---|
| Unauthenticated connection | None of them — they return `NotAuthenticated` (wait for the `Authenticated` event) |
| Authenticated, outside a room | `join_room`, `join_as_spectator`, or `reconnect` |
| `RoomRole::Player` | leave, game-data, readiness/game-start, authority, connection-info, signaling, and transport-status operations |
| `RoomRole::Spectator` | `leave_spectator` |
| Any nonterminal connection | `ping` |

Wrong-state errors are deterministic: `NotConnected` wins first, then
`NotAuthenticated` for the five directed room operations, then
`RoomOperationPending`, membership/role and authority errors, protocol/format
or session-plan errors, and finally `SendBufferFull` at queue admission.

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

Shutdown proceeds in four stages:

1. Sends a oneshot signal to the background transport loop.
2. The loop finishes a backend-owned send and drives `Transport::poll_close`
   within the configurable deadline (default **1 second**, set via
   [`SignalFishConfig::shutdown_timeout`](#signalfishconfig)).
3. If the deadline expires, the transport's required `abort` fallback releases
   or safely detaches backend resources and the loop normally returns. A later
   watchdog cancels the task only if it still does not stop. The `Disconnected`
   event may not be delivered in this case.
4. Regardless of whether `Disconnected` is delivered, connection/session state
   is cleared (`is_connected() == false`, `is_transport_ready() == false`,
   `is_authenticated() == false`, and room/player accessors return `None`).

!!! note "Cancelling `shutdown()` mid-await"
    Dropping the `shutdown()` future before it completes forfeits only the
    *outer* watchdog — the signal was already delivered and the background
    loop still finishes its own teardown within the configured
    `shutdown_timeout`. A later `shutdown()` call returns immediately (the
    work is already in flight or done) and never hangs; at most one
    `Disconnected` event is emitted.

!!! warning "Drop fallback"
    If `shutdown()` is never called, the `Drop` implementation **aborts** the
    background task immediately; the task's ownership guard invokes
    `Transport::abort` as it is cancelled. Always prefer an explicit
    `shutdown().await` for a clean disconnect.

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
    This feature is also enabled by the lockstep `signal-fish-client-godot`
    adapter and by `transport-websocket-emscripten`.

Unlike `SignalFishClient`, the polling client does **not** spawn background
tasks. Instead, the caller drives the protocol by calling
[`poll()`](#poll) once per frame from the game loop. All state is owned
directly — no `Arc`, `Mutex`, or atomics.

---

### Creation

#### `new` and `new_with_options`

Create a new polling client with a transport and config. The transport may
already be ready, or it may finish an asynchronous handshake while `poll()`
drives it.

```rust,ignore
fn new(transport: impl Transport, config: SignalFishConfig) -> Self
fn new_with_options(
    transport: impl Transport,
    config: SignalFishConfig,
    options: PollingClientOptions,
) -> Self
```

```rust,ignore
use signal_fish_client::{SignalFishPollingClient, SignalFishConfig};
use signal_fish_client_godot::GodotWebSocketTransport;

let transport = GodotWebSocketTransport::connect("wss://server/v2/ws")
    .expect("connection failed");
let config = SignalFishConfig::new("mb_app_abc123");
let mut client = SignalFishPollingClient::new(transport, config);
```

On construction, the client immediately queues an `Authenticate` message
(just like `SignalFishClient::start`). It is offered on every `poll()` as
transport admission and work budgets allow, even before `is_ready()` becomes
true; a connecting transport must return `Pending` without taking it. `new`
uses defaults of 64 frames/64 KiB in each
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
    (no messages buffered = no work done). Each additional call begins a new
    bounded work cycle and a new adaptive-transport sample, so use extra calls
    only when intentionally granting more networking work in that frame.

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
| `set_ready()` | Signal readiness in the lobby; this does not start the game. Each call **toggles** readiness, so call it once per room membership. |
| `start_game()` | Explicitly request game start after all players are ready. |
| `send_game_data(data: serde_json::Value)` | Send protocol-reliable JSON game data. |
| `send_game_data_with_delivery(data, delivery)` | Select a protocol-v3 JSON delivery class. |
| `send_binary_game_data(payload: Vec<u8>)` | Send a protocol-v3 binary game-data frame. |
| `request_authority(become_authority: bool)` | Request or release room authority. |
| `provide_connection_info(info: ConnectionInfo)` | Provide P2P connection information. |
| `reconnect(player_id, room_id, auth_token)` | Reconnect to a previous session. |
| `ping()` | Send a heartbeat ping. |
| `join_as_spectator(game, room, name)` | Join a room as a spectator. |
| `leave_spectator()` | Leave spectator mode. |
| `send_signal(to, signal)` / `send_offer` / `send_answer` / `send_ice_candidate` | Send typed protocol-v3 WebRTC signaling using the current plan generation. |
| `send_signal_for_generation(to, generation, signal)` | Send driver-produced signaling only if its originating plan generation is still current. |
| `send_raw_signal(to, value)` | Send an unmodeled protocol-v3 signal shape. |
| `send_raw_signal_for_generation(to, generation, value)` | Generation-bound form of the raw signaling escape hatch. |
| `report_transport_status(transport, connected)` | Report protocol-v3 data-path status. |

All methods return `Err(SignalFishError::NotConnected)` if the transport has
closed.

---

### State Accessors

All accessors are **synchronous** (no async, no mutex):

| Method | Returns | Description |
|---|---|---|
| `is_connected()` | `bool` | Whether the client owns a nonterminal transport attempt, including connecting. |
| `is_transport_ready()` | `bool` | Whether the driver has observed the transport handshake complete. |
| `is_authenticated()` | `bool` | Whether the server confirmed authentication. |
| `room_role()` | `Option<RoomRole>` | Server-confirmed player/spectator role. |
| `is_closing()` | `bool` | Whether `poll()` must continue driving a close lifecycle. |
| `negotiated_protocol_version()` | `Option<u16>` | Negotiated v3-or-newer version; `None` before `ProtocolInfo` or on the v2 floor. |
| `requested_game_data_format()` | `Option<GameDataEncoding>` | Exact configured preference, preserving omission. |
| `effective_game_data_format()` | `Option<GameDataEncoding>` | Server-selected format; `None` before valid `ProtocolInfo` or after disconnect. |
| `supports_mesh()` | `bool` | Negotiated v3 + WebRTC + Host/Mesh capability; not active-plan state. |
| `session_topology()` | `Option<Topology>` | Topology selected by the latest authoritative plan. |
| `session_transport()` | `Option<TransportKind>` | Transport selected by the latest authoritative plan. |
| `is_p2p_active()` | `bool` | Whether the selected plan uses a Host or Mesh topology. |
| `current_player_id()` | `Option<PlayerId>` | Legacy name for the local player-or-spectator participant ID. |
| `current_room_id()` | `Option<RoomId>` | Current room ID, if in a room. |
| `current_room_code()` | `Option<&str>` | Current room code, if in a room. |
| `send_capacity()` | `usize` | Messages that can still be queued before `SendBufferFull`. |
| `max_send_capacity()` | `usize` | Configured command-queue capacity. |
| `stats()` | `ClientStats` | Cumulative `game_data_sent` / `game_data_received` / `messages_undecodable` counters (see [Send Queue and Traffic Stats](#send-queue-and-traffic-stats)). |
| `snapshot()` | `ClientSnapshot` | Coherent connection readiness, room, reconnect-token, negotiation, selected-plan, and quarantine state. |
| `polling_stats()` | `PollingStats` | Client-owned queue depth, budget exhaustion, abandoned-command, and deadline counters. |
| `queue_age_stats()` | `PollingQueueAgeStats` | Sampled current/peak age of the oldest client-owned outbound item. |
| `reset_queue_age_peak()` | `()` | Refresh current age and reset its sampled peak; useful after setup. |
| `transport_diagnostics()` | `TransportDiagnostics` | Backend acceptance, buffering, watermark, and capacity counters. |
| `transport()` | `&T` | Read-only access to transport-specific diagnostics; I/O remains driven by `poll()`. |

!!! note "No async accessors"
    Unlike `SignalFishClient`, polling diagnostics and state access require no
    `.await`: read-only accessors take `&self`, while
    `reset_queue_age_peak()` takes `&mut self` because it resets sampled state.
    The polling client owns its state directly and uses no mutex.

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
drive the lifecycle while `is_closing()` is true. Already-buffered inbound
transport frames are drained under the normal receive budget so the peer close
can complete; because session state is already cleared, these late frames are
not emitted as application events. If
`SignalFishConfig::shutdown_timeout` expires, remaining work is counted as
abandoned, the transport is aborted, and `is_closing()` becomes false.
After calling `close()`, both `is_connected()` and `is_transport_ready()`
return `false`, and all command methods return
`Err(SignalFishError::NotConnected)`.

!!! warning "Drop fallback"
    The polling client has no background task. If it is dropped before
    graceful close completes, it synchronously calls the transport's required
    `abort` method. Call `close()` and continue polling while `is_closing()` for
    a graceful WebSocket close handshake.
