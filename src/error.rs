//! Error types for the Signal Fish client.

use crate::error_codes::ErrorCode;
use crate::protocol::SessionGeneration;
use crate::RoomRole;
use thiserror::Error;

/// A non-secret reason that token-binding-v2 setup or message protection failed.
///
/// The variants deliberately contain no handshake key, derived key, nonce,
/// proof, signature, certificate fingerprint, or application payload, so the
/// exhaustive public error remains safe to format in ambient logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenBindingFailure {
    /// The crate was built without the opt-in `token-binding` feature.
    FeatureDisabled,
    /// The server rejected the WebSocket upgrade while token binding was offered.
    NegotiationRejected,
    /// Required mode completed an upgrade without selecting token-binding-v2.
    SubprotocolNotNegotiated,
    /// The server selected a WebSocket subprotocol other than the one offered.
    UnexpectedSubprotocol,
    /// The selected connection closed before sending its challenge.
    MissingChallenge,
    /// The selected connection did not send its challenge before the deadline.
    ChallengeTimeout,
    /// The first application frame was not a valid token-binding challenge.
    MalformedChallenge,
    /// The challenge declared an unsupported protocol version.
    UnsupportedVersion,
    /// The challenge declared an unsupported proof scheme.
    UnsupportedScheme,
    /// The challenge nonce was not canonical base64 for exactly 32 bytes.
    InvalidNonce,
    /// The challenge did not start at the pinned protocol's first sequence.
    InvalidFirstSequence,
    /// The WebSocket handshake key was missing, malformed, or not 16 bytes.
    InvalidHandshakeKey,
    /// The per-connection HKDF key could not be derived.
    KeyDerivation,
    /// An outbound JSON frame cannot be represented by the pinned canonical form.
    UnsupportedJson,
    /// An outbound binary proof envelope could not be encoded.
    MessageEncoding,
    /// The shared text/binary sequence space has been exhausted.
    SequenceExhausted,
}

impl std::fmt::Display for TokenBindingFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::FeatureDisabled => "crate feature `token-binding` is disabled",
            Self::NegotiationRejected => "the server rejected token-binding negotiation",
            Self::SubprotocolNotNegotiated => {
                "the server did not select signalfish.tokenbinding.v2"
            }
            Self::UnexpectedSubprotocol => "the server selected an unexpected subprotocol",
            Self::MissingChallenge => "the server closed before the token-binding challenge",
            Self::ChallengeTimeout => "the server token-binding challenge timed out",
            Self::MalformedChallenge => "the server sent a malformed token-binding challenge",
            Self::UnsupportedVersion => "the server selected an unsupported token-binding version",
            Self::UnsupportedScheme => "the server selected an unsupported token-binding scheme",
            Self::InvalidNonce => "the server challenge nonce is invalid",
            Self::InvalidFirstSequence => "the server challenge sequence is invalid",
            Self::InvalidHandshakeKey => "the WebSocket handshake key is unavailable or invalid",
            Self::KeyDerivation => "the token-binding session key could not be derived",
            Self::UnsupportedJson => "the outbound JSON frame is not token-binding compatible",
            Self::MessageEncoding => "the token-bound frame could not be encoded",
            Self::SequenceExhausted => "the token-binding sequence space is exhausted",
        })
    }
}

/// Errors that can occur when using the Signal Fish client.
#[derive(Debug, Error)]
pub enum SignalFishError {
    /// Failed to send a message through the transport.
    #[error("transport send error: {0}")]
    TransportSend(String),

    /// Failed to receive a message from the transport.
    #[error("transport receive error: {0}")]
    TransportReceive(String),

    /// The transport connection was closed unexpectedly.
    #[error("transport connection closed")]
    TransportClosed,

    /// Failed to serialize or deserialize a protocol message.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Attempted an operation that requires an active connection, but the client is not connected.
    #[error("not connected to server")]
    NotConnected,

    /// The bounded outgoing command queue is full — the caller is producing
    /// messages faster than the transport can drain them.
    ///
    /// This is the client's send-side backpressure signal: nothing was lost,
    /// the message was simply refused. Either retry later (e.g. next frame),
    /// pace high-rate payloads with a waiting `*_reliable` variant
    /// ([`SignalFishClient::send_game_data_reliable`](crate::SignalFishClient::send_game_data_reliable),
    /// [`SignalFishClient::send_signal_reliable`](crate::SignalFishClient::send_signal_reliable)),
    /// or raise
    /// [`SignalFishConfig::command_channel_capacity`](crate::SignalFishConfig::command_channel_capacity).
    #[error(
        "outgoing command queue full (capacity {capacity}): the transport cannot keep up; \
         retry later, pace high-rate sends with a waiting *_reliable variant, or increase \
         command_channel_capacity"
    )]
    SendBufferFull {
        /// Configured capacity of the outgoing command queue.
        capacity: usize,
    },

    /// Attempted a room operation but the client is not in a room.
    #[error("not in a room")]
    NotInRoom,

    /// Attempted to join or reconnect while already in a room.
    #[error("already in a room")]
    AlreadyInRoom,

    /// Attempted a room operation while a prior room transition awaits a
    /// matching typed terminal response. Generic errors stay fenced until
    /// connection teardown.
    #[error("a room join, leave, or reconnect operation is already pending")]
    RoomOperationPending,

    /// Attempted an operation that is not valid for the current room role.
    #[error("operation requires the {required} room role, but the current role is {actual}")]
    WrongRoomRole {
        /// Role required by the attempted operation.
        required: RoomRole,
        /// Current server-confirmed role.
        actual: RoomRole,
    },

    /// Attempted an operation reserved for the room's current authority.
    #[error("operation requires the room's current authority role")]
    AuthorityRequired,

    /// The server returned an error message.
    #[error("server error: {message}")]
    ServerError {
        /// Human-readable error message from the server.
        message: String,
        /// Structured error code, if provided by the server.
        error_code: Option<ErrorCode>,
    },

    /// A protocol-v3-only operation was attempted on a connection that has not
    /// negotiated v3.
    ///
    /// The server would reject the message, so the client fails fast at the call
    /// site instead — better UX than an asynchronous, unattributed error event.
    /// Opt into relay/accountability v3 with
    /// [`SignalFishConfig::enable_v3`](crate::SignalFishConfig::enable_v3), or
    /// opt into mesh signaling with
    /// [`SignalFishConfig::enable_mesh`](crate::SignalFishConfig::enable_mesh).
    #[error(
        "operation requires a negotiated protocol v3 session (current mode: {mode}); \
         opt into v3 with SignalFishConfig::enable_v3() or SignalFishConfig::enable_mesh()"
    )]
    ProtocolUnsupported {
        /// Why v3 is unavailable:
        /// - `"relay-only"` — a `ProtocolInfo` was received but negotiated below
        ///   v3 (the v2 relay floor); waiting will not help. Enable the required
        ///   v3 capabilities and reconnect.
        /// - `"pre-negotiation"` — no `ProtocolInfo` has been received yet;
        ///   negotiation is still in flight, so retry once it completes.
        mode: &'static str,
    },

    /// No authoritative WebRTC session plan currently authorizes this signal.
    ///
    /// This covers signaling before a plan and targeting self, an unknown or
    /// departed player, or a peer removed by a replacement plan.
    #[error("no authoritative SessionPlan authorizes this WebRTC signal")]
    SessionPlanUnavailable,

    /// A generation-bound WebRTC signal was produced for a session plan that
    /// has already been replaced.
    #[error(
        "WebRTC signal belongs to stale session generation {attempted:?}; current generation is {current:?}"
    )]
    StaleSessionGeneration {
        /// Generation under which the signal was produced.
        attempted: Option<SessionGeneration>,
        /// Authoritative generation of the latest session plan.
        current: Option<SessionGeneration>,
    },

    /// Binary game data was requested without negotiating a binary encoding.
    #[error(
        "binary game data requires an effectively negotiated MessagePack format; this connection uses JSON"
    )]
    BinaryFormatNotNegotiated,

    /// Native WebSocket token-binding-v2 setup or outbound protection failed.
    ///
    /// The reason is intentionally structured and contains no secret or proof
    /// material. Required mode never silently falls back after this error.
    #[error("token binding error: {0}")]
    TokenBinding(TokenBindingFailure),

    /// An operation timed out.
    #[error("operation timed out")]
    Timeout,

    /// An I/O error occurred.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// A specialized [`Result`] type for Signal Fish client operations.
pub type Result<T> = std::result::Result<T, SignalFishError>;

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    #[test]
    fn token_binding_failure_display_is_actionable_for_every_reason() {
        let cases = [
            (TokenBindingFailure::FeatureDisabled, "feature"),
            (TokenBindingFailure::NegotiationRejected, "rejected"),
            (
                TokenBindingFailure::SubprotocolNotNegotiated,
                "did not select",
            ),
            (TokenBindingFailure::UnexpectedSubprotocol, "unexpected"),
            (TokenBindingFailure::MissingChallenge, "closed"),
            (TokenBindingFailure::ChallengeTimeout, "timed out"),
            (TokenBindingFailure::MalformedChallenge, "malformed"),
            (TokenBindingFailure::UnsupportedVersion, "version"),
            (TokenBindingFailure::UnsupportedScheme, "scheme"),
            (TokenBindingFailure::InvalidNonce, "nonce"),
            (TokenBindingFailure::InvalidFirstSequence, "sequence"),
            (TokenBindingFailure::InvalidHandshakeKey, "handshake key"),
            (TokenBindingFailure::KeyDerivation, "derived"),
            (TokenBindingFailure::UnsupportedJson, "JSON"),
            (TokenBindingFailure::MessageEncoding, "encoded"),
            (TokenBindingFailure::SequenceExhausted, "exhausted"),
        ];

        for (failure, expected) in cases {
            let message = failure.to_string();
            assert!(
                message.contains(expected),
                "{failure:?} must retain an actionable display message: {message}"
            );
        }
    }

    #[test]
    fn server_error_uses_typed_error_code() {
        let err = SignalFishError::ServerError {
            message: "room full".into(),
            error_code: Some(ErrorCode::RoomFull),
        };

        if let SignalFishError::ServerError {
            message,
            error_code,
        } = err
        {
            assert_eq!(message, "room full");
            assert_eq!(error_code, Some(ErrorCode::RoomFull));
        } else {
            panic!("expected ServerError");
        }
    }
}
