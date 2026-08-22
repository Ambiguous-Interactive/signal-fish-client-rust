# Protocol Types Reference

This page documents the wire-compatible protocol types used by the Signal Fish
Client SDK. These types mirror the server's protocol definitions and are
serialized as JSON over the transport layer.

!!! info "You rarely construct these directly"
    Most of these types are used internally by `SignalFishClient`. You interact
    with them through the client's methods and receive them as fields inside
    `SignalFishEvent` variants. This page is a reference for understanding the
    data shapes flowing over the wire.

---

## Type Aliases

Two UUID-based aliases are used throughout the protocol:

```rust,ignore
pub type PlayerId = uuid::Uuid;
pub type RoomId = uuid::Uuid;
```

| Alias | Underlying Type | Purpose |
|-------|----------------|---------|
| `PlayerId` | `uuid::Uuid` | Uniquely identifies a player across all rooms. |
| `RoomId` | `uuid::Uuid` | Uniquely identifies a room on the server. |

---

## Enums

### `RelayTransport`

Legacy relay labels retained for wire compatibility. The value appears in the
ignored `JoinRoom.relay_transport` preference and in self-declared
`ConnectionInfo::Relay` metadata; it does not select this crate's signaling
[`Transport`](transport.md), open a socket, or define a datagram envelope.
Signal Fish Server 0.7 continues to use its WebSocket connection for relayed
`GameData`. See
[Datagram and raw-stream scope](transport.md#datagram-and-raw-stream-scope).

- **Default:** `Auto`
- **Serde:** `rename_all = "lowercase"`

```rust,ignore
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum RelayTransport {
    Tcp,
    Udp,
    Websocket,
    #[default]
    Auto,
}
```

| Variant | JSON value | Description |
|---------|-----------|-------------|
| `Tcp` | `"tcp"` | Label an external relay path as TCP. |
| `Udp` | `"udp"` | Label an external relay path as UDP; this SDK does not open or parse its datagrams. |
| `Websocket` | `"websocket"` | Label a relay path as WebSocket. |
| `Auto` | `"auto"` | Legacy automatic-selection label; no selection occurs in this SDK (the enum default). |

---

### `GameDataEncoding`

Encoding format for sequenced game-data payloads.

- **Default:** `Json`
- **Serde:** `rename_all = "snake_case"` (with per-variant overrides)

```rust,ignore
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum GameDataEncoding {
    #[default]
    Json,
    #[serde(rename = "message_pack")]
    MessagePack,
    #[serde(rename = "rkyv")]
    Rkyv,
}
```

| Variant | JSON value | Description |
|---------|-----------|-------------|
| `Json` | `"json"` | JSON payloads delivered over text frames (default). |
| `MessagePack` | `"message_pack"` | MessagePack binary payloads delivered over binary frames. |
| `Rkyv` | `"rkyv"` | Rkyv zero-copy binary format. **Reserved:** Server 0.7 never advertises it; requesting it produces an `UnsupportedGameDataFormat` advisory and resolves to JSON. |

Server 0.7 advertises exactly `[Json]` or `[Json, MessagePack]`. The first valid
`ProtocolInfo` atomically resolves the client's effective format: a requested
advertised format wins, while omission or an unsupported request resolves to
JSON. The earlier unsupported-format error is advisory and does not mutate the
negotiated state.

---

### `Topology` (protocol v3)

The session topology the server selects for a finalized room and reports in a
[`SessionPlanPayload`](#sessionplanpayload-protocol-v3). The server is
authoritative — the client never computes a topology.

- **Serde:** `rename_all = "snake_case"`

```rust,ignore
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Topology {
    Relay,
    Host,
    Mesh,
}
```

| Variant | JSON value | Description |
|---------|-----------|-------------|
| `Relay` | `"relay"` | Server relay hub — the v2 behavior, always available (the "relay floor"). |
| `Host` | `"host"` | Star topology around a single elected host/authority. |
| `Mesh` | `"mesh"` | Full mesh: every peer connects to every other peer. |

---

### `TransportKind` (protocol v3)

The data-path transport the server selects for game data between peers.

!!! note "Distinct from the `Transport` trait"
    `TransportKind` is a wire **value** describing how peers exchange game data.
    It is *not* the [`Transport`](transport.md) I/O trait, which is the byte
    channel to the signaling server.

- **Serde:** `rename_all = "snake_case"`, except `WebRtc` is renamed to `"webrtc"`.

```rust,ignore
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Relay,
    Direct,
    #[serde(rename = "webrtc")]
    WebRtc,
}
```

| Variant | JSON value | Description |
|---------|-----------|-------------|
| `Relay` | `"relay"` | Server WebSocket fan-out — the mandatory floor every client supports. |
| `Direct` | `"direct"` | Direct IP:port connection (LAN / routable host). |
| `WebRtc` | `"webrtc"` | Peer-to-peer WebRTC data channel. (Note: serializes as `"webrtc"`, not `"web_rtc"`.) |

---

### `ConnectionInfo`

Connection information for peer-to-peer establishment. This is an internally
tagged enum (`serde(tag = "type")`), so each variant includes a `"type"`
discriminator field in JSON.

```rust,ignore
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ConnectionInfo {
    Direct { host: String, port: u16 },
    UnityRelay { allocation_id: String, connection_data: String, key: String },
    Relay {
        host: String,
        port: u16,
        transport: RelayTransport,
        allocation_id: String,
        token: String,
        client_id: Option<u16>,
    },
    WebRTC { sdp: Option<String>, ice_candidates: Vec<String> },
    Custom { data: serde_json::Value },
}
```

| Variant | Fields | Description |
|---------|--------|-------------|
| `Direct` | `host: String`, `port: u16` | Direct IP:port connection (Mirror, FishNet, Unity NetCode direct). |
| `UnityRelay` | `allocation_id: String`, `connection_data: String`, `key: String` | Unity Relay allocation (Unity NetCode via Unity Relay). |
| `Relay` | `host: String`, `port: u16`, `transport: RelayTransport`, `allocation_id: String`, `token: String`, `client_id: Option<u16>` | Legacy self-declared relay metadata forwarded to peers; neither the SDK nor Server 0.7 opens or authenticates the endpoint. |
| `WebRTC` | `sdp: Option<String>`, `ice_candidates: Vec<String>` | WebRTC connection info (Matchbox). |
| `Custom` | `data: serde_json::Value` | Arbitrary JSON blob for custom networking solutions. |

??? example "JSON — `Direct` variant"
    ```json
    {
        "type": "direct",
        "host": "192.168.1.10",
        "port": 7777
    }
    ```

??? example "JSON — `Relay` variant"
    ```json
    {
        "type": "relay",
        "host": "relay.example.com",
        "port": 9000,
        "transport": "auto",
        "allocation_id": "room-abc",
        "token": "tok_xyz",
        "client_id": null
    }
    ```

---

### `LobbyState`

Lobby readiness state for a room.

- **Default:** `Waiting`
- **Serde:** `rename_all = "snake_case"`

```rust,ignore
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LobbyState {
    #[default]
    Waiting,
    Lobby,
    Finalized,
}
```

| Variant | JSON value | Description |
|---------|-----------|-------------|
| `Waiting` | `"waiting"` | Room is waiting for players (default). |
| `Lobby` | `"lobby"` | All players are present; lobby is active. |
| `Finalized` | `"finalized"` | All players are ready; game is about to start. |

---

### `SpectatorStateChangeReason`

Describes why a spectator state change occurred.

- **Default:** `Joined`
- **Serde:** `rename_all = "snake_case"`

```rust,ignore
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpectatorStateChangeReason {
    #[default]
    Joined,
    VoluntaryLeave,
    Disconnected,
    Removed,
    RoomClosed,
}
```

| Variant | JSON value | Description |
|---------|-----------|-------------|
| `Joined` | `"joined"` | Spectator joined the room (default). |
| `VoluntaryLeave` | `"voluntary_leave"` | Spectator left voluntarily. |
| `Disconnected` | `"disconnected"` | Spectator's connection was lost. |
| `Removed` | `"removed"` | Spectator was removed by the server or authority. |
| `RoomClosed` | `"room_closed"` | The room was closed. |

---

## Payload Structs

### `PlayerInfo`

Information about a player in a room.

```rust,ignore
pub struct PlayerInfo {
    pub id: PlayerId,
    pub name: String,
    pub is_authority: bool,
    pub is_ready: bool,
    pub connected_at: String,
    pub connection_info: Option<ConnectionInfo>,
}
```

| Field | Type | Description |
|-------|------|-------------|
| `id` | `PlayerId` | The player's unique identifier. |
| `name` | `String` | Display name chosen at join time. |
| `is_authority` | `bool` | Whether this player is the room authority. |
| `is_ready` | `bool` | Whether the player has signaled readiness. |
| `connected_at` | `String` | ISO 8601 timestamp of when the player connected. |
| `connection_info` | `Option<ConnectionInfo>` | P2P connection info (present when the player is ready). |

---

### `SpectatorInfo`

Information about a spectator watching a room.

```rust,ignore
pub struct SpectatorInfo {
    pub id: PlayerId,
    pub name: String,
    pub connected_at: String,
}
```

| Field | Type | Description |
|-------|------|-------------|
| `id` | `PlayerId` | The spectator's unique identifier. |
| `name` | `String` | Display name. |
| `connected_at` | `String` | ISO 8601 timestamp of when the spectator joined. |

---

### `PeerConnectionInfo`

Peer connection information included in `GameStarting` events.

```rust,ignore
pub struct PeerConnectionInfo {
    pub player_id: PlayerId,
    pub player_name: String,
    pub is_authority: bool,
    pub relay_type: String,
    pub connection_info: Option<ConnectionInfo>,
}
```

| Field | Type | Description |
|-------|------|-------------|
| `player_id` | `PlayerId` | The peer's unique identifier. |
| `player_name` | `String` | The peer's display name. |
| `is_authority` | `bool` | Whether this peer is the room authority. |
| `relay_type` | `String` | Legacy deployment relay label; Server 0.7 uses it as protocol metadata, not proof of a physical path. |
| `connection_info` | `Option<ConnectionInfo>` | Connection info provided by the peer for P2P establishment. |

---

### `RateLimitInfo`

Rate-limit information returned after authentication.

```rust,ignore
pub struct RateLimitInfo {
    pub per_minute: u32,
    pub per_hour: u32,
    pub per_day: u32,
}
```

| Field | Type | Description |
|-------|------|-------------|
| `per_minute` | `u32` | Maximum requests allowed per minute. |
| `per_hour` | `u32` | Maximum requests allowed per hour. |
| `per_day` | `u32` | Maximum requests allowed per day. |

---

### `ProtocolInfoPayload`

Describes negotiated protocol capabilities for a specific SDK, sent by the
server immediately after authentication.

```rust,ignore
pub struct ProtocolInfoPayload {
    pub platform: Option<String>,
    pub sdk_version: Option<String>,
    pub minimum_version: Option<String>,
    pub recommended_version: Option<String>,
    pub capabilities: Vec<String>,
    pub notes: Option<String>,
    pub game_data_formats: Vec<GameDataEncoding>,
    pub player_name_rules: Option<PlayerNameRulesPayload>,
    // Protocol v3+ — omitted (None) for a negotiated v2 connection.
    pub protocol_version: Option<u16>,
    pub min_protocol_version: Option<u16>,
    pub max_protocol_version: Option<u16>,
}
```

| Field | Type | Description |
|-------|------|-------------|
| `platform` | `Option<String>` | Platform the server recognized (e.g. `"unity"`, `"rust"`). |
| `sdk_version` | `Option<String>` | SDK version echoed back by the server. |
| `minimum_version` | `Option<String>` | Minimum SDK version the server supports. |
| `recommended_version` | `Option<String>` | Recommended SDK version. |
| `capabilities` | `Vec<String>` | List of server-supported capability flags. |
| `notes` | `Option<String>` | Freeform notes from the server (e.g. deprecation warnings). |
| `game_data_formats` | `Vec<GameDataEncoding>` | Ordered server-supported encodings. Server 0.7 emits exactly `[Json]` or `[Json, MessagePack]`; malformed, empty, duplicate, or reordered lists are rejected transactionally. |
| `player_name_rules` | `Option<PlayerNameRulesPayload>` | Validation rules for player names (if enforced). |
| `protocol_version` | `Option<u16>` | **Protocol v3+.** The negotiated protocol version. `None` for a v2 negotiation, keeping v2 bytes identical. |
| `min_protocol_version` | `Option<u16>` | **Protocol v3+.** Lowest version this deployment accepts. |
| `max_protocol_version` | `Option<u16>` | **Protocol v3+.** Highest version this deployment speaks. |

---

### `PlayerNameRulesPayload`

Describes the characters a deployment allows inside player names.

```rust,ignore
pub struct PlayerNameRulesPayload {
    pub max_length: usize,
    pub min_length: usize,
    pub allow_unicode_alphanumeric: bool,
    pub allow_spaces: bool,
    pub allow_leading_trailing_whitespace: bool,
    pub allowed_symbols: Vec<char>,
    pub additional_allowed_characters: Option<String>,
}
```

| Field | Type | Description |
|-------|------|-------------|
| `max_length` | `usize` | Maximum allowed length for a player name. |
| `min_length` | `usize` | Minimum allowed length for a player name. |
| `allow_unicode_alphanumeric` | `bool` | Whether Unicode alphanumeric characters are allowed. |
| `allow_spaces` | `bool` | Whether spaces are allowed in the name. |
| `allow_leading_trailing_whitespace` | `bool` | Whether leading/trailing whitespace is allowed. |
| `allowed_symbols` | `Vec<char>` | Specific symbol characters that are permitted. |
| `additional_allowed_characters` | `Option<String>` | Extra characters beyond the base rules. |

---

### `IceServer` (protocol v3)

A STUN/TURN server for WebRTC ICE negotiation. `username` / `credential` are
present only for TURN servers; bare STUN entries omit them.

```rust,ignore
pub struct IceServer {
    pub urls: Vec<String>,
    pub username: Option<String>,
    pub credential: Option<String>,
}
```

| Field | Type | Description |
|-------|------|-------------|
| `urls` | `Vec<String>` | STUN/TURN URLs (e.g. `stun:stun.l.google.com:19302`). |
| `username` | `Option<String>` | TURN username (omitted for credential-less STUN servers). |
| `credential` | `Option<String>` | TURN credential (omitted for credential-less STUN servers). |

---

### `SessionPeer` (protocol v3)

A peer the recipient should connect to within a
[`SessionPlanPayload`](#sessionplanpayload-protocol-v3).

```rust,ignore
pub struct SessionPeer {
    pub player_id: PlayerId,
    pub player_name: String,
    pub is_authority: bool,
    pub initiate: bool,
}
```

| Field | Type | Description |
|-------|------|-------------|
| `player_id` | `PlayerId` | The other peer's identifier. |
| `player_name` | `String` | The other peer's display name. |
| `is_authority` | `bool` | Whether this peer is the session's authoritative host. |
| `initiate` | `bool` | Whether the recipient sends the WebRTC offer to this peer. **Server-assigned — obey it verbatim; the client never computes who initiates.** |

---

### `SessionPlanPayload` (protocol v3)

The per-recipient authoritative plan the server sends when a room finalizes
(delivered as a [`SessionPlan`](events.md#mesh-events-protocol-v3) event).
Relay plans can explicitly reset a prior peer-to-peer plan. A plan is also sent
again on late join, host re-election, or as a fresh live message after a
successful reconnect; each one **fully replaces** the previous plan. A
`Reconnected` baseline fences the prior plan immediately, and `SessionPlan` is
not a legal `missed_events` replay entry.

```rust,ignore
pub struct SessionPlanPayload {
    pub generation: Option<SessionGeneration>,
    pub topology: Topology,
    pub transport: TransportKind,
    pub host: Option<PlayerId>,
    pub direct_endpoint: Option<DirectEndpoint>,
    pub peers: Vec<SessionPeer>,
    pub ice_servers: Vec<IceServer>,
    pub fallback: TransportKind,
}
```

| Field | Type | Description |
|-------|------|-------------|
| `generation` | `Option<SessionGeneration>` | Server 0.6+ handshake generation. Required on server 0.7; optional in the SDK only to decode generation-less 0.4 plans. |
| `topology` | `Topology` | Chosen session topology (`relay`, `host`, or `mesh`). |
| `transport` | `TransportKind` | Chosen data-path transport (`relay`, `direct`, or `webrtc`). |
| `host` | `Option<PlayerId>` | The elected host, present only for `host` topology. |
| `direct_endpoint` | `Option<DirectEndpoint>` | Server-validated host and port for a `host + direct` plan. The built-in WebRTC controller exposes this value but does not open a direct socket. |
| `peers` | `Vec<SessionPeer>` | Peers this recipient should connect to (excludes the recipient itself). |
| `ice_servers` | `Vec<IceServer>` | ICE (STUN/TURN) servers for WebRTC; omitted for non-WebRTC plans. |
| `fallback` | `TransportKind` | The universal fallback transport — always `Relay`, the floor. |

The client accepts only the server's four canonical combinations:
`relay + relay`, `host + direct`, `host + webrtc`, and `mesh + webrtc`.
It also checks the required/forbidden host, direct-endpoint, peer, and ICE
fields, rejects self/duplicate/non-room peers, and validates direct host/port
syntax before replacing the prior plan. A rejected plan emits
`ProtocolViolation` and leaves the previous plan intact. The client always
obeys each accepted peer's server-assigned `initiate` value verbatim.

---

### `PeerSignal` (protocol v3)

The typed convenience view over the opaque `signal` field carried by
`ClientMessage::Signal` / `ServerMessage::Signal`. Those wire fields are
`serde_json::Value` so an unknown future signal shape can never break
deserialization; `PeerSignal` lets you work with the common shapes ergonomically
via its `From`/`TryFrom` conversions.

Every server 0.6+ client/server `Signal` also carries the current session
generation. The client stamps outgoing signals from its latest plan and drops
incoming signals before a plan or from another generation. Server 0.4 traffic
omits the field and remains generation-less end to end.

`PeerSignal` is **externally tagged** (serde's default for enums), byte-identical
to `matchbox_socket::PeerSignal`:

```rust,ignore
pub enum PeerSignal {
    Offer(String),
    Answer(String),
    IceCandidate(String),
}
```

| Variant | JSON value |
|---------|-----------|
| `Offer(sdp)` | `{ "Offer": "<sdp>" }` |
| `Answer(sdp)` | `{ "Answer": "<sdp>" }` |
| `IceCandidate(cand)` | `{ "IceCandidate": "<candidate>" }` |

```rust,ignore
use signal_fish_client::PeerSignal;

// PeerSignal <-> serde_json::Value
let value: serde_json::Value = PeerSignal::Offer(sdp).into();   // infallible
let signal = PeerSignal::try_from(&value)?;                     // fallible
```

!!! warning "External tagging, not the `{ type: ..., data: ... }` envelope"
    Unlike `ClientMessage`/`ServerMessage` (adjacently tagged), `PeerSignal`
    uses serde's default **external** tagging — the variant name is the key.
    This matches the matchbox wire format exactly.

---

## `ClientMessage`

Messages sent from the client to the server. There are **15 variants**, all
constructed internally by `SignalFishClient` methods — you never need to build
these by hand. `StartGame` is the protocol-v2 explicit-start message; `Signal`
and `TransportStatus` are protocol-v3 additions.

```rust,ignore
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ClientMessage { /* ... */ }
```

| Variant | Description |
|---------|-------------|
| `Authenticate` | Send App ID and optional SDK metadata. Must be the first message. |
| `JoinRoom` | Join or create a room for a specific game. |
| `LeaveRoom` | Leave the current room. |
| `GameData` | Send arbitrary JSON game data to other players. |
| `AuthorityRequest` | Request to become (or yield) the room authority. |
| `PlayerReady` | Signal readiness to start the game. |
| `ProvideConnectionInfo` | Provide your P2P connection info to peers. |
| `Ping` | Heartbeat to keep the connection alive. |
| `Reconnect` | Reconnect to a room after a disconnection. |
| `JoinAsSpectator` | Join a room as a read-only spectator. |
| `LeaveSpectator` | Leave spectator mode. |
| `RoomOperation` | **(negotiated v3)** Wrap one of the five directed room commands with a fresh canonical UUID after `room_operation_ids` is echoed. |
| `StartGame` | **(v2)** Explicitly start the game, finalizing the lobby (via `client.start_game()`). |
| `Signal` | **(v3)** Relay an opaque WebRTC signal to a single peer (via `client.send_signal(...)`). |
| `TransportStatus` | **(v3)** Report whether a data-path transport is established (via `client.report_transport_status(...)`). |

!!! note
    You don't construct `ClientMessage` values directly. Call the corresponding
    method on `SignalFishClient` instead — e.g. `client.join_room(...)`,
    `client.send_game_data(...)`, `client.ping()`.

---

## `ServerMessage`

Messages received from the server. There are **32 variants**. You don't parse
these manually — they arrive as `SignalFishEvent` variants through the event
channel. The mesh, delivery, and drain additions are sent only on a v3-negotiated
connection.

```rust,ignore
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ServerMessage { /* ... */ }
```

| Variant | Description |
|---------|-------------|
| `Authenticated` | Authentication successful. Contains app name, organization, and rate limits. |
| `ProtocolInfo` | SDK/protocol compatibility details sent after authentication. |
| `AuthenticationError` | Authentication failed with an error message and code. |
| `RoomJoined` | Successfully joined a room. Contains full room state. |
| `RoomJoinFailed` | Failed to join a room. |
| `RoomLeft` | Successfully left the room. |
| `PlayerJoined` | Another player joined the room. |
| `PlayerLeft` | Another player left the room. |
| `GameData` | Game data received from another player (JSON). |
| `GameDataBinary` | Binary game data received from another player. |
| `AuthorityChanged` | Room authority changed. |
| `AuthorityResponse` | Response to an authority request. |
| `LobbyStateChanged` | Lobby state changed (player readiness, room full, etc.). |
| `GameStarting` | Game is starting — includes peer connection info for all players. |
| `Pong` | Connection-scoped response to a `Ping`, including before authentication/negotiation completes. |
| `Reconnected` | Reconnection successful. Contains full room state and the canonical replayable control-event subset. Under v3, replay status, a rotated nonempty token, exact snapshot stamps, and complete matching sender watermarks are required. |
| `ReconnectionFailed` | Reconnection failed. |
| `PlayerReconnected` | Another player reconnected. |
| `SpectatorJoined` | Successfully joined as a spectator. |
| `SpectatorJoinFailed` | Failed to join as a spectator. |
| `SpectatorLeft` | Successfully left spectator mode. |
| `RoomOperationResult` | **(negotiated v3)** Echoes the exact UUID and carries an operation-specific terminal result or `OperationFailed`. |
| `NewSpectatorJoined` | Another spectator joined the room. |
| `SpectatorDisconnected` | Another spectator disconnected. |
| `Error` | Generic server error. |
| `Signal` | **(v3)** An opaque WebRTC signal relayed from a peer. |
| `NewPeer` | **(v3)** A late-joining peer to connect to after the session was finalized. |
| `SessionPlan` | **(v3)** The per-recipient authoritative plan for a finalized room, including an explicit relay reset when selected. |
| `PeerTransportStatus` | **(v3)** A peer's data-path transport state changed (informational). |
| `DeliveryReport` | **(v3)** Cumulative per-class outcomes plus exact omitted sequence ranges. |
| `RelayStats` | **(v3)** Optional cumulative connection-level relay diagnostics. |
| `GoingAway` | **(v3)** Best-effort server drain advisory preceding a structured close. |

!!! note
    You don't parse `ServerMessage` directly. The `SignalFishClient` run loop
    deserializes incoming JSON and emits typed `SignalFishEvent` variants
    through the event receiver. See the [Events](events.md) page for details.

---

## New optional fields (protocol v3)

v3 stays backward compatible by **adding optional fields to existing messages**.
Each is `Option` + `skip_serializing_if` (or a `Vec` skipped when empty), so a v2
connection that sets none of them produces byte-identical v2 JSON. A v2 client
safely ignores any of these it doesn't recognize.

| Message | New field(s) | Purpose |
|---------|--------------|---------|
| `ClientMessage::Authenticate` | `protocol_version`, `supported_transports`, `supported_topologies`, `requested_capabilities` | Advertise the highest version, data-path transports, topologies, and additive tokens the client can fulfill. V3-capable configurations request `room_operation_ids`; default and explicit-v2 configurations omit it. |
| `ServerMessage::ProtocolInfo` | `protocol_version`, `min_protocol_version`, `max_protocol_version`, `capabilities` | The negotiated version and exact enabled capability tokens. A requested capability is inactive until echoed. |
| `RoomJoinedPayload` / `ReconnectedPayload` | `ice_servers: Vec<IceServer>` | ICE pre-gather: STUN/TURN servers delivered during the lobby wait so WebRTC candidate gathering can start early. Empty (and absent from the wire) for v2. |
| `ReconnectedPayload` | `replay`, `sender_watermarks`, `reconnection_token` | Required v3 authoritative replay/accountability baseline and rotated reconnect credential. All are absent/empty under v2. |

### Correlated room operations (negotiated v3)

`RoomOperationId` is a client-generated UUID serialized in lowercase hyphenated
form. After `room_operation_ids` is requested and echoed on v3, `JoinRoom`,
`LeaveRoom`, `Reconnect`, `JoinAsSpectator`, and `LeaveSpectator` are carried in
`RoomOperation`; their terminal response arrives in `RoomOperationResult` with
the identical ID. IDs are correlation fences, not idempotency keys: the server
echoes them but does not deduplicate operations. A fresh physical connection
negotiates a new scope. Autonomous spectator removal, disconnection, and room
closure remain top-level `SpectatorLeft` messages.

---

## Wire Format

Both `ClientMessage` and `ServerMessage` use **adjacently-tagged** serde
encoding:

```rust,ignore
#[serde(tag = "type", content = "data")]
```

Every message on the wire is a JSON object with two top-level keys:

- **`type`** — the variant name (e.g. `"Authenticate"`, `"RoomJoined"`)
- **`data`** — the variant's payload (an object, or absent for unit variants)

??? example "Example — `Authenticate` message"
    ```json
    {
        "type": "Authenticate",
        "data": {
            "app_id": "mb_app_abc123",
            "sdk_version": "0.10.0"
        }
    }
    ```

    Optional fields (`platform`, `game_data_format`) are omitted when `None`
    thanks to `#[serde(skip_serializing_if = "Option::is_none")]`.

??? example "Example — `Pong` (unit variant)"
    ```json
    {
        "type": "Pong"
    }
    ```

!!! tip "Inspecting traffic"
    Because all messages are plain JSON, you can inspect WebSocket frames with
    browser developer tools or a tool like `websocat` for debugging.

---

## Exhaustive Enums

All public enums in this crate (`SignalFishEvent`, `ErrorCode`, `SignalFishError`, etc.) are
**exhaustive**. Adding new variants is a semver breaking change, so your `match`
expressions should stay explicit to preserve compile-time detection when a major
version introduces new variants.

You can match exhaustively without a wildcard arm:

```rust,ignore
match event {
    SignalFishEvent::Authenticated { .. } => { /* handle */ }
    SignalFishEvent::RoomJoined { .. } => { /* handle */ }
    // … handle every variant …
}
```

Avoid `_ => {}` catch-all arms for public enum matches so unhandled variants
remain compile-time errors.
