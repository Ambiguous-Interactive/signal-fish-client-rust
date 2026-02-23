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

```rust
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

Selects the transport protocol for relay connections.

- **Default:** `Auto`
- **Serde:** `rename_all = "lowercase"`

```rust
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
| `Tcp` | `"tcp"` | TCP transport — reliable, ordered delivery. Recommended for turn-based games, lobby systems, RPGs. |
| `Udp` | `"udp"` | UDP transport — low-latency, unreliable. Recommended for FPS, racing, real-time action. |
| `Websocket` | `"websocket"` | WebSocket transport — reliable, browser-compatible. Recommended for WebGL and cross-platform builds. |
| `Auto` | `"auto"` | Automatic selection based on room size and game type (default). |

---

### `GameDataEncoding`

Encoding format for sequenced game-data payloads.

- **Default:** `Json`
- **Serde:** `rename_all = "snake_case"` (with per-variant overrides)

```rust
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
| `Rkyv` | `"rkyv"` | Rkyv zero-copy binary format for maximum performance. |

---

### `ConnectionInfo`

Connection information for peer-to-peer establishment. This is an internally
tagged enum (`serde(tag = "type")`), so each variant includes a `"type"`
discriminator field in JSON.

```rust
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
| `Relay` | `host: String`, `port: u16`, `transport: RelayTransport`, `allocation_id: String`, `token: String`, `client_id: Option<u16>` | Built-in relay server (Unity NetCode, FishNet, Mirror). |
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

```rust
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

```rust
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

```rust
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

```rust
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

```rust
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
| `relay_type` | `String` | Relay type label (e.g. `"direct"`, `"relay"`). |
| `connection_info` | `Option<ConnectionInfo>` | Connection info provided by the peer for P2P establishment. |

---

### `RateLimitInfo`

Rate-limit information returned after authentication.

```rust
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

```rust
pub struct ProtocolInfoPayload {
    pub platform: Option<String>,
    pub sdk_version: Option<String>,
    pub minimum_version: Option<String>,
    pub recommended_version: Option<String>,
    pub capabilities: Vec<String>,
    pub notes: Option<String>,
    pub game_data_formats: Vec<GameDataEncoding>,
    pub player_name_rules: Option<PlayerNameRulesPayload>,
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
| `game_data_formats` | `Vec<GameDataEncoding>` | Game-data encodings the server supports. |
| `player_name_rules` | `Option<PlayerNameRulesPayload>` | Validation rules for player names (if enforced). |

---

### `PlayerNameRulesPayload`

Describes the characters a deployment allows inside player names.

```rust
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

## `ClientMessage`

Messages sent from the client to the server. There are **11 variants**, all
constructed internally by `SignalFishClient` methods — you never need to build
these by hand.

```rust
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

!!! note
    You don't construct `ClientMessage` values directly. Call the corresponding
    method on `SignalFishClient` instead — e.g. `client.join_room(...)`,
    `client.send_game_data(...)`, `client.ping()`.

---

## `ServerMessage`

Messages received from the server. There are **24 variants**. You don't parse
these manually — they arrive as `SignalFishEvent` variants through the event
channel.

```rust
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
| `Pong` | Response to a `Ping`. |
| `Reconnected` | Reconnection successful. Contains full room state and missed events. |
| `ReconnectionFailed` | Reconnection failed. |
| `PlayerReconnected` | Another player reconnected. |
| `SpectatorJoined` | Successfully joined as a spectator. |
| `SpectatorJoinFailed` | Failed to join as a spectator. |
| `SpectatorLeft` | Successfully left spectator mode. |
| `NewSpectatorJoined` | Another spectator joined the room. |
| `SpectatorDisconnected` | Another spectator disconnected. |
| `Error` | Generic server error. |

!!! note
    You don't parse `ServerMessage` directly. The `SignalFishClient` run loop
    deserializes incoming JSON and emits typed `SignalFishEvent` variants
    through the event receiver. See the [Events](events.md) page for details.

---

## Wire Format

Both `ClientMessage` and `ServerMessage` use **adjacently-tagged** serde
encoding:

```rust
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
            "sdk_version": "0.2.2"
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

```rust
match event {
    SignalFishEvent::Authenticated { .. } => { /* handle */ }
    SignalFishEvent::RoomJoined { .. } => { /* handle */ }
    // … handle every variant …
}
```

Avoid `_ => {}` catch-all arms for public enum matches so unhandled variants
remain compile-time errors.
