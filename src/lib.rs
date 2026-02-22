#![cfg_attr(docsrs, feature(doc_auto_cfg))]
//! # Signal Fish Client
//!
//! Transport-agnostic Rust client for the Signal Fish multiplayer signaling protocol.
//!
//! This crate provides a high-level async client that communicates with a Signal Fish
//! signaling server using JSON text messages over any bidirectional transport.
//!
//! ## Features
//!
//! - **Transport-agnostic** — implement the [`Transport`] trait for any backend
//! - **Wire-compatible** — all protocol types match the server's v2 format exactly
//! - **WebSocket built-in** — default `transport-websocket` feature provides `WebSocketTransport`
//! - **Event-driven** — receive typed `SignalFishEvent`s via a channel
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use signal_fish_client::{
//!     WebSocketTransport, SignalFishClient, SignalFishConfig,
//!     JoinRoomParams, SignalFishEvent,
//! };
//!
//! #[tokio::main]
//! async fn main() -> Result<(), signal_fish_client::SignalFishError> {
//!     // 1. Connect a WebSocket transport to the signaling server.
//!     let transport = WebSocketTransport::connect("ws://localhost:3536/ws").await?;
//!
//!     // 2. Build a client config with your application ID.
//!     let config = SignalFishConfig::new("mb_app_abc123");
//!
//!     // 3. Start the client — returns a handle and an event receiver.
//!     //    The client automatically sends Authenticate on start.
//!     let (mut client, mut event_rx) = SignalFishClient::start(transport, config);
//!
//!     // 4. Process events — wait for Authenticated before joining a room.
//!     while let Some(event) = event_rx.recv().await {
//!         match event {
//!             SignalFishEvent::Authenticated { app_name, .. } => {
//!                 println!("Authenticated as {app_name}");
//!                 // Now it's safe to join a room.
//!                 client.join_room(JoinRoomParams::new("my-game", "Alice"))?;
//!             }
//!             SignalFishEvent::RoomJoined { room_code, .. } => {
//!                 println!("Joined room {room_code}");
//!             }
//!             SignalFishEvent::Disconnected { .. } => break,
//!             _ => {}
//!         }
//!     }
//!
//!     // 5. Shut down gracefully.
//!     client.shutdown().await;
//!     Ok(())
//! }
//! ```

pub mod client;
pub mod error;
pub mod error_codes;
pub mod event;
pub mod protocol;
pub mod transport;
pub mod transports;

// Re-export primary types for ergonomic imports.
pub use client::{JoinRoomParams, SignalFishClient, SignalFishConfig};
pub use error::SignalFishError;
pub use error_codes::ErrorCode;
pub use event::SignalFishEvent;
pub use protocol::{ClientMessage, ServerMessage};
pub use transport::Transport;

#[cfg(feature = "transport-websocket")]
pub use transports::WebSocketTransport;
