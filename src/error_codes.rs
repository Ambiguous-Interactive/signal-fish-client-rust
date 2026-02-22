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
    Unauthorized,
    InvalidToken,
    AuthenticationRequired,
    InvalidAppId,
    AppIdExpired,
    AppIdRevoked,
    AppIdSuspended,
    MissingAppId,
    AuthenticationTimeout,
    SdkVersionUnsupported,
    UnsupportedGameDataFormat,

    // Validation errors
    InvalidInput,
    InvalidGameName,
    InvalidRoomCode,
    InvalidPlayerName,
    InvalidMaxPlayers,
    MessageTooLarge,

    // Room errors
    RoomNotFound,
    RoomFull,
    AlreadyInRoom,
    NotInRoom,
    RoomCreationFailed,
    MaxRoomsPerGameExceeded,
    InvalidRoomState,

    // Authority errors
    AuthorityNotSupported,
    AuthorityConflict,
    AuthorityDenied,

    // Rate limiting
    RateLimitExceeded,
    TooManyConnections,

    // Reconnection errors
    ReconnectionFailed,
    ReconnectionTokenInvalid,
    ReconnectionExpired,
    PlayerAlreadyConnected,

    // Spectator errors
    SpectatorNotAllowed,
    TooManySpectators,
    NotASpectator,
    SpectatorJoinFailed,

    // Server errors
    InternalError,
    StorageError,
    ServiceUnavailable,
}

impl ErrorCode {
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
                "The provided application ID is not recognized. Verify your app ID is correct."
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
                "Too many requests in a short time. Please slow down and try again later."
            }
            Self::TooManyConnections => {
                "You have too many active connections. Close some connections before opening new ones."
            }

            // Reconnection errors
            Self::ReconnectionFailed => {
                "Failed to reconnect to the room. The session may have expired or the room may be closed."
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
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.description())
    }
}
