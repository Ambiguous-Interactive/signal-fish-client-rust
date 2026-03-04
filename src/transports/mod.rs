//! Transport implementations for the Signal Fish signaling protocol.
//!
//! This module provides concrete [`Transport`](crate::Transport) implementations
//! behind feature gates. Enable the corresponding Cargo feature to pull in
//! a transport:
//!
//! | Feature                | Transport              |
//! |------------------------|------------------------|
//! | `transport-websocket`  | [`WebSocketTransport`] |
//! | `transport-websocket-emscripten` | [`EmscriptenWebSocketTransport`] |
//!
//! # Example
//!
//! ```rust,ignore
//! # async fn example() -> Result<(), signal_fish_client::SignalFishError> {
//! use signal_fish_client::{WebSocketTransport, Transport};
//!
//! let mut ws = WebSocketTransport::connect("ws://localhost:3536/ws").await?;
//! ws.send(r#"{"type":"ping"}"#.to_string()).await?;
//!
//! if let Some(Ok(msg)) = ws.recv().await {
//!     println!("server said: {msg}");
//! }
//!
//! ws.close().await?;
//! # Ok(())
//! # }
//! ```

#[cfg(feature = "transport-websocket")]
pub mod websocket;

#[cfg(feature = "transport-websocket")]
pub use websocket::WebSocketTransport;

#[cfg(feature = "transport-websocket-emscripten")]
pub mod emscripten_websocket;

#[cfg(feature = "transport-websocket-emscripten")]
pub use emscripten_websocket::EmscriptenWebSocketTransport;
