//! Async client for the Signal Fish signaling protocol.
//!
//! [`SignalFishClient`] is a thin handle that communicates with a background
//! transport loop task via a bounded MPSC command channel. Events are emitted
//! on a bounded channel ([`tokio::sync::mpsc::Receiver<SignalFishEvent>`])
//! returned from [`SignalFishClient::start`].
//!
//! # Delivery guarantees
//!
//! Neither direction silently drops data:
//!
//! - **Events** are delivered with backpressure. If the consumer lags, the
//!   transport loop pauses reading from the transport until the event channel
//!   has room — backpressure propagates to the server instead of losing
//!   events. An event can only be missed when the loop stops delivering
//!   entirely: the receiver was dropped, a
//!   [`shutdown`](SignalFishClient::shutdown) timeout aborted the loop, or
//!   the client handle was dropped without calling `shutdown` (which aborts
//!   immediately).
//! - **Commands** go through a bounded queue and queue admission is never
//!   silent: the synchronous send methods fail fast with
//!   [`SignalFishError::SendBufferFull`] when it is full, and the
//!   `*_reliable` async variants wait for capacity instead. Congestion is
//!   always surfaced, never buffered without bound. Note that *queued* is
//!   not *delivered*: commands still in the queue when the connection ends
//!   (transport error, shutdown, handle drop) are discarded with the
//!   connection, which is surfaced by the `Disconnected` event.
//!
//! # Driving the client (runtime contract)
//!
//! [`SignalFishClient::start`] spawns the transport loop with
//! [`tokio::spawn`], so the loop only makes progress while the tokio runtime
//! is **driven** — i.e. some task is being awaited (`block_on`, `#[tokio::main]`,
//! worker threads). Both multi-thread and `current_thread` runtimes work, as
//! long as the runtime is actually running. What does *not* work is "ticking"
//! a runtime manually (e.g. one `yield_now().await` per game frame): the loop
//! starves and messages appear to vanish. For frame-driven or single-threaded
//! environments (game engines, `wasm32`), use
//! [`SignalFishPollingClient`](crate::polling_client::SignalFishPollingClient)
//! (feature `polling-client`), which is a synchronous pump you call once per
//! frame and needs no runtime at all.
//!
//! # Example
//!
//! ```rust,ignore
//! let transport = connect_somehow().await;
//! let config = SignalFishConfig::new("mb_app_abc123");
//! let (client, mut events) = SignalFishClient::start(transport, config);
//!
//! client.join_room(
//!     JoinRoomParams::new("my-game", "Alice")
//!         .with_max_players(4)
//! )?;
//!
//! while let Some(event) = events.recv().await {
//!     match event {
//!         SignalFishEvent::RoomJoined { room_code, .. } => { /* … */ }
//!         SignalFishEvent::Disconnected { .. } => break,
//!         _ => {}
//!     }
//! }
//! ```

use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

// tokio/sync is always available (not gated on `tokio-runtime`) because
// `SignalFishClient` uses `mpsc` and `ClientState` uses `Mutex` unconditionally.
// These types have no reachable usage path without `tokio-runtime` (the only
// constructor, `SignalFishClient::start`, is feature-gated), so they are
// effectively dead code in that configuration — suppressed by
// `#[cfg_attr(..., allow(dead_code))]` on the struct. If a future refactoring
// needs a different sync primitive for the no-runtime path, this import and
// the struct fields would need feature-gating.
use tokio::sync::{mpsc, Mutex};
#[cfg(feature = "tokio-runtime")]
use tracing::{debug, error, warn};

use crate::error::{Result, SignalFishError};
#[cfg(feature = "tokio-runtime")]
use crate::event::SignalFishEvent;
#[cfg(feature = "tokio-runtime")]
use crate::protocol::ServerMessage;
use crate::protocol::{
    ClientMessage, ConnectionInfo, GameDataEncoding, PlayerId, RelayTransport, RoomId, Topology,
    TransportKind,
};
use crate::signal::PeerSignal;
#[cfg(feature = "tokio-runtime")]
use crate::transport::Transport;

/// Default capacity of the bounded event channel.
const DEFAULT_EVENT_CHANNEL_CAPACITY: usize = 256;

/// Default capacity of the bounded outgoing command queue.
const DEFAULT_COMMAND_CHANNEL_CAPACITY: usize = 1024;

/// Default timeout for the graceful shutdown.
const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

// ── Configuration ───────────────────────────────────────────────────

/// Configuration for a [`SignalFishClient`] connection.
///
/// Must be supplied to [`SignalFishClient::start`]. The only required field is
/// `app_id`; all others have sensible defaults.
///
/// # Example
///
/// ```
/// use signal_fish_client::client::SignalFishConfig;
///
/// let config = SignalFishConfig::new("mb_app_abc123");
/// assert_eq!(config.app_id, "mb_app_abc123");
/// assert!(config.sdk_version.is_some());
/// ```
///
/// # Tuning
///
/// ```
/// use signal_fish_client::client::SignalFishConfig;
/// use std::time::Duration;
///
/// let config = SignalFishConfig::new("mb_app_abc123")
///     .with_event_channel_capacity(512)
///     .with_shutdown_timeout(Duration::from_secs(5));
/// ```
#[derive(Debug, Clone)]
pub struct SignalFishConfig {
    /// Public App ID that identifies the game application.
    pub app_id: String,
    /// SDK version string sent during authentication.
    /// Defaults to the crate version at compile time.
    pub sdk_version: Option<String>,
    /// Platform identifier (e.g. `"unity"`, `"godot"`, `"rust"`).
    pub platform: Option<String>,
    /// Preferred game data encoding format.
    pub game_data_format: Option<GameDataEncoding>,
    /// Highest signaling protocol version to advertise (protocol v3+).
    ///
    /// `None` (the default) keeps the client on the v2 **relay floor**: the
    /// `Authenticate` message omits all negotiation fields and is byte-identical
    /// to v2. Opt into the mesh with
    /// [`enable_mesh`](Self::enable_mesh) or [`with_protocol_version`](Self::with_protocol_version).
    pub protocol_version: Option<u16>,
    /// Data-path transports the client can actually fulfill (protocol v3+).
    ///
    /// `None` advertises nothing. Only advertise a transport (e.g.
    /// [`TransportKind::WebRtc`]) you have a real WebRTC stack to back.
    pub supported_transports: Option<Vec<TransportKind>>,
    /// Session topologies the client can participate in (protocol v3+).
    pub supported_topologies: Option<Vec<Topology>>,
    /// Capacity of the bounded event channel.
    ///
    /// Events are **never dropped**. When the consumer cannot keep up with
    /// incoming server messages, the transport loop pauses until the consumer
    /// drains the channel, propagating backpressure to the server instead of
    /// losing data. The capacity only controls how much buffering the consumer
    /// gets before that backpressure kicks in. An event can only be missed
    /// when delivery stops entirely: the receiver is dropped,
    /// [`SignalFishClient::shutdown`] times out and aborts the transport
    /// task, or the client handle is dropped without calling `shutdown`.
    ///
    /// Defaults to **256**. Values below 1 are clamped to 1.
    pub event_channel_capacity: usize,
    /// Capacity of the bounded outgoing command queue.
    ///
    /// Queue admission is **never silent**. When the queue is full, the
    /// synchronous send methods fail fast with
    /// [`SignalFishError::SendBufferFull`], and the waiting variants (e.g.
    /// [`SignalFishClient::send_game_data_reliable`]) pause until the
    /// transport drains a slot. Either way the caller gets a deterministic
    /// congestion signal instead of an unbounded backlog. Commands still
    /// queued when the connection ends are discarded with it (surfaced by
    /// the `Disconnected` event); *queued* is not *delivered*.
    ///
    /// Defaults to **1024**. Values below 1 are clamped to 1.
    pub command_channel_capacity: usize,
    /// Timeout for the graceful shutdown.
    ///
    /// When [`SignalFishClient::shutdown`] is called, the background transport
    /// loop is given this much time to close the transport and emit a final
    /// `Disconnected` event. If the timeout expires the task is aborted and
    /// the `Disconnected` event may not be delivered.
    ///
    /// Defaults to **1 second**. A zero timeout aborts the transport loop
    /// immediately without waiting for graceful shutdown, meaning the
    /// `Disconnected` event will likely not be emitted.
    pub shutdown_timeout: Duration,
}

impl SignalFishConfig {
    /// Create a new configuration with the given App ID and default values.
    pub fn new(app_id: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            sdk_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            platform: None,
            game_data_format: None,
            protocol_version: None,
            supported_transports: None,
            supported_topologies: None,
            event_channel_capacity: DEFAULT_EVENT_CHANNEL_CAPACITY,
            command_channel_capacity: DEFAULT_COMMAND_CHANNEL_CAPACITY,
            shutdown_timeout: DEFAULT_SHUTDOWN_TIMEOUT,
        }
    }

    /// Set the capacity of the bounded event channel.
    ///
    /// Defaults to **256**. Values below 1 are clamped to 1.
    #[must_use]
    pub fn with_event_channel_capacity(mut self, capacity: usize) -> Self {
        self.event_channel_capacity = capacity.max(1);
        self
    }

    /// Set the capacity of the bounded outgoing command queue.
    ///
    /// See [`command_channel_capacity`](Self::command_channel_capacity) for
    /// the backpressure semantics.
    ///
    /// Defaults to **1024**. Values below 1 are clamped to 1.
    #[must_use]
    pub fn with_command_channel_capacity(mut self, capacity: usize) -> Self {
        self.command_channel_capacity = capacity.max(1);
        self
    }

    /// Set the timeout for the graceful shutdown.
    ///
    /// Defaults to **1 second**. A zero timeout aborts the transport loop
    /// immediately without waiting for graceful shutdown.
    #[must_use]
    pub fn with_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    /// Opt into the protocol v3 P2P mesh.
    ///
    /// This is the one-liner for "I have a WebRTC stack — give me mesh with relay
    /// fallback." It advertises protocol version [`PROTOCOL_VERSION`](crate::PROTOCOL_VERSION),
    /// the `webrtc` and `relay` transports, and the `mesh`, `host`, and `relay`
    /// topologies. The **server still chooses** the actual topology/transport and
    /// may keep the room on the relay floor; the client merely declares what it can
    /// fulfill.
    ///
    /// Only call this when you actually bridge the resulting signaling events
    /// (`SessionPlan`, `SignalReceived`, `NewPeer`) to a WebRTC implementation —
    /// never advertise a transport you cannot fulfill. Leaving this unset keeps
    /// the client on the byte-identical-to-v2 relay floor.
    ///
    /// When wiring up WebRTC, feed the `ice_servers` from the `SessionPlan`
    /// (and the pre-gathered `ice_servers` on `RoomJoined`/`Reconnected`) into
    /// your peer connection's STUN/TURN configuration, or NAT traversal will
    /// silently fail.
    #[must_use]
    pub fn enable_mesh(mut self) -> Self {
        self.protocol_version = Some(crate::PROTOCOL_VERSION);
        self.supported_transports = Some(vec![TransportKind::WebRtc, TransportKind::Relay]);
        self.supported_topologies = Some(vec![Topology::Mesh, Topology::Host, Topology::Relay]);
        self
    }

    /// Advertise the highest protocol version this client speaks.
    ///
    /// Power-user escape hatch; most consumers want [`enable_mesh`](Self::enable_mesh)
    /// instead. Setting a version without also setting transports/topologies keeps
    /// the room on the relay floor (the server requires both to form a session).
    #[must_use]
    pub fn with_protocol_version(mut self, version: u16) -> Self {
        self.protocol_version = Some(version);
        self
    }

    /// Advertise the data-path transports this client can fulfill.
    ///
    /// Power-user escape hatch (e.g. `[TransportKind::WebRtc]` for mesh-only, no
    /// relay fallback for this client). Only advertise a transport you have a real
    /// implementation to back.
    #[must_use]
    pub fn with_transports(mut self, transports: impl IntoIterator<Item = TransportKind>) -> Self {
        self.supported_transports = Some(transports.into_iter().collect());
        self
    }

    /// Advertise the session topologies this client can participate in.
    ///
    /// Power-user escape hatch (e.g. `[Topology::Mesh, Topology::Relay]` for
    /// strictly full-mesh-or-relay).
    #[must_use]
    pub fn with_topologies(mut self, topologies: impl IntoIterator<Item = Topology>) -> Self {
        self.supported_topologies = Some(topologies.into_iter().collect());
        self
    }
}

// ── JoinRoomParams ──────────────────────────────────────────────────

/// Parameters for joining (or creating) a room.
///
/// Only `game_name` and `player_name` are required. Leave `room_code` as
/// `None` for quick-match / auto-create behavior.
///
/// Use [`JoinRoomParams::new`] to construct an instance — the `Default` impl
/// produces empty strings for the required fields and is intended only for
/// internal `..Default::default()` patterns.
///
/// # Example
///
/// ```
/// use signal_fish_client::client::JoinRoomParams;
///
/// let params = JoinRoomParams::new("my-game", "Alice")
///     .with_max_players(4);
/// assert_eq!(params.game_name, "my-game");
/// assert_eq!(params.max_players, Some(4));
/// ```
#[derive(Debug, Clone, Default)]
pub struct JoinRoomParams {
    /// Name of the game to join.
    pub game_name: String,
    /// Display name for the joining player.
    pub player_name: String,
    /// Room code to join. `None` = quick-match / create new room.
    pub room_code: Option<String>,
    /// Maximum number of players allowed in the room.
    pub max_players: Option<u8>,
    /// Whether the room should support authority delegation.
    pub supports_authority: Option<bool>,
    /// Preferred relay transport protocol.
    pub relay_transport: Option<RelayTransport>,
}

impl JoinRoomParams {
    /// Create new join-room parameters with the required fields.
    pub fn new(game_name: impl Into<String>, player_name: impl Into<String>) -> Self {
        Self {
            game_name: game_name.into(),
            player_name: player_name.into(),
            ..Default::default()
        }
    }

    /// Set an explicit room code to join.
    #[must_use]
    pub fn with_room_code(mut self, room_code: impl Into<String>) -> Self {
        self.room_code = Some(room_code.into());
        self
    }

    /// Set the maximum number of players.
    #[must_use]
    pub fn with_max_players(mut self, max_players: u8) -> Self {
        self.max_players = Some(max_players);
        self
    }

    /// Enable or disable authority delegation support.
    #[must_use]
    pub fn with_supports_authority(mut self, supports_authority: bool) -> Self {
        self.supports_authority = Some(supports_authority);
        self
    }

    /// Set the preferred relay transport protocol.
    #[must_use]
    pub fn with_relay_transport(mut self, relay_transport: RelayTransport) -> Self {
        self.relay_transport = Some(relay_transport);
        self
    }
}

// ── Traffic statistics ──────────────────────────────────────────────

/// Snapshot of a client's game-data traffic counters.
///
/// Returned by [`SignalFishClient::stats`] and
/// [`SignalFishPollingClient::stats`](crate::polling_client::SignalFishPollingClient::stats).
///
/// The client itself never drops game data (events are delivered with
/// backpressure and refused sends return
/// [`SendBufferFull`](crate::SignalFishError::SendBufferFull)), so these
/// counters make loss *elsewhere* observable: exchange or log them across
/// peers, and a persistent sent-vs-received deficit points at the relay
/// path or a peer — not at this client.
///
/// Counters are cumulative for the lifetime of the client (they survive
/// room changes and disconnection).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClientStats {
    /// `GameData` messages successfully written to the transport.
    pub game_data_sent: u64,
    /// `GameData`/`GameDataBinary` events received from the server.
    pub game_data_received: u64,
}

// ── Shared state ────────────────────────────────────────────────────

/// Internal shared state between the client handle and the transport loop.
struct ClientState {
    connected: AtomicBool,
    authenticated: AtomicBool,
    /// Protocol version negotiated by the server (from `ProtocolInfo`).
    /// `0` means "not yet negotiated, or negotiated v2" — i.e. not v3-capable.
    negotiated_protocol_version: AtomicU16,
    /// Whether a `ProtocolInfo` has been observed on this connection. This
    /// distinguishes "negotiation hasn't happened yet" from "negotiation
    /// completed at the v2 relay floor" — both leave
    /// `negotiated_protocol_version` at `0`, but only the latter is terminal.
    /// Drives the `ProtocolUnsupported { mode }` diagnostic.
    protocol_info_seen: AtomicBool,
    player_id: Mutex<Option<PlayerId>>,
    room_id: Mutex<Option<RoomId>>,
    room_code: Mutex<Option<String>>,
    /// `GameData` messages successfully written to the transport.
    /// Cumulative — intentionally not reset by `clear_session_state`.
    game_data_sent: AtomicU64,
    /// `GameData`/`GameDataBinary` events received from the server.
    /// Cumulative — intentionally not reset by `clear_session_state`.
    game_data_received: AtomicU64,
}

#[cfg_attr(not(feature = "tokio-runtime"), allow(dead_code))]
impl ClientState {
    fn new() -> Self {
        Self {
            connected: AtomicBool::new(true),
            authenticated: AtomicBool::new(false),
            negotiated_protocol_version: AtomicU16::new(0),
            protocol_info_seen: AtomicBool::new(false),
            player_id: Mutex::new(None),
            room_id: Mutex::new(None),
            room_code: Mutex::new(None),
            game_data_sent: AtomicU64::new(0),
            game_data_received: AtomicU64::new(0),
        }
    }

    async fn clear_session_state(&self) {
        self.authenticated.store(false, Ordering::Release);
        self.negotiated_protocol_version.store(0, Ordering::Release);
        self.protocol_info_seen.store(false, Ordering::Release);
        *self.player_id.lock().await = None;
        *self.room_id.lock().await = None;
        *self.room_code.lock().await = None;
    }
}

// ── Client handle ───────────────────────────────────────────────────

/// Async client handle for the Signal Fish signaling protocol.
///
/// Created via [`SignalFishClient::start`], which spawns a background transport
/// loop and returns this handle together with an event receiver.
///
/// All synchronous public methods serialize a [`ClientMessage`] and queue it
/// to the transport loop over a **bounded** channel, returning immediately
/// once the message is queued (no round-trip await). When the queue is full
/// they fail fast with [`SignalFishError::SendBufferFull`]; the waiting
/// variants ([`send_game_data_reliable`](Self::send_game_data_reliable),
/// [`send_signal_reliable`](Self::send_signal_reliable)) instead await
/// capacity, pacing the caller to actual transport throughput.
#[cfg_attr(not(feature = "tokio-runtime"), allow(dead_code))]
pub struct SignalFishClient {
    /// Sender half of the bounded command channel to the transport loop.
    cmd_tx: mpsc::Sender<ClientMessage>,
    /// Shared state updated by the transport loop.
    state: Arc<ClientState>,
    /// Handle to the background transport loop task.
    #[cfg(feature = "tokio-runtime")]
    task: Option<tokio::task::JoinHandle<()>>,
    /// Oneshot sender to signal the transport loop to shut down gracefully.
    #[cfg(feature = "tokio-runtime")]
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    /// Timeout for the graceful shutdown.
    #[cfg(feature = "tokio-runtime")]
    shutdown_timeout: Duration,
}

#[cfg(feature = "tokio-runtime")]
impl SignalFishClient {
    /// Start the client transport loop and return a handle plus event receiver.
    ///
    /// The transport loop immediately sends an [`Authenticate`](ClientMessage::Authenticate)
    /// message using the provided [`SignalFishConfig`].
    ///
    /// The loop is spawned with [`tokio::spawn`] and therefore only makes
    /// progress while the tokio runtime is driven — see
    /// [the driving contract](self#driving-the-client-runtime-contract). For
    /// frame-driven or runtime-less environments use
    /// [`SignalFishPollingClient`](crate::polling_client::SignalFishPollingClient)
    /// instead.
    ///
    /// # Arguments
    ///
    /// * `transport` — A connected [`Transport`] implementation.
    /// * `config` — Client configuration including the App ID.
    ///
    /// # Returns
    ///
    /// A tuple of `(client_handle, event_receiver)`. The event receiver yields
    /// [`SignalFishEvent`]s until the transport closes or the client shuts down.
    #[must_use = "the event receiver must be used to receive events"]
    pub fn start(
        transport: impl Transport,
        config: SignalFishConfig,
    ) -> (Self, mpsc::Receiver<SignalFishEvent>) {
        // Clamp capacities to at least 1 (tokio panics on 0).
        let cmd_capacity = config.command_channel_capacity.max(1);
        let (cmd_tx, cmd_rx) = mpsc::channel::<ClientMessage>(cmd_capacity);
        let capacity = config.event_channel_capacity.max(1);
        let (event_tx, event_rx) = mpsc::channel::<SignalFishEvent>(capacity);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let state = Arc::new(ClientState::new());
        let loop_state = Arc::clone(&state);

        // Send the Authenticate message through the command channel so the
        // transport loop picks it up as the very first outgoing message.
        let auth_msg = ClientMessage::Authenticate {
            app_id: config.app_id,
            sdk_version: config.sdk_version,
            platform: config.platform,
            game_data_format: config.game_data_format,
            protocol_version: config.protocol_version,
            supported_transports: config.supported_transports,
            supported_topologies: config.supported_topologies,
        };
        // This cannot fail: the channel was just created empty and its
        // capacity is clamped to at least 1.
        let _ = cmd_tx.try_send(auth_msg);

        let task = tokio::spawn(transport_loop(
            transport,
            cmd_rx,
            event_tx,
            loop_state,
            shutdown_rx,
        ));

        let client = Self {
            cmd_tx,
            state,
            task: Some(task),
            shutdown_tx: Some(shutdown_tx),
            shutdown_timeout: config.shutdown_timeout,
        };

        (client, event_rx)
    }

    /// Shut down the client, closing the transport and stopping the background task.
    ///
    /// The transport loop is given [`shutdown_timeout`](SignalFishConfig::shutdown_timeout)
    /// to close cleanly and emit a [`Disconnected`](SignalFishEvent::Disconnected)
    /// event. If the timeout expires, the task is aborted and the `Disconnected`
    /// event may not be delivered. After shutdown completes, the event receiver
    /// will yield `None`.
    pub async fn shutdown(&mut self) {
        debug!("SignalFishClient: shutdown requested");

        // Signal the transport loop to shut down gracefully.
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }

        // Await the transport loop with a timeout. If it doesn't exit in time,
        // abort it so the task cannot detach and run indefinitely.
        if let Some(mut task) = self.task.take() {
            match tokio::time::timeout(self.shutdown_timeout, &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(join_err)) => {
                    warn!("transport loop terminated with join error: {join_err}");
                }
                Err(_) => {
                    warn!("transport loop did not exit within timeout; aborting task");
                    task.abort();
                    if let Err(join_err) = task.await {
                        debug!("transport loop aborted: {join_err}");
                    }
                }
            }
        }

        self.state.connected.store(false, Ordering::Release);
        self.state.clear_session_state().await;
    }
}

#[cfg_attr(not(feature = "tokio-runtime"), allow(dead_code))]
impl SignalFishClient {
    // ── Public API methods ──────────────────────────────────────────

    /// Join or create a room with the given parameters.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn join_room(&self, params: JoinRoomParams) -> Result<()> {
        self.send(ClientMessage::JoinRoom {
            game_name: params.game_name,
            room_code: params.room_code,
            player_name: params.player_name,
            max_players: params.max_players,
            supports_authority: params.supports_authority,
            relay_transport: params.relay_transport,
        })
    }

    /// Leave the current room.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn leave_room(&self) -> Result<()> {
        self.send(ClientMessage::LeaveRoom)
    }

    /// Send arbitrary JSON game data to other players in the room.
    ///
    /// Returns as soon as the message is queued. For high-rate payloads
    /// (e.g. per-frame input packets), prefer
    /// [`send_game_data_reliable`](Self::send_game_data_reliable), which
    /// waits for queue capacity instead of failing fast under congestion.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn send_game_data(&self, data: serde_json::Value) -> Result<()> {
        self.send(ClientMessage::GameData { data })
    }

    /// Send arbitrary JSON game data, waiting for space in the outgoing
    /// command queue when it is full.
    ///
    /// This is the backpressure-aware counterpart to
    /// [`send_game_data`](Self::send_game_data): instead of failing fast with
    /// [`SignalFishError::SendBufferFull`], it pauses until the transport
    /// drains a slot, pacing the caller to actual transport throughput. This
    /// is the recommended way to stream high-rate payloads (rollback input
    /// packets, state sync) without guessing at sleep durations.
    ///
    /// # Keep draining events
    ///
    /// The command queue only drains while the transport loop runs, and the
    /// transport loop pauses whenever the **event** channel is full (events
    /// are never dropped). A task that awaits this method while it is also
    /// the only consumer of the event receiver can therefore deadlock under
    /// simultaneous send + receive pressure. Drain events from a separate
    /// task rather than strictly sequentially. (Do **not** race this send
    /// against the event receiver in a `tokio::select!`: if the event arm
    /// wins, the cancelled send future discards the payload.)
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed.
    pub async fn send_game_data_reliable(&self, data: serde_json::Value) -> Result<()> {
        self.send_reliable(ClientMessage::GameData { data }).await
    }

    /// Signal readiness to start the game in the lobby.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn set_ready(&self) -> Result<()> {
        self.send(ClientMessage::PlayerReady)
    }

    /// Request to become (or relinquish) authority.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn request_authority(&self, become_authority: bool) -> Result<()> {
        self.send(ClientMessage::AuthorityRequest { become_authority })
    }

    /// Provide connection information for P2P establishment.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn provide_connection_info(&self, connection_info: ConnectionInfo) -> Result<()> {
        self.send(ClientMessage::ProvideConnectionInfo { connection_info })
    }

    /// Reconnect to a room after a disconnection.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn reconnect(
        &self,
        player_id: PlayerId,
        room_id: RoomId,
        auth_token: String,
    ) -> Result<()> {
        self.send(ClientMessage::Reconnect {
            player_id,
            room_id,
            auth_token,
        })
    }

    /// Join a room as a read-only spectator.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn join_as_spectator(
        &self,
        game_name: String,
        room_code: String,
        spectator_name: String,
    ) -> Result<()> {
        self.send(ClientMessage::JoinAsSpectator {
            game_name,
            room_code,
            spectator_name,
        })
    }

    /// Leave spectator mode.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn leave_spectator(&self) -> Result<()> {
        self.send(ClientMessage::LeaveSpectator)
    }

    /// Send a heartbeat ping to the server.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn ping(&self) -> Result<()> {
        self.send(ClientMessage::Ping)
    }

    // ── Game start (protocol v2) ────────────────────────────────────

    /// Request that the server start the game (protocol v2).
    ///
    /// The game now starts **explicitly** rather than implicitly when everyone
    /// is ready. The server accepts this only when every player in the room is
    /// ready; if the room has a designated authority, only that authority may
    /// start it. A rejected request surfaces as an [`Error`](SignalFishEvent::Error)
    /// event with [`ErrorCode::GameStartNotReady`](crate::ErrorCode::GameStartNotReady)
    /// or [`ErrorCode::GameStartForbidden`](crate::ErrorCode::GameStartForbidden).
    ///
    /// This is available on every connection (it is the universal v2 behavior),
    /// not gated behind the mesh opt-in.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn start_game(&self) -> Result<()> {
        self.send(ClientMessage::StartGame)
    }

    // ── Mesh signaling (protocol v3) ────────────────────────────────

    /// Send a typed WebRTC signal to a single peer.
    ///
    /// **Protocol v3 only.** Fails fast on a relay-floor connection (see Errors).
    ///
    /// Accepts a [`PeerSignal`] or anything `Into<PeerSignal>`. Use this (or the
    /// [`send_offer`](Self::send_offer)/[`send_answer`](Self::send_answer)/
    /// [`send_ice_candidate`](Self::send_ice_candidate) helpers) to relay your
    /// WebRTC stack's offers, answers, and ICE candidates to the peer the server
    /// named in a [`SessionPlan`](SignalFishEvent::SessionPlan) or
    /// [`NewPeer`](SignalFishEvent::NewPeer) event.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::ProtocolUnsupported`] if the connection has not
    /// negotiated protocol v3 (fail-fast — the server would otherwise reject it),
    /// [`SignalFishError::NotConnected`] if the transport has closed, or
    /// [`SignalFishError::SendBufferFull`] if the outgoing command queue is
    /// full (see [`send_signal_reliable`](Self::send_signal_reliable) for a
    /// waiting variant).
    pub fn send_signal(&self, to: PlayerId, signal: impl Into<PeerSignal>) -> Result<()> {
        self.ensure_v3()?;
        self.send(ClientMessage::Signal {
            to,
            signal: signal.into().into(),
        })
    }

    /// Send a typed WebRTC signal, waiting for space in the outgoing command
    /// queue when it is full. **Protocol v3 only.**
    ///
    /// The backpressure-aware counterpart to [`send_signal`](Self::send_signal):
    /// a lost offer/answer/ICE candidate stalls a WebRTC handshake, so waiting
    /// beats failing when the queue is congested (e.g. by game-data bursts).
    ///
    /// The same caveat as
    /// [`send_game_data_reliable`](Self::send_game_data_reliable#keep-draining-events)
    /// applies: keep draining events from another task while awaiting this.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::ProtocolUnsupported`] if the connection has
    /// not negotiated protocol v3, or [`SignalFishError::NotConnected`] if the
    /// transport has closed.
    pub async fn send_signal_reliable(
        &self,
        to: PlayerId,
        signal: impl Into<PeerSignal>,
    ) -> Result<()> {
        self.ensure_v3()?;
        self.send_reliable(ClientMessage::Signal {
            to,
            signal: signal.into().into(),
        })
        .await
    }

    /// Send an SDP offer to a peer. **Protocol v3 only.**
    /// See [`send_signal`](Self::send_signal).
    ///
    /// # Errors
    ///
    /// See [`send_signal`](Self::send_signal).
    pub fn send_offer(&self, to: PlayerId, sdp: impl Into<String>) -> Result<()> {
        self.send_signal(to, PeerSignal::Offer(sdp.into()))
    }

    /// Send an SDP answer to a peer. **Protocol v3 only.**
    /// See [`send_signal`](Self::send_signal).
    ///
    /// # Errors
    ///
    /// See [`send_signal`](Self::send_signal).
    pub fn send_answer(&self, to: PlayerId, sdp: impl Into<String>) -> Result<()> {
        self.send_signal(to, PeerSignal::Answer(sdp.into()))
    }

    /// Send a single trickle ICE candidate to a peer. **Protocol v3 only.**
    /// See [`send_signal`](Self::send_signal).
    ///
    /// # Errors
    ///
    /// See [`send_signal`](Self::send_signal).
    pub fn send_ice_candidate(&self, to: PlayerId, candidate: impl Into<String>) -> Result<()> {
        self.send_signal(to, PeerSignal::IceCandidate(candidate.into()))
    }

    /// Raw escape hatch: relay a signal whose shape the SDK does not model.
    ///
    /// **Protocol v3 only.** The `signal` value is forwarded to the peer verbatim.
    ///
    /// Like the typed helpers, this is still gated on a negotiated v3 session —
    /// the escape hatch bypasses the *typing*, not the negotiation guard.
    ///
    /// # Errors
    ///
    /// See [`send_signal`](Self::send_signal).
    pub fn send_raw_signal(&self, to: PlayerId, signal: serde_json::Value) -> Result<()> {
        self.ensure_v3()?;
        self.send(ClientMessage::Signal { to, signal })
    }

    /// Report to the server whether a data-path transport is established.
    ///
    /// **Protocol v3 only.** The server fans this out to peers as
    /// [`PeerTransportStatus`](SignalFishEvent::PeerTransportStatus) and uses it
    /// for fallback decisions. Purely informational; the relay floor stays open
    /// regardless.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::ProtocolUnsupported`] if the connection has not
    /// negotiated protocol v3, [`SignalFishError::NotConnected`] if the
    /// transport has closed, or [`SignalFishError::SendBufferFull`] if the
    /// outgoing command queue is full.
    pub fn report_transport_status(&self, transport: TransportKind, connected: bool) -> Result<()> {
        self.ensure_v3()?;
        self.send(ClientMessage::TransportStatus {
            transport,
            connected,
        })
    }

    // ── State accessors ─────────────────────────────────────────────

    /// The protocol version negotiated with the server, or `None` if not yet
    /// negotiated or negotiated as v2 (the relay floor).
    ///
    /// Set from the server's [`ProtocolInfo`](SignalFishEvent::ProtocolInfo)
    /// message. A value of `Some(3)` or higher means mesh signaling is available.
    pub fn negotiated_protocol_version(&self) -> Option<u16> {
        match self
            .state
            .negotiated_protocol_version
            .load(Ordering::Acquire)
        {
            0 => None,
            v => Some(v),
        }
    }

    /// Returns `true` once the connection has negotiated protocol v3 — i.e. mesh
    /// signaling (`send_signal`/`report_transport_status`) is available.
    ///
    /// This is the "am I in mesh mode?" check; it returns `false` both before
    /// negotiation completes and on a v2 relay-floor connection.
    pub fn supports_mesh(&self) -> bool {
        self.negotiated_protocol_version().is_some_and(|v| v >= 3)
    }

    /// Returns `true` if the transport is believed to be connected.
    pub fn is_connected(&self) -> bool {
        self.state.connected.load(Ordering::Acquire)
    }

    /// Returns `true` if the server has confirmed authentication.
    pub fn is_authenticated(&self) -> bool {
        self.state.authenticated.load(Ordering::Acquire)
    }

    /// Returns the current room ID, if the client is in a room.
    pub async fn current_room_id(&self) -> Option<RoomId> {
        *self.state.room_id.lock().await
    }

    /// Returns the current player ID, if assigned by the server.
    pub async fn current_player_id(&self) -> Option<PlayerId> {
        *self.state.player_id.lock().await
    }

    /// Returns the current room code, if the client is in a room.
    pub async fn current_room_code(&self) -> Option<String> {
        self.state.room_code.lock().await.clone()
    }

    /// Number of messages that can currently be queued before the synchronous
    /// send methods return [`SignalFishError::SendBufferFull`].
    ///
    /// A shrinking value is the congestion signal: the caller is producing
    /// faster than the transport drains. `0` means the next fail-fast send
    /// will be refused.
    pub fn send_capacity(&self) -> usize {
        self.cmd_tx.capacity()
    }

    /// Configured capacity of the outgoing command queue
    /// (see [`SignalFishConfig::command_channel_capacity`]).
    pub fn max_send_capacity(&self) -> usize {
        self.cmd_tx.max_capacity()
    }

    /// Cumulative game-data traffic counters (see [`ClientStats`]).
    pub fn stats(&self) -> ClientStats {
        ClientStats {
            game_data_sent: self.state.game_data_sent.load(Ordering::Relaxed),
            game_data_received: self.state.game_data_received.load(Ordering::Relaxed),
        }
    }

    // ── Internal helpers ────────────────────────────────────────────

    /// Guard for protocol-v3-only operations: returns an error unless the
    /// connection negotiated v3, so signaling never goes out on a relay-floor
    /// connection the server would reject.
    fn ensure_v3(&self) -> Result<()> {
        // Mesh signaling was introduced in protocol v3; any negotiated version
        // >= 3 supports it (independent of the highest version this SDK speaks).
        if self
            .state
            .negotiated_protocol_version
            .load(Ordering::Acquire)
            >= 3
        {
            return Ok(());
        }
        // A `ProtocolInfo` that resolved below v3 is a terminal relay floor;
        // its absence means negotiation is still in flight. Keying off the
        // observed `ProtocolInfo` (not authentication) keeps this diagnostic
        // correct regardless of handshake message ordering.
        let mode = if self.state.protocol_info_seen.load(Ordering::Acquire) {
            "relay-only"
        } else {
            "pre-negotiation"
        };
        Err(SignalFishError::ProtocolUnsupported { mode })
    }

    /// Queue a `ClientMessage` to the transport loop, failing fast when the
    /// bounded command queue is full.
    fn send(&self, msg: ClientMessage) -> Result<()> {
        if !self.state.connected.load(Ordering::Acquire) {
            return Err(SignalFishError::NotConnected);
        }
        match self.cmd_tx.try_send(msg) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => Err(SignalFishError::SendBufferFull {
                capacity: self.cmd_tx.max_capacity(),
            }),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(SignalFishError::NotConnected),
        }
    }

    /// Queue a `ClientMessage` to the transport loop, waiting for capacity
    /// when the bounded command queue is full.
    async fn send_reliable(&self, msg: ClientMessage) -> Result<()> {
        if !self.state.connected.load(Ordering::Acquire) {
            return Err(SignalFishError::NotConnected);
        }
        self.cmd_tx
            .send(msg)
            .await
            .map_err(|_| SignalFishError::NotConnected)
    }
}

#[cfg_attr(not(feature = "tokio-runtime"), allow(dead_code))]
impl std::fmt::Debug for SignalFishClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut dbg = f.debug_struct("SignalFishClient");
        dbg.field("connected", &self.is_connected())
            .field("authenticated", &self.is_authenticated());
        #[cfg(feature = "tokio-runtime")]
        dbg.field("has_task", &self.task.is_some());
        dbg.finish()
    }
}

#[cfg(feature = "tokio-runtime")]
impl Drop for SignalFishClient {
    fn drop(&mut self) {
        // `Drop` is synchronous so we cannot await a graceful shutdown.
        // The only safe action is to abort the spawned task, which causes
        // the transport loop future to be dropped immediately.  The
        // `shutdown_tx` oneshot is intentionally *not* sent here: sending
        // it would trigger a graceful path that calls async `transport.close()`,
        // but there is no executor context to drive it inside `Drop`.
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

// ── Transport loop ──────────────────────────────────────────────────

/// Background transport loop that multiplexes send/receive via `tokio::select!`.
///
/// Exits when:
/// - The command channel closes (client handle dropped or shutdown called)
/// - The transport returns `None` (server closed connection)
/// - A transport error occurs
#[cfg(feature = "tokio-runtime")]
async fn transport_loop(
    mut transport: impl Transport,
    mut cmd_rx: mpsc::Receiver<ClientMessage>,
    event_tx: mpsc::Sender<SignalFishEvent>,
    state: Arc<ClientState>,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    debug!("transport loop started");

    // Emit the synthetic Connected event before entering the select loop.
    emit_event(&event_tx, SignalFishEvent::Connected).await;

    loop {
        tokio::select! {
            // Branch 1: outgoing command from the client handle
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(msg) => {
                        debug!("sending client message: {:?}", std::mem::discriminant(&msg));
                        match serde_json::to_string(&msg) {
                            Ok(json) => {
                                if let Err(e) = transport.send(json).await {
                                    error!("transport send error: {e}");
                                    emit_disconnected(
                                        &event_tx,
                                        &state,
                                        Some(format!("transport send error: {e}")),
                                    ).await;
                                    break;
                                }
                                if matches!(msg, ClientMessage::GameData { .. }) {
                                    state.game_data_sent.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            Err(e) => {
                                error!("failed to serialize ClientMessage: {e}");
                                // Serialization errors are programming bugs; don't kill the loop.
                            }
                        }
                    }
                    // Command channel closed — client handle dropped.
                    None => {
                        debug!("command channel closed, shutting down transport loop");
                        let _ = transport.close().await;
                        emit_disconnected(&event_tx, &state, Some("client shut down".into())).await;
                        break;
                    }
                }
            }

            // Branch 2: shutdown signal
            _ = &mut shutdown_rx => {
                debug!("shutdown signal received");
                let _ = transport.close().await;
                emit_disconnected(&event_tx, &state, Some("client shut down".into())).await;
                break;
            }

            // Branch 3: incoming message from the server
            incoming = transport.recv() => {
                match incoming {
                    Some(Ok(text)) => {
                        match serde_json::from_str::<ServerMessage>(&text) {
                            Ok(server_msg) => {
                                // Update shared state based on the message.
                                update_state(&state, &server_msg).await;

                                // Convert to event and forward to the event channel.
                                let event = SignalFishEvent::from(server_msg);
                                emit_event(&event_tx, event).await;
                            }
                            Err(e) => {
                                warn!("failed to deserialize server message: {e} — raw: {text}");
                            }
                        }
                    }
                    Some(Err(e)) => {
                        error!("transport receive error: {e}");
                        emit_disconnected(
                            &event_tx,
                            &state,
                            Some(format!("transport receive error: {e}")),
                        ).await;
                        break;
                    }
                    // Transport closed cleanly.
                    None => {
                        debug!("transport closed by server");
                        emit_disconnected(&event_tx, &state, None).await;
                        break;
                    }
                }
            }
        }
    }

    debug!("transport loop exited");
}

/// Update shared [`ClientState`] based on a received [`ServerMessage`].
#[cfg(feature = "tokio-runtime")]
async fn update_state(state: &ClientState, msg: &ServerMessage) {
    match msg {
        ServerMessage::Authenticated { .. } => {
            state.authenticated.store(true, Ordering::Release);
            debug!("state: authenticated");
        }
        ServerMessage::ProtocolInfo(payload) => {
            // Record the negotiated protocol version so v3-only sends can fail
            // fast on a relay-floor connection. v2 negotiation omits the field
            // (parses as None → 0 → not v3-capable).
            let version = payload.protocol_version.unwrap_or(0);
            state
                .negotiated_protocol_version
                .store(version, Ordering::Release);
            // Mark negotiation as observed even for a v2 floor (version 0): this
            // is what separates "relay-only" from "pre-negotiation" in the guard.
            state.protocol_info_seen.store(true, Ordering::Release);
            debug!("state: negotiated protocol version {version}");
        }
        ServerMessage::RoomJoined(payload) => {
            *state.player_id.lock().await = Some(payload.player_id);
            *state.room_id.lock().await = Some(payload.room_id);
            *state.room_code.lock().await = Some(payload.room_code.clone());
            debug!(
                "state: joined room {} ({})",
                payload.room_code, payload.room_id
            );
        }
        ServerMessage::RoomLeft => {
            *state.room_id.lock().await = None;
            *state.room_code.lock().await = None;
            debug!("state: left room");
        }
        ServerMessage::Reconnected(payload) => {
            *state.player_id.lock().await = Some(payload.player_id);
            *state.room_id.lock().await = Some(payload.room_id);
            *state.room_code.lock().await = Some(payload.room_code.clone());
            // If the negotiated version was replayed via missed_events, restore
            // it so v3 sends aren't wrongly blocked after a reconnect. (A
            // top-level ProtocolInfo is already handled by its own arm.) Only a
            // versioned (v3+) ProtocolInfo restores — a replayed v2 one must not
            // silently downgrade an active v3 session.
            if let Some(version) =
                crate::protocol::replayed_negotiated_version(&payload.missed_events)
            {
                state
                    .negotiated_protocol_version
                    .store(version, Ordering::Release);
                // A replayed `ProtocolInfo` *was* observed, so keep the flag
                // consistent with its name. (Behaviorally moot while the guard
                // short-circuits on version >= 3, but it preserves the
                // "seen implies a ProtocolInfo arrived" invariant for any future
                // reader.)
                state.protocol_info_seen.store(true, Ordering::Release);
            }
            debug!(
                "state: reconnected to room {} ({})",
                payload.room_code, payload.room_id
            );
        }
        ServerMessage::SpectatorJoined(payload) => {
            *state.player_id.lock().await = Some(payload.spectator_id);
            *state.room_id.lock().await = Some(payload.room_id);
            *state.room_code.lock().await = Some(payload.room_code.clone());
            debug!(
                "state: spectator joined room {} ({})",
                payload.room_code, payload.room_id
            );
        }
        ServerMessage::SpectatorLeft { .. } => {
            *state.room_id.lock().await = None;
            *state.room_code.lock().await = None;
            debug!("state: left spectator mode");
        }
        ServerMessage::GameData { .. } | ServerMessage::GameDataBinary { .. } => {
            state.game_data_received.fetch_add(1, Ordering::Relaxed);
        }
        _ => {}
    }
}

/// Emit an event to the event channel, waiting for capacity if it is full.
///
/// Events are **never dropped**: when the consumer lags, the transport loop
/// pauses here, which stops reading from the transport and propagates
/// backpressure to the server (e.g. via TCP receive windows). Delivery only
/// fails if the receiver has been dropped, or if the transport task is
/// aborted while this send is still waiting (a
/// [`SignalFishClient::shutdown`] timeout, or the client handle dropped
/// without `shutdown`).
#[cfg(feature = "tokio-runtime")]
async fn emit_event(event_tx: &mpsc::Sender<SignalFishEvent>, event: SignalFishEvent) {
    if event_tx.send(event).await.is_err() {
        debug!("event channel closed, receiver dropped");
    }
}

/// Emit a [`Disconnected`](SignalFishEvent::Disconnected) event and update state.
///
/// Like every event, `Disconnected` is delivered with backpressure (see
/// [`emit_event`]); it can only be missed if the receiver has been dropped or
/// if [`SignalFishClient::shutdown`] aborts the transport task first.
#[cfg(feature = "tokio-runtime")]
async fn emit_disconnected(
    event_tx: &mpsc::Sender<SignalFishEvent>,
    state: &ClientState,
    reason: Option<String>,
) {
    state.connected.store(false, Ordering::Release);
    state.clear_session_state().await;
    emit_event(event_tx, SignalFishEvent::Disconnected { reason }).await;
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(all(test, feature = "tokio-runtime"))]
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
    use crate::protocol::{LobbyState, RateLimitInfo, RoomJoinedPayload};
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;

    // ── Mock transport ──────────────────────────────────────────────

    /// A mock transport that records sent messages and replays scripted responses.
    struct MockTransport {
        /// Messages that `recv()` will yield in order.
        incoming: VecDeque<Option<std::result::Result<String, SignalFishError>>>,
        /// Recorded outgoing messages.
        sent: Arc<StdMutex<Vec<String>>>,
        /// Whether `close()` was called.
        closed: Arc<AtomicBool>,
    }

    impl MockTransport {
        fn new(
            incoming: Vec<Option<std::result::Result<String, SignalFishError>>>,
        ) -> (Self, Arc<StdMutex<Vec<String>>>, Arc<AtomicBool>) {
            let sent = Arc::new(StdMutex::new(Vec::new()));
            let closed = Arc::new(AtomicBool::new(false));
            let transport = Self {
                incoming: VecDeque::from(incoming),
                sent: Arc::clone(&sent),
                closed: Arc::clone(&closed),
            };
            (transport, sent, closed)
        }
    }

    #[async_trait]
    impl Transport for MockTransport {
        async fn send(&mut self, message: String) -> std::result::Result<(), SignalFishError> {
            self.sent.lock().unwrap().push(message);
            Ok(())
        }

        async fn recv(&mut self) -> Option<std::result::Result<String, SignalFishError>> {
            if let Some(item) = self.incoming.pop_front() {
                // An explicit `None` entry signals a clean transport close;
                // `Some(result)` delivers the scripted message or error.
                item
            } else {
                // All scripted messages have been delivered — pending()
                // never completes, keeping the transport loop alive
                // until shutdown aborts it.
                std::future::pending().await
            }
        }

        async fn close(&mut self) -> std::result::Result<(), SignalFishError> {
            self.closed.store(true, Ordering::Relaxed);
            Ok(())
        }
    }

    // ── Helper ──────────────────────────────────────────────────────

    async fn wait_for_sent_len(sent: &Arc<StdMutex<Vec<String>>>, expected_len: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if sent.lock().unwrap().len() >= expected_len {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for {expected_len} sent message(s); got {}",
                sent.lock().unwrap().len()
            )
        });
    }

    fn authenticated_json() -> String {
        serde_json::to_string(&ServerMessage::Authenticated {
            app_name: "test-app".into(),
            organization: None,
            rate_limits: RateLimitInfo {
                per_minute: 60,
                per_hour: 1000,
                per_day: 10000,
            },
        })
        .unwrap()
    }

    fn room_joined_json() -> String {
        let payload = RoomJoinedPayload {
            room_id: uuid::Uuid::nil(),
            room_code: "ABC123".into(),
            player_id: uuid::Uuid::from_u128(42),
            game_name: "test-game".into(),
            max_players: 4,
            supports_authority: true,
            current_players: vec![],
            is_authority: false,
            lobby_state: LobbyState::Waiting,
            ready_players: vec![],
            relay_type: "auto".into(),
            current_spectators: vec![],
            ice_servers: vec![],
        };
        serde_json::to_string(&ServerMessage::RoomJoined(Box::new(payload))).unwrap()
    }

    // ── Tests ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn start_sends_authenticate_message() {
        let (transport, sent, _closed) = MockTransport::new(vec![Some(Ok(authenticated_json()))]);

        let config = SignalFishConfig::new("mb_test_123");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        // First event should be Connected.
        let event = events.recv().await.unwrap();
        assert!(matches!(event, SignalFishEvent::Connected));

        // Wait for the Authenticated event.
        let event = events.recv().await.unwrap();
        assert!(matches!(event, SignalFishEvent::Authenticated { .. }));

        // The first sent message should be Authenticate.
        {
            let messages = sent.lock().unwrap();
            assert!(!messages.is_empty());
            let first: ClientMessage = serde_json::from_str(&messages[0]).unwrap();
            assert!(matches!(first, ClientMessage::Authenticate { .. }));
            if let ClientMessage::Authenticate { app_id, .. } = &first {
                assert_eq!(app_id, "mb_test_123");
            }
            // Relay floor on the CLIENT-PRODUCED path: the actually-sent bytes
            // (not a hand-built message) must omit every v3 negotiation key, so a
            // default client stays byte-identical to v2.
            let val: serde_json::Value = serde_json::from_str(&messages[0]).unwrap();
            assert!(val["data"].get("protocol_version").is_none());
            assert!(val["data"].get("supported_transports").is_none());
            assert!(val["data"].get("supported_topologies").is_none());
        }

        client.shutdown().await;
    }

    #[tokio::test]
    async fn start_with_enable_mesh_advertises_v3_on_the_wire() {
        let (transport, sent, _closed) = MockTransport::new(vec![Some(Ok(authenticated_json()))]);

        let config = SignalFishConfig::new("mb_mesh").enable_mesh();
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        // Drain Connected + Authenticated so the auth message is flushed.
        let _ = events.recv().await.unwrap();
        let _ = events.recv().await.unwrap();

        {
            let messages = sent.lock().unwrap();
            let val: serde_json::Value = serde_json::from_str(&messages[0]).unwrap();
            assert_eq!(val["data"]["protocol_version"], 3);
            assert_eq!(
                val["data"]["supported_transports"],
                serde_json::json!(["webrtc", "relay"])
            );
            assert_eq!(
                val["data"]["supported_topologies"],
                serde_json::json!(["mesh", "host", "relay"])
            );
        }

        client.shutdown().await;
    }

    #[tokio::test]
    async fn state_updates_on_authenticated() {
        let (transport, _sent, _closed) = MockTransport::new(vec![Some(Ok(authenticated_json()))]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated

        assert!(client.is_authenticated());
        assert!(client.is_connected());

        client.shutdown().await;
    }

    #[tokio::test]
    async fn state_updates_on_room_joined() {
        let (transport, _sent, _closed) = MockTransport::new(vec![
            Some(Ok(authenticated_json())),
            Some(Ok(room_joined_json())),
        ]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated
        let _ = events.recv().await; // RoomJoined

        assert_eq!(client.current_room_code().await.as_deref(), Some("ABC123"));
        assert!(client.current_room_id().await.is_some());
        assert!(client.current_player_id().await.is_some());

        client.shutdown().await;
    }

    #[tokio::test]
    async fn join_room_sends_correct_message() {
        let (transport, sent, _closed) = MockTransport::new(vec![Some(Ok(authenticated_json()))]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated

        let params = JoinRoomParams::new("my-game", "Alice").with_max_players(4);
        client.join_room(params).unwrap();

        wait_for_sent_len(&sent, 2).await;

        {
            let messages = sent.lock().unwrap();
            // Second message should be JoinRoom (first was Authenticate).
            assert!(messages.len() >= 2);
            let join_msg: ClientMessage = serde_json::from_str(&messages[1]).unwrap();
            if let ClientMessage::JoinRoom {
                game_name,
                player_name,
                max_players,
                ..
            } = join_msg
            {
                assert_eq!(game_name, "my-game");
                assert_eq!(player_name, "Alice");
                assert_eq!(max_players, Some(4));
            } else {
                panic!("expected JoinRoom message");
            }
        }

        client.shutdown().await;
    }

    #[tokio::test]
    async fn disconnected_on_transport_close() {
        let (transport, _sent, _closed) = MockTransport::new(vec![
            Some(Ok(authenticated_json())),
            // Explicit None signals clean transport close.
            None,
        ]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated
        let event = events.recv().await.unwrap(); // Disconnected
        assert!(matches!(event, SignalFishEvent::Disconnected { .. }));

        assert!(!client.is_connected());

        client.shutdown().await;
    }

    #[tokio::test]
    async fn not_connected_error_after_shutdown() {
        let (transport, _sent, _closed) = MockTransport::new(vec![Some(Ok(authenticated_json()))]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated

        client.shutdown().await;

        let result = client.ping();
        assert!(matches!(result, Err(SignalFishError::NotConnected)));
        // The waiting variant refuses just the same after shutdown.
        let result = client
            .send_game_data_reliable(serde_json::json!({ "seq": 0 }))
            .await;
        assert!(matches!(result, Err(SignalFishError::NotConnected)));
    }

    #[tokio::test]
    async fn ping_sends_ping_message() {
        let (transport, sent, _closed) = MockTransport::new(vec![Some(Ok(authenticated_json()))]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated
        client.ping().unwrap();

        wait_for_sent_len(&sent, 2).await;

        {
            let messages = sent.lock().unwrap();
            let last: ClientMessage = serde_json::from_str(messages.last().unwrap()).unwrap();
            assert!(matches!(last, ClientMessage::Ping));
        }

        client.shutdown().await;
    }

    #[tokio::test]
    async fn config_defaults() {
        let config = SignalFishConfig::new("mb_test_defaults");
        assert_eq!(config.app_id, "mb_test_defaults");
        assert!(config.sdk_version.is_some());
        assert!(config.platform.is_none());
        assert!(config.game_data_format.is_none());
        // Relay floor by default: no protocol negotiation advertised.
        assert!(config.protocol_version.is_none());
        assert!(config.supported_transports.is_none());
        assert!(config.supported_topologies.is_none());
        assert_eq!(config.event_channel_capacity, 256);
        assert_eq!(config.command_channel_capacity, 1024);
        assert_eq!(config.shutdown_timeout, std::time::Duration::from_secs(1));
    }

    #[tokio::test]
    async fn config_builder_methods() {
        let config = SignalFishConfig::new("mb_test")
            .with_event_channel_capacity(512)
            .with_command_channel_capacity(64)
            .with_shutdown_timeout(std::time::Duration::from_secs(5));
        assert_eq!(config.event_channel_capacity, 512);
        assert_eq!(config.command_channel_capacity, 64);
        assert_eq!(config.shutdown_timeout, std::time::Duration::from_secs(5));
    }

    #[tokio::test]
    async fn config_enable_mesh_advertises_v3() {
        let config = SignalFishConfig::new("mb_test").enable_mesh();
        assert_eq!(config.protocol_version, Some(crate::PROTOCOL_VERSION));
        assert_eq!(
            config.supported_transports,
            Some(vec![TransportKind::WebRtc, TransportKind::Relay])
        );
        assert_eq!(
            config.supported_topologies,
            Some(vec![Topology::Mesh, Topology::Host, Topology::Relay])
        );
    }

    #[tokio::test]
    async fn config_mesh_power_user_builders() {
        let config = SignalFishConfig::new("mb_test")
            .with_protocol_version(3)
            .with_transports([TransportKind::WebRtc])
            .with_topologies([Topology::Mesh, Topology::Relay]);
        assert_eq!(config.protocol_version, Some(3));
        assert_eq!(
            config.supported_transports,
            Some(vec![TransportKind::WebRtc])
        );
        assert_eq!(
            config.supported_topologies,
            Some(vec![Topology::Mesh, Topology::Relay])
        );
    }

    #[tokio::test]
    async fn event_channel_capacity_is_clamped_to_one() {
        let config = SignalFishConfig::new("mb_test").with_event_channel_capacity(0);
        assert_eq!(config.event_channel_capacity, 1);
    }

    #[tokio::test]
    async fn command_channel_capacity_is_clamped_to_one() {
        let config = SignalFishConfig::new("mb_test").with_command_channel_capacity(0);
        assert_eq!(config.command_channel_capacity, 1);
    }

    #[tokio::test]
    async fn zero_event_channel_capacity_does_not_panic() {
        let (transport, _sent, _closed) = MockTransport::new(vec![]);

        let config = SignalFishConfig::new("mb_test")
            .with_event_channel_capacity(0)
            .with_shutdown_timeout(std::time::Duration::from_millis(50));
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        // Should not panic despite capacity 0 — clamped to 1.
        let event = events.recv().await.unwrap();
        assert!(matches!(event, SignalFishEvent::Connected));

        client.shutdown().await;
    }

    #[tokio::test]
    async fn small_event_channel_capacity_delivers_all_events_losslessly() {
        // Capacity 1 forces maximum backpressure: the transport loop must wait
        // for the consumer on every event instead of dropping any.
        let mut incoming: Vec<Option<std::result::Result<String, SignalFishError>>> = Vec::new();
        incoming.push(Some(Ok(authenticated_json())));
        let pong_json = serde_json::to_string(&ServerMessage::Pong).unwrap();
        for _ in 0..20 {
            incoming.push(Some(Ok(pong_json.clone())));
        }
        incoming.push(None);

        let (transport, _sent, _closed) = MockTransport::new(incoming);

        let config = SignalFishConfig::new("mb_test").with_event_channel_capacity(1);
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        // Give the transport loop time to run ahead; it must block, not drop.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut received = Vec::new();
        while let Some(event) = events.recv().await {
            received.push(event);
        }
        // Connected + Authenticated + 20 Pongs + Disconnected — nothing dropped.
        assert_eq!(
            received.len(),
            23,
            "every event must be delivered, got {}",
            received.len()
        );
        assert!(matches!(received[0], SignalFishEvent::Connected));
        assert!(matches!(received[1], SignalFishEvent::Authenticated { .. }));
        assert!(matches!(
            received.last(),
            Some(SignalFishEvent::Disconnected { .. })
        ));

        client.shutdown().await;
    }

    #[tokio::test]
    async fn game_data_events_are_never_dropped_and_stay_ordered() {
        // Data-driven regression for issue #47: a burst of sequenced GameData
        // far larger than the event buffer must arrive complete and in order.
        const MESSAGES: u64 = 500;
        let mut incoming: Vec<Option<std::result::Result<String, SignalFishError>>> = Vec::new();
        incoming.push(Some(Ok(authenticated_json())));
        for seq in 0..MESSAGES {
            let msg = ServerMessage::GameData {
                from_player: uuid::Uuid::from_u128(7),
                data: serde_json::json!({ "seq": seq }),
            };
            incoming.push(Some(Ok(serde_json::to_string(&msg).unwrap())));
        }
        incoming.push(None);

        let (transport, _sent, _closed) = MockTransport::new(incoming);

        // Tiny event buffer: correctness must not depend on channel capacity.
        let config = SignalFishConfig::new("mb_test").with_event_channel_capacity(2);
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let mut seqs = Vec::new();
        while let Some(event) = events.recv().await {
            if let SignalFishEvent::GameData { data, .. } = event {
                seqs.push(data["seq"].as_u64().unwrap());
            }
        }
        let expected: Vec<u64> = (0..MESSAGES).collect();
        assert_eq!(
            seqs, expected,
            "GameData must be delivered losslessly and in order"
        );

        client.shutdown().await;
    }

    /// Issue #47, item 3 (driving contract): a `current_thread` runtime is
    /// fully supported as long as it is actually *driven* — every await here
    /// yields to the runtime, which is what lets the spawned transport loop
    /// progress. No sleeps and no multi-thread runtime are required for a
    /// complete authenticate → send → receive round-trip.
    #[tokio::test(flavor = "current_thread")]
    async fn current_thread_runtime_completes_round_trip() {
        let game_data_json = serde_json::to_string(&ServerMessage::GameData {
            from_player: uuid::Uuid::from_u128(9),
            data: serde_json::json!({ "seq": 0 }),
        })
        .unwrap();
        let (transport, sent, _closed) = MockTransport::new(vec![
            Some(Ok(authenticated_json())),
            Some(Ok(game_data_json)),
        ]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let event = events.recv().await.unwrap();
        assert!(matches!(event, SignalFishEvent::Connected));
        let event = events.recv().await.unwrap();
        assert!(matches!(event, SignalFishEvent::Authenticated { .. }));

        for seq in 0..3 {
            client
                .send_game_data_reliable(serde_json::json!({ "seq": seq }))
                .await
                .unwrap();
        }

        let event = events.recv().await.unwrap();
        assert!(matches!(event, SignalFishEvent::GameData { .. }));

        // Authenticate + 3 GameData all reach the wire on a single thread.
        wait_for_sent_len(&sent, 4).await;
        wait_until(|| client.stats().game_data_sent == 3).await;

        client.shutdown().await;
    }

    #[tokio::test]
    async fn stats_count_game_data_sent_and_received() {
        let game_data_json = |seq: u64| {
            serde_json::to_string(&ServerMessage::GameData {
                from_player: uuid::Uuid::from_u128(9),
                data: serde_json::json!({ "seq": seq }),
            })
            .unwrap()
        };
        let (transport, sent, _closed) = MockTransport::new(vec![
            Some(Ok(authenticated_json())),
            Some(Ok(game_data_json(0))),
            Some(Ok(game_data_json(1))),
        ]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        assert_eq!(client.stats(), ClientStats::default());

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated
        let _ = events.recv().await; // GameData 0
        let _ = events.recv().await; // GameData 1

        for seq in 0..3 {
            client
                .send_game_data(serde_json::json!({ "seq": seq }))
                .unwrap();
        }
        // Authenticate + 3 GameData on the wire; only GameData is counted.
        wait_for_sent_len(&sent, 4).await;
        wait_until(|| client.stats().game_data_sent == 3).await;

        assert_eq!(
            client.stats(),
            ClientStats {
                game_data_sent: 3,
                game_data_received: 2,
            }
        );

        client.shutdown().await;
    }

    // ── Send-side backpressure (issue #47, item 2) ──────────────────

    /// Transport whose `send()` requires a semaphore permit per message, so
    /// tests can stall the outgoing path deterministically.
    struct GatedSendTransport {
        entered_send: Arc<AtomicBool>,
        permits: Arc<tokio::sync::Semaphore>,
        sent: Arc<StdMutex<Vec<String>>>,
    }

    impl GatedSendTransport {
        #[allow(clippy::type_complexity)]
        fn new(
            initial_permits: usize,
        ) -> (
            Self,
            Arc<AtomicBool>,
            Arc<tokio::sync::Semaphore>,
            Arc<StdMutex<Vec<String>>>,
        ) {
            let entered_send = Arc::new(AtomicBool::new(false));
            let permits = Arc::new(tokio::sync::Semaphore::new(initial_permits));
            let sent = Arc::new(StdMutex::new(Vec::new()));
            (
                Self {
                    entered_send: Arc::clone(&entered_send),
                    permits: Arc::clone(&permits),
                    sent: Arc::clone(&sent),
                },
                entered_send,
                permits,
                sent,
            )
        }
    }

    #[async_trait]
    impl Transport for GatedSendTransport {
        async fn send(&mut self, message: String) -> std::result::Result<(), SignalFishError> {
            self.entered_send.store(true, Ordering::Release);
            let permit = self
                .permits
                .acquire()
                .await
                .map_err(|_| SignalFishError::TransportClosed)?;
            permit.forget();
            self.sent.lock().unwrap().push(message);
            Ok(())
        }

        async fn recv(&mut self) -> Option<std::result::Result<String, SignalFishError>> {
            // No scripted messages — pending() never completes, keeping the
            // transport loop alive until shutdown aborts it.
            std::future::pending().await
        }

        async fn close(&mut self) -> std::result::Result<(), SignalFishError> {
            Ok(())
        }
    }

    async fn wait_until(condition: impl Fn() -> bool) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !condition() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for condition"));
    }

    #[tokio::test]
    async fn sync_send_fails_fast_when_command_queue_is_full() {
        // No permits: the transport loop stalls inside send(Authenticate),
        // leaving exactly `capacity` free slots in the command channel.
        let (transport, entered_send, permits, sent) = GatedSendTransport::new(0);

        let config = SignalFishConfig::new("mb_test").with_command_channel_capacity(2);
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected

        // Wait until the loop has pulled Authenticate and stalled in send().
        wait_until(|| entered_send.load(Ordering::Acquire)).await;
        assert_eq!(client.max_send_capacity(), 2);

        // Fill the queue to capacity, then observe the loud refusal.
        client
            .send_game_data(serde_json::json!({ "seq": 0 }))
            .unwrap();
        client
            .send_game_data(serde_json::json!({ "seq": 1 }))
            .unwrap();
        assert_eq!(client.send_capacity(), 0);
        let err = client
            .send_game_data(serde_json::json!({ "seq": 2 }))
            .unwrap_err();
        assert!(
            matches!(err, SignalFishError::SendBufferFull { capacity: 2 }),
            "expected SendBufferFull, got {err:?}"
        );

        // Unblock the transport: the queue drains and sends succeed again.
        permits.add_permits(16);
        wait_for_sent_len(&sent, 3).await;
        wait_until(|| client.send_capacity() > 0).await;
        client
            .send_game_data(serde_json::json!({ "seq": 3 }))
            .unwrap();

        client.shutdown().await;
    }

    #[tokio::test]
    async fn send_game_data_reliable_waits_for_capacity_instead_of_failing() {
        // No permits: Authenticate stalls in send(), then one queued message
        // saturates the capacity-1 command channel.
        let (transport, entered_send, permits, sent) = GatedSendTransport::new(0);

        let config = SignalFishConfig::new("mb_test").with_command_channel_capacity(1);
        let (client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        wait_until(|| entered_send.load(Ordering::Acquire)).await;

        client
            .send_game_data(serde_json::json!({ "seq": 0 }))
            .unwrap();
        assert!(matches!(
            client.send_game_data(serde_json::json!({ "nope": true })),
            Err(SignalFishError::SendBufferFull { .. })
        ));

        // The reliable variant must wait (not fail) while the queue is full…
        let client = Arc::new(client);
        let sender = Arc::clone(&client);
        let mut reliable = tokio::spawn(async move {
            sender
                .send_game_data_reliable(serde_json::json!({ "seq": 1 }))
                .await
        });
        let still_waiting =
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut reliable).await;
        assert!(
            still_waiting.is_err(),
            "reliable send must wait while the queue is full"
        );

        // …and complete once the transport drains the queue.
        permits.add_permits(16);
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), reliable)
            .await
            .expect("reliable send should complete once capacity frees")
            .expect("task must not panic");
        assert!(result.is_ok(), "reliable send should succeed: {result:?}");

        // All three messages reach the wire: Authenticate + both game datas.
        wait_for_sent_len(&sent, 3).await;

        let mut client = Arc::into_inner(client).expect("all clones dropped");
        client.shutdown().await;
    }

    fn protocol_info_v3_json() -> String {
        use crate::protocol::ProtocolInfoPayload;
        serde_json::to_string(&ServerMessage::ProtocolInfo(ProtocolInfoPayload {
            platform: None,
            sdk_version: None,
            minimum_version: None,
            recommended_version: None,
            capabilities: vec![],
            notes: None,
            game_data_formats: vec![],
            player_name_rules: None,
            protocol_version: Some(3),
            min_protocol_version: Some(2),
            max_protocol_version: Some(3),
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn send_signal_reliable_fails_fast_pre_negotiation_even_when_queue_full() {
        // Saturate the capacity-1 command queue behind a stalled transport.
        let (transport, entered_send, permits, _sent) = GatedSendTransport::new(0);
        let config = SignalFishConfig::new("mb_test").with_command_channel_capacity(1);
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        wait_until(|| entered_send.load(Ordering::Acquire)).await;
        client
            .send_game_data(serde_json::json!({ "seq": 0 }))
            .unwrap();
        assert_eq!(client.send_capacity(), 0);

        // The v3 guard must be evaluated BEFORE waiting for queue capacity:
        // pre-negotiation, this returns immediately (nothing is queued)
        // instead of blocking on the full queue.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client.send_signal_reliable(uuid::Uuid::from_u128(5), PeerSignal::Offer("sdp".into())),
        )
        .await
        .expect("guard must fail fast, not wait for capacity");
        assert!(
            matches!(
                result,
                Err(SignalFishError::ProtocolUnsupported {
                    mode: "pre-negotiation"
                })
            ),
            "expected ProtocolUnsupported, got {result:?}"
        );

        permits.add_permits(16);
        client.shutdown().await;
    }

    #[tokio::test]
    async fn send_signal_reliable_reaches_wire_after_v3() {
        let (transport, sent, _closed) = MockTransport::new(vec![
            Some(Ok(authenticated_json())),
            Some(Ok(protocol_info_v3_json())),
        ]);

        let config = SignalFishConfig::new("mb_test").enable_mesh();
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated
        let _ = events.recv().await; // ProtocolInfo (negotiates v3)

        client
            .send_signal_reliable(uuid::Uuid::from_u128(5), PeerSignal::Offer("sdp".into()))
            .await
            .unwrap();

        wait_for_sent_len(&sent, 2).await;
        {
            let messages = sent.lock().unwrap();
            let last: ClientMessage = serde_json::from_str(messages.last().unwrap()).unwrap();
            assert!(
                matches!(last, ClientMessage::Signal { .. }),
                "expected Signal on the wire, got {last:?}"
            );
        }

        client.shutdown().await;
    }

    #[tokio::test]
    async fn custom_shutdown_timeout_is_used() {
        let (transport, _sent, _closed) = MockTransport::new(vec![Some(Ok(authenticated_json()))]);

        let config = SignalFishConfig::new("mb_test")
            .with_shutdown_timeout(std::time::Duration::from_millis(100));
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated

        // Shutdown should complete successfully with the custom timeout.
        client.shutdown().await;
        assert!(!client.is_connected());
    }

    /// Transport that hangs forever in `close()` so shutdown timeout/abort can be tested.
    struct HangingCloseTransport {
        incoming: VecDeque<Option<std::result::Result<String, SignalFishError>>>,
        close_called: Arc<AtomicBool>,
        dropped: Arc<AtomicBool>,
    }

    impl HangingCloseTransport {
        fn new() -> (Self, Arc<AtomicBool>, Arc<AtomicBool>) {
            Self::with_incoming(Vec::new())
        }

        fn with_incoming(
            incoming: Vec<Option<std::result::Result<String, SignalFishError>>>,
        ) -> (Self, Arc<AtomicBool>, Arc<AtomicBool>) {
            let close_called = Arc::new(AtomicBool::new(false));
            let dropped = Arc::new(AtomicBool::new(false));
            (
                Self {
                    incoming: VecDeque::from(incoming),
                    close_called: Arc::clone(&close_called),
                    dropped: Arc::clone(&dropped),
                },
                close_called,
                dropped,
            )
        }
    }

    impl Drop for HangingCloseTransport {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }

    #[async_trait]
    impl Transport for HangingCloseTransport {
        async fn send(&mut self, _message: String) -> std::result::Result<(), SignalFishError> {
            Ok(())
        }

        async fn recv(&mut self) -> Option<std::result::Result<String, SignalFishError>> {
            if let Some(item) = self.incoming.pop_front() {
                item
            } else {
                // No scripted messages — pending() never completes,
                // keeping the task alive until shutdown aborts it.
                std::future::pending().await
            }
        }

        async fn close(&mut self) -> std::result::Result<(), SignalFishError> {
            self.close_called.store(true, Ordering::Release);
            // Simulate a close() that never completes, so the
            // shutdown timeout/abort path can be exercised.
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn shutdown_timeout_aborts_stuck_transport_task() {
        let (transport, close_called, dropped) = HangingCloseTransport::new();
        let config = SignalFishConfig::new("mb_test")
            .with_shutdown_timeout(std::time::Duration::from_millis(20));
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        // Drain Connected so the channel remains uncongested.
        let event = events.recv().await.unwrap();
        assert!(matches!(event, SignalFishEvent::Connected));

        client.shutdown().await;

        assert!(
            close_called.load(Ordering::Acquire),
            "transport.close() should have been attempted during graceful shutdown"
        );
        assert!(
            dropped.load(Ordering::Acquire),
            "timed-out shutdown should abort and drop the transport loop task"
        );
        assert!(!client.is_connected());
    }

    #[tokio::test]
    async fn join_room_params_default() {
        let params = JoinRoomParams::new("g", "p");
        assert!(params.room_code.is_none());
        assert!(params.max_players.is_none());
        assert!(params.supports_authority.is_none());
        assert!(params.relay_transport.is_none());
    }

    #[tokio::test]
    async fn room_left_clears_state() {
        let room_left_json = serde_json::to_string(&ServerMessage::RoomLeft).unwrap();

        let (transport, _sent, _closed) = MockTransport::new(vec![
            Some(Ok(authenticated_json())),
            Some(Ok(room_joined_json())),
            Some(Ok(room_left_json)),
        ]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated
        let _ = events.recv().await; // RoomJoined
        let _ = events.recv().await; // RoomLeft

        assert!(client.current_room_id().await.is_none());
        assert!(client.current_room_code().await.is_none());

        client.shutdown().await;
    }

    #[tokio::test]
    async fn transport_recv_error_emits_disconnected() {
        let (transport, _sent, _closed) = MockTransport::new(vec![Some(Err(
            SignalFishError::TransportReceive("boom".into()),
        ))]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let event = events.recv().await.unwrap();
        assert!(matches!(event, SignalFishEvent::Disconnected { .. }));
        if let SignalFishEvent::Disconnected { reason } = event {
            assert!(reason.unwrap().contains("boom"));
        }

        client.shutdown().await;
    }

    #[tokio::test]
    async fn leave_room_sends_message() {
        let (transport, sent, _closed) = MockTransport::new(vec![Some(Ok(authenticated_json()))]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated
        client.leave_room().unwrap();

        wait_for_sent_len(&sent, 2).await;

        {
            let messages = sent.lock().unwrap();
            let last: ClientMessage = serde_json::from_str(messages.last().unwrap()).unwrap();
            assert!(matches!(last, ClientMessage::LeaveRoom));
        }

        client.shutdown().await;
    }

    #[tokio::test]
    async fn set_ready_sends_player_ready() {
        let (transport, sent, _closed) = MockTransport::new(vec![Some(Ok(authenticated_json()))]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated
        client.set_ready().unwrap();

        wait_for_sent_len(&sent, 2).await;

        {
            let messages = sent.lock().unwrap();
            let last: ClientMessage = serde_json::from_str(messages.last().unwrap()).unwrap();
            assert!(matches!(last, ClientMessage::PlayerReady));
        }

        client.shutdown().await;
    }

    #[tokio::test]
    async fn connected_is_first_event() {
        let (transport, _sent, _closed) = MockTransport::new(vec![Some(Ok(authenticated_json()))]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let first = events.recv().await.unwrap();
        assert!(
            matches!(first, SignalFishEvent::Connected),
            "expected Connected as first event, got {first:?}"
        );

        client.shutdown().await;
    }

    #[tokio::test]
    async fn double_shutdown_does_not_panic() {
        let (transport, _sent, _closed) = MockTransport::new(vec![Some(Ok(authenticated_json()))]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated

        client.shutdown().await;
        client.shutdown().await; // should not panic
    }

    #[tokio::test]
    async fn drop_without_explicit_shutdown() {
        let (transport, _sent, _closed) = MockTransport::new(vec![Some(Ok(authenticated_json()))]);

        let config = SignalFishConfig::new("mb_test");
        let (client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated

        // Drop the client without calling shutdown.
        drop(client);

        // The transport loop should eventually exit; the event channel
        // will close. We just verify we don't hang or panic.
        // Drain remaining events (should be Disconnected then None).
        while let Some(_event) = events.recv().await {}

        // The closed flag may or may not be set depending on timing,
        // but we should reach this point without hanging.
    }

    #[tokio::test]
    async fn event_channel_overflow_backpressures_without_loss() {
        // Create a transport with more messages than the event channel capacity.
        let mut incoming: Vec<Option<std::result::Result<String, SignalFishError>>> = Vec::new();
        incoming.push(Some(Ok(authenticated_json())));
        // Fill more than DEFAULT_EVENT_CHANNEL_CAPACITY pong messages.
        let pongs = DEFAULT_EVENT_CHANNEL_CAPACITY + 50;
        let pong_json = serde_json::to_string(&ServerMessage::Pong).unwrap();
        for _ in 0..pongs {
            incoming.push(Some(Ok(pong_json.clone())));
        }
        // End with a clean close.
        incoming.push(None);

        let (transport, _sent, _closed) = MockTransport::new(incoming);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        // Don't read events immediately — let the channel fill up. The
        // transport loop must pause on the full channel, not drop events.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Now drain events: every single one must have survived the overflow.
        let mut count = 0;
        while let Some(_event) = events.recv().await {
            count += 1;
        }
        // Connected + Authenticated + pongs + Disconnected.
        assert_eq!(
            count,
            pongs + 3,
            "backpressure must preserve every event, got {count}"
        );

        client.shutdown().await;
    }

    #[tokio::test]
    async fn debug_impl_for_client() {
        let (transport, _sent, _closed) = MockTransport::new(vec![Some(Ok(authenticated_json()))]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated

        let debug_str = format!("{:?}", client);
        assert!(debug_str.contains("SignalFishClient"));
        assert!(debug_str.contains("connected"));

        client.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_emits_disconnected() {
        let (transport, _sent, closed) = MockTransport::new(vec![Some(Ok(authenticated_json()))]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated

        client.shutdown().await;

        // After shutdown, a Disconnected event should have been emitted.
        let event = events.recv().await.unwrap();
        assert!(matches!(event, SignalFishEvent::Disconnected { .. }));
        if let SignalFishEvent::Disconnected { reason } = event {
            assert_eq!(reason.as_deref(), Some("client shut down"));
        }

        // The transport should have been closed.
        assert!(closed.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn join_room_params_builder() {
        let params = JoinRoomParams::new("my-game", "Alice")
            .with_room_code("XYZ")
            .with_max_players(6)
            .with_supports_authority(true);

        assert_eq!(params.game_name, "my-game");
        assert_eq!(params.player_name, "Alice");
        assert_eq!(params.room_code.as_deref(), Some("XYZ"));
        assert_eq!(params.max_players, Some(6));
        assert_eq!(params.supports_authority, Some(true));
        assert!(params.relay_transport.is_none());
    }

    // ── RS-1: Tests for untested API methods ────────────────────────

    #[tokio::test]
    async fn send_game_data_sends_correct_message() {
        let (transport, sent, _closed) = MockTransport::new(vec![Some(Ok(authenticated_json()))]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated

        let data = serde_json::json!({ "action": "move", "x": 10, "y": 20 });
        client.send_game_data(data.clone()).unwrap();

        wait_for_sent_len(&sent, 2).await;

        {
            let messages = sent.lock().unwrap();
            assert!(messages.len() >= 2);
            let last: ClientMessage = serde_json::from_str(messages.last().unwrap()).unwrap();
            if let ClientMessage::GameData { data: sent_data } = last {
                assert_eq!(
                    sent_data,
                    serde_json::json!({ "action": "move", "x": 10, "y": 20 })
                );
            } else {
                panic!("expected GameData message, got {last:?}");
            }
        }

        client.shutdown().await;
    }

    #[tokio::test]
    async fn reconnect_sends_correct_message() {
        let (transport, sent, _closed) = MockTransport::new(vec![Some(Ok(authenticated_json()))]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated

        let player_id = uuid::Uuid::from_u128(1);
        let room_id = uuid::Uuid::from_u128(2);
        client
            .reconnect(player_id, room_id, "tok123".into())
            .unwrap();

        wait_for_sent_len(&sent, 2).await;

        {
            let messages = sent.lock().unwrap();
            assert!(messages.len() >= 2);
            let last: ClientMessage = serde_json::from_str(messages.last().unwrap()).unwrap();
            if let ClientMessage::Reconnect {
                player_id: pid,
                room_id: rid,
                auth_token,
            } = last
            {
                assert_eq!(pid, player_id);
                assert_eq!(rid, room_id);
                assert_eq!(auth_token, "tok123");
            } else {
                panic!("expected Reconnect message, got {last:?}");
            }
        }

        client.shutdown().await;
    }

    // ── RS-2: State tests for Reconnected, SpectatorJoined, SpectatorLeft ──

    fn reconnected_json() -> String {
        use crate::protocol::ReconnectedPayload;
        let payload = ReconnectedPayload {
            room_id: uuid::Uuid::from_u128(100),
            room_code: "RECON1".into(),
            player_id: uuid::Uuid::from_u128(200),
            game_name: "recon-game".into(),
            max_players: 6,
            supports_authority: false,
            current_players: vec![],
            is_authority: true,
            lobby_state: LobbyState::Waiting,
            ready_players: vec![],
            relay_type: "tcp".into(),
            current_spectators: vec![],
            ice_servers: vec![],
            missed_events: vec![],
        };
        serde_json::to_string(&ServerMessage::Reconnected(Box::new(payload))).unwrap()
    }

    fn spectator_joined_json() -> String {
        use crate::protocol::SpectatorJoinedPayload;
        let payload = SpectatorJoinedPayload {
            room_id: uuid::Uuid::from_u128(300),
            room_code: "SPEC1".into(),
            spectator_id: uuid::Uuid::from_u128(400),
            game_name: "spec-game".into(),
            current_players: vec![],
            current_spectators: vec![],
            lobby_state: LobbyState::Waiting,
            reason: None,
        };
        serde_json::to_string(&ServerMessage::SpectatorJoined(Box::new(payload))).unwrap()
    }

    fn spectator_left_json() -> String {
        serde_json::to_string(&ServerMessage::SpectatorLeft {
            room_id: Some(uuid::Uuid::from_u128(300)),
            room_code: Some("SPEC1".into()),
            reason: None,
            current_spectators: vec![],
        })
        .unwrap()
    }

    #[tokio::test]
    async fn state_updates_on_reconnected() {
        let (transport, _sent, _closed) = MockTransport::new(vec![
            Some(Ok(authenticated_json())),
            Some(Ok(reconnected_json())),
        ]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated
        let ev = events.recv().await.unwrap(); // Reconnected
        assert!(matches!(ev, SignalFishEvent::Reconnected { .. }));

        assert_eq!(client.current_room_code().await.as_deref(), Some("RECON1"));
        assert_eq!(
            client.current_room_id().await,
            Some(uuid::Uuid::from_u128(100))
        );
        assert_eq!(
            client.current_player_id().await,
            Some(uuid::Uuid::from_u128(200))
        );

        client.shutdown().await;
    }

    #[tokio::test]
    async fn state_updates_on_spectator_joined() {
        let (transport, _sent, _closed) = MockTransport::new(vec![
            Some(Ok(authenticated_json())),
            Some(Ok(spectator_joined_json())),
        ]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated
        let ev = events.recv().await.unwrap(); // SpectatorJoined
        assert!(matches!(ev, SignalFishEvent::SpectatorJoined { .. }));

        assert_eq!(client.current_room_code().await.as_deref(), Some("SPEC1"));
        assert_eq!(
            client.current_room_id().await,
            Some(uuid::Uuid::from_u128(300))
        );
        assert_eq!(
            client.current_player_id().await,
            Some(uuid::Uuid::from_u128(400))
        );

        client.shutdown().await;
    }

    #[tokio::test]
    async fn state_updates_on_spectator_left() {
        let (transport, _sent, _closed) = MockTransport::new(vec![
            Some(Ok(authenticated_json())),
            Some(Ok(spectator_joined_json())),
            Some(Ok(spectator_left_json())),
        ]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated
        let _ = events.recv().await; // SpectatorJoined
        let ev = events.recv().await.unwrap(); // SpectatorLeft
        assert!(matches!(ev, SignalFishEvent::SpectatorLeft { .. }));

        assert!(client.current_room_id().await.is_none());
        assert!(client.current_room_code().await.is_none());

        client.shutdown().await;
    }

    /// Validates the documented best-effort delivery guarantee: when `shutdown()`
    /// times out and aborts the transport task, the `Disconnected` event may NOT
    /// be delivered because the transport loop is forcibly cancelled before it can
    /// emit the event. Both outcomes (event received or not) are acceptable.
    #[tokio::test]
    async fn shutdown_abort_may_skip_disconnected_event() {
        let (transport, _close_called, _dropped) = HangingCloseTransport::new();
        let config = SignalFishConfig::new("mb_test")
            .with_shutdown_timeout(std::time::Duration::from_millis(1));
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        // Drain the initial Connected event so the channel is not congested.
        let event = events.recv().await.unwrap();
        assert!(matches!(event, SignalFishEvent::Connected));

        // Shutdown will timeout (close() hangs) and abort the transport task.
        client.shutdown().await;

        // The transport loop was aborted, so `emit_disconnected` may never have
        // executed. Try to receive with a short timeout — either outcome is valid.
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(50), events.recv()).await;

        match result {
            Ok(Some(SignalFishEvent::Disconnected { .. })) => {
                // Disconnected was delivered before the abort took effect — acceptable.
            }
            Ok(None) => {
                // Channel closed without a Disconnected event — acceptable.
            }
            Err(_) => {
                // Timed out waiting; no Disconnected event was delivered — acceptable.
            }
            Ok(Some(other)) => {
                panic!("unexpected event after shutdown abort: {other:?}");
            }
        }

        assert!(!client.is_connected());
    }

    #[tokio::test]
    async fn shutdown_abort_clears_auth_and_room_state() {
        let (transport, _close_called, _dropped) = HangingCloseTransport::with_incoming(vec![
            Some(Ok(authenticated_json())),
            Some(Ok(room_joined_json())),
        ]);
        let config = SignalFishConfig::new("mb_test")
            .with_shutdown_timeout(std::time::Duration::from_millis(1));
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated
        let _ = events.recv().await; // RoomJoined

        assert!(client.is_authenticated());
        assert_eq!(client.current_room_code().await.as_deref(), Some("ABC123"));
        assert!(client.current_room_id().await.is_some());
        assert!(client.current_player_id().await.is_some());

        client.shutdown().await;

        assert!(!client.is_connected());
        assert!(!client.is_authenticated());
        assert!(client.current_room_id().await.is_none());
        assert!(client.current_room_code().await.is_none());
        assert!(client.current_player_id().await.is_none());
    }
}
