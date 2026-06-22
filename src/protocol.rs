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

/// Session topology selected by the server for a finalized room (protocol v3).
///
/// The server is authoritative: it chooses the topology and communicates the
/// result via [`SessionPlanPayload`]. The client obeys it; it never computes a
/// topology itself.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Topology {
    /// Server relay hub — the v2 behavior, always available (the "relay floor").
    Relay,
    /// Star topology around a single elected host/authority.
    Host,
    /// Full mesh: every peer connects to every other peer.
    Mesh,
}

/// Data-path transport selected by the server for a finalized room (protocol v3).
///
/// Distinct from the [`Transport`](crate::Transport) I/O trait: `TransportKind`
/// is a wire *value* the server sends to describe how peers should exchange game
/// data, whereas `Transport` is the byte channel to the signaling server.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    /// Server WebSocket fan-out — the mandatory floor every client supports.
    Relay,
    /// Direct IP:port connection (LAN / routable host).
    Direct,
    /// Peer-to-peer WebRTC data channel.
    ///
    /// `rename_all = "snake_case"` would emit `web_rtc`; the protocol requires
    /// the token `webrtc`, so the variant is renamed explicitly. This token
    /// deliberately matches [`ConnectionInfo::WebRTC`]'s `#[serde(rename = "webrtc")]`.
    #[serde(rename = "webrtc")]
    WebRtc,
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
    /// Protocol version negotiated for this connection (protocol v3+ only).
    ///
    /// `None` for a negotiated v2 connection, keeping the v2 wire contract
    /// byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<u16>,
    /// Lowest protocol version this deployment accepts (protocol v3+ only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_protocol_version: Option<u16>,
    /// Highest protocol version this deployment speaks (protocol v3+ only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_protocol_version: Option<u16>,
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

/// A STUN/TURN server for WebRTC ICE negotiation (protocol v3).
///
/// `username`/`credential` are present only for TURN servers; bare STUN entries
/// omit them, keeping the wire bytes minimal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IceServer {
    /// STUN/TURN URLs (e.g. `stun:stun.l.google.com:19302`).
    pub urls: Vec<String>,
    /// TURN username (omitted for credential-less STUN servers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// TURN credential (omitted for credential-less STUN servers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

/// A peer the recipient should connect to within a [`SessionPlanPayload`] (protocol v3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionPeer {
    /// The other peer's identifier.
    pub player_id: PlayerId,
    /// The other peer's display name.
    pub player_name: String,
    /// Whether this peer is the session's authoritative host.
    pub is_authority: bool,
    /// Whether the recipient sends the WebRTC offer to this peer.
    ///
    /// Server-assigned (a deterministic "designated offerer"). Obey it
    /// verbatim — the client never computes who initiates.
    pub initiate: bool,
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
    /// ICE (STUN/TURN) servers for early WebRTC candidate gathering during the
    /// lobby wait (protocol v3 only; "ICE pre-gather"). Empty — and absent from
    /// the wire via `skip_serializing_if` — for v2 connections, keeping the v2
    /// JSON byte-identical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ice_servers: Vec<IceServer>,
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
    /// ICE (STUN/TURN) servers for early WebRTC candidate gathering (protocol
    /// v3 only). Empty — and absent from the wire — for v2 connections.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ice_servers: Vec<IceServer>,
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

/// Payload for the `SessionPlan` server message (protocol v3).
/// Boxed in `ServerMessage` to reduce enum size.
///
/// Sent per-recipient when a room finalizes to a non-relay session (and again
/// on late-join or host re-election). Each recipient receives a plan tailored
/// to it: `peers` excludes the recipient, and each `initiate` flag is set from
/// the recipient's perspective.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPlanPayload {
    /// Chosen session topology (`relay`, `host`, or `mesh`).
    pub topology: Topology,
    /// Chosen data-path transport (`relay`, `direct`, or `webrtc`).
    pub transport: TransportKind,
    /// The elected host, present only for `host` topology.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<PlayerId>,
    /// Peers this recipient should connect to (excludes the recipient itself).
    pub peers: Vec<SessionPeer>,
    /// ICE (STUN/TURN) servers for WebRTC; omitted for non-WebRTC plans.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ice_servers: Vec<IceServer>,
    /// The universal fallback transport — always [`TransportKind::Relay`], the floor.
    pub fallback: TransportKind,
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
        /// Highest protocol version the client speaks (protocol v3+).
        ///
        /// Omitted by default so the server treats the client as v2 relay-only
        /// and the wire bytes stay identical to v2.
        #[serde(skip_serializing_if = "Option::is_none")]
        protocol_version: Option<u16>,
        /// Data-path transports the client can actually fulfill (protocol v3+).
        #[serde(skip_serializing_if = "Option::is_none")]
        supported_transports: Option<Vec<TransportKind>>,
        /// Session topologies the client can participate in (protocol v3+).
        #[serde(skip_serializing_if = "Option::is_none")]
        supported_topologies: Option<Vec<Topology>>,
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
    /// Explicitly start the game, finalizing the lobby with its current members
    /// (protocol v2).
    ///
    /// Accepted only when every current player is ready. If the room has a
    /// designated authority, only that authority may start; otherwise any
    /// member may. On success the server broadcasts `GameStarting` (and, for a
    /// negotiated v3 non-relay room, a per-recipient `SessionPlan`).
    StartGame,
    /// Relay an opaque WebRTC signal to a single peer.
    ///
    /// **Protocol v3 only.** Rejected on a relay-floor (v2) connection.
    ///
    /// The `signal` is forwarded verbatim by the server. It is typically a
    /// [`PeerSignal`](crate::PeerSignal) (`{"Offer"|"Answer"|"IceCandidate": …}`)
    /// but is modeled as `serde_json::Value` so unknown future shapes never
    /// fail to round-trip.
    Signal {
        /// The recipient peer.
        to: PlayerId,
        /// The opaque signal payload (offer/answer/ICE candidate).
        signal: serde_json::Value,
    },
    /// Report whether a data-path transport to peers is currently established.
    ///
    /// **Protocol v3 only.** Informational; the server fans it out as
    /// `PeerTransportStatus` and uses it for fallback decisions.
    TransportStatus {
        /// The transport being reported on.
        transport: TransportKind,
        /// Whether that transport is currently connected.
        connected: bool,
    },
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
    /// An opaque WebRTC signal relayed from a peer.
    ///
    /// **Protocol v3 only.** Sent only on a v3-negotiated connection.
    ///
    /// `signal` is modeled as `serde_json::Value` (typically a
    /// [`PeerSignal`](crate::PeerSignal)) so unknown future shapes always
    /// round-trip. Convert with `PeerSignal::try_from(&signal)`.
    Signal {
        /// The peer the signal came from.
        from: PlayerId,
        /// The opaque signal payload (offer/answer/ICE candidate).
        signal: serde_json::Value,
    },
    /// A new peer to connect to after the session was finalized — a late joiner.
    ///
    /// **Protocol v3 only.** Sent only on a v3-negotiated connection.
    NewPeer {
        /// The new peer's identifier.
        peer_id: PlayerId,
        /// Whether the recipient sends the WebRTC offer to this peer.
        /// Server-assigned; obey verbatim.
        you_initiate: bool,
    },
    /// Per-recipient session plan for a finalized non-relay room.
    ///
    /// **Protocol v3 only.** Boxed to reduce enum size. May be received multiple
    /// times (host re-election / late-join); each one fully replaces the
    /// previous plan.
    SessionPlan(Box<SessionPlanPayload>),
    /// A peer's data-path transport state changed. Informational.
    ///
    /// **Protocol v3 only.** Sent only on a v3-negotiated connection.
    PeerTransportStatus {
        /// The peer whose transport state changed.
        peer_id: PlayerId,
        /// The transport being reported on.
        transport: TransportKind,
        /// Whether that transport is currently connected for the peer.
        connected: bool,
    },
}

/// The negotiated protocol version to restore from a reconnect's `missed_events`,
/// if any — the last versioned (v3+) [`ProtocolInfo`](ServerMessage::ProtocolInfo)
/// replayed in the batch.
///
/// A replayed v2 `ProtocolInfo` (version `None`) is ignored so it can never
/// silently downgrade an already-negotiated v3 session; later versioned entries
/// win over earlier ones. Shared by the async and polling clients so the two
/// reconnect paths cannot drift apart.
///
/// Gated to its consumers (the async client needs `tokio-runtime`; the polling
/// client needs `polling-client`) so it is not dead code in a build with neither.
#[cfg(any(feature = "tokio-runtime", feature = "polling-client"))]
#[must_use]
pub(crate) fn replayed_negotiated_version(missed_events: &[ServerMessage]) -> Option<u16> {
    missed_events.iter().rev().find_map(|msg| match msg {
        ServerMessage::ProtocolInfo(info) => info.protocol_version,
        _ => None,
    })
}
