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
//! - **Non-exhaustive** — all public enums/structs are `#[non_exhaustive]` for forward compatibility
//!
//! ## Quick Start
//!
//! ```text
//! // Full usage examples coming in Phase 6+
//! ```

pub mod error;
pub mod transport;

// Re-export primary types for ergonomic imports.
pub use error::SignalFishError;
pub use transport::Transport;

// Modules will be added in subsequent phases:
// pub mod protocol;     // Phase 3
// pub mod error_codes;  // Phase 3
// pub mod event;        // Phase 5
// pub mod client;       // Phase 6
// pub mod transports;   // Phase 7
