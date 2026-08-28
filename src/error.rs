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
    /// Reserved for compatibility; no longer produced.
    ///
    /// Current SDK versions report HTTP upgrade rejections as
    /// [`SignalFishError::Io`] carrying the
    /// underlying HTTP error (including its status), whether or not token
    /// binding was offered, so a misconfigured URL reports the same way in
    /// every token-binding mode. Match on this variant only in exhaustive
    /// arms.
    NegotiationRejected,
    /// Required mode completed an upgrade without selecting token-binding-v2.
    SubprotocolNotNegotiated,
    /// The server selected a WebSocket subprotocol other than the one offered.
    UnexpectedSubprotocol,
    /// The selected connection closed before sending its challenge.
    MissingChallenge,
    /// The selected connection did not send its challenge before the deadline.
    ChallengeTimeout,
    /// The connection never produced a valid token-binding challenge: the
    /// first application frame was not a valid challenge, or the server
    /// exhausted the client's control-frame budget before sending one.
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
    /// A client-certificate fingerprint was required, but the connect path
    /// could not observe an X.509 client signer.
    ///
    /// Raised only by the opt-in
    /// [`require_client_fingerprint`](crate::WebSocketConnectOptions::with_require_client_fingerprint)
    /// policy: either the connect path can never observe a certificate
    /// selection (it is not a custom-rustls token-binding connection), or the
    /// TLS handshake completed without the server selecting one.
    MissingClientFingerprint,
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
            Self::MalformedChallenge => "the server did not send a valid token-binding challenge",
            Self::UnsupportedVersion => "the server selected an unsupported token-binding version",
            Self::UnsupportedScheme => "the server selected an unsupported token-binding scheme",
            Self::InvalidNonce => "the server challenge nonce is invalid",
            Self::InvalidFirstSequence => "the server challenge sequence is invalid",
            Self::InvalidHandshakeKey => "the WebSocket handshake key is unavailable or invalid",
            Self::KeyDerivation => "the token-binding session key could not be derived",
            Self::UnsupportedJson => "the outbound JSON frame is not token-binding compatible",
            Self::MessageEncoding => "the token-bound frame could not be encoded",
            Self::SequenceExhausted => "the token-binding sequence space is exhausted",
            Self::MissingClientFingerprint => {
                "a client certificate fingerprint was required, but no X.509 client signer \
                 was selected"
            }
        })
    }
}

/// Errors that can occur when using the Signal Fish client.
#[derive(Debug, Error)]
pub enum SignalFishError {
    /// Failed to send a message through the transport.
    ///
    /// The boxed cause is the transport backend's original error, so
    /// [`Error::source`](std::error::Error::source) reaches the root cause for
    /// programmatic handling. The built-in native WebSocket transport boxes
    /// the backend's own `tungstenite::Error`, whose `source()` chain then
    /// reaches the underlying [`std::io::Error`] when there is one; custom
    /// transports box whatever error they produce (an `io::Error`, a typed
    /// backend error, or a plain string detail via `.into()`). The `Display`
    /// text is the cause's own message; the variant is safe to format in
    /// ambient logs.
    ///
    /// When a cause's own `Debug` would embed application payload bytes (for
    /// example `std::ffi::NulError` retains the whole input vector), box its
    /// `Display` text instead.
    #[error("transport send error: {0}")]
    TransportSend(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// Failed to receive a message from the transport.
    ///
    /// The boxed cause follows the same structure as
    /// [`TransportSend`](SignalFishError::TransportSend): `Error::source()`
    /// reaches the backend's original error for programmatic handling.
    #[error("transport receive error: {0}")]
    TransportReceive(#[source] Box<dyn std::error::Error + Send + Sync>),

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
    /// drain events promptly, or raise
    /// [`SignalFishConfig::command_channel_capacity`](crate::SignalFishConfig::command_channel_capacity).
    /// Draining matters when the task awaiting a reliable send is also the
    /// sole event consumer: a full event channel pauses the transport loop
    /// that drains the command queue, so that task can deadlock against
    /// itself (drain events from a separate task).
    #[error(
        "outgoing command queue full (capacity {capacity}): the transport cannot keep up; \
         retry later, pace high-rate sends with a waiting *_reliable variant, drain events \
         promptly, or increase command_channel_capacity"
    )]
    SendBufferFull {
        /// Configured capacity of the outgoing command queue.
        capacity: usize,
    },

    /// Attempted a directed room operation before the server confirmed
    /// authentication.
    ///
    /// The five admission-fencing operations
    /// ([`join_room`](crate::SignalFishClient::join_room),
    /// [`leave_room`](crate::SignalFishClient::leave_room),
    /// [`reconnect`](crate::SignalFishClient::reconnect),
    /// [`join_as_spectator`](crate::SignalFishClient::join_as_spectator), and
    /// [`leave_spectator`](crate::SignalFishClient::leave_spectator)) require
    /// the server's `Authenticated` confirmation first. The command was
    /// **not** queued: retry once
    /// [`SignalFishEvent::Authenticated`](crate::SignalFishEvent::Authenticated)
    /// arrives.
    #[error(
        "not yet authenticated: wait for SignalFishEvent::Authenticated before room operations"
    )]
    NotAuthenticated,

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
    ///
    /// Reserved for compatibility: no current server/SDK combination
    /// constructs this variant. Server error messages surface as events
    /// ([`SignalFishEvent::Error`](crate::SignalFishEvent::Error),
    /// [`SignalFishEvent::AuthenticationError`](crate::SignalFishEvent::AuthenticationError),
    /// [`SignalFishEvent::RoomJoinFailed`](crate::SignalFishEvent::RoomJoinFailed),
    /// or [`SignalFishEvent::RoomOperationFailed`](crate::SignalFishEvent::RoomOperationFailed)),
    /// so exhaustive matches should treat this arm as unreachable.
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
    /// This covers signaling before a plan, targeting self, an unknown or
    /// departed player, a peer removed by a replacement plan, and connections
    /// whose negotiated session transport is not WebRTC (a relay plan).
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

    /// The WebSocket handshake did not complete within the deadline given to
    /// `WebSocketTransport::connect_with_timeout`.
    #[error(
        "the WebSocket handshake did not complete within its deadline; retry or raise the connect_with_timeout duration"
    )]
    Timeout,

    /// A caller-supplied configuration value was rejected because the value
    /// itself is unusable, before any network I/O.
    ///
    /// Raised when a URL, connect option, transport setting, or required
    /// build feature is invalid on its face — for example a zero
    /// inbound-size limit, a URL that cannot be parsed into a WebSocket
    /// request, a URL containing interior NUL bytes, or `wss://` without the
    /// opt-in `tls` feature. The failure is determined by the value or the
    /// build, not by a network outcome: retrying without correcting it keeps
    /// failing.
    #[error("invalid configuration: {field}: {problem}")]
    InvalidConfig {
        /// The rejected configuration field, option, or parameter name.
        field: &'static str,
        /// Why the value was rejected; non-secret and safe for ambient logs.
        problem: String,
    },

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
            (
                TokenBindingFailure::MalformedChallenge,
                "did not send a valid",
            ),
            (TokenBindingFailure::UnsupportedVersion, "version"),
            (TokenBindingFailure::UnsupportedScheme, "scheme"),
            (TokenBindingFailure::InvalidNonce, "nonce"),
            (TokenBindingFailure::InvalidFirstSequence, "sequence"),
            (TokenBindingFailure::InvalidHandshakeKey, "handshake key"),
            (TokenBindingFailure::KeyDerivation, "derived"),
            (TokenBindingFailure::UnsupportedJson, "JSON"),
            (TokenBindingFailure::MessageEncoding, "encoded"),
            (TokenBindingFailure::SequenceExhausted, "exhausted"),
            (
                TokenBindingFailure::MissingClientFingerprint,
                "client certificate fingerprint",
            ),
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

    #[test]
    fn transport_send_and_receive_expose_the_boxed_cause_through_source() {
        use std::error::Error as _;
        use std::io::ErrorKind;

        // Custom transports box the error they produce; a directly boxed
        // `io::Error` is reachable in one `source()` hop.
        let send = SignalFishError::TransportSend(Box::new(std::io::Error::new(
            ErrorKind::ConnectionRefused,
            "refused by peer",
        )));
        assert_eq!(send.to_string(), "transport send error: refused by peer");
        let kind = send
            .source()
            .and_then(|cause| cause.downcast_ref::<std::io::Error>())
            .map(std::io::Error::kind);
        assert_eq!(kind, Some(ErrorKind::ConnectionRefused));

        // The built-in native WebSocket boxes the backend's own error
        // (`tungstenite::Error`), whose chain reaches the underlying
        // `io::Error` one hop further down.
        #[derive(Debug)]
        struct BackendError(std::io::Error);
        impl std::fmt::Display for BackendError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "backend io failure: {}", self.0)
            }
        }
        impl std::error::Error for BackendError {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        let receive = SignalFishError::TransportReceive(Box::new(BackendError(
            std::io::Error::new(ErrorKind::ConnectionReset, "connection reset"),
        )));
        assert_eq!(
            receive.to_string(),
            "transport receive error: backend io failure: connection reset"
        );
        let nested_kind = receive
            .source()
            .and_then(|backend| backend.source())
            .and_then(|cause| cause.downcast_ref::<std::io::Error>())
            .map(std::io::Error::kind);
        assert_eq!(nested_kind, Some(ErrorKind::ConnectionReset));
    }

    #[test]
    fn invalid_config_display_names_field_and_problem() {
        let error = SignalFishError::InvalidConfig {
            field: "max_inbound_message_size",
            problem: "must be greater than zero or None".into(),
        };
        assert_eq!(
            error.to_string(),
            "invalid configuration: max_inbound_message_size: must be greater than zero or None"
        );
    }
}
