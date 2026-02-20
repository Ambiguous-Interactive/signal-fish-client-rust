//! Wire-compatible protocol types for the Signal Fish signaling protocol.
//!
//! Every type in this module produces identical JSON to the server's
//! `protocol::messages` and `protocol::types` modules. Key adaptations:
//!
//! - `bytes::Bytes` → `Vec<u8>` with `#[serde(with = "serde_bytes")]`
//! - `chrono::DateTime<Utc>` → `String` (ISO 8601)
//! - No `rkyv` derives (server-only concern)

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error_codes::ErrorCode;

// ── Type aliases ────────────────────────────────────────────────────

/// Unique identifier for players.
pub type PlayerId = Uuid;

/// Unique identifier for rooms.
pub type RoomId = Uuid;

// ── Enums ───────────────────────────────────────────────────────────

/// Relay transport protocol selection.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum RelayTransport {
    /// TCP transport (reliable, ordered delivery).
    /// Recommended for: Turn-based games, lobby systems, RPGs.
    Tcp,
    /// UDP transport (low-latency, unreliable).
    /// Recommended for: FPS, racing games, real-time action.
    Udp,
    /// WebSocket transport (reliable, browser-compatible).
    /// Recommended for: WebGL builds, browser games, cross-platform.
    Websocket,
    /// Automatic selection based on room size and game type.
    /// Default: UDP for 2-4 players, TCP for 5+ players, WebSocket for browser builds.
    #[default]
    Auto,
}

/// Encoding format for sequenced game data payloads.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum GameDataEncoding {
    /// JSON payloads delivered over text frames.
    #[default]
    Json,
    /// MessagePack payloads delivered over binary frames.
    #[serde(rename = "message_pack")]
    MessagePack,
    /// Rkyv zero-copy binary format for maximum performance.
    /// Recommended for: High-frequency updates, large player counts, latency-sensitive games.
    #[serde(rename = "rkyv")]
    Rkyv,
}

/// Connection information for P2P establishment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ConnectionInfo {
    /// Direct IP:port connection (for Mirror, FishNet, Unity NetCode direct).
    #[serde(rename = "direct")]
    Direct { host: String, port: u16 },
    /// Unity Relay allocation (for Unity NetCode via Unity Relay).
    #[serde(rename = "unity_relay")]
    UnityRelay {
        allocation_id: String,
        connection_data: String,
        key: String,
    },
    /// Built-in relay server (for Unity NetCode, FishNet, Mirror).
    #[serde(rename = "relay")]
    Relay {
        /// Relay server host.
        host: String,
        /// Relay server port (TCP or UDP depending on transport).
        port: u16,
        /// Transport protocol (TCP, UDP, or Auto).
        #[serde(default)]
        transport: RelayTransport,
        /// Allocation ID (room ID).
        allocation_id: String,
        /// Client authentication token (opaque server-issued value).
        token: String,
        /// Assigned client ID (set by server after connection).
        #[serde(skip_serializing_if = "Option::is_none")]
        client_id: Option<u16>,
    },
    /// WebRTC connection info (for Matchbox).
    #[serde(rename = "webrtc")]
    WebRTC {
        sdp: Option<String>,
        ice_candidates: Vec<String>,
    },
    /// Custom connection data (extensible for other types).
    #[serde(rename = "custom")]
    Custom { data: serde_json::Value },
}

/// Describes why a spectator state change occurred.
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

/// Lobby readiness state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LobbyState {
    #[default]
    Waiting,
    Lobby,
    Finalized,
}

// ── Structs ─────────────────────────────────────────────────────────

/// Information about a player in a room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerInfo {
    pub id: PlayerId,
    pub name: String,
    pub is_authority: bool,
    pub is_ready: bool,
    pub connected_at: String,
    /// Connection info for P2P establishment (provided when player is ready).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_info: Option<ConnectionInfo>,
}

/// Information about a spectator watching a room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpectatorInfo {
    pub id: PlayerId,
    pub name: String,
    pub connected_at: String,
}

/// Peer connection information for game start.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerConnectionInfo {
    pub player_id: PlayerId,
    pub player_name: String,
    pub is_authority: bool,
    pub relay_type: String,
    /// Connection info provided by the peer for P2P establishment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_info: Option<ConnectionInfo>,
}

/// Rate limit information for an application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitInfo {
    /// Requests allowed per minute.
    pub per_minute: u32,
    /// Requests allowed per hour.
    pub per_hour: u32,
    /// Requests allowed per day.
    pub per_day: u32,
}

/// Describes negotiated protocol capabilities for a specific SDK.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolInfoPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended_version: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default)]
    pub game_data_formats: Vec<GameDataEncoding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player_name_rules: Option<PlayerNameRulesPayload>,
}

/// Describes the characters a deployment allows inside `player_name`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerNameRulesPayload {
    pub max_length: usize,
    pub min_length: usize,
    pub allow_unicode_alphanumeric: bool,
    pub allow_spaces: bool,
    pub allow_leading_trailing_whitespace: bool,
    #[serde(default)]
    pub allowed_symbols: Vec<char>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_allowed_characters: Option<String>,
}

// ── Payload structs ─────────────────────────────────────────────────

/// Payload for the `RoomJoined` server message.
/// Boxed in `ServerMessage` to reduce enum size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomJoinedPayload {
    pub room_id: RoomId,
    pub room_code: String,
    pub player_id: PlayerId,
    pub game_name: String,
    pub max_players: u8,
    pub supports_authority: bool,
    pub current_players: Vec<PlayerInfo>,
    pub is_authority: bool,
    pub lobby_state: LobbyState,
    pub ready_players: Vec<PlayerId>,
    pub relay_type: String,
    /// List of spectators currently watching (if any).
    #[serde(default)]
    pub current_spectators: Vec<SpectatorInfo>,
}

/// Payload for the `Reconnected` server message.
/// Boxed in `ServerMessage` to reduce enum size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconnectedPayload {
    pub room_id: RoomId,
    pub room_code: String,
    pub player_id: PlayerId,
    pub game_name: String,
    pub max_players: u8,
    pub supports_authority: bool,
    pub current_players: Vec<PlayerInfo>,
    pub is_authority: bool,
    pub lobby_state: LobbyState,
    pub ready_players: Vec<PlayerId>,
    pub relay_type: String,
    /// List of spectators currently watching (if any).
    #[serde(default)]
    pub current_spectators: Vec<SpectatorInfo>,
    /// Events that occurred while disconnected.
    pub missed_events: Vec<ServerMessage>,
}

/// Payload for the `SpectatorJoined` server message.
/// Boxed in `ServerMessage` to reduce enum size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpectatorJoinedPayload {
    pub room_id: RoomId,
    pub room_code: String,
    pub spectator_id: PlayerId,
    pub game_name: String,
    pub current_players: Vec<PlayerInfo>,
    pub current_spectators: Vec<SpectatorInfo>,
    pub lobby_state: LobbyState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<SpectatorStateChangeReason>,
}

// ── Messages ────────────────────────────────────────────────────────

/// Message types sent from client to server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ClientMessage {
    /// Authenticate with App ID (MUST be first message).
    /// App ID is a public identifier (not a secret!) that identifies the game application.
    Authenticate {
        /// Public App ID (safe to embed in game builds, e.g., "mb_app_abc123...").
        app_id: String,
        /// SDK version for debugging and analytics.
        #[serde(skip_serializing_if = "Option::is_none")]
        sdk_version: Option<String>,
        /// Platform information (e.g., "unity", "godot", "unreal").
        #[serde(skip_serializing_if = "Option::is_none")]
        platform: Option<String>,
        /// Preferred game data encoding (defaults to JSON text frames).
        #[serde(skip_serializing_if = "Option::is_none")]
        game_data_format: Option<GameDataEncoding>,
    },
    /// Join or create a room for a specific game.
    JoinRoom {
        game_name: String,
        room_code: Option<String>,
        player_name: String,
        max_players: Option<u8>,
        supports_authority: Option<bool>,
        /// Preferred relay transport protocol (TCP, UDP, or Auto).
        /// If not specified, defaults to Auto.
        #[serde(default)]
        relay_transport: Option<RelayTransport>,
    },
    /// Leave the current room.
    LeaveRoom,
    /// Send game data to other players in the room.
    GameData { data: serde_json::Value },
    /// Request to become or connect to authoritative server.
    AuthorityRequest { become_authority: bool },
    /// Signal readiness to start the game in lobby.
    PlayerReady,
    /// Provide connection info for P2P establishment.
    ProvideConnectionInfo { connection_info: ConnectionInfo },
    /// Heartbeat to maintain connection.
    Ping,
    /// Reconnect to a room after disconnection.
    Reconnect {
        player_id: PlayerId,
        room_id: RoomId,
        /// Authentication token generated on initial join.
        auth_token: String,
    },
    /// Join a room as a spectator (read-only observer).
    JoinAsSpectator {
        game_name: String,
        room_code: String,
        spectator_name: String,
    },
    /// Leave spectator mode.
    LeaveSpectator,
}

/// Message types sent from server to client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ServerMessage {
    /// Authentication successful.
    Authenticated {
        /// App name for confirmation.
        app_name: String,
        /// Organization name (if any).
        #[serde(skip_serializing_if = "Option::is_none")]
        organization: Option<String>,
        /// Rate limits for this app.
        rate_limits: RateLimitInfo,
    },
    /// SDK/protocol compatibility details advertised after authentication.
    ProtocolInfo(ProtocolInfoPayload),
    /// Authentication failed.
    AuthenticationError {
        /// Error message.
        error: String,
        /// Error code for programmatic handling.
        error_code: ErrorCode,
    },
    /// Successfully joined a room (boxed to reduce enum size).
    RoomJoined(Box<RoomJoinedPayload>),
    /// Failed to join room.
    RoomJoinFailed {
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<ErrorCode>,
    },
    /// Successfully left room.
    RoomLeft,
    /// Another player joined the room.
    PlayerJoined { player: PlayerInfo },
    /// Another player left the room.
    PlayerLeft { player_id: PlayerId },
    /// Game data from another player.
    GameData {
        from_player: PlayerId,
        data: serde_json::Value,
    },
    /// Binary game data payload from another player.
    /// Uses `Vec<u8>` with `serde_bytes` for efficient serialization.
    GameDataBinary {
        from_player: PlayerId,
        encoding: GameDataEncoding,
        #[serde(with = "serde_bytes")]
        payload: Vec<u8>,
    },
    /// Authority status changed.
    AuthorityChanged {
        authority_player: Option<PlayerId>,
        you_are_authority: bool,
    },
    /// Authority request response.
    AuthorityResponse {
        granted: bool,
        reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<ErrorCode>,
    },
    /// Lobby state changed (room full, player readiness changed, etc.).
    LobbyStateChanged {
        lobby_state: LobbyState,
        ready_players: Vec<PlayerId>,
        all_ready: bool,
    },
    /// Game is starting with peer connection information.
    GameStarting {
        peer_connections: Vec<PeerConnectionInfo>,
    },
    /// Pong response to ping.
    Pong,
    /// Reconnection successful (boxed to reduce enum size).
    Reconnected(Box<ReconnectedPayload>),
    /// Reconnection failed.
    ReconnectionFailed {
        reason: String,
        error_code: ErrorCode,
    },
    /// Another player reconnected to the room.
    PlayerReconnected { player_id: PlayerId },
    /// Successfully joined a room as spectator (boxed to reduce enum size).
    SpectatorJoined(Box<SpectatorJoinedPayload>),
    /// Failed to join as spectator.
    SpectatorJoinFailed {
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<ErrorCode>,
    },
    /// Successfully left spectator mode.
    SpectatorLeft {
        #[serde(skip_serializing_if = "Option::is_none")]
        room_id: Option<RoomId>,
        #[serde(skip_serializing_if = "Option::is_none")]
        room_code: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<SpectatorStateChangeReason>,
        #[serde(default)]
        current_spectators: Vec<SpectatorInfo>,
    },
    /// Another spectator joined the room.
    NewSpectatorJoined {
        spectator: SpectatorInfo,
        #[serde(default)]
        current_spectators: Vec<SpectatorInfo>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<SpectatorStateChangeReason>,
    },
    /// Another spectator left the room.
    SpectatorDisconnected {
        spectator_id: PlayerId,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<SpectatorStateChangeReason>,
        #[serde(default)]
        current_spectators: Vec<SpectatorInfo>,
    },
    /// Error message.
    Error {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<ErrorCode>,
    },
}
