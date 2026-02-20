//! Error types for the Signal Fish client.

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
        error_code: Option<String>,
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
