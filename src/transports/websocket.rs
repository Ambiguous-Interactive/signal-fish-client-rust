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
//! use signal_fish_client::WebSocketTransport;
//!
//! let transport = WebSocketTransport::connect("ws://localhost:3536/ws").await?;
//! let _transport = transport; // pass it to SignalFishClient::start
//! # Ok(())
//! # }
//! ```

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::{Sink, Stream};
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::error::SignalFishError;
use crate::transport::{Transport, TransportCloseInfo, TransportFrame};

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
/// # Polling Safety
///
/// [`poll_recv`](Transport::poll_recv) preserves the WebSocket stream's partial
/// receive state across `Poll::Pending` and registers the supplied waker.
#[derive(Debug)]
pub struct WebSocketTransport {
    stream: WsStream,
    closed: bool,
    close_info: Option<TransportCloseInfo>,
    send_started: bool,
    control_flush_pending: bool,
    peer_close_pending: bool,
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
            close_info: None,
            send_started: false,
            control_flush_pending: false,
            peer_close_pending: false,
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
            close_info: None,
            send_started: false,
            control_flush_pending: false,
            peer_close_pending: false,
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

impl Transport for WebSocketTransport {
    fn poll_send(
        &mut self,
        cx: &mut Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> Poll<Result<(), SignalFishError>> {
        if self.closed || self.peer_close_pending {
            return Poll::Ready(Err(SignalFishError::TransportClosed));
        }
        if !self.send_started {
            match Pin::new(&mut self.stream).poll_ready(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => {
                    return Poll::Ready(Err(SignalFishError::TransportSend(error.to_string())))
                }
                Poll::Ready(Ok(())) => {}
            }
            let Some(frame) = frame.take() else {
                return Poll::Ready(Ok(()));
            };
            let message = match frame {
                TransportFrame::Text(text) => Message::Text(text.into()),
                TransportFrame::Binary(bytes) => Message::Binary(bytes.into()),
            };
            if let Err(error) = Pin::new(&mut self.stream).start_send(message) {
                return Poll::Ready(Err(SignalFishError::TransportSend(error.to_string())));
            }
            self.send_started = true;
        }
        match Pin::new(&mut self.stream).poll_flush(cx) {
            Poll::Ready(Ok(())) => {
                self.send_started = false;
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => {
                self.send_started = false;
                Poll::Ready(Err(SignalFishError::TransportSend(error.to_string())))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<TransportFrame, SignalFishError>>> {
        loop {
            if self.control_flush_pending {
                match Pin::new(&mut self.stream).poll_flush(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Err(error)) => {
                        self.control_flush_pending = false;
                        self.peer_close_pending = false;
                        self.closed = true;
                        return Poll::Ready(Some(Err(SignalFishError::TransportReceive(
                            error.to_string(),
                        ))));
                    }
                    Poll::Ready(Ok(())) => {
                        self.control_flush_pending = false;
                        if self.peer_close_pending {
                            self.peer_close_pending = false;
                            self.closed = true;
                            return Poll::Ready(None);
                        }
                    }
                }
            }
            let msg = match Pin::new(&mut self.stream).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(value) => match value {
                    Some(Ok(msg)) => msg,
                    Some(Err(e)) => {
                        return Poll::Ready(Some(Err(SignalFishError::TransportReceive(
                            e.to_string(),
                        ))));
                    }
                    None => return Poll::Ready(None),
                },
            };

            match msg {
                // `Utf8Bytes::to_string()` copies the payload into a new `String`
                // because `Utf8Bytes` does not expose the inner buffer by value.
                Message::Text(text) => {
                    return Poll::Ready(Some(Ok(TransportFrame::Text(text.to_string()))))
                }
                Message::Binary(bytes) => {
                    return Poll::Ready(Some(Ok(TransportFrame::Binary(bytes.to_vec()))))
                }
                Message::Close(frame) => {
                    tracing::debug!(?frame, "received WebSocket close frame");
                    // Remember structured close metadata so the client can
                    // attribute the disconnect via `close_info()`.
                    if let Some(frame) = frame {
                        self.close_info = Some(TransportCloseInfo {
                            code: Some(frame.code.into()),
                            reason: (!frame.reason.is_empty()).then(|| frame.reason.to_string()),
                            clean: None,
                            initiated_by_peer: true,
                        });
                    } else {
                        self.close_info = Some(TransportCloseInfo {
                            initiated_by_peer: true,
                            ..TransportCloseInfo::default()
                        });
                    }
                    // Tungstenite has queued the mandatory close response. Drive
                    // its flush before reporting the terminal receive state so a
                    // polling client cannot strand the handshake after seeing
                    // `None` and ceasing to poll the transport.
                    self.peer_close_pending = true;
                    self.control_flush_pending = true;
                }
                Message::Ping(_) => {
                    tracing::debug!("received WebSocket ping (auto-pong handled by tungstenite)");
                    self.control_flush_pending = true;
                }
                Message::Pong(_) => {
                    tracing::debug!("received WebSocket pong (ignored)");
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

    fn poll_close(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), SignalFishError>> {
        if self.closed {
            return Poll::Ready(Ok(()));
        }
        if self.peer_close_pending {
            return match Pin::new(&mut self.stream).poll_flush(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Ok(())) => {
                    self.control_flush_pending = false;
                    self.peer_close_pending = false;
                    self.closed = true;
                    Poll::Ready(Ok(()))
                }
                Poll::Ready(Err(error)) => {
                    self.control_flush_pending = false;
                    self.peer_close_pending = false;
                    self.closed = true;
                    Poll::Ready(Err(SignalFishError::TransportSend(error.to_string())))
                }
            };
        }
        match Pin::new(&mut self.stream).poll_close(cx) {
            Poll::Ready(Ok(())) => {
                self.closed = true;
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => {
                self.closed = true;
                Poll::Ready(Err(SignalFishError::TransportSend(error.to_string())))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn close_info(&self) -> Option<TransportCloseInfo> {
        self.close_info.clone()
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
    use futures_util::{SinkExt, StreamExt};

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
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("TcpListener must bind to localhost");
        let addr = listener
            .local_addr()
            .expect("TcpListener must have a local address");

        tokio::spawn(async move {
            let (tcp, _) = listener
                .accept()
                .await
                .expect("TcpListener must accept a connection");
            let ws = tokio_tungstenite::accept_async(tcp)
                .await
                .expect("WebSocket handshake must succeed");
            handler(ws).await;
        });

        format!("ws://{addr}")
    }

    // ── Mock-stream tests ────────────────────────────────────────────────

    #[tokio::test]
    async fn recv_receives_text_messages() {
        let url = start_mock_server(|mut ws| async move {
            ws.send(Message::Text("hello".into()))
                .await
                .expect("server must send 'hello'");
            ws.send(Message::Text("world".into()))
                .await
                .expect("server must send 'world'");
            ws.close(None).await.expect("server must close cleanly");
        })
        .await;

        let mut transport = WebSocketTransport::connect(&url)
            .await
            .expect("WebSocket connect must succeed");

        let msg1 = crate::transport::recv_frame(&mut transport)
            .await
            .expect("recv must return Some")
            .expect("recv must return Ok");
        assert_eq!(msg1, TransportFrame::Text("hello".into()));

        let msg2 = crate::transport::recv_frame(&mut transport)
            .await
            .expect("recv must return Some")
            .expect("recv must return Ok");
        assert_eq!(msg2, TransportFrame::Text("world".into()));
    }

    #[tokio::test]
    async fn recv_returns_none_on_close_frame() {
        let url = start_mock_server(|mut ws| async move {
            ws.close(None).await.expect("server must close cleanly");
        })
        .await;

        let mut transport = WebSocketTransport::connect(&url)
            .await
            .expect("WebSocket connect must succeed");
        let result = crate::transport::recv_frame(&mut transport).await;
        assert!(result.is_none());
        // A bare close (today's server behavior) carries no explanation.
        assert_eq!(transport.close_info().and_then(|info| info.reason), None);
    }

    #[tokio::test]
    async fn close_frame_reason_is_captured() {
        use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
        use tokio_tungstenite::tungstenite::protocol::frame::CloseFrame;

        let url = start_mock_server(|mut ws| async move {
            ws.close(Some(CloseFrame {
                code: CloseCode::Policy,
                reason: "slow consumer".into(),
            }))
            .await
            .expect("server must close with a frame");
        })
        .await;

        let mut transport = WebSocketTransport::connect(&url)
            .await
            .expect("WebSocket connect must succeed");
        let result = crate::transport::recv_frame(&mut transport).await;
        assert!(result.is_none());

        let reason = transport
            .close_info()
            .and_then(|info| info.reason)
            .expect("close frame explanation must be captured");
        assert!(
            reason.contains("slow consumer"),
            "captured reason should include the frame text: {reason}"
        );
    }

    #[tokio::test]
    async fn peer_close_response_is_flushed_before_recv_finishes() {
        use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
        use tokio_tungstenite::tungstenite::protocol::frame::CloseFrame;

        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        let url = start_mock_server(move |mut ws| async move {
            ws.send(Message::Close(Some(CloseFrame {
                code: CloseCode::Away,
                reason: "server draining".into(),
            })))
            .await
            .expect("server must send a peer close frame");

            let response = tokio::time::timeout(std::time::Duration::from_secs(1), ws.next()).await;
            let observed_close_response = matches!(
                response,
                Ok(Some(Ok(Message::Close(Some(CloseFrame {
                    code: CloseCode::Away,
                    ..
                })))))
            );
            let _ = response_tx.send(observed_close_response);
        })
        .await;

        let mut transport = WebSocketTransport::connect(&url)
            .await
            .expect("WebSocket connect must succeed");
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            crate::transport::recv_frame(&mut transport),
        )
        .await
        .expect("receiving a peer close must make progress");

        assert!(result.is_none());
        assert!(
            response_rx
                .await
                .expect("server task must report whether it received the response"),
            "client must flush a matching close response before recv returns None"
        );
        assert!(transport.closed);
        assert!(!transport.peer_close_pending);
    }

    #[tokio::test]
    async fn recv_passes_binary_frames_through() {
        let url = start_mock_server(|mut ws| async move {
            ws.send(Message::Binary(vec![0xDE, 0xAD].into()))
                .await
                .expect("server must send binary frame");
            ws.send(Message::Text("after_binary".into()))
                .await
                .expect("server must send 'after_binary'");
            ws.close(None).await.expect("server must close cleanly");
        })
        .await;

        let mut transport = WebSocketTransport::connect(&url)
            .await
            .expect("WebSocket connect must succeed");

        let msg = crate::transport::recv_frame(&mut transport)
            .await
            .expect("recv must return Some")
            .expect("recv must return Ok");
        assert_eq!(msg, TransportFrame::Binary(vec![0xDE, 0xAD]));
        let next = crate::transport::recv_frame(&mut transport)
            .await
            .expect("recv must return Some")
            .expect("recv must return Ok");
        assert_eq!(next, TransportFrame::Text("after_binary".into()));
    }

    #[tokio::test]
    async fn send_after_close_returns_transport_closed() {
        let url = start_mock_server(|mut ws| async move {
            // Read until the client closes.
            while let Some(Ok(_)) = ws.next().await {}
        })
        .await;

        let mut transport = WebSocketTransport::connect(&url)
            .await
            .expect("WebSocket connect must succeed");
        crate::transport::close_transport(&mut transport)
            .await
            .expect("close must succeed");

        let err = crate::transport::send_frame(&mut transport, TransportFrame::Text("oops".into()))
            .await
            .unwrap_err();
        assert!(matches!(err, SignalFishError::TransportClosed));
    }

    #[tokio::test]
    async fn double_close_is_idempotent() {
        let url =
            start_mock_server(|mut ws| async move { while let Some(Ok(_)) = ws.next().await {} })
                .await;

        let mut transport = WebSocketTransport::connect(&url)
            .await
            .expect("WebSocket connect must succeed");
        crate::transport::close_transport(&mut transport)
            .await
            .expect("first close must succeed");
        // Second close should also succeed.
        crate::transport::close_transport(&mut transport)
            .await
            .expect("second close must succeed (idempotent)");
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
                .expect("server must send 'from_stream_msg'");
            ws.close(None).await.expect("server must close cleanly");
        })
        .await;

        // Connect the raw stream ourselves, then wrap it.
        let (ws_stream, _) = tokio_tungstenite::connect_async(&url)
            .await
            .expect("raw WebSocket connect must succeed");
        let mut transport = WebSocketTransport::from_stream(ws_stream);

        let msg = crate::transport::recv_frame(&mut transport)
            .await
            .expect("recv must return Some")
            .expect("recv must return Ok");
        assert_eq!(msg, TransportFrame::Text("from_stream_msg".into()));
    }

    #[tokio::test]
    async fn send_round_trip() {
        let url = start_mock_server(|mut ws| async move {
            // Read one message and echo it back.
            if let Some(Ok(Message::Text(text))) = ws.next().await {
                ws.send(Message::Text(text))
                    .await
                    .expect("server must echo message back");
            }
            ws.close(None).await.expect("server must close cleanly");
        })
        .await;

        let mut transport = WebSocketTransport::connect(&url)
            .await
            .expect("WebSocket connect must succeed");
        crate::transport::send_frame(&mut transport, TransportFrame::Text("ping_echo".into()))
            .await
            .expect("send must succeed");

        let msg = crate::transport::recv_frame(&mut transport)
            .await
            .expect("recv must return Some")
            .expect("recv must return Ok");
        assert_eq!(msg, TransportFrame::Text("ping_echo".into()));
    }

    #[tokio::test]
    async fn recv_after_close_returns_none_or_error() {
        let url =
            start_mock_server(|mut ws| async move { while let Some(Ok(_)) = ws.next().await {} })
                .await;

        let mut transport = WebSocketTransport::connect(&url)
            .await
            .expect("WebSocket connect must succeed");
        crate::transport::close_transport(&mut transport)
            .await
            .expect("close must succeed");

        // After closing, recv must not hang — it should return None or an error.
        let result = crate::transport::recv_frame(&mut transport).await;
        match result {
            None => {}         // stream ended — expected
            Some(Err(_)) => {} // transport error — also acceptable
            Some(Ok(msg)) => panic!("expected None or error after close, got Ok({msg:?})"),
        }
    }
}
