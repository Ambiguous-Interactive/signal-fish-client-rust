//! Error codes for structured error handling in the Signal Fish protocol.
//!
//! These codes are wire-compatible with the server's `ErrorCode` enum and
//! serialize using `SCREAMING_SNAKE_CASE` to match the server's JSON format.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Structured error codes returned by the Signal Fish server.
///
/// Each variant corresponds to a specific error condition. The server sends these
/// as `"SCREAMING_SNAKE_CASE"` strings (e.g., `"ROOM_NOT_FOUND"`).
///
/// Use [`description()`](ErrorCode::description) for a human-readable explanation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    // Authentication errors
    /// Authentication credentials were missing or invalid.
    Unauthorized,
    /// Compatibility only (see [`NON_EMITTED`](ErrorCode::NON_EMITTED)): no
    /// longer emitted by Server 0.7+; retained so older deployments stay
    /// decodable.
    InvalidToken,
    /// Compatibility only (see [`NON_EMITTED`](ErrorCode::NON_EMITTED)): no
    /// longer emitted by Server 0.7+; retained so older deployments stay
    /// decodable.
    AuthenticationRequired,
    /// The provided application ID is unrecognized or unacceptable.
    InvalidAppId,
    /// Compatibility only (see [`NON_EMITTED`](ErrorCode::NON_EMITTED)): no
    /// longer emitted by Server 0.7+; retained so older deployments stay
    /// decodable.
    AppIdExpired,
    /// Compatibility only (see [`NON_EMITTED`](ErrorCode::NON_EMITTED)): no
    /// longer emitted by Server 0.7+; retained so older deployments stay
    /// decodable.
    AppIdRevoked,
    /// Compatibility only (see [`NON_EMITTED`](ErrorCode::NON_EMITTED)): no
    /// longer emitted by Server 0.7+; retained so older deployments stay
    /// decodable.
    AppIdSuspended,
    /// No application ID was provided; the server requires one.
    MissingAppId,
    /// Authentication did not complete within the server's time limit.
    AuthenticationTimeout,
    /// This SDK version is no longer supported by the server.
    SdkVersionUnsupported,
    /// The requested game-data format is unsupported; the server falls back
    /// to JSON.
    UnsupportedGameDataFormat,

    // Validation errors
    /// A request parameter was invalid or malformed.
    InvalidInput,
    /// The game name violates the server's naming requirements.
    InvalidGameName,
    /// The room code is invalid or malformed.
    InvalidRoomCode,
    /// The player name violates the server's naming requirements.
    InvalidPlayerName,
    /// The requested maximum player count is invalid or out of range.
    InvalidMaxPlayers,
    /// The message exceeds the server's size limit.
    MessageTooLarge,

    // Room errors
    /// The requested room does not exist (closed, or wrong code).
    RoomNotFound,
    /// The room has reached its advertised player capacity.
    RoomFull,
    /// The connection is already a participant of a room.
    AlreadyInRoom,
    /// The operation requires room membership the connection does not have.
    NotInRoom,
    /// The server failed to create the requested room.
    RoomCreationFailed,
    /// The game already has the maximum number of rooms the server allows.
    MaxRoomsPerGameExceeded,
    /// The room's state does not permit this operation.
    InvalidRoomState,

    // Authority errors
    /// The server does not support authority handoff at all.
    AuthorityNotSupported,
    /// Another participant already holds the room's authority.
    AuthorityConflict,
    /// The connection may not claim the room's authority.
    AuthorityDenied,

    // Rate limiting
    /// Too many requests in the current window; retry later. Room/spectator
    /// admission refusals leave the connection open; a refused handshake
    /// closes it.
    RateLimitExceeded,
    /// The deployment's connection limit for this client was reached.
    TooManyConnections,

    // Reconnection errors
    /// The directed reconnect failed; the token is not consumed by such a
    /// refusal, so retry from a fresh connection while the window is open.
    ReconnectionFailed,
    /// The reconnection token is invalid or malformed; join the room again.
    ReconnectionTokenInvalid,
    /// The reconnection window has expired; join the room again as a new
    /// player.
    ReconnectionExpired,
    /// The player is already connected to the room from another session.
    PlayerAlreadyConnected,

    // Spectator errors
    /// The room does not permit spectators.
    SpectatorNotAllowed,
    /// The room has reached its spectator capacity.
    TooManySpectators,
    /// The connection is not a spectator in this room.
    NotASpectator,
    /// The spectator join failed (room full or spectating disabled).
    SpectatorJoinFailed,

    // Server errors
    /// An internal server error occurred; retry or contact the operator.
    InternalError,
    /// A server-side storage error occurred while handling the request.
    StorageError,
    /// Compatibility only (see [`NON_EMITTED`](ErrorCode::NON_EMITTED)): no
    /// longer emitted by Server 0.7+; retained so older deployments stay
    /// decodable.
    ServiceUnavailable,

    // Game-start errors (protocol v2)
    /// Not every player in the room is ready yet.
    GameStartNotReady,
    /// Only the room's authority may start the game.
    GameStartForbidden,
    /// The room already finalized a peer-to-peer session whose sticky
    /// topology and transport were not negotiated by this connection.
    RoomSessionIncompatible,

    // Signaling errors (protocol v3)
    /// The signal's target is not a participant of this room.
    CrossRoomSignal,
    /// The requested data-path transport is unsupported or was not
    /// negotiated for this connection.
    UnsupportedTransport,
    /// The signal's target peer could not be found in the room.
    SignalTargetNotFound,
    /// Too many signaling messages were sent in the current window.
    SignalRateLimited,
    /// The signal payload exceeds the server's size limit.
    SignalTooLarge,

    // Connection lifecycle (protocol v3)
    /// The server closed the connection after it stayed idle for too long.
    /// See also [`ActivityTimeout`](ErrorCode::ActivityTimeout).
    ConnectionIdleTimeout,

    // Delivery & liveness
    /// The server evicted this connection because its outbound queue stayed
    /// full past the slow-consumer grace window (5 seconds by default): the
    /// client was not draining messages fast enough.
    ///
    /// The farewell `Error` frame carrying this code is written best-effort
    /// into an already-congested socket, so it may never arrive; a bare
    /// disconnect can be the only observable signal. Wire: `"SLOW_CONSUMER"`.
    SlowConsumer,
    /// The server closed the connection after prolonged protocol inactivity
    /// (no messages received within the activity window). Wire:
    /// `"ACTIVITY_TIMEOUT"`.
    ActivityTimeout,
    /// The server is draining for shutdown and refusing new room creation,
    /// reconnection attempts, and spectator joins. Existing connections close
    /// with semantic code 4000 at the deadline.
    ServerDraining,
    /// The requested protocol-v3 delivery class/key combination is invalid.
    InvalidDeliveryClass,
    /// The client's highest supported protocol version is below the server's
    /// configured minimum, or a pre-v3 connection sent a frame class that
    /// requires a newer protocol surface (such as the v3 `RoomOperation`
    /// envelope).
    UnsupportedProtocolVersion,
}

impl ErrorCode {
    /// Compatibility variants retained for servers older than 0.7.0 even
    /// though the 0.7 AsyncAPI no longer declares them as emitted tokens.
    pub const NON_EMITTED: &'static [Self] = &[
        Self::InvalidToken,
        Self::AuthenticationRequired,
        Self::AppIdExpired,
        Self::AppIdRevoked,
        Self::AppIdSuspended,
        Self::ServiceUnavailable,
    ];

    /// Returns a human-readable description of this error code.
    ///
    /// This method provides actionable error messages that SDK developers
    /// can display to end users or use for debugging.
    pub fn description(&self) -> &'static str {
        match self {
            // Authentication errors
            Self::Unauthorized => {
                "Access denied. Authentication credentials are missing or invalid."
            }
            Self::InvalidToken => {
                "The authentication token is invalid, malformed, or has expired. Please obtain a new token."
            }
            Self::AuthenticationRequired => {
                "This operation requires authentication. Please provide valid credentials."
            }
            Self::InvalidAppId => {
                "The provided application ID is not recognized or is not acceptable. Verify your app ID is correct and free of control characters (maximum 256 bytes)."
            }
            Self::AppIdExpired => {
                "The application ID has expired. Please renew your application registration."
            }
            Self::AppIdRevoked => {
                "The application ID has been revoked. Contact the administrator for assistance."
            }
            Self::AppIdSuspended => {
                "The application ID has been suspended. Contact the administrator for assistance."
            }
            Self::MissingAppId => {
                "Application ID is required but was not provided. Include your app ID in the request."
            }
            Self::AuthenticationTimeout => {
                "Authentication took too long to complete. Please try again."
            }
            Self::SdkVersionUnsupported => {
                "The SDK version you are using is no longer supported. Please upgrade to the latest version."
            }
            Self::UnsupportedGameDataFormat => {
                "The requested game data format is not supported by this server. Falling back to JSON encoding."
            }

            // Validation errors
            Self::InvalidInput => {
                "The provided input is invalid or malformed. Check your request parameters."
            }
            Self::InvalidGameName => {
                "The game name is invalid. Game names must be non-empty and follow naming requirements."
            }
            Self::InvalidRoomCode => {
                "The room code is invalid or malformed. Room codes must follow the required format."
            }
            Self::InvalidPlayerName => {
                "The player name is invalid. Player names must be non-empty and meet length requirements."
            }
            Self::InvalidMaxPlayers => {
                "The maximum player count is invalid. It must be a positive number within allowed limits."
            }
            Self::MessageTooLarge => {
                "The message size exceeds the maximum allowed limit. Please send a smaller message."
            }

            // Room errors
            Self::RoomNotFound => {
                "The requested room could not be found. It may have been closed or the code is incorrect."
            }
            Self::RoomFull => {
                "The room has reached its maximum player capacity. Try joining a different room."
            }
            Self::AlreadyInRoom => {
                "You are already in a room. Leave the current room before joining another."
            }
            Self::NotInRoom => {
                "You are not currently in any room. Join a room before performing this action."
            }
            Self::RoomCreationFailed => {
                "Failed to create the room. Please try again or contact support if the issue persists."
            }
            Self::MaxRoomsPerGameExceeded => {
                "The maximum number of rooms for this game has been reached. Please try again later."
            }
            Self::InvalidRoomState => {
                "The room is in an invalid state for this operation. Try refreshing or rejoining the room."
            }

            // Authority errors
            Self::AuthorityNotSupported => {
                "Authority features are not enabled on this server. Check your server configuration."
            }
            Self::AuthorityConflict => {
                "Another client has already claimed authority. Only one client can have authority at a time."
            }
            Self::AuthorityDenied => {
                "You do not have permission to claim authority in this room."
            }

            // Rate limiting
            Self::RateLimitExceeded => {
                "Too many requests in a short time. Room/spectator admission refusals leave the connection open; a refused handshake closes it. Wait out the window before retrying."
            }
            Self::TooManyConnections => {
                "You have too many active connections. Close some connections before opening new ones."
            }

            // Reconnection errors
            Self::ReconnectionFailed => {
                "Failed to reconnect. The session may have expired, the room closed, or the attempt landed on a socket the server had already scheduled to close. The token is not consumed by such a refusal; retry from a fresh connection while the window is open."
            }
            Self::ReconnectionTokenInvalid => {
                "The reconnection token is invalid or malformed. You may need to join the room again."
            }
            Self::ReconnectionExpired => {
                "The reconnection window has expired. You must join the room again as a new player."
            }
            Self::PlayerAlreadyConnected => {
                "This player is already connected to the room from another session."
            }

            // Spectator errors
            Self::SpectatorNotAllowed => {
                "Spectator mode is not enabled for this room. Only players can join."
            }
            Self::TooManySpectators => {
                "The room has reached its maximum spectator capacity. Try again later."
            }
            Self::NotASpectator => {
                "You are not a spectator in this room. This action is only available to spectators."
            }
            Self::SpectatorJoinFailed => {
                "Failed to join as a spectator. The room may be full or spectating may be disabled."
            }

            // Server errors
            Self::InternalError => {
                "An internal server error occurred. Please try again or contact support if the issue persists."
            }
            Self::StorageError => {
                "A storage error occurred while processing your request. Please try again later."
            }
            Self::ServiceUnavailable => {
                "The service is temporarily unavailable. Please try again in a few moments."
            }

            // Game-start errors (protocol v2)
            Self::GameStartNotReady => {
                "Cannot start the game: not every player in the room is ready yet."
            }
            Self::GameStartForbidden => {
                "You are not permitted to start the game. Only the room's authority may start it."
            }
            Self::RoomSessionIncompatible => {
                "This room already started a peer-to-peer session with a topology or transport this client did not negotiate. Reconnect with compatible capabilities or join another room; rooms finalized to the relay floor remain open to everyone."
            }

            // Signaling errors (protocol v3)
            Self::CrossRoomSignal => {
                "The signal targets a peer that is not in your room."
            }
            Self::UnsupportedTransport => {
                "The requested data-path transport is not supported or was not negotiated for this connection."
            }
            Self::SignalTargetNotFound => {
                "The signal's target peer could not be found in the room."
            }
            Self::SignalRateLimited => {
                "Too many signaling messages were sent in a short time. Please slow down and try again."
            }
            Self::SignalTooLarge => {
                "The signal payload exceeds the maximum size allowed by the server."
            }

            // Connection lifecycle (protocol v3)
            Self::ConnectionIdleTimeout => {
                "The connection was closed by the server after being idle for too long."
            }

            // Delivery & liveness
            Self::SlowConsumer => {
                "The server closed this connection because the client was not reading messages fast enough. Drain events promptly, or reduce inbound volume."
            }
            Self::ActivityTimeout => {
                "The connection was closed by the server due to prolonged inactivity. Send periodic application pings (or keep answering server probes) to keep the connection alive; frames rejected for size or content do not refresh the window."
            }
            Self::ServerDraining => {
                "The server is shutting down and is refusing new room creation, reconnection attempts, and spectator joins. Existing sockets close with code 4000 at the drain deadline; retry on another healthy instance."
            }
            Self::InvalidDeliveryClass => {
                "The requested game-data delivery class and key combination is invalid. Latest requires a key; reliable and volatile forbid one."
            }
            Self::UnsupportedProtocolVersion => {
                "The client's highest supported protocol version is below this server's configured minimum, or a pre-v3 connection sent a frame class that requires a newer protocol surface. Upgrade the client or connect to a compatible deployment."
            }
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.description())
    }
}
