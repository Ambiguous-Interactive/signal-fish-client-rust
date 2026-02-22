//! Error types for the Signal Fish client.

use crate::error_codes::ErrorCode;
use thiserror::Error;

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

    /// Attempted a room operation but the client is not in a room.
    #[error("not in a room")]
    NotInRoom,

    /// The server returned an error message.
    #[error("server error: {message}")]
    ServerError {
        /// Human-readable error message from the server.
        message: String,
        /// Structured error code, if provided by the server.
        error_code: Option<ErrorCode>,
    },

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
