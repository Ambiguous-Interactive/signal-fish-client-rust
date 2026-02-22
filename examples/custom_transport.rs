//! # Custom Transport Example
//!
//! Shows how to implement the [`Transport`] trait with a simple in-process
//! loopback channel. This is useful for:
//!
//! - **Testing** — unit-test your game logic without a real server
//! - **Custom backends** — adapt any I/O layer (TCP, QUIC, WebRTC data channels)
//!
//! ## Running
//!
//! ```sh
//! cargo run --example custom_transport
//! ```

use async_trait::async_trait;
use signal_fish_client::{
    SignalFishClient, SignalFishConfig, SignalFishError, SignalFishEvent, Transport,
};
use tokio::sync::mpsc;

// ─────────────────────────────────────────────────────────────────────
// Step 1: Define a channel-based "loopback" transport
// ─────────────────────────────────────────────────────────────────────

/// A loopback transport that shuttles messages through in-process channels.
///
/// This transport consists of two halves:
/// - The **client half** (`LoopbackTransport`) implements [`Transport`] and is
///   handed to `SignalFishClient::start`.
/// - The **server half** (`LoopbackServer`) lets you inject responses and read
///   what the client sent — perfect for testing.
pub struct LoopbackTransport {
    /// Messages the client sends go here (server reads from the other end).
    tx: mpsc::UnboundedSender<String>,
    /// Messages the server sends arrive here (client reads them).
    rx: mpsc::UnboundedReceiver<String>,
}

/// The "server side" of the loopback — use this to drive the conversation.
pub struct LoopbackServer {
    /// Read what the client sent.
    pub rx: mpsc::UnboundedReceiver<String>,
    /// Send messages to the client (as if they came from a server).
    pub tx: mpsc::UnboundedSender<String>,
}

/// Create a connected `(transport, server)` pair.
fn loopback_pair() -> (LoopbackTransport, LoopbackServer) {
    // Client → Server channel
    let (client_tx, server_rx) = mpsc::unbounded_channel();
    // Server → Client channel
    let (server_tx, client_rx) = mpsc::unbounded_channel();

    let transport = LoopbackTransport {
        tx: client_tx,
        rx: client_rx,
    };
    let server = LoopbackServer {
        rx: server_rx,
        tx: server_tx,
    };

    (transport, server)
}

// ─────────────────────────────────────────────────────────────────────
// Step 2: Implement the Transport trait
// ─────────────────────────────────────────────────────────────────────

#[async_trait]
impl Transport for LoopbackTransport {
    /// Send a JSON message to the "server" side of the loopback.
    async fn send(&mut self, message: String) -> Result<(), SignalFishError> {
        self.tx
            .send(message)
            .map_err(|e| SignalFishError::TransportSend(e.to_string()))
    }

    /// Receive the next message from the "server" side.
    ///
    /// Returns `None` when the server channel is closed — this is how the
    /// client discovers that the connection has ended.
    ///
    /// This method is **cancel-safe** because `mpsc::UnboundedReceiver::recv`
    /// is cancel-safe.
    async fn recv(&mut self) -> Option<Result<String, SignalFishError>> {
        self.rx.recv().await.map(Ok)
    }

    /// Close is a no-op for channels — dropping is sufficient.
    async fn close(&mut self) -> Result<(), SignalFishError> {
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────
// Step 3: Wire together the client and the fake server
// ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing for readable output.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Create the loopback pair.
    let (transport, mut server) = loopback_pair();

    // Start the client — it will immediately send an Authenticate message
    // through the loopback.
    let config = SignalFishConfig::new("mb_app_test");
    let (mut client, mut event_rx) = SignalFishClient::start(transport, config);

    // ── Fake server: read the Authenticate message and respond ──────
    // The client auto-sends Authenticate on start.
    let Some(auth_msg) = server.rx.recv().await else {
        return Err("server channel closed before Authenticate was received".into());
    };
    tracing::info!("Server received: {auth_msg}");

    // Respond with a synthetic Authenticated event (the JSON must match
    // the server's wire format — adjacently-tagged: {"type": "Variant", "data": {…}}).
    let auth_response = serde_json::json!({
        "type": "Authenticated",
        "data": {
            "app_name": "Test App",
            "organization": null,
            "rate_limits": {
                "per_minute": 60,
                "per_hour": 3600,
                "per_day": 86400
            }
        }
    });
    server.tx.send(auth_response.to_string())?;

    // ── Read events from the client ─────────────────────────────────
    // We expect Connected (synthetic) and then Authenticated.
    let mut events_seen = 0;
    while let Some(event) = event_rx.recv().await {
        match &event {
            SignalFishEvent::Connected => {
                tracing::info!("Event: Connected (synthetic)");
            }
            SignalFishEvent::Authenticated { app_name, .. } => {
                tracing::info!("Event: Authenticated — app_name={app_name}");
            }
            SignalFishEvent::Disconnected { reason } => {
                tracing::info!(
                    "Event: Disconnected — {}",
                    reason.as_deref().unwrap_or("clean")
                );
                break;
            }
            other => {
                tracing::info!("Event: {other:?}");
            }
        }

        events_seen += 1;
        // After seeing both events, shut down.
        if events_seen >= 2 {
            break;
        }
    }

    // ── Clean shutdown ──────────────────────────────────────────────
    client.shutdown().await;
    tracing::info!("Done — saw {events_seen} event(s). Custom transport works!");
    Ok(())
}
