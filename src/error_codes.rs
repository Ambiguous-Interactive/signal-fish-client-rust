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
    /// Access was denied by the app-ID handshake policy.
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
    /// This SDK version is no longer supported by the server. Current
    /// servers deliver this refusal on an open socket as a retryable
    /// handshake refusal (upstream PR #577); recovery is upgrading the SDK
    /// or connecting to a deployment that accepts this version.
    SdkVersionUnsupported,
    /// The requested game-data format is unsupported; the server falls back
    /// to JSON.
    UnsupportedGameDataFormat,
    /// The optional tenant connect token failed verification (encoding,
    /// signature, expiry, TTL ceiling, or app-id binding). The socket stays
    /// open; since the configured token is fixed for a client's lifetime,
    /// recovery means issuing a fresh token and starting a new client.
    ConnectTokenInvalid,
    /// The deployment enforces tenant credentials and the handshake carried
    /// no connect token at all. The socket stays open; recovery means
    /// obtaining an `sfct_v1.` token from the deployment's control plane,
    /// presenting it via
    /// [`with_connect_token`](crate::SignalFishConfig::with_connect_token),
    /// and starting a new client.
    ConnectTokenRequired,

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
    /// Signaling requires the WebRTC transport, which was not
    /// negotiated for this connection.
    UnsupportedTransport,
    /// The signal's target peer could not be found in the room, or does not
    /// support WebRTC.
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
    /// envelope). Current servers deliver this on an open socket as a
    /// retryable handshake refusal (upstream PR #577); the deployment governs
    /// the socket's fate.
    UnsupportedProtocolVersion,

    // Moderation errors (authority-only room operations)
    /// A moderation operation (`KickPlayer` / `RegenerateRoomCode`) was sent
    /// by a connection that is not the room's designated authority player.
    NotRoomAuthority,
    /// The player named by `KickPlayer` is not a seated member of the room.
    KickTargetNotFound,
    /// This connection was removed from its room by the room's authority
    /// player. The WebSocket closed with private close code `4007`
    /// (`kicked`); the server never arms reconnection for a kicked seat (a
    /// configured `ReconnectPolicy` retries the close like any peer close
    /// unless the deployment listed 4007 in
    /// [`with_terminal_close_codes`](crate::client::ReconnectPolicy::with_terminal_close_codes),
    /// and on a spec-conformant server the automatic rejoin is
    /// refused in-band).
    Kicked,

    // Access-control errors (authority-only room operations). Appended after
    // the moderation block, matching the upstream token order. Raised by the
    // room access-control tier (SetRoomAccess / BanPlayer / TransferAuthority
    // and password-protected joins).
    /// The room requires a join password and the request presented none,
    /// the wrong one, or a password for an open room (upstream issue #546).
    /// The server does not distinguish the three cases.
    PasswordRequired,
    /// This player id is banned from the room by its authority player and
    /// cannot join it (as a player or spectator) while the room lives. The
    /// ban is room-scoped and expires with the room. Banning a seated member
    /// removes them exactly like a kick (close code `4007`/`kicked`). Newer
    /// upstream servers also refuse a banned seat's reconnection restore
    /// with this code on `ReconnectionFailed`, keeping the pending record so
    /// a mid-window unban lets the token work again, tombstone a banned
    /// pending record instead of removing a seat, and withhold the 4007
    /// close when the target is live in another room.
    Banned,
    /// The player named by `TransferAuthority` is not a seated member of the
    /// room.
    TransferTargetNotFound,
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
                "Access denied by the app-ID handshake policy."
            }
            Self::InvalidToken => {
                "The authentication token is invalid, malformed, or has expired. Please obtain a new token."
            }
            Self::AuthenticationRequired => {
                "Complete the legacy Authenticate handshake before this operation."
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
                "The required app-ID handshake was not completed. Send Authenticate before application messages."
            }
            Self::AuthenticationTimeout => {
                "The app-ID and protocol handshake took too long to complete. Please try again."
            }
            Self::SdkVersionUnsupported => {
                "This SDK version is no longer supported by the server. Upgrade the SDK or connect to a compatible deployment; current servers deliver this refusal on an open socket (the deployment, not the client, ends the connection)."
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
                "The game cannot start yet. Every current player must be ready before StartGame is accepted."
            }
            Self::GameStartForbidden => {
                "You are not permitted to start the game. Only the room's authority player may start it."
            }
            Self::RoomSessionIncompatible => {
                "This room already started a peer-to-peer session with a topology or transport this client did not negotiate. Reconnect with compatible capabilities or join another room; rooms finalized to the relay floor remain open to everyone."
            }

            // Signaling errors (protocol v3)
            Self::CrossRoomSignal => {
                "Cannot signal a peer in a different room. WebRTC signaling is restricted to peers within the same room."
            }
            Self::UnsupportedTransport => {
                "Signaling requires the WebRTC transport, which was not negotiated for this connection. Re-authenticate advertising WebRTC support."
            }
            Self::SignalTargetNotFound => {
                "The signal target peer could not be found in your room, or does not support WebRTC. Verify the peer id and that the peer is connected."
            }
            Self::SignalRateLimited => {
                "Too many signaling messages in a short time. Please slow down trickle-ICE and try again shortly."
            }
            Self::SignalTooLarge => {
                "The signal payload exceeds the maximum allowed size. Send smaller SDP/ICE payloads, e.g. individual trickle-ICE candidates."
            }

            // Connection lifecycle (protocol v3)
            Self::ConnectionIdleTimeout => {
                "The connection was closed because no messages were received within the idle timeout. Send periodic Ping messages to keep the connection alive."
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
                "The game-data delivery class is invalid: latest requires a key, while reliable and volatile must not include one."
            }
            Self::UnsupportedProtocolVersion => {
                "The client's highest supported protocol version is below this server's configured minimum, or a pre-v3 connection sent a frame class that requires a newer protocol surface. Upgrade the client or connect to a compatible deployment; current servers deliver this refusal on an open socket (the deployment, not the client, ends the connection)."
            }

            // Moderation errors (authority-only room operations)
            Self::NotRoomAuthority => {
                "Only the room's authority player may perform this moderation operation."
            }
            Self::KickTargetNotFound => {
                "The player to kick is not a current member of this room."
            }
            Self::Kicked => {
                "You were removed from the room by its authority player. Reconnection is not offered; join again with a valid room code."
            }
            Self::PasswordRequired => {
                "This room is password-protected, or the join presented a password to an open room. Send the password chosen by the room's authority player, or join without one."
            }
            Self::Banned => {
                "This player id is banned from the room by its authority player and cannot join it while the room lives. Newer upstream servers also refuse a banned seat's reconnection restore with this code, keeping the pending record so a mid-window unban lets the token work again."
            }
            Self::TransferTargetNotFound => {
                "The player to transfer authority to is not a current member of this room."
            }
            Self::ConnectTokenInvalid => {
                "The optional tenant connect token failed verification (encoding, signature, expiry, TTL ceiling, or app-id binding). Issue a fresh token from your deployment's control plane and start a new client; the configured token is fixed for this client's lifetime."
            }
            Self::ConnectTokenRequired => {
                "This deployment requires a tenant connect token and the handshake carried none. Obtain an sfct_v1. token from your deployment's control plane, present it via with_connect_token, and start a new client."
            }
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.description())
    }
}
