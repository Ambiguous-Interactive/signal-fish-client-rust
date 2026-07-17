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
//! - **Protocol v2 relay + v3 mesh** — v3 is additive and opt-in; a default client
//!   stays byte-identical to v2 (see [Protocol versions](#protocol-versions))
//! - **WebSocket built-in** — default `transport-websocket` feature provides `WebSocketTransport`
//! - **Event-driven** — receive typed `SignalFishEvent`s via a channel
//! - **No silent loss** — events are delivered with backpressure and sends are
//!   bounded with explicit congestion signals (see
//!   [Delivery guarantees](client#delivery-guarantees))
//!
//! ## Choosing a client
//!
//! The crate ships two clients with identical protocol behavior; pick by how
//! your application is driven:
//!
//! - [`SignalFishClient`] (async) — spawns a background transport loop with
//!   [`tokio::spawn`]. Use it when a tokio runtime is *running* (a
//!   `#[tokio::main]`/`block_on` application, multi-thread or
//!   `current_thread`). It only makes progress while the runtime is driven —
//!   manually "ticking" a runtime once per frame starves it (see
//!   [the driving contract](client#driving-the-client-runtime-contract)).
//! - [`SignalFishPollingClient`] (sync, feature `polling-client`) — no
//!   background task, no runtime. You
//!   call [`poll()`](polling_client::SignalFishPollingClient::poll) once per
//!   frame from a game loop. This is the right client for frame-driven
//!   engines (Godot, Bevy without tokio, Unity via FFI) and `wasm32` targets.
//!
//! ## Protocol versions
//!
//! The SDK speaks two protocol generations, and you choose which by how you
//! build [`SignalFishConfig`]:
//!
//! - **v2 — the relay floor (default).** [`SignalFishConfig::new`] advertises no
//!   v3 capabilities, the server relays all traffic through itself, and the
//!   `Authenticate` bytes are byte-identical to the old v2 client. This is the
//!   *relay-floor guarantee*: opt into nothing and nothing changes.
//! - **v3 — additive mesh (opt-in).** [`SignalFishConfig::enable_mesh`] advertises
//!   the WebRTC/relay transports and mesh/host/relay topologies, letting the
//!   server form a peer-to-peer session. v3 capabilities are additive to the
//!   v2 relay floor, and the server falls back to relay whenever it cannot form
//!   a session. On current servers, an eligible client explicitly calls
//!   [`SignalFishClient::start_game`] after readiness instead of relying on
//!   automatic start.
//!
//! The negotiated version comes back in the server's `ProtocolInfo`; check it via
//! [`SignalFishClient::negotiated_protocol_version`] /
//! [`SignalFishClient::supports_mesh`]. v3-only sends fail fast with
//! [`SignalFishError::ProtocolUnsupported`] until v3 is negotiated. The SDK is
//! *signaling-only* — it bundles no WebRTC stack; with the `mesh` feature you
//! implement the [`webrtc::WebRtcDriver`] seam (or use
//! [`webrtc::MeshController`]) against str0m / webrtc-rs / web-sys. The highest
//! version this SDK speaks is [`PROTOCOL_VERSION`].
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
//!                 client.set_ready()?;
//!             }
//!             // Protocol v2: the game starts explicitly, not on readiness.
//!             SignalFishEvent::LobbyStateChanged { all_ready: true, .. } => {
//!                 client.start_game()?;
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

#[cfg(any(feature = "tokio-runtime", feature = "polling-client"))]
mod accountability;
pub mod client;
pub mod client_api;
#[cfg(any(feature = "tokio-runtime", feature = "polling-client"))]
mod client_core;
pub mod error;
pub mod error_codes;
pub mod event;
pub mod protocol;
pub mod signal;
pub mod transport;
pub mod transports;

/// Highest signaling protocol version this SDK speaks.
///
/// Advertised in `Authenticate` when a consumer opts into the mesh via
/// [`SignalFishConfig::enable_mesh`](crate::SignalFishConfig::enable_mesh).
pub const PROTOCOL_VERSION: u16 = 3;

// Re-export primary types for ergonomic imports.
pub use client::{
    ClientSnapshot, ClientStats, GameDataDelivery, JoinRoomParams, ProtocolViolationPolicy,
    SignalFishClient, SignalFishConfig,
};
pub use client_api::SignalFishClientApi;
pub use error::SignalFishError;
pub use error_codes::ErrorCode;
pub use event::{
    ProtocolViolationKind, ServerErrorInfo, SignalFishEvent, DECODE_FAILED_RAW_PREFIX_MAX,
};
pub use protocol::{
    decode_v3_binary_game_data, ClientMessage, DeliveryClass, DeliveryCountersByClass, DeliveryGap,
    DeliveryGapReason, DeliveryReportPayload, IceServer, LatestDeliveryCounters, MessageTransport,
    ReliableDeliveryCounters, ReplayStatus, SenderWatermark, ServerMessage, SessionPeer,
    SessionPlanPayload, Topology, TransportKind, V3BinaryGameDataFrame, VolatileDeliveryCounters,
};
pub use signal::PeerSignal;
pub use transport::{Transport, TransportCloseInfo, TransportDiagnostics, TransportFrame};

#[cfg(feature = "transport-websocket")]
pub use transports::WebSocketTransport;

#[cfg(feature = "polling-client")]
pub mod polling_client;

#[cfg(feature = "polling-client")]
pub use polling_client::{
    PollingClientOptions, PollingClosePolicy, PollingQueueAgeStats, PollingStats,
    PollingWorkBudget, SignalFishPollingClient,
};

#[cfg(feature = "mesh")]
pub mod mesh;

#[cfg(feature = "mesh")]
pub use mesh::{MeshPeer, MeshSession};

#[cfg(feature = "mesh")]
pub mod webrtc;

#[cfg(feature = "mesh")]
pub use webrtc::{DriverEvent, MeshEvent, WebRtcDriver};

#[cfg(all(feature = "mesh", feature = "tokio-runtime"))]
pub use webrtc::MeshController;

// Re-export only on the correct target (see transports/mod.rs for rationale).
#[cfg(all(feature = "transport-websocket-emscripten", target_os = "emscripten"))]
#[allow(deprecated)]
pub use transports::EmscriptenWebSocketTransport;
