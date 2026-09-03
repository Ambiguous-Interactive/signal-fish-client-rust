//! # Automatic Reconnection Example
//!
//! Opt-in reconnection ([`SignalFishConfig::with_reconnect_policy`]) rebuilds
//! the connection from a caller-supplied **factory**: a synchronous function
//! that returns a fresh, *unconnected* transport per attempt. The built-in
//! [`WebSocketTransport`] connects asynchronously, so it cannot be constructed
//! inside the factory directly — this example ships the small lazy wrapper
//! that bridges the two: it defers `WebSocketTransport::connect` until the
//! driver first polls the transport.
//!
//! Use this example as a template whenever you want
//!
//! - **automatic reconnect with backoff** after retryable disconnects, and
//! - the built-in WebSocket transport (no hand-written framing).
//!
//! ## Running
//!
//! ```sh
//! # Start a Signal Fish server on localhost:3536, then:
//! cargo run --example auto_reconnect
//!
//! # Override the server URL:
//! SIGNAL_FISH_URL=ws://my-server:3536/v2/ws cargo run --example auto_reconnect
//! ```
//!
//! Try killing and restarting the server while the example runs: every
//! retryable disconnect prints a `Reconnecting` event and the client
//! re-authenticates and re-joins its room automatically.
//!
//! On protocol v3 (`.enable_v3()`) the driver additionally auto-rejoins a
//! player room after reconnecting; drop this template's
//! `join_room`-on-`Authenticated` call in that configuration, or the seat is
//! requested twice per round.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use signal_fish_client::transport::TransportFrame;
use signal_fish_client::{
    JoinRoomParams, ReconnectPolicy, SignalFishClient, SignalFishConfig, SignalFishError,
    SignalFishEvent, Transport, WebSocketTransport,
};

/// Default server URL when `SIGNAL_FISH_URL` is not set.
const DEFAULT_URL: &str = "ws://localhost:3536/v2/ws";

// ─────────────────────────────────────────────────────────────────────
// Step 1: A lazy WebSocket transport for the reconnect factory
// ─────────────────────────────────────────────────────────────────────

/// A [`WebSocketTransport`] that connects on first use.
///
/// The reconnect factory must return a transport *immediately* and must not
/// block, but a WebSocket handshake is asynchronous. This wrapper stores the
/// URL and starts the handshake the first time the client polls for I/O.
/// Until the handshake completes the wrapper reports `is_ready() == false`
/// and defers every send (`Pending` keeps the caller's frame intact), which
/// is exactly the pre-ready contract the drivers already implement.
enum LazyWebSocketTransport {
    /// No handshake started yet; the URL is connected on first poll.
    Idle { url: String },
    /// The handshake future is being polled to completion.
    Connecting(Pin<Box<dyn Future<Output = Result<WebSocketTransport, SignalFishError>> + Send>>),
    /// The handshake finished; all operations forward to the real transport.
    Ready(Box<WebSocketTransport>),
    /// The handshake failed. The stored error is delivered once, then the
    /// wrapper stays terminal: the driver tears the round down on the first
    /// error, and a configured reconnect policy starts the next attempt from
    /// a fresh value.
    Failed(Option<SignalFishError>),
    /// [`Transport::abort`] ran; no further work is possible.
    Aborted,
}

impl LazyWebSocketTransport {
    fn new(url: String) -> Self {
        Self::Idle { url }
    }

    /// Start the handshake if it has not started yet.
    fn ensure_connecting(&mut self) {
        if let Self::Idle { url } = self {
            // The future must own the URL: the stored future outlives this
            // `&mut self` call.
            let url = url.clone();
            *self = Self::Connecting(Box::pin(
                async move { WebSocketTransport::connect(&url).await },
            ));
        }
    }

    /// Drive a pending handshake to completion, if one is running.
    ///
    /// `Ready(Ok(()))` always leaves the wrapper in the `Ready` state.
    fn poll_connect(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), SignalFishError>> {
        if let Self::Connecting(future) = self {
            match future.as_mut().poll(cx) {
                Poll::Ready(Ok(transport)) => {
                    *self = Self::Ready(Box::new(transport));
                }
                Poll::Ready(Err(error)) => {
                    *self = Self::Failed(Some(error));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
        match self {
            Self::Ready(_) => Poll::Ready(Ok(())),
            // Deliver the handshake error once; later polls observe the
            // already-terminated connection.
            Self::Failed(error) => Poll::Ready(Err(error
                .take()
                .unwrap_or(SignalFishError::TransportClosed))),
            _ => Poll::Pending,
        }
    }
}

impl Transport for LazyWebSocketTransport {
    fn poll_send(
        &mut self,
        cx: &mut Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> Poll<Result<(), SignalFishError>> {
        self.ensure_connecting();
        match self.poll_connect(cx) {
            // The frame stays in the caller's slot: the ownership-transfer
            // point is backend acceptance, which has not happened yet.
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Ready(Ok(())) => match self {
                Self::Ready(transport) => transport.poll_send(cx, frame),
                // Unreachable: poll_connect only reports Ok once the wrapper
                // reached (or already sat in) the Ready state.
                _ => Poll::Pending,
            },
        }
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<TransportFrame, SignalFishError>>> {
        self.ensure_connecting();
        match self.poll_connect(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => Poll::Ready(Some(Err(error))),
            Poll::Ready(Ok(())) => match self {
                Self::Ready(transport) => transport.poll_recv(cx),
                _ => Poll::Ready(None),
            },
        }
    }

    fn poll_close(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), SignalFishError>> {
        match self {
            // Nothing was ever connected; a close is trivially complete.
            Self::Idle { .. } => Poll::Ready(Ok(())),
            Self::Connecting(_) => {
                // Cancel the handshake and report the close as complete; the
                // socket, if it races to completion underneath us, is dropped
                // with the cancelled future.
                *self = Self::Aborted;
                Poll::Ready(Ok(()))
            }
            Self::Ready(transport) => transport.poll_close(cx),
            Self::Failed(_) | Self::Aborted => Poll::Ready(Ok(())),
        }
    }

    fn abort(&mut self) {
        // Required to be prompt, non-blocking, non-panicking, and idempotent.
        *self = Self::Aborted;
    }

    fn is_ready(&self) -> bool {
        match self {
            Self::Ready(transport) => transport.is_ready(),
            _ => false,
        }
    }

    fn close_info(&self) -> Option<signal_fish_client::transport::TransportCloseInfo> {
        match self {
            Self::Ready(transport) => transport.close_info(),
            _ => None,
        }
    }

    fn max_frame_hint(&self) -> Option<usize> {
        match self {
            // Declared per connection: the drivers sample this hint once per
            // round, before the handshake runs, so a pre-ready `Some` would
            // have to hard-code the delegate's bound. Returning `None` until
            // connected simply leaves the driver's second enforcement layer
            // inactive; the delegate's own in-transport bound still applies.
            // A first-class lazy constructor in the SDK could declare the
            // bound up front.
            Self::Ready(transport) => transport.max_frame_hint(),
            _ => None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Step 2: Run a lobby client that reconnects automatically
// ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let url = std::env::var("SIGNAL_FISH_URL").unwrap_or_else(|_| DEFAULT_URL.to_string());

    // The factory is called at most once per reconnect attempt and must not
    // block. It produces a fresh *unconnected* transport each time.
    let factory_url = url.clone();
    let config = SignalFishConfig::new("mb_app_reconnect_example").with_reconnect_policy(
        ReconnectPolicy::new(move || Box::new(LazyWebSocketTransport::new(factory_url.clone()))),
    );

    // The initial transport is the same lazy type; the driver connects it on
    // its first loop iteration.
    let initial = LazyWebSocketTransport::new(url);
    let (mut client, mut events) = SignalFishClient::start(initial, config);

    let mut joined = false;
    while let Some(event) = events.recv().await {
        match event {
            SignalFishEvent::Authenticated { .. } => {
                println!("authenticated; joining the room");
                client.join_room(JoinRoomParams::new("reconnect-demo", "Alice"))?;
            }
            SignalFishEvent::RoomJoined { room_code, .. } => {
                joined = true;
                println!("joined room (presence only: {} bytes)", room_code.len());
                client.set_ready()?;
            }
            SignalFishEvent::Reconnecting {
                attempt,
                next_backoff,
            } => {
                joined = false;
                println!(
                    "connection lost; reconnect attempt {attempt} in {:.1?}",
                    next_backoff
                );
            }
            SignalFishEvent::ReconnectAbandoned { attempts, .. } => {
                println!("server unreachable after {attempts} attempts; giving up");
                break;
            }
            SignalFishEvent::Disconnected { .. } => {
                if !joined {
                    // Every retryable round delivers `Disconnected` first —
                    // including one that a `Reconnecting` event immediately
                    // follows — so this arm stays quiet mid-room to avoid
                    // noise, and only speaks up when no room membership was
                    // active.
                    println!("disconnected without an active room membership");
                }
            }
            SignalFishEvent::PlayerJoined { player, .. } => {
                println!("player {} joined", player.id);
            }
            SignalFishEvent::Error {
                message,
                error_code,
            } => {
                println!("server error ({error_code:?}): {message}");
            }
            _ => {}
        }
    }

    client.shutdown().await;
    Ok(())
}
