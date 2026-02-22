# Serde Patterns

Reference for serde_json, enum tagging, field attributes, and wire format as used in this codebase.

## Core Imports

```rust
use serde::{Deserialize, Serialize};
use serde_json;
```

## Protocol Message Tagging (CRITICAL)

Both `ClientMessage` and `ServerMessage` use **adjacent tagging**:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ClientMessage {
    Authenticate { app_id: String, /* ... */ },
    JoinRoom { game_name: String, /* ... */ },
    LeaveRoom,
    // ...
}
```

This produces JSON like:

```json
{ "type": "Authenticate", "data": { "app_id": "mb_app_abc123" } }
{ "type": "JoinRoom", "data": { "game_name": "my-game", "player_name": "Alice" } }
{ "type": "LeaveRoom" }
```

Variant names are **PascalCase** in JSON (serde default — no `rename_all` on
the enum itself). This matches the Signal Fish server v2 protocol exactly.
Do NOT add `rename_all` to `ClientMessage` or `ServerMessage` without verifying
against the server.

`ServerMessage` uses the same `#[serde(tag = "type", content = "data")]`
pattern:

```json
{ "type": "Authenticated", "data": { "app_name": "...", "rate_limits": { ... } } }
{ "type": "RoomJoined", "data": { "room_id": "...", "room_code": "ABC123", ... } }
```

## Field Naming: snake_case (not camelCase)

Protocol struct fields use **snake_case** in JSON (serde default for struct
fields). Do NOT apply `rename_all = "camelCase"` to protocol structs unless
the server explicitly uses camelCase for that field.

```rust
pub struct RoomJoinedPayload {
    pub room_id: RoomId,       // → "room_id" in JSON
    pub room_code: String,     // → "room_code" in JSON
    pub player_id: PlayerId,   // → "player_id" in JSON
    pub game_name: String,     // → "game_name" in JSON
}
```

## ErrorCode Serialization

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    RoomNotFound,       // → "ROOM_NOT_FOUND"
    RoomFull,           // → "ROOM_FULL"
    InternalError,      // → "INTERNAL_ERROR"
    // ...
}
```

## Other Enum Serialization

Other enums use `rename_all = "snake_case"` or `rename_all = "lowercase"`:

```rust
// RelayTransport: lowercase
#[serde(rename_all = "lowercase")]
pub enum RelayTransport { Tcp, Udp, Websocket, Auto }
// → "tcp", "udp", "websocket", "auto"

// GameDataEncoding: snake_case (with some explicit renames)
#[serde(rename_all = "snake_case")]
pub enum GameDataEncoding {
    Json,
    #[serde(rename = "message_pack")]
    MessagePack,
    #[serde(rename = "rkyv")]
    Rkyv,
}

// LobbyState: snake_case
#[serde(rename_all = "snake_case")]
pub enum LobbyState { Waiting, Lobby, Finalized }

// ConnectionInfo: internally tagged; uses explicit #[serde(rename = "...")] on each variant
#[serde(tag = "type")]
pub enum ConnectionInfo {
    #[serde(rename = "direct")]
    Direct { host: String, port: u16 },
    #[serde(rename = "unity_relay")]
    UnityRelay { /* ... */ },
    #[serde(rename = "relay")]
    Relay { /* ... */ },
    // ...
}
```

## Optional Fields

```rust
// Omit when None (common in ClientMessage)
#[serde(skip_serializing_if = "Option::is_none")]
pub sdk_version: Option<String>,

// Default to empty vec if absent
#[serde(default)]
pub current_spectators: Vec<SpectatorInfo>,
```

## Binary Payloads with serde_bytes

`GameDataBinary` carries raw bytes. `serde_bytes` provides efficient
serialization without the `bytes` crate:

```rust
#[derive(Serialize, Deserialize)]
pub struct GameDataBinary {
    pub from_player: PlayerId,
    pub encoding: GameDataEncoding,
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
}
```

**cargo-machete note:** Because `serde_bytes` is referenced only via the
`#[serde(with = "serde_bytes")]` attribute (no `use` statement), cargo-machete
will report it as unused. It must be listed in
`[package.metadata.cargo-machete] ignored` in `Cargo.toml`.

## UUID Serialization

```toml
uuid = { version = "1", features = ["v4", "serde"] }
```

With the `serde` feature, `Uuid` serializes as a lowercase hyphenated string:

```json
"player_id": "550e8400-e29b-41d4-a716-446655440000"
```

## Parsing Protocol Messages

```rust
// Deserialize incoming server message
let msg: ServerMessage = serde_json::from_str(&raw_text)?;

// Serialize outgoing client message
let json: String = serde_json::to_string(&client_msg)?;
```

## Debugging Serde Issues

```rust
// Pretty-print for debugging
let pretty = serde_json::to_string_pretty(&msg).unwrap();
println!("{pretty}");

// Deserialize into generic Value first to inspect
let value: serde_json::Value = serde_json::from_str(raw)?;
println!("{:#?}", value);

// Check tag field
assert_eq!(value["type"], "JoinRoom");
assert_eq!(value["data"]["game_name"], "my-game");
```

## Common Attribute Summary

| Attribute | Effect |
|-----------|--------|
| `#[serde(tag = "type", content = "data")]` | Adjacent tagging (ClientMessage, ServerMessage) |
| `#[serde(tag = "type")]` | Internal tagging (ConnectionInfo) |
| `#[serde(rename = "name")]` | Override variant/field name in JSON |
| `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]` | ErrorCode → `"ROOM_NOT_FOUND"` |
| `#[serde(rename_all = "snake_case")]` | LobbyState, GameDataEncoding |
| `#[serde(rename_all = "lowercase")]` | RelayTransport |
| `#[serde(skip_serializing_if = "Option::is_none")]` | Omit None fields |
| `#[serde(default)]` | Use Default when field absent in input |
| `#[serde(with = "serde_bytes")]` | Efficient byte array (Vec<u8>) |
