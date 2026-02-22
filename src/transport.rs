//! Transport abstraction for the Signal Fish signaling protocol.
//!
//! The [`Transport`] trait defines a bidirectional text message channel between
//! the client and server. The signaling protocol uses JSON text messages, so
//! every transport implementation must handle message framing internally
//! (e.g., WebSocket frames, length-prefixed TCP, QUIC streams, UDP datagrams).
//!
//! # Connection Setup
//!
//! Connection setup is intentionally NOT part of this trait — different
//! transports have fundamentally different connection parameters (URLs for
//! WebSocket, host:port for TCP, QUIC endpoints, etc.). Construct a connected
//! transport externally, then pass it to `SignalFishClient::start`.
//!
//! # Implementing a Custom Transport
//!
//! ```rust,no_run
//! use async_trait::async_trait;
//! use signal_fish_client::error::SignalFishError;
//! use signal_fish_client::transport::Transport;
//!
//! struct MyTransport { /* ... */ }
//!
//! #[async_trait]
//! impl Transport for MyTransport {
//!     async fn send(&mut self, message: String) -> Result<(), SignalFishError> {
//!         // Send the JSON text message over your transport
//!         todo!()
//!     }
//!
//!     async fn recv(&mut self) -> Option<Result<String, SignalFishError>> {
//!         // Receive the next JSON text message
//!         // Return None when the connection is closed cleanly
//!         todo!()
//!     }
//!
//!     async fn close(&mut self) -> Result<(), SignalFishError> {
//!         // Gracefully shut down the connection
//!         todo!()
//!     }
//! }
//! ```

use async_trait::async_trait;

use crate::error::SignalFishError;

/// A bidirectional text message transport for the Signal Fish signaling protocol.
///
/// Implementors shuttle serialized JSON strings between the client and server.
/// Each call to [`send`](Transport::send) transmits one complete JSON message.
/// Each call to [`recv`](Transport::recv) returns one complete JSON message.
///
/// # Object Safety
///
/// This trait is object-safe, so `Box<dyn Transport>` works for dynamic dispatch.
/// However, `SignalFishClient::start` accepts `impl Transport` (monomorphized)
/// for the common case.
///
/// # Cancel Safety
///
/// The [`recv`](Transport::recv) method **MUST** be cancel-safe because it is used
/// inside `tokio::select!`. If `recv` is cancelled before completion, calling it
/// again must not lose data. Channel-based implementations (e.g., wrapping
/// `mpsc::Receiver`) are naturally cancel-safe.
#[async_trait]
pub trait Transport: Send + 'static {
    /// Send a JSON text message to the server.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::TransportSend`] if the message could not be sent
    /// (e.g., connection broken, write buffer full).
    async fn send(&mut self, message: String) -> Result<(), SignalFishError>;

    /// Receive the next JSON text message from the server.
    ///
    /// Returns:
    /// - `Some(Ok(text))` — a complete message was received
    /// - `Some(Err(e))` — a transport error occurred (e.g., [`SignalFishError::TransportReceive`])
    /// - `None` — the connection was closed cleanly by the server
    ///
    /// # Cancel Safety
    ///
    /// This method **MUST** be cancel-safe (see [trait documentation](Transport)).
    async fn recv(&mut self) -> Option<Result<String, SignalFishError>>;

    /// Close the transport connection gracefully.
    ///
    /// After calling this method, subsequent calls to [`send`](Transport::send) and
    /// [`recv`](Transport::recv) may return errors or `None`.
    ///
    /// # Errors
    ///
    /// Returns an error if the graceful shutdown fails. Implementations should
    /// still release resources even if the close handshake fails.
    async fn close(&mut self) -> Result<(), SignalFishError>;
}
