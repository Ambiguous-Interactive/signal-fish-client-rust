//! Transport implementations for the Signal Fish signaling protocol.
//!
//! This module provides concrete [`Transport`](crate::Transport) implementations
//! behind feature gates. Enable the corresponding Cargo feature to pull in
//! a transport:
//!
//! | Feature                | Transport              |
//! |------------------------|------------------------|
//! | `transport-websocket`  | [`WebSocketTransport`] |
//! | `transport-websocket-emscripten` | `EmscriptenWebSocketTransport` |
//!
//! # Example
//!
//! ```rust,no_run
//! # #[cfg(feature = "transport-websocket")]
//! # async fn example() -> Result<(), signal_fish_client::SignalFishError> {
//! use signal_fish_client::{SignalFishClient, SignalFishConfig, WebSocketTransport};
//!
//! let transport = WebSocketTransport::connect("ws://localhost:3536/ws").await?;
//! let config = SignalFishConfig::new("mb_app_example");
//! let (mut client, mut events) = SignalFishClient::start(transport, config);
//!
//! // Observe the initial readiness or terminal event, then choose when to stop.
//! let _first_event = events.recv().await;
//! client.shutdown().await;
//! while events.recv().await.is_some() {}
//! # Ok(())
//! # }
//! ```

#[cfg(feature = "transport-websocket")]
pub mod websocket;

#[cfg(feature = "transport-websocket")]
pub use websocket::{WebSocketConnectOptions, WebSocketTransport};

#[cfg(all(
    feature = "transport-websocket-emscripten",
    any(target_os = "emscripten", test)
))]
mod emscripten_cleanup;

// Gated on both feature and target: this module uses Emscripten's C WebSocket API,
// which only exists on wasm32-unknown-emscripten. The dual gate keeps `--all-features`
// working on non-Emscripten hosts (features must be additive per Cargo convention).
// A defense-in-depth `compile_error!()` inside the file catches any bypass.
#[cfg(all(feature = "transport-websocket-emscripten", target_os = "emscripten"))]
pub mod emscripten_websocket;

#[cfg(all(feature = "transport-websocket-emscripten", target_os = "emscripten"))]
#[allow(deprecated)]
pub use emscripten_websocket::EmscriptenWebSocketTransport;
