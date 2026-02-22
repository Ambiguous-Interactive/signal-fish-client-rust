//! WebSocket transport implementation using `tokio-tungstenite`.
//!
//! This module provides [`WebSocketTransport`], a [`Transport`]
//! implementation that communicates over a WebSocket connection. Both `ws://` and
//! `wss://` URLs are supported — TLS is handled transparently via
//! [`MaybeTlsStream`](tokio_tungstenite::MaybeTlsStream).
//!
//! # Feature gate
//!
//! This module is only available when the `transport-websocket` feature is enabled
//! (it is enabled by default).
//!
//! # Example
//!
//! ```rust,no_run
//! # async fn example() -> Result<(), signal_fish_client::SignalFishError> {
//! use signal_fish_client::{WebSocketTransport, Transport};
//!
//! let mut transport = WebSocketTransport::connect("ws://localhost:3536/ws").await?;
//! transport.send("hello".to_string()).await?;
//!
//! if let Some(Ok(msg)) = transport.recv().await {
//!     println!("received: {msg}");
//! }
//!
//! transport.close().await?;
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::error::SignalFishError;
use crate::transport::Transport;

/// Type alias for the underlying WebSocket stream.
///
/// Made public so that callers can construct a [`WebSocketTransport`] from an
/// existing stream via [`WebSocketTransport::from_stream`].
pub type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A [`Transport`] implementation backed by a WebSocket connection.
///
/// Wraps a `tokio-tungstenite` [`WebSocketStream`](tokio_tungstenite::WebSocketStream)
/// and translates between the Signal Fish text-message protocol and WebSocket frames.
///
/// # Construction
///
/// Use [`WebSocketTransport::connect`] to establish a new connection:
///
/// ```rust,no_run
/// # async fn example() -> Result<(), signal_fish_client::SignalFishError> {
/// use signal_fish_client::WebSocketTransport;
///
/// let transport = WebSocketTransport::connect("ws://localhost:3536/ws").await?;
/// # Ok(())
/// # }
/// ```
///
/// For advanced use-cases (custom TLS, proxy, headers) construct the stream
/// yourself and use [`WebSocketTransport::from_stream`].
///
/// # Cancel Safety
///
/// The [`recv`](Transport::recv) method is cancel-safe. Dropping the future
/// returned by `recv` before it completes will not consume or lose any messages,
/// making it safe to use inside `tokio::select!`.
#[derive(Debug)]
pub struct WebSocketTransport {
    stream: WsStream,
    closed: bool,
}

impl WebSocketTransport {
    /// Establish a new WebSocket connection to the given URL.
    ///
    /// Supports both `ws://` and `wss://` schemes. TLS is handled automatically
    /// by `tokio-tungstenite` via [`MaybeTlsStream`](tokio_tungstenite::MaybeTlsStream).
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::Io`] if the URL is invalid or the connection
    /// cannot be established. When the underlying error is an I/O error its
    /// [`ErrorKind`](std::io::ErrorKind) is preserved; all other errors are
    /// mapped to [`ErrorKind::Other`](std::io::ErrorKind::Other).
    pub async fn connect(url: &str) -> Result<Self, SignalFishError> {
        tracing::debug!(url = %url, "connecting to WebSocket server");

        let (stream, _response) = tokio_tungstenite::connect_async(url).await.map_err(|e| {
            let kind = match &e {
                tokio_tungstenite::tungstenite::Error::Io(io) => io.kind(),
                _ => std::io::ErrorKind::Other,
            };
            SignalFishError::Io(std::io::Error::new(kind, e))
        })?;

        tracing::info!(url = %url, "WebSocket connection established");

        Ok(Self {
            stream,
            closed: false,
        })
    }

    /// Create a [`WebSocketTransport`] from an already-established WebSocket stream.
    ///
    /// This is useful when you need custom TLS configuration, proxy headers, or
    /// any other connection setup that [`connect`](Self::connect) does not expose.
    pub fn from_stream(stream: WsStream) -> Self {
        Self {
            stream,
            closed: false,
        }
    }

    /// Establish a new WebSocket connection with a timeout.
    ///
    /// Behaves identically to [`connect`](Self::connect) but fails with
    /// [`SignalFishError::Timeout`] if the connection is not established within
    /// the given duration.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::Timeout`] if the deadline elapses, or any
    /// error that [`connect`](Self::connect) may return.
    pub async fn connect_with_timeout(
        url: &str,
        timeout: std::time::Duration,
    ) -> Result<Self, SignalFishError> {
        tokio::time::timeout(timeout, Self::connect(url))
            .await
            .map_err(|_| SignalFishError::Timeout)?
    }
}

#[async_trait]
impl Transport for WebSocketTransport {
    async fn send(&mut self, message: String) -> Result<(), SignalFishError> {
        if self.closed {
            return Err(SignalFishError::TransportClosed);
        }
        self.stream
            .send(Message::Text(message.into()))
            .await
            .map_err(|e| SignalFishError::TransportSend(e.to_string()))
    }

    async fn recv(&mut self) -> Option<Result<String, SignalFishError>> {
        loop {
            let msg = match self.stream.next().await {
                Some(Ok(msg)) => msg,
                Some(Err(e)) => {
                    return Some(Err(SignalFishError::TransportReceive(e.to_string())));
                }
                None => return None,
            };

            match msg {
                // `Utf8Bytes::to_string()` copies the payload into a new `String`
                // because `Utf8Bytes` does not expose the inner buffer by value.
                Message::Text(text) => return Some(Ok(text.to_string())),
                Message::Close(frame) => {
                    tracing::debug!(?frame, "received WebSocket close frame");
                    return None;
                }
                Message::Ping(_) => {
                    tracing::debug!("received WebSocket ping (auto-pong handled by tungstenite)");
                    // tungstenite auto-queues a Pong reply; no manual response needed.
                }
                Message::Pong(_) => {
                    tracing::debug!("received WebSocket pong (ignored)");
                    // Continue the loop.
                }
                Message::Binary(_) => {
                    tracing::warn!("received unexpected binary WebSocket frame, skipping");
                    // Continue the loop.
                }
                Message::Frame(_) => {
                    // This variant is never produced by the read half of the stream;
                    // it exists only for exhaustiveness against future `Message`
                    // variants. We keep the arm to satisfy exhaustiveness checks.
                    tracing::debug!("received raw WebSocket frame, skipping");
                    // Continue the loop.
                }
            }
        }
    }

    async fn close(&mut self) -> Result<(), SignalFishError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.stream
            .close(None)
            .await
            .map_err(|e| SignalFishError::TransportSend(e.to_string()))
    }
}

#[cfg(test)]
#[cfg(feature = "transport-websocket")]
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
    fn websocket_transport_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<WebSocketTransport>();
    }

    #[test]
    fn websocket_transport_is_debug() {
        fn assert_debug<T: std::fmt::Debug>() {}
        assert_debug::<WebSocketTransport>();
    }

    #[tokio::test]
    async fn connect_fails_with_invalid_url() {
        let result = WebSocketTransport::connect("not-a-valid-url").await;
        let err = result.unwrap_err();
        assert!(matches!(err, SignalFishError::Io(_)));
    }

    #[tokio::test]
    async fn connect_fails_with_unreachable_host() {
        let result = WebSocketTransport::connect("ws://127.0.0.1:1").await;
        let err = result.unwrap_err();
        assert!(matches!(err, SignalFishError::Io(_)));
    }

    // ── Mock-stream helpers ──────────────────────────────────────────────

    use tokio::net::TcpListener;

    /// Start a local WebSocket server that runs `handler` on the accepted
    /// connection and returns the address to connect to.
    async fn start_mock_server<F, Fut>(handler: F) -> String
    where
        F: FnOnce(tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) -> Fut
            + Send
            + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            handler(ws).await;
        });

        format!("ws://{addr}")
    }

    // ── Mock-stream tests ────────────────────────────────────────────────

    #[tokio::test]
    async fn recv_receives_text_messages() {
        let url = start_mock_server(|mut ws| async move {
            ws.send(Message::Text("hello".into())).await.unwrap();
            ws.send(Message::Text("world".into())).await.unwrap();
            ws.close(None).await.unwrap();
        })
        .await;

        let mut transport = WebSocketTransport::connect(&url).await.unwrap();

        let msg1 = transport.recv().await.unwrap().unwrap();
        assert_eq!(msg1, "hello");

        let msg2 = transport.recv().await.unwrap().unwrap();
        assert_eq!(msg2, "world");
    }

    #[tokio::test]
    async fn recv_returns_none_on_close_frame() {
        let url = start_mock_server(|mut ws| async move {
            ws.close(None).await.unwrap();
        })
        .await;

        let mut transport = WebSocketTransport::connect(&url).await.unwrap();
        let result = transport.recv().await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn recv_skips_binary_frames() {
        let url = start_mock_server(|mut ws| async move {
            ws.send(Message::Binary(vec![0xDE, 0xAD].into()))
                .await
                .unwrap();
            ws.send(Message::Text("after_binary".into())).await.unwrap();
            ws.close(None).await.unwrap();
        })
        .await;

        let mut transport = WebSocketTransport::connect(&url).await.unwrap();

        // The binary frame should be silently skipped.
        let msg = transport.recv().await.unwrap().unwrap();
        assert_eq!(msg, "after_binary");
    }

    #[tokio::test]
    async fn send_after_close_returns_transport_closed() {
        let url = start_mock_server(|mut ws| async move {
            // Read until the client closes.
            while let Some(Ok(_)) = ws.next().await {}
        })
        .await;

        let mut transport = WebSocketTransport::connect(&url).await.unwrap();
        transport.close().await.unwrap();

        let err = transport.send("oops".to_string()).await.unwrap_err();
        assert!(matches!(err, SignalFishError::TransportClosed));
    }

    #[tokio::test]
    async fn double_close_is_idempotent() {
        let url =
            start_mock_server(|mut ws| async move { while let Some(Ok(_)) = ws.next().await {} })
                .await;

        let mut transport = WebSocketTransport::connect(&url).await.unwrap();
        transport.close().await.unwrap();
        // Second close should also succeed.
        transport.close().await.unwrap();
    }

    #[tokio::test]
    async fn connect_with_timeout_times_out() {
        // Use a non-routable address to guarantee a timeout.
        let result = WebSocketTransport::connect_with_timeout(
            "ws://192.0.2.1:1",
            std::time::Duration::from_millis(50),
        )
        .await;

        let err = result.unwrap_err();
        assert!(matches!(err, SignalFishError::Timeout));
    }

    #[tokio::test]
    async fn from_stream_constructor_works() {
        let url = start_mock_server(|mut ws| async move {
            ws.send(Message::Text("from_stream_msg".into()))
                .await
                .unwrap();
            ws.close(None).await.unwrap();
        })
        .await;

        // Connect the raw stream ourselves, then wrap it.
        let (ws_stream, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let mut transport = WebSocketTransport::from_stream(ws_stream);

        let msg = transport.recv().await.unwrap().unwrap();
        assert_eq!(msg, "from_stream_msg");
    }

    #[tokio::test]
    async fn send_round_trip() {
        let url = start_mock_server(|mut ws| async move {
            // Read one message and echo it back.
            if let Some(Ok(Message::Text(text))) = ws.next().await {
                ws.send(Message::Text(text)).await.unwrap();
            }
            ws.close(None).await.unwrap();
        })
        .await;

        let mut transport = WebSocketTransport::connect(&url).await.unwrap();
        transport.send("ping_echo".to_string()).await.unwrap();

        let msg = transport.recv().await.unwrap().unwrap();
        assert_eq!(msg, "ping_echo");
    }

    #[tokio::test]
    async fn recv_after_close_returns_none_or_error() {
        let url =
            start_mock_server(|mut ws| async move { while let Some(Ok(_)) = ws.next().await {} })
                .await;

        let mut transport = WebSocketTransport::connect(&url).await.unwrap();
        transport.close().await.unwrap();

        // After closing, recv must not hang — it should return None or an error.
        let result = transport.recv().await;
        match result {
            None => {}         // stream ended — expected
            Some(Err(_)) => {} // transport error — also acceptable
            Some(Ok(msg)) => panic!("expected None or error after close, got Ok({msg:?})"),
        }
    }
}
