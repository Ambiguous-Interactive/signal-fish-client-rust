//! # Signal Fish Client
//!
//! Framed-transport-agnostic Rust client for the Signal Fish multiplayer signaling
//! protocol.
//!
//! This crate provides a high-level async client that communicates with a Signal Fish
//! signaling server using complete text/binary frames over a bidirectional
//! [`Transport`]. Raw stream/datagram framing and signaling-server
//! trust/source binding are backend responsibilities.
//!
//! ## Features
//!
//! - **Framed-transport-agnostic** — implement [`Transport`] for a backend that
//!   supplies complete text/binary frames
//! - **Wire-compatible** — all protocol types match the server's v2 format exactly
//! - **Protocol v2 relay + v3 mesh** — v3 is additive and opt-in; a default client
//!   stays byte-identical to v2 (see [Protocol versions](#protocol-versions))
//! - **WebSocket built-in** — default `transport-websocket` feature provides `WebSocketTransport`
//! - **Event-driven** — receive typed `SignalFishEvent`s via a channel
//! - **No silent loss** — events are delivered with backpressure and sends are
//!   bounded with explicit congestion signals (see
//!   [Delivery guarantees](client#delivery-guarantees))
//!
//! ## Cargo features
//!
//! | Feature | Default | Purpose |
//! |---------|---------|---------|
//! | `transport-websocket` | on | Built-in `WebSocketTransport` via `tokio-tungstenite` |
//! | `token-binding` | off | Native `signalfish.tokenbinding.v2` negotiation and outbound proofs |
//! | `tls` | off | `wss://` TLS for the built-in WebSocket transport (opt-in so the default build pulls no crypto stack) |
//! | `transport-websocket-emscripten` | off | Emscripten WebSocket transport (implies `polling-client`) |
//! | `polling-client` | off | Sync, polling-based `SignalFishPollingClient` for frame-driven engines and `wasm32` |
//! | `tokio-runtime` | off (on via `transport-websocket`) | Tokio `rt` + `time` features for the async client |
//! | `mesh` | off | Protocol v3 mesh tracker plus the `WebRtcDriver` seam; `MeshController` additionally requires `tokio-runtime` (async driver only) |
//!
//! `tls` requires `transport-websocket`; the mesh feature requires you to
//! supply a WebRTC implementation behind the `WebRtcDriver` seam — this
//! crate bundles no WebRTC stack.
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
//! - **v3 — additive negotiation (opt-in).** [`SignalFishConfig::enable_v3`]
//!   enables v3 relay/accountability semantics. [`SignalFishConfig::enable_mesh`]
//!   additionally advertises WebRTC plus mesh/host topologies, letting the server
//!   form a peer-to-peer session when appropriate. v3 capabilities are additive
//!   to the v2 relay floor, and the server falls back to relay whenever it cannot
//!   form a session. V3-capable configurations also request UUID-correlated room
//!   operations and use them only after an exact `ProtocolInfo` capability echo.
//!   On current servers, an eligible client explicitly calls
//!   [`SignalFishClient::start_game`] after readiness instead of relying on
//!   automatic start.
//!
//! The negotiated version comes back in the server's `ProtocolInfo`;
//! [`SignalFishClient::supports_mesh`] reports negotiated WebRTC plus Host/Mesh
//! capability, while `snapshot()` exposes the server-selected plan. v3-only sends fail fast with
//! [`SignalFishError::ProtocolUnsupported`] until v3 is negotiated. The SDK is
//! *signaling-only* — it bundles no WebRTC stack; with the `mesh` feature you
//! implement the `webrtc::WebRtcDriver` seam (or use
//! `webrtc::MeshController`) against str0m / webrtc-rs / web-sys. The highest
//! version this SDK speaks is [`PROTOCOL_VERSION`].
//!
//! ## Quick Start
//!
//! The sketch below is not compile-checked because `WebSocketTransport`
//! exists only with the default `transport-websocket` feature and doctests
//! must build under every feature combination. The complete compiling
//! counterpart lives in `examples/basic_lobby.rs`, which CI builds on every
//! change.
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
//!     let transport = WebSocketTransport::connect("ws://localhost:3536/v2/ws").await?;
//!
//!     // 2. Build a client config with your application ID.
//!     let config = SignalFishConfig::new("mb_app_abc123");
//!
//!     // 3. Start the client — returns a handle and an event receiver.
//!     //    The client automatically sends Authenticate on start.
//!     let (mut client, mut event_rx) = SignalFishClient::start(transport, config);
//!
//!     // 4. Process events — wait for Authenticated before joining a room.
//!     let mut start_requested = false;
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
//!             // Ready-state updates repeat — request the start only once.
//!             SignalFishEvent::LobbyStateChanged { all_ready: true, .. }
//!                 if !start_requested =>
//!             {
//!                 start_requested = true;
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
#[cfg(any(feature = "tokio-runtime", feature = "polling-client"))]
mod terminal_drain;
#[cfg(feature = "transport-websocket")]
pub mod token_binding;
pub mod transport;
pub mod transports;

/// Highest signaling protocol version this SDK speaks.
///
/// Advertised in `Authenticate` when a consumer opts into v3 via
/// [`SignalFishConfig::enable_v3`](crate::SignalFishConfig::enable_v3),
/// [`SignalFishConfig::enable_mesh`](crate::SignalFishConfig::enable_mesh), or
/// the lower-level configuration builders. A `webrtc::MeshController` also
/// ensures its owned client advertises the capabilities its driver fulfills.
pub const PROTOCOL_VERSION: u16 = 3;

// Re-export primary types for ergonomic imports.
pub use client::{
    ClientSnapshot, ClientStats, GameDataDelivery, JoinRoomParams, ProtocolViolationPolicy,
    RoomRole, SignalFishClient, SignalFishConfig,
};
pub use client_api::SignalFishClientApi;
pub use error::{SignalFishError, TokenBindingFailure};
pub use error_codes::ErrorCode;
pub use event::{
    ProtocolViolationKind, ServerErrorInfo, SignalFishEvent, DECODE_FAILED_RAW_PREFIX_MAX,
};
pub use protocol::{
    decode_v3_binary_game_data, ClientMessage, ConnectionInfo, DeliveryClass,
    DeliveryCountersByClass, DeliveryGap, DeliveryGapReason, DeliveryReportPayload, DirectEndpoint,
    GameDataEncoding, IceServer, LatestDeliveryCounters, LobbyState, MessageTransport,
    PeerConnectionInfo, PlayerId, PlayerInfo, RateLimitInfo, RelayTransport,
    ReliableDeliveryCounters, ReplayStatus, RoomId, RoomOperationId, RoomOperationRequest,
    RoomOperationResult, SenderWatermark, ServerMessage, SessionGeneration, SessionPeer,
    SessionPlanPayload, SpectatorInfo, SpectatorStateChangeReason, Topology, TransportKind,
    V3BinaryGameDataFrame, VolatileDeliveryCounters, ROOM_OPERATION_IDS_CAPABILITY,
};
pub use signal::PeerSignal;
#[cfg(feature = "transport-websocket")]
pub use token_binding::{
    TokenBindingChallenge, TokenBindingMode, TokenBindingScheme, TokenBindingStatus,
};
pub use transport::{Transport, TransportCloseInfo, TransportDiagnostics, TransportFrame};

#[cfg(feature = "transport-websocket")]
pub use transports::{WebSocketConnectOptions, WebSocketTransport};

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
pub use transports::{EmscriptenWebSocketConnectOptions, EmscriptenWebSocketTransport};
