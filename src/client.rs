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
//!   events. Inbound frames that fail to decode are surfaced as
//!   [`DecodeFailed`](SignalFishEvent::DecodeFailed) events (and counted in
//!   [`ClientStats::messages_undecodable`]) rather than dropped. An event can
//!   only be missed when the loop stops delivering entirely: the receiver was
//!   dropped, the client handle was dropped without calling
//!   [`shutdown`](SignalFishClient::shutdown) (which aborts immediately), or
//!   delivery was preempted — `shutdown`, or a terminal disconnect facing a
//!   consumer that never drains, abandon at most the one event delivery they
//!   interrupt (remaining batch events get one nonblocking attempt), close
//!   the transport gracefully under [`shutdown_timeout`](SignalFishConfig::shutdown_timeout),
//!   and deliver the terminal `Disconnected` best-effort (a receiver that
//!   outlives the loop also observes the event channel closing).
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
//! let (mut client, mut events) = SignalFishClient::start(transport, config);
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

#[cfg(all(test, feature = "tokio-runtime"))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "tokio-runtime")]
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(feature = "tokio-runtime")]
use tokio::sync::mpsc;
#[cfg(feature = "tokio-runtime")]
use tracing::{debug, error, warn};

#[cfg(feature = "tokio-runtime")]
use crate::client_core::{
    serialize_client_message, ClientCore, ClientOperation, CoreCommand as ClientCommand,
    SignalGeneration,
};
#[cfg(feature = "tokio-runtime")]
use crate::error::{Result, SignalFishError};
#[cfg(feature = "tokio-runtime")]
use crate::event::SignalFishEvent;
#[cfg(feature = "tokio-runtime")]
use crate::protocol::ClientMessage;
#[cfg(feature = "tokio-runtime")]
use crate::protocol::ConnectionInfo;
#[cfg(any(feature = "tokio-runtime", feature = "polling-client"))]
use crate::protocol::ServerMessage;
#[cfg(feature = "tokio-runtime")]
use crate::protocol::SessionGeneration;
use crate::protocol::{
    GameDataEncoding, PlayerId, RelayTransport, RoomId, Topology, TransportKind,
};
#[cfg(feature = "tokio-runtime")]
use crate::signal::PeerSignal;
#[cfg(feature = "tokio-runtime")]
use crate::terminal_drain::{
    close_reason, peer_close_reason, ReadyFrameDrain, ReadyFrameDrainBudget, ReadyFrameDrainPoll,
};
#[cfg(feature = "tokio-runtime")]
use crate::transport::{close_transport, Transport, TransportFrame};

/// Default capacity of the bounded event channel.
const DEFAULT_EVENT_CHANNEL_CAPACITY: usize = 256;

/// Default capacity of the bounded outgoing command queue.
const DEFAULT_COMMAND_CHANNEL_CAPACITY: usize = 1024;

/// Default timeout for the graceful shutdown.
const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

/// Scheduling grace for the task watchdog after the transport loop's own
/// close deadline. This lets the loop invoke `Transport::abort` first.
#[cfg(feature = "tokio-runtime")]
const SHUTDOWN_TASK_GRACE: Duration = Duration::from_millis(100);

#[cfg(any(feature = "tokio-runtime", feature = "polling-client"))]
pub(crate) fn bounded_binary_preview(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut preview = String::with_capacity(128);
    for byte in bytes.iter().take(64) {
        let _ = write!(&mut preview, "{byte:02x}");
    }
    preview
}

#[cfg(any(feature = "tokio-runtime", feature = "polling-client"))]
pub(crate) fn decode_binary_server_message(
    bytes: &[u8],
    protocol_v3: bool,
) -> std::result::Result<ServerMessage, String> {
    if protocol_v3 {
        let frame = crate::protocol::decode_v3_binary_game_data(bytes)?;
        Ok(ServerMessage::GameDataBinary {
            from_player: frame.from_player,
            encoding: frame.encoding,
            payload: frame.payload,
            seq: Some(frame.seq),
            epoch: Some(frame.epoch),
        })
    } else {
        let frame = crate::protocol::decode_v2_binary_game_data(bytes)?;
        Ok(ServerMessage::GameDataBinary {
            from_player: frame.from_player,
            encoding: frame.encoding,
            payload: frame.payload,
            seq: None,
            epoch: None,
        })
    }
}

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
    /// to v2. Opt into v3 with [`enable_v3`](Self::enable_v3), or advertise
    /// WebRTC/P2P capability with [`enable_mesh`](Self::enable_mesh). The
    /// [`with_protocol_version`](Self::with_protocol_version) builder is the
    /// power-user form and does not add transport or topology capabilities.
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
    /// Events are **never dropped on overflow**. When the consumer cannot keep
    /// up with incoming server messages, the transport loop pauses until the
    /// consumer drains the channel, propagating backpressure to the server
    /// instead of losing data. The capacity only controls how much buffering
    /// the consumer gets before that backpressure kicks in. An event can only
    /// be missed when delivery stops entirely: the receiver is dropped, the
    /// client handle is dropped without calling [`SignalFishClient::shutdown`],
    /// or delivery was preempted — by `shutdown`, or by a terminal disconnect
    /// facing a consumer that never drains, which abandons at most one
    /// in-flight event (remaining events of the same frame get one
    /// nonblocking attempt) after [`shutdown_timeout`](SignalFishConfig::shutdown_timeout)
    /// and delivers the terminal `Disconnected` best-effort.
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
    /// Deadline for graceful async shutdown and polling-client close.
    ///
    /// When [`SignalFishClient::shutdown`] is called, the background transport
    /// loop is given this much time to finish a backend-owned send and drive
    /// [`Transport::poll_close`]. Expiry invokes [`Transport::abort`], after
    /// which the loop normally returns; a later watchdog cancels the task only
    /// if it still does not stop. The polling client uses the same duration to
    /// bound queued-work flushing and its transport close handshake.
    ///
    /// Defaults to **1 second**. A zero timeout invokes the transport abort
    /// fallback without waiting for graceful close. Terminal `Disconnected`
    /// delivery remains best-effort during shutdown.
    pub shutdown_timeout: Duration,
    /// Response to a protocol-v3 delivery-accountability violation.
    pub protocol_violation_policy: ProtocolViolationPolicy,
}

impl SignalFishConfig {
    #[cfg(any(feature = "tokio-runtime", feature = "polling-client"))]
    pub(crate) fn requests_room_operation_ids(&self) -> bool {
        match self.protocol_version {
            Some(version) => version >= 3,
            None => self.supported_transports.is_some() || self.supported_topologies.is_some(),
        }
    }

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
            protocol_violation_policy: ProtocolViolationPolicy::Quarantine,
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

    /// Set the deadline for graceful async shutdown and polling-client close.
    ///
    /// Defaults to **1 second**. A zero timeout invokes `Transport::abort`
    /// without waiting for graceful close; the loop then normally returns.
    #[must_use]
    pub fn with_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    /// Select how delivery-accountability violations affect the connection.
    #[must_use]
    pub fn with_protocol_violation_policy(mut self, policy: ProtocolViolationPolicy) -> Self {
        self.protocol_violation_policy = policy;
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
        self = self.enable_v3();
        self.supported_transports = Some(vec![TransportKind::WebRtc, TransportKind::Relay]);
        self.supported_topologies = Some(vec![Topology::Mesh, Topology::Host, Topology::Relay]);
        self
    }

    /// Ensure a controller-owned WebRTC driver is represented in negotiation.
    ///
    /// Unlike [`enable_mesh`](Self::enable_mesh), this preserves compatible
    /// power-user transport and topology choices. Missing lists receive the
    /// normal mesh-with-relay defaults; explicit lists gain only a missing
    /// WebRTC transport or P2P topology.
    #[cfg(all(feature = "mesh", feature = "tokio-runtime"))]
    pub(crate) fn enable_controller_mesh(mut self) -> Self {
        if self.protocol_version.is_none_or(|version| version < 3) {
            self.protocol_version = Some(crate::PROTOCOL_VERSION.max(3));
        }

        match &mut self.supported_transports {
            Some(transports) if !transports.contains(&TransportKind::WebRtc) => {
                transports.push(TransportKind::WebRtc);
            }
            None => {
                self.supported_transports = Some(vec![TransportKind::WebRtc, TransportKind::Relay]);
            }
            Some(_) => {}
        }

        match &mut self.supported_topologies {
            Some(topologies)
                if !topologies
                    .iter()
                    .any(|topology| matches!(topology, Topology::Host | Topology::Mesh)) =>
            {
                topologies.push(Topology::Mesh);
            }
            None => {
                self.supported_topologies =
                    Some(vec![Topology::Mesh, Topology::Host, Topology::Relay]);
            }
            Some(_) => {}
        }

        self
    }

    #[cfg(any(feature = "tokio-runtime", feature = "polling-client"))]
    pub(crate) fn advertises_mesh_capability(&self) -> bool {
        self.supported_transports
            .as_ref()
            .is_some_and(|transports| transports.contains(&TransportKind::WebRtc))
            && self
                .supported_topologies
                .as_ref()
                .is_some_and(|topologies| {
                    topologies
                        .iter()
                        .any(|topology| matches!(topology, Topology::Host | Topology::Mesh))
                })
    }

    /// Opt into protocol-v3 relay features without advertising WebRTC.
    ///
    /// This enables delivery classes, accountability, reconnect snapshots, and
    /// binary relay while keeping both transport and topology on the universal
    /// server-relay floor.
    #[must_use]
    pub fn enable_v3(mut self) -> Self {
        self.protocol_version = Some(crate::PROTOCOL_VERSION);
        self.supported_transports = Some(vec![TransportKind::Relay]);
        self.supported_topologies = Some(vec![Topology::Relay]);
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

/// Valid protocol-v3 delivery choices for a JSON game-data send.
///
/// The enum makes invalid class/key combinations unrepresentable: only
/// [`Latest`](Self::Latest) carries the required coalescing key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GameDataDelivery {
    /// Preserve every message or disconnect the recipient loudly.
    #[default]
    Reliable,
    /// Retain only the newest undelivered value for this sender-defined key.
    Latest { key: u32 },
    /// Deliver opportunistically without sender backpressure.
    Volatile,
}

/// Runtime response to a decoded server message that violates negotiated
/// lifecycle, session-plan, signaling, or delivery-accountability rules.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProtocolViolationPolicy {
    /// Emit a violation and suppress subsequent room game data until a fresh snapshot.
    #[default]
    Quarantine,
    /// Emit a violation and close the signaling connection.
    Disconnect,
    /// Emit a violation and continue. Lifecycle, plan, and signaling offenders
    /// remain suppressed; delivery-accountability violations retain the
    /// diagnostic delivery behavior documented by the delivery contract.
    Observe,
}

/// The local client's authoritative role in its current room.
///
/// A client outside a room has no role, represented by `None` in
/// [`ClientSnapshot::room_role`]. The role changes only when the server
/// confirms a player or spectator join, reconnect, or departure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomRole {
    /// A room player that may send game data and participate in gameplay.
    Player,
    /// A read-only spectator.
    Spectator,
}

impl std::fmt::Display for RoomRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Player => "player",
            Self::Spectator => "spectator",
        })
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
    /// Optional legacy relay data-path descriptor.
    ///
    /// This does not reconfigure the client's signaling
    /// [`crate::Transport`] or add raw datagram support. Signal Fish
    /// Server 0.7 accepts but ignores it.
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

    /// Set the legacy relay data-path descriptor serialized into `JoinRoom`.
    ///
    /// It does not reconfigure the client's signaling
    /// [`crate::Transport`] or open a socket. Signal Fish Server 0.7
    /// accepts but ignores this field.
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
/// During normal operation, async event-channel overflow applies backpressure,
/// polling work remains queued across bounded cycles, and refused sends return
/// [`SendBufferFull`](crate::SignalFishError::SendBufferFull). Exchange or log
/// these counters across peers to locate a persistent deficit after accounting
/// for explicit terminal boundaries, server delivery policy, and fanout.
///
/// Counters are cumulative for the lifetime of the client (they survive
/// room changes and disconnection).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClientStats {
    /// `GameData` messages whose frames the transport accepted from the client.
    ///
    /// Counted at the ownership-transfer boundary: the first
    /// [`Transport::poll_send`] call that takes the frame from the client. A
    /// later backend completion error does not undo that transfer, and this
    /// counter does not imply peer or server delivery.
    pub game_data_sent: u64,
    /// `GameData`/`GameDataBinary` messages received from the server.
    ///
    /// Counted at **receipt** (when the message is read off the transport and
    /// parsed), before delivery of that message to your event loop. Async
    /// event-channel backpressure can still stop later transport reads, and a
    /// terminal boundary can leave buffered frames unread; account for both
    /// when diagnosing a cross-peer deficit.
    /// Successfully decoded stale, accountability-invalid, and
    /// quarantine-suppressed messages are included. Malformed frames are
    /// excluded and counted by [`messages_undecodable`](Self::messages_undecodable).
    /// Physical binary frames rejected by lifecycle or representation policy
    /// before logical decoding are also excluded.
    pub game_data_received: u64,
    /// Inbound frames that failed to decode into a `ServerMessage`.
    ///
    /// Counted when a frame is read off the transport and fails to parse;
    /// each one also surfaces as a
    /// [`DecodeFailed`](crate::SignalFishEvent::DecodeFailed) event. Steady
    /// growth means protocol drift (a server newer than this SDK) or a
    /// corrupting middlebox.
    pub messages_undecodable: u64,
}

/// Coherent synchronous view of client/session state.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ClientSnapshot {
    /// Whether the client still owns a nonterminal transport connection.
    ///
    /// This becomes `true` when the client is constructed, before a transport
    /// with an asynchronous handshake is necessarily ready. Read
    /// [`transport_ready`](Self::transport_ready) to distinguish that
    /// connecting phase.
    pub connected: bool,
    /// Whether the transport handshake has completed for this connection.
    ///
    /// This becomes `true` immediately before the synthetic
    /// [`Connected`](SignalFishEvent::Connected) event and returns to `false`
    /// on terminal disconnect or close. Commands may still be queued while
    /// this is `false`; transport ownership rules keep their frames pending.
    pub transport_ready: bool,
    /// Whether the server has confirmed authentication for this connection.
    pub authenticated: bool,
    pub negotiated_protocol_version: Option<u16>,
    /// Exact game-data preference supplied in [`SignalFishConfig`].
    ///
    /// `None` means the caller accepted the default JSON format. This value is
    /// retained for the lifetime of the client, including reconnects and after
    /// disconnect, so diagnostics never confuse the request with the format
    /// selected by the server.
    pub requested_game_data_format: Option<GameDataEncoding>,
    /// Game-data format selected for this connection from the first valid
    /// [`ProtocolInfo`](SignalFishEvent::ProtocolInfo).
    ///
    /// `None` until negotiation completes and after the connection ends. An
    /// unsupported preference resolves to `Some(GameDataEncoding::Json)` in
    /// accordance with the Signal Fish Server 0.7 fallback contract.
    pub effective_game_data_format: Option<GameDataEncoding>,
    /// Maximum complete application-payload size, in bytes, that the connected
    /// deployment advertises for its own outbound WebSocket messages.
    ///
    /// Resolved from the negotiated v3
    /// [`ProtocolInfo`](SignalFishEvent::ProtocolInfo)
    /// `max_outbound_message_size`. The server counts the value after
    /// protocol encoding and before WebSocket framing, rejects an over-limit
    /// delivery whole, and closes that connection with RFC 6455 close code
    /// 1009. `None` until negotiation completes, after the connection ends,
    /// and for servers that omit the field — including every frozen-v2
    /// connection.
    pub server_max_outbound_message_size: Option<usize>,
    /// Authoritative local room role, or `None` outside a room.
    ///
    /// This changes only on server-confirmed room, reconnect, and spectator
    /// lifecycle messages. When it is `None`, `player_id`, `room_id`, and
    /// `room_code` are also `None`.
    pub room_role: Option<RoomRole>,
    /// Local room participant ID, whether the role is player or spectator.
    ///
    /// Cleared together with [`room_role`](Self::room_role) on every confirmed
    /// room or spectator exit.
    pub player_id: Option<PlayerId>,
    pub room_id: Option<RoomId>,
    pub room_code: Option<String>,
    /// Generation from the latest authoritative session plan.
    ///
    /// `None` before a plan and for legacy Server 0.4 protocol-v3 plans.
    pub session_generation: Option<crate::protocol::SessionGeneration>,
    /// Topology selected by the latest authoritative session plan.
    ///
    /// This is distinct from locally advertised capability. It is `None`
    /// before the first plan and after leaving the room or disconnecting.
    pub session_topology: Option<Topology>,
    /// Data-path transport selected by the latest authoritative session plan.
    ///
    /// Read this together with [`session_topology`](Self::session_topology) from
    /// the same snapshot when making routing decisions.
    pub session_transport: Option<TransportKind>,
    /// Latest server-issued room reconnection token.
    pub reconnection_token: Option<String>,
    /// Whether accountability policy currently suppresses room game data.
    pub quarantined: bool,
}

impl std::fmt::Debug for ClientSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientSnapshot")
            .field("connected", &self.connected)
            .field("transport_ready", &self.transport_ready)
            .field("authenticated", &self.authenticated)
            .field(
                "negotiated_protocol_version",
                &self.negotiated_protocol_version,
            )
            .field(
                "requested_game_data_format",
                &self.requested_game_data_format,
            )
            .field(
                "effective_game_data_format",
                &self.effective_game_data_format,
            )
            .field(
                "server_max_outbound_message_size",
                &self.server_max_outbound_message_size,
            )
            .field("room_role", &self.room_role)
            .field("player_id", &self.player_id)
            .field("room_id", &self.room_id)
            .field("room_code", &self.room_code)
            .field("session_generation", &self.session_generation)
            .field("session_topology", &self.session_topology)
            .field("session_transport", &self.session_transport)
            .field(
                "reconnection_token",
                &self.reconnection_token.as_ref().map(|_| "<redacted>"),
            )
            .field("quarantined", &self.quarantined)
            .finish()
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
#[cfg(feature = "tokio-runtime")]
pub struct SignalFishClient {
    /// Sender half of the bounded command channel to the transport loop.
    cmd_tx: mpsc::Sender<ClientCommand>,
    /// Shared state updated by the transport loop.
    state: Arc<Mutex<ClientCore>>,
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

/// Async client handle unavailable without the `tokio-runtime` feature.
#[cfg(not(feature = "tokio-runtime"))]
pub struct SignalFishClient {
    _private: (),
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
    /// * `transport` — A [`Transport`] implementation. Its handshake may still
    ///   be in progress if [`Transport::is_ready`] returns `false`.
    /// * `config` — Client configuration including the App ID.
    ///
    /// # Returns
    ///
    /// A tuple of `(client_handle, event_receiver)`. The event receiver yields
    /// [`SignalFishEvent`]s until the transport closes or the client shuts down.
    #[must_use = "the event receiver must be used to receive events"]
    pub fn start(
        transport: impl Transport + Send + 'static,
        config: SignalFishConfig,
    ) -> (Self, mpsc::Receiver<SignalFishEvent>) {
        // Clamp capacities to at least 1 (tokio panics on 0).
        let cmd_capacity = config.command_channel_capacity.max(1);
        let (cmd_tx, cmd_rx) = mpsc::channel::<ClientCommand>(cmd_capacity);
        let capacity = config.event_channel_capacity.max(1);
        let (event_tx, event_rx) = mpsc::channel::<SignalFishEvent>(capacity);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let mesh_capable = config.advertises_mesh_capability();
        let state = Arc::new(Mutex::new(ClientCore::new_with_room_operation_ids(
            config.game_data_format,
            config.protocol_violation_policy,
            mesh_capable,
            config.requests_room_operation_ids(),
        )));
        let loop_state = Arc::clone(&state);

        // Send the Authenticate message through the command channel so the
        // transport loop picks it up as the very first outgoing message.
        let auth_msg = ClientCore::authenticate(&config);
        // This cannot fail: the channel was just created empty and its
        // capacity is clamped to at least 1.
        let _ = cmd_tx.try_send(auth_msg);

        // Arm backend abandonment before constructing/spawning the future so
        // cancellation before its first poll cannot bypass `Transport::abort`.
        let guarded_transport = AbortOnDropTransport::new(transport);
        let task = tokio::spawn(transport_loop(
            guarded_transport,
            cmd_rx,
            event_tx,
            loop_state,
            shutdown_rx,
            config.shutdown_timeout,
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
    /// The shutdown signal preempts even a transport loop blocked on a full
    /// event channel (a consumer that stopped draining): the loop abandons at
    /// most the one event delivery it was waiting on, closes the transport
    /// gracefully, and delivers a terminal
    /// [`Disconnected`](SignalFishEvent::Disconnected) best-effort. The loop
    /// is given [`shutdown_timeout`](SignalFishConfig::shutdown_timeout) to
    /// finish; if the timeout expires (e.g. a transport whose `poll_close`
    /// remains pending), the transport is aborted and the loop normally
    /// returns. A later watchdog cancels the task if it still does not stop.
    /// Task cancellation and handle drop also invoke the transport's required
    /// abort fallback. The same budget bounds a terminal disconnect when
    /// `shutdown` is never called: a consumer that wedges permanently cannot
    /// keep the loop (and every waiting reliable sender) alive past it.
    /// After shutdown completes, the event
    /// receiver yields the remaining buffered events and then `None` — treat
    /// the channel closing as the authoritative end-of-stream signal.
    pub async fn shutdown(&mut self) {
        debug!("SignalFishClient: shutdown requested");

        // Signal the transport loop to shut down gracefully.
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }

        // The loop owns the configured graceful-close deadline. This outer
        // watchdog adds scheduling grace, then cancels only if the loop still
        // does not exit after invoking the transport abort fallback.
        if let Some(mut task) = self.task.take() {
            let task_timeout = self.shutdown_timeout.saturating_add(SHUTDOWN_TASK_GRACE);
            match tokio::time::timeout(task_timeout, &mut task).await {
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

        let mut core = lock_core(&self.state);
        if core.is_connected() {
            let _ = core.disconnect(Some("client shut down".into()));
        }
    }
}

#[cfg(feature = "tokio-runtime")]
impl SignalFishClient {
    // ── Public API methods ──────────────────────────────────────────

    /// Join or create a room with the given parameters.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotAuthenticated`] before the server
    /// confirms authentication, [`SignalFishError::AlreadyInRoom`] when
    /// membership already exists,
    /// [`SignalFishError::RoomOperationPending`] during another room
    /// transition, [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn join_room(&mut self, params: JoinRoomParams) -> Result<()> {
        self.send_operation(ClientOperation::JoinRoom(params))
    }

    /// Leave the current room.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotAuthenticated`] before the server
    /// confirms authentication, [`SignalFishError::NotInRoom`] outside a room,
    /// [`SignalFishError::WrongRoomRole`] as a spectator,
    /// [`SignalFishError::RoomOperationPending`] during another room transition,
    /// [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn leave_room(&mut self) -> Result<()> {
        self.send_operation(ClientOperation::LeaveRoom)
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
    /// Returns [`SignalFishError::NotInRoom`] outside a room,
    /// [`SignalFishError::WrongRoomRole`] as a spectator,
    /// [`SignalFishError::RoomOperationPending`] during a room transition,
    /// [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn send_game_data(&mut self, data: serde_json::Value) -> Result<()> {
        self.send_operation(ClientOperation::GameData(data, GameDataDelivery::Reliable))
    }

    /// Send JSON game data with an explicit protocol-v3 delivery policy.
    ///
    /// # Errors
    ///
    /// Returns the membership/queue errors documented by
    /// [`send_game_data`](Self::send_game_data), or
    /// [`SignalFishError::ProtocolUnsupported`] for a non-reliable delivery
    /// class before protocol v3 is negotiated.
    pub fn send_game_data_with_delivery(
        &mut self,
        data: serde_json::Value,
        delivery: GameDataDelivery,
    ) -> Result<()> {
        self.send_operation(ClientOperation::GameData(data, delivery))
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
    /// are never dropped on overflow — the loop pauses instead). A task that
    /// awaits this method while it is also
    /// the only consumer of the event receiver can therefore deadlock under
    /// simultaneous send + receive pressure. Drain events from a separate
    /// task rather than strictly sequentially. (Do **not** race this send
    /// against the event receiver in a `tokio::select!`: if the event arm
    /// wins, the cancelled send future discards the payload.)
    ///
    /// Apart from that racing pattern this method is cancel-safe: dropping
    /// the returned future while it waits for queue capacity neither queues
    /// the command nor mutates any state, and the operation is admitted at
    /// most once per awaited completion.
    ///
    /// # Errors
    ///
    /// Returns the membership errors documented by
    /// [`send_game_data`](Self::send_game_data), or
    /// [`SignalFishError::NotConnected`] if the transport closes while waiting.
    pub async fn send_game_data_reliable(&self, data: serde_json::Value) -> Result<()> {
        self.send_operation_reliable(ClientOperation::GameData(data, GameDataDelivery::Reliable))
            .await
    }

    /// Waiting counterpart to [`send_game_data_with_delivery`](Self::send_game_data_with_delivery).
    ///
    /// # Errors
    ///
    /// Returns the same membership and protocol errors as
    /// [`send_game_data_with_delivery`](Self::send_game_data_with_delivery), or
    /// [`SignalFishError::NotConnected`] if the transport closes while waiting.
    pub async fn send_game_data_with_delivery_reliable(
        &self,
        data: serde_json::Value,
        delivery: GameDataDelivery,
    ) -> Result<()> {
        self.send_operation_reliable(ClientOperation::GameData(data, delivery))
            .await
    }

    /// Send opaque binary game data over the negotiated protocol-v3 relay.
    ///
    /// # Errors
    ///
    /// Returns the membership errors documented by
    /// [`send_game_data`](Self::send_game_data),
    /// [`SignalFishError::ProtocolUnsupported`] before v3 negotiation, or
    /// [`SignalFishError::BinaryFormatNotNegotiated`] in JSON mode.
    pub fn send_binary_game_data(&mut self, payload: Vec<u8>) -> Result<()> {
        self.send_operation(ClientOperation::Binary(payload))
    }

    /// Waiting binary send that paces on command-queue capacity.
    ///
    /// # Errors
    ///
    /// Returns the same state, protocol, and format errors as
    /// [`send_binary_game_data`](Self::send_binary_game_data), or
    /// [`SignalFishError::NotConnected`] if the transport closes while waiting.
    pub async fn send_binary_game_data_reliable(&self, payload: Vec<u8>) -> Result<()> {
        self.send_operation_reliable(ClientOperation::Binary(payload))
            .await
    }

    /// Signal readiness to start the game in the lobby.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotInRoom`] outside a room,
    /// [`SignalFishError::WrongRoomRole`] as a spectator,
    /// [`SignalFishError::RoomOperationPending`] during a room transition,
    /// [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn set_ready(&mut self) -> Result<()> {
        self.send_operation(ClientOperation::SetReady)
    }

    /// Request to become (or relinquish) authority.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotInRoom`],
    /// [`SignalFishError::WrongRoomRole`], or
    /// [`SignalFishError::RoomOperationPending`] for invalid membership state,
    /// [`SignalFishError::AuthorityRequired`]
    /// when relinquishing authority without holding it,
    /// [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn request_authority(&mut self, become_authority: bool) -> Result<()> {
        self.send_operation(ClientOperation::RequestAuthority(become_authority))
    }

    /// Provide connection information for P2P establishment.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotInRoom`],
    /// [`SignalFishError::WrongRoomRole`], or
    /// [`SignalFishError::RoomOperationPending`] for invalid membership state,
    /// [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn provide_connection_info(&mut self, connection_info: ConnectionInfo) -> Result<()> {
        self.send_operation(ClientOperation::ProvideConnectionInfo(connection_info))
    }

    /// Reconnect to a room after a disconnection.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotAuthenticated`] before the server
    /// confirms authentication, [`SignalFishError::AlreadyInRoom`] when
    /// membership already exists,
    /// [`SignalFishError::RoomOperationPending`] during another room
    /// transition, [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn reconnect(
        &mut self,
        player_id: PlayerId,
        room_id: RoomId,
        auth_token: String,
    ) -> Result<()> {
        self.send_operation(ClientOperation::Reconnect(player_id, room_id, auth_token))
    }

    /// Join a room as a read-only spectator.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotAuthenticated`] before the server
    /// confirms authentication, [`SignalFishError::AlreadyInRoom`] when
    /// membership already exists,
    /// [`SignalFishError::RoomOperationPending`] during another room
    /// transition, [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn join_as_spectator(
        &mut self,
        game_name: String,
        room_code: String,
        spectator_name: String,
    ) -> Result<()> {
        self.send_operation(ClientOperation::JoinAsSpectator(
            game_name,
            room_code,
            spectator_name,
        ))
    }

    /// Leave spectator mode.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotAuthenticated`] before the server
    /// confirms authentication, [`SignalFishError::NotInRoom`] outside a room,
    /// [`SignalFishError::WrongRoomRole`] as a player,
    /// [`SignalFishError::RoomOperationPending`] during another room transition,
    /// [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn leave_spectator(&mut self) -> Result<()> {
        self.send_operation(ClientOperation::LeaveSpectator)
    }

    /// Send a heartbeat ping to the server.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn ping(&mut self) -> Result<()> {
        self.send_operation(ClientOperation::Ping)
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
    /// Returns [`SignalFishError::NotInRoom`],
    /// [`SignalFishError::WrongRoomRole`], or
    /// [`SignalFishError::RoomOperationPending`] for invalid membership state,
    /// [`SignalFishError::AuthorityRequired`] when another authority is assigned,
    /// [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn start_game(&mut self) -> Result<()> {
        self.send_operation(ClientOperation::StartGame)
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
    /// [`SignalFishError::SessionPlanUnavailable`] when no authoritative WebRTC
    /// plan authorizes `to` (including no plan, a non-WebRTC plan, self, or a
    /// target absent from the latest plan/current room roster),
    /// [`SignalFishError::NotInRoom`] outside a room,
    /// [`SignalFishError::WrongRoomRole`] as a spectator,
    /// [`SignalFishError::RoomOperationPending`] during a room transition,
    /// [`SignalFishError::NotConnected`] if the transport has closed, or
    /// [`SignalFishError::SendBufferFull`] if the outgoing command queue is
    /// full (see [`send_signal_reliable`](Self::send_signal_reliable) for a
    /// waiting variant).
    pub fn send_signal(&mut self, to: PlayerId, signal: impl Into<PeerSignal>) -> Result<()> {
        self.send_operation(ClientOperation::Signal(
            to,
            SignalGeneration::Current,
            signal.into(),
        ))
    }

    /// Send a typed WebRTC signal only if `generation` is still the current
    /// authoritative session-plan generation.
    ///
    /// Driver integrations should use this generation-bound form so an offer
    /// or ICE candidate produced just before a re-plan can never be relabeled
    /// with the new generation. `MeshController` uses
    /// this automatically.
    ///
    /// # Errors
    ///
    /// In addition to [`send_signal`](Self::send_signal) errors, returns
    /// [`SignalFishError::StaleSessionGeneration`] when the plan changed before
    /// the signal could be queued.
    pub fn send_signal_for_generation(
        &mut self,
        to: PlayerId,
        generation: Option<SessionGeneration>,
        signal: impl Into<PeerSignal>,
    ) -> Result<()> {
        self.send_operation(ClientOperation::Signal(
            to,
            SignalGeneration::Exact(generation),
            signal.into(),
        ))
    }

    /// Send a typed WebRTC signal, waiting for space in the outgoing command
    /// queue when it is full. **Protocol v3 only.**
    ///
    /// The backpressure-aware counterpart to [`send_signal`](Self::send_signal):
    /// a lost offer/answer/ICE candidate stalls a WebRTC handshake, so waiting
    /// beats failing when the queue is congested (e.g. by game-data bursts).
    ///
    /// The "Keep draining events" caveat on
    /// [`send_game_data_reliable`](Self::send_game_data_reliable)
    /// applies here too: drain events from another task while awaiting this.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::ProtocolUnsupported`] if the connection has
    /// not negotiated protocol v3, the membership errors documented by
    /// [`send_signal`](Self::send_signal), [`SignalFishError::SessionPlanUnavailable`]
    /// when no authoritative WebRTC plan authorizes `to`, or
    /// [`SignalFishError::NotConnected`] if the transport has closed.
    pub async fn send_signal_reliable(
        &self,
        to: PlayerId,
        signal: impl Into<PeerSignal>,
    ) -> Result<()> {
        self.send_operation_reliable(ClientOperation::Signal(
            to,
            SignalGeneration::Current,
            signal.into(),
        ))
        .await
    }

    /// Send an SDP offer to a peer. **Protocol v3 only.**
    /// See [`send_signal`](Self::send_signal).
    ///
    /// # Errors
    ///
    /// See [`send_signal`](Self::send_signal).
    pub fn send_offer(&mut self, to: PlayerId, sdp: impl Into<String>) -> Result<()> {
        self.send_signal(to, PeerSignal::Offer(sdp.into()))
    }

    /// Send an SDP answer to a peer. **Protocol v3 only.**
    /// See [`send_signal`](Self::send_signal).
    ///
    /// # Errors
    ///
    /// See [`send_signal`](Self::send_signal).
    pub fn send_answer(&mut self, to: PlayerId, sdp: impl Into<String>) -> Result<()> {
        self.send_signal(to, PeerSignal::Answer(sdp.into()))
    }

    /// Send a single trickle ICE candidate to a peer. **Protocol v3 only.**
    /// See [`send_signal`](Self::send_signal).
    ///
    /// # Errors
    ///
    /// See [`send_signal`](Self::send_signal).
    pub fn send_ice_candidate(&mut self, to: PlayerId, candidate: impl Into<String>) -> Result<()> {
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
    pub fn send_raw_signal(&mut self, to: PlayerId, signal: serde_json::Value) -> Result<()> {
        self.send_operation(ClientOperation::RawSignal(
            to,
            SignalGeneration::Current,
            signal,
        ))
    }

    /// Relay an unmodeled signal only while `generation` remains current.
    ///
    /// # Errors
    ///
    /// See [`send_signal_for_generation`](Self::send_signal_for_generation).
    pub fn send_raw_signal_for_generation(
        &mut self,
        to: PlayerId,
        generation: Option<SessionGeneration>,
        signal: serde_json::Value,
    ) -> Result<()> {
        self.send_operation(ClientOperation::RawSignal(
            to,
            SignalGeneration::Exact(generation),
            signal,
        ))
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
    /// negotiated protocol v3, the membership errors documented by
    /// [`send_signal`](Self::send_signal), [`SignalFishError::NotConnected`] if the
    /// transport has closed, or [`SignalFishError::SendBufferFull`] if the
    /// outgoing command queue is full.
    pub fn report_transport_status(
        &mut self,
        transport: TransportKind,
        connected: bool,
    ) -> Result<()> {
        self.send_operation(ClientOperation::TransportStatus(transport, connected))
    }

    // ── State accessors ─────────────────────────────────────────────

    /// The protocol version negotiated with the server, or `None` if not yet
    /// negotiated or negotiated as v2 (the relay floor).
    ///
    /// Set from the server's [`ProtocolInfo`](SignalFishEvent::ProtocolInfo)
    /// message. A value of `Some(3)` or higher means v3 was negotiated; local
    /// WebRTC/P2P capability additionally requires the corresponding transport
    /// and topology advertisement and can be queried with [`Self::supports_mesh`].
    pub fn negotiated_protocol_version(&self) -> Option<u16> {
        lock_core(&self.state).negotiated_protocol_version()
    }

    /// Exact game-data preference supplied in [`SignalFishConfig`].
    pub fn requested_game_data_format(&self) -> Option<GameDataEncoding> {
        lock_core(&self.state).requested_game_data_format()
    }

    /// Server-selected game-data format, or `None` while negotiation is
    /// incomplete or after disconnect.
    pub fn effective_game_data_format(&self) -> Option<GameDataEncoding> {
        lock_core(&self.state).effective_game_data_format()
    }

    #[cfg(feature = "mesh")]
    pub(crate) fn session_plan_revision(&self) -> u64 {
        lock_core(&self.state).session_plan_revision()
    }

    /// Whether protocol v3 was negotiated after this client advertised both
    /// WebRTC and at least one P2P topology (`host` or `mesh`).
    ///
    /// This reports local negotiated capability, not the server-selected active
    /// plan. Use [`session_topology`](Self::session_topology),
    /// [`session_transport`](Self::session_transport), or
    /// [`is_p2p_active`](Self::is_p2p_active) for current plan state.
    pub fn supports_mesh(&self) -> bool {
        lock_core(&self.state).supports_mesh()
    }

    /// Topology selected by the latest authoritative session plan.
    ///
    /// This reports active plan state, not local capability. Read it together
    /// with [`session_transport`](Self::session_transport) from one
    /// [`snapshot`](Self::snapshot) when an atomic pair is required.
    pub fn session_topology(&self) -> Option<Topology> {
        lock_core(&self.state).session_topology()
    }

    /// Data-path transport selected by the latest authoritative session plan.
    pub fn session_transport(&self) -> Option<TransportKind> {
        lock_core(&self.state).session_transport()
    }

    /// Whether the latest authoritative plan selects a peer-to-peer topology.
    ///
    /// This active-plan query is independent of [`supports_mesh`](Self::supports_mesh),
    /// which reports negotiated local capability.
    pub fn is_p2p_active(&self) -> bool {
        lock_core(&self.state).is_p2p_active()
    }

    /// Whether the client owns a nonterminal transport attempt.
    ///
    /// This is already `true` while the transport handshake is in progress.
    /// Use [`is_transport_ready`](Self::is_transport_ready) when readiness is
    /// required.
    pub fn is_connected(&self) -> bool {
        lock_core(&self.state).is_connected()
    }

    /// Returns `true` once the transport handshake has completed.
    pub fn is_transport_ready(&self) -> bool {
        lock_core(&self.state).is_transport_ready()
    }

    /// Returns `true` if the server has confirmed authentication.
    pub fn is_authenticated(&self) -> bool {
        lock_core(&self.state).is_authenticated()
    }

    /// Returns the current room ID, if the client is in a room.
    pub async fn current_room_id(&self) -> Option<RoomId> {
        lock_core(&self.state).snapshot().room_id
    }

    /// Returns the local room participant ID, for either a player or spectator.
    ///
    /// This legacy accessor name is retained for compatibility. Use one
    /// [`snapshot`](Self::snapshot) and match its `room_role` with `player_id`
    /// when the interpretation must be atomic. Both values clear on exit.
    pub async fn current_player_id(&self) -> Option<PlayerId> {
        lock_core(&self.state).snapshot().player_id
    }

    /// Returns the server-confirmed local role in the current room.
    pub fn room_role(&self) -> Option<RoomRole> {
        lock_core(&self.state).snapshot().room_role
    }

    /// Returns the current room code, if the client is in a room.
    pub async fn current_room_code(&self) -> Option<String> {
        lock_core(&self.state).snapshot().room_code
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
        lock_core(&self.state).stats()
    }

    /// Return a coherent synchronous snapshot of connection and room state.
    pub fn snapshot(&self) -> ClientSnapshot {
        lock_core(&self.state).snapshot()
    }

    // ── Internal helpers ────────────────────────────────────────────

    fn send_operation(&self, operation: ClientOperation) -> Result<()> {
        // Keep the state lock through nonblocking queue admission. In
        // particular, an exact-generation signal must not pass validation and
        // then race with a replacement SessionPlan before it is queued.
        let mut core = lock_core(&self.state);
        let (command, admission) = core.prepare_with_admission(operation)?;
        let result = match self.cmd_tx.try_send(command) {
            Ok(()) => {
                core.record_admission(admission);
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => Err(SignalFishError::SendBufferFull {
                capacity: self.cmd_tx.max_capacity(),
            }),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(SignalFishError::NotConnected),
        };
        drop(core);
        result
    }

    async fn send_operation_reliable(&self, operation: ClientOperation) -> Result<()> {
        self.send_operation_reliable_after_reserve(operation, || {})
            .await
    }

    async fn send_operation_reliable_after_reserve(
        &self,
        mut operation: ClientOperation,
        after_reserve: impl FnOnce(),
    ) -> Result<()> {
        // Preserve immediate state/negotiation errors even when the queue is
        // full, then revalidate after waiting because room, plan, and format
        // state can change while capacity is unavailable.
        let binding = {
            let core = lock_core(&self.state);
            core.validate(&operation)?;
            // A signal value is produced for the plan current when this call
            // begins. Freeze that generation before waiting so revalidation
            // rejects a stale signal instead of relabeling it after a re-plan.
            core.bind_reliable_operation(&mut operation)
        };
        let permit = self
            .cmd_tx
            .reserve()
            .await
            .map_err(|_| SignalFishError::NotConnected)?;
        after_reserve();
        let core = lock_core(&self.state);
        let command = core.prepare_reliable(operation, binding)?;
        permit.send(command);
        drop(core);
        Ok(())
    }
}

#[cfg(feature = "tokio-runtime")]
impl std::fmt::Debug for SignalFishClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut dbg = f.debug_struct("SignalFishClient");
        dbg.field("connected", &self.is_connected())
            .field("transport_ready", &self.is_transport_ready())
            .field("authenticated", &self.is_authenticated());
        #[cfg(feature = "tokio-runtime")]
        dbg.field("has_task", &self.task.is_some());
        dbg.finish()
    }
}

#[cfg(feature = "tokio-runtime")]
impl crate::client_api::SignalFishClientApi for SignalFishClient {
    fn join_room(&mut self, params: JoinRoomParams) -> Result<()> {
        SignalFishClient::join_room(self, params)
    }

    fn leave_room(&mut self) -> Result<()> {
        SignalFishClient::leave_room(self)
    }

    fn send_game_data(&mut self, data: serde_json::Value) -> Result<()> {
        SignalFishClient::send_game_data(self, data)
    }

    fn send_game_data_with_delivery(
        &mut self,
        data: serde_json::Value,
        delivery: GameDataDelivery,
    ) -> Result<()> {
        SignalFishClient::send_game_data_with_delivery(self, data, delivery)
    }

    fn send_binary_game_data(&mut self, payload: Vec<u8>) -> Result<()> {
        SignalFishClient::send_binary_game_data(self, payload)
    }

    fn set_ready(&mut self) -> Result<()> {
        SignalFishClient::set_ready(self)
    }

    fn start_game(&mut self) -> Result<()> {
        SignalFishClient::start_game(self)
    }

    fn request_authority(&mut self, become_authority: bool) -> Result<()> {
        SignalFishClient::request_authority(self, become_authority)
    }

    fn provide_connection_info(&mut self, connection_info: ConnectionInfo) -> Result<()> {
        SignalFishClient::provide_connection_info(self, connection_info)
    }

    fn reconnect(
        &mut self,
        player_id: PlayerId,
        room_id: RoomId,
        auth_token: String,
    ) -> Result<()> {
        SignalFishClient::reconnect(self, player_id, room_id, auth_token)
    }

    fn join_as_spectator(
        &mut self,
        game_name: String,
        room_code: String,
        spectator_name: String,
    ) -> Result<()> {
        SignalFishClient::join_as_spectator(self, game_name, room_code, spectator_name)
    }

    fn leave_spectator(&mut self) -> Result<()> {
        SignalFishClient::leave_spectator(self)
    }

    fn ping(&mut self) -> Result<()> {
        SignalFishClient::ping(self)
    }

    fn send_signal(&mut self, to: PlayerId, signal: PeerSignal) -> Result<()> {
        SignalFishClient::send_signal(self, to, signal)
    }

    fn send_signal_for_generation(
        &mut self,
        to: PlayerId,
        generation: Option<SessionGeneration>,
        signal: PeerSignal,
    ) -> Result<()> {
        SignalFishClient::send_signal_for_generation(self, to, generation, signal)
    }

    fn send_raw_signal(&mut self, to: PlayerId, signal: serde_json::Value) -> Result<()> {
        SignalFishClient::send_raw_signal(self, to, signal)
    }

    fn send_raw_signal_for_generation(
        &mut self,
        to: PlayerId,
        generation: Option<SessionGeneration>,
        signal: serde_json::Value,
    ) -> Result<()> {
        SignalFishClient::send_raw_signal_for_generation(self, to, generation, signal)
    }

    fn report_transport_status(&mut self, transport: TransportKind, connected: bool) -> Result<()> {
        SignalFishClient::report_transport_status(self, transport, connected)
    }

    fn send_capacity(&self) -> usize {
        SignalFishClient::send_capacity(self)
    }

    fn max_send_capacity(&self) -> usize {
        SignalFishClient::max_send_capacity(self)
    }

    fn stats(&self) -> ClientStats {
        SignalFishClient::stats(self)
    }

    fn snapshot(&self) -> ClientSnapshot {
        SignalFishClient::snapshot(self)
    }

    fn supports_mesh(&self) -> bool {
        SignalFishClient::supports_mesh(self)
    }
}

#[cfg(feature = "tokio-runtime")]
impl Drop for SignalFishClient {
    fn drop(&mut self) {
        // `Drop` is synchronous so we cannot await a graceful shutdown.
        // The only safe action is to abort the spawned task, which causes
        // the transport loop future to be dropped immediately.  The
        // `shutdown_tx` oneshot is intentionally *not* sent here: sending
        // it would trigger a graceful path that awaits `poll_close`, but there
        // is no executor context to drive it inside `Drop`. Aborting
        // the task drops its transport guard, which synchronously invokes the
        // required backend `Transport::abort` fallback.
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

// ── Transport loop ──────────────────────────────────────────────────

#[cfg(feature = "tokio-runtime")]
fn lock_core(state: &Arc<Mutex<ClientCore>>) -> std::sync::MutexGuard<'_, ClientCore> {
    match state.lock() {
        Ok(core) => core,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Owns the async task's transport and enforces backend abandonment whenever
/// task cancellation skips the ordinary graceful-close path.
#[cfg(feature = "tokio-runtime")]
struct AbortOnDropTransport<T: Transport> {
    inner: T,
    armed: bool,
}

#[cfg(feature = "tokio-runtime")]
impl<T: Transport> AbortOnDropTransport<T> {
    const fn new(inner: T) -> Self {
        Self { inner, armed: true }
    }
}

#[cfg(feature = "tokio-runtime")]
impl<T: Transport> Transport for AbortOnDropTransport<T> {
    fn begin_poll_cycle(&mut self) {
        self.inner.begin_poll_cycle();
    }

    fn poll_send(
        &mut self,
        cx: &mut std::task::Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> std::task::Poll<Result<()>> {
        self.inner.poll_send(cx, frame)
    }

    fn poll_recv(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<TransportFrame>>> {
        self.inner.poll_recv(cx)
    }

    fn poll_close(&mut self, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<()>> {
        let result = self.inner.poll_close(cx);
        if matches!(result, std::task::Poll::Ready(Ok(()))) {
            self.armed = false;
        }
        result
    }

    fn abort(&mut self) {
        if self.armed {
            self.armed = false;
            self.inner.abort();
        }
    }

    fn is_ready(&self) -> bool {
        self.inner.is_ready()
    }

    fn close_info(&self) -> Option<crate::transport::TransportCloseInfo> {
        self.inner.close_info()
    }

    fn diagnostics(&self) -> crate::transport::TransportDiagnostics {
        self.inner.diagnostics()
    }
}

#[cfg(feature = "tokio-runtime")]
impl<T: Transport> Drop for AbortOnDropTransport<T> {
    fn drop(&mut self) {
        self.abort();
    }
}

#[cfg(feature = "tokio-runtime")]
async fn finish_core_shutdown(
    transport: &mut impl Transport,
    pending_send: &mut Option<PendingSend>,
    event_tx: &mpsc::Sender<SignalFishEvent>,
    state: &Arc<Mutex<ClientCore>>,
    shutdown_timeout: Duration,
) {
    let event = lock_core(state).disconnect(Some("client shut down".into()));
    let _ = event_tx.try_send(event);
    finish_send_and_close_bounded(transport, pending_send, state, shutdown_timeout).await;
}

#[cfg(feature = "tokio-runtime")]
async fn emit_core_disconnected_or_shutdown(
    transport: &mut impl Transport,
    pending_send: &mut Option<PendingSend>,
    event_tx: &mpsc::Sender<SignalFishEvent>,
    shutdown: &mut ShutdownSignal,
    state: &Arc<Mutex<ClientCore>>,
    teardown: TerminalTeardown,
) {
    // Peer-close delivery is bounded by the same budget as graceful
    // termination: a wedged consumer must not leak the task holding the
    // command receiver, which would park every waiting reliable sender
    // forever. On expiry the terminal event falls back to a nonblocking
    // attempt before the loop terminates.
    let TerminalTeardown {
        reason,
        deadline,
        timeout,
    } = teardown;
    let event = lock_core(state).disconnect(reason);
    let deliver_event = async {
        if !emit_terminal_event(event_tx, shutdown, deadline, event.clone()).await {
            let _ = event_tx.try_send(event);
        }
    };
    let close = finish_send_and_close_bounded(
        transport,
        pending_send,
        state,
        remaining_shutdown_budget(timeout, deadline),
    );
    let ((), ()) = tokio::join!(deliver_event, close);
}

/// One terminal disconnect's attribution and shared shutdown budget: every
/// delivery of the teardown races the same deadline so a wedged consumer
/// cannot keep the loop alive past the configured total.
#[cfg(feature = "tokio-runtime")]
struct TerminalTeardown {
    /// Attribution for the core-computed farewell event.
    reason: Option<String>,
    /// The configured budget total, kept so the close window can be derived
    /// from what is actually left at use time instead of freezing it before
    /// earlier deliveries consume their share.
    timeout: Duration,
    /// Shared budget start for every delivery of this teardown. `None` only
    /// when the configured timeout is too large to represent its deadline,
    /// which restores the documented effectively-never-expiring wait.
    deadline: Option<tokio::time::Instant>,
}

#[cfg(feature = "tokio-runtime")]
impl TerminalTeardown {
    fn starting(timeout: Duration, reason: Option<String>) -> Self {
        Self {
            reason,
            timeout,
            deadline: tokio::time::Instant::now().checked_add(timeout),
        }
    }
}

/// The time left in a shutdown budget whose deadline was computed earlier.
#[cfg(feature = "tokio-runtime")]
fn remaining_shutdown_budget(
    timeout: Duration,
    deadline: Option<tokio::time::Instant>,
) -> Duration {
    deadline.map_or(timeout, |deadline| {
        deadline.saturating_duration_since(tokio::time::Instant::now())
    })
}

/// Deliver a frame's events, racing each wait against shutdown (and an
/// optional terminal deadline).
///
/// Returns `true` when every event was handed to the channel. When a wait is
/// preempted mid-batch, the remaining events are attempted through the
/// nonblocking fallback before being abandoned, so a multi-event frame loses
/// nothing that could still fit instead of everything after the interrupted
/// delivery.
#[cfg(feature = "tokio-runtime")]
async fn emit_event_batch(
    event_tx: &mpsc::Sender<SignalFishEvent>,
    shutdown: &mut ShutdownSignal,
    deadline: Option<tokio::time::Instant>,
    events: Vec<SignalFishEvent>,
) -> bool {
    let mut iter = events.into_iter();
    for event in iter.by_ref() {
        let delivered = match deadline {
            Some(_) => emit_terminal_event(event_tx, shutdown, deadline, event).await,
            None => !matches!(
                emit_event_or_shutdown(event_tx, shutdown, event).await,
                EmitOutcome::ShutdownRequested
            ),
        };
        if !delivered {
            for remaining in iter {
                let _ = event_tx.try_send(remaining);
            }
            return false;
        }
    }
    true
}

#[cfg(feature = "tokio-runtime")]
async fn finish_send_and_close_bounded(
    transport: &mut impl Transport,
    pending_send: &mut Option<PendingSend>,
    state: &Arc<Mutex<ClientCore>>,
    timeout: Duration,
) {
    if timeout.is_zero() {
        *pending_send = None;
        transport.abort();
        return;
    }
    let accepted_send = pending_send
        .as_ref()
        .is_some_and(|pending| pending.frame.is_none());
    let result = tokio::time::timeout(timeout, async {
        if accepted_send {
            let send_result = std::future::poll_fn(|cx| {
                pending_send
                    .as_mut()
                    .map_or(std::task::Poll::Ready(Ok(())), |pending| {
                        poll_pending_send(transport, pending, state, cx)
                    })
            })
            .await;
            if let Err(error) = send_result {
                warn!("accepted send failed while closing transport: {error}");
            }
        }
        *pending_send = None;
        if let Err(error) = close_transport(transport).await {
            warn!("transport close failed; aborting transport: {error}");
            transport.abort();
        }
    })
    .await;

    if result.is_err() {
        warn!("transport close did not finish within timeout; aborting transport");
        transport.abort();
    }
    *pending_send = None;
}

#[cfg(feature = "tokio-runtime")]
struct PendingSend {
    frame: Option<TransportFrame>,
    /// `true` only until the transport first takes ownership of this game-data frame.
    is_game_data: bool,
}

#[cfg(feature = "tokio-runtime")]
fn poll_pending_send(
    transport: &mut impl Transport,
    pending: &mut PendingSend,
    state: &Arc<Mutex<ClientCore>>,
    cx: &mut std::task::Context<'_>,
) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
    let was_client_owned = pending.frame.is_some();
    let result = transport.poll_send(cx, &mut pending.frame);
    if was_client_owned && pending.frame.is_none() && pending.is_game_data {
        lock_core(state).record_game_data_sent();
        pending.is_game_data = false;
    }
    result
}

#[cfg(feature = "tokio-runtime")]
enum TransportIo {
    Ready,
    Sent,
    SendFailed(SignalFishError),
    Received(Option<std::result::Result<TransportFrame, SignalFishError>>),
}

/// Poll an in-flight send and receive together so a backpressured outbound
/// frame cannot hide peer close, inbound errors, or server messages.
#[cfg(feature = "tokio-runtime")]
async fn poll_transport_io(
    transport: &mut impl Transport,
    pending_send: &mut Option<PendingSend>,
    state: &Arc<Mutex<ClientCore>>,
    waiting_for_ready: bool,
) -> TransportIo {
    std::future::poll_fn(|cx| {
        if waiting_for_ready && transport.is_ready() {
            return std::task::Poll::Ready(TransportIo::Ready);
        }
        if let Some(pending) = pending_send.as_mut() {
            match poll_pending_send(transport, pending, state, cx) {
                std::task::Poll::Ready(Ok(())) => {
                    *pending_send = None;
                    return std::task::Poll::Ready(TransportIo::Sent);
                }
                std::task::Poll::Ready(Err(error)) => {
                    *pending_send = None;
                    return std::task::Poll::Ready(TransportIo::SendFailed(error));
                }
                std::task::Poll::Pending => {}
            }
        }

        let receive = transport.poll_recv(cx);
        match receive {
            std::task::Poll::Ready(incoming) => {
                std::task::Poll::Ready(TransportIo::Received(incoming))
            }
            std::task::Poll::Pending if waiting_for_ready && transport.is_ready() => {
                std::task::Poll::Ready(TransportIo::Ready)
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    })
    .await
}

#[cfg(feature = "tokio-runtime")]
async fn wait_for_terminal_deadline(deadline: Option<tokio::time::Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    } else {
        std::future::pending::<()>().await;
    }
}

#[cfg(feature = "tokio-runtime")]
async fn emit_terminal_event(
    event_tx: &mpsc::Sender<SignalFishEvent>,
    shutdown: &mut ShutdownSignal,
    deadline: Option<tokio::time::Instant>,
    event: SignalFishEvent,
) -> bool {
    tokio::select! {
        biased;
        result = event_tx.send(event) => {
            if result.is_err() {
                debug!("event channel closed, receiver dropped");
            }
            true
        }
        _ = shutdown.fired() => false,
        () = wait_for_terminal_deadline(deadline) => false,
    }
}

#[cfg(feature = "tokio-runtime")]
async fn finish_send_failure(
    transport: &mut impl Transport,
    pending_send: &mut Option<PendingSend>,
    event_tx: &mpsc::Sender<SignalFishEvent>,
    shutdown: &mut ShutdownSignal,
    state: &Arc<Mutex<ClientCore>>,
    error: SignalFishError,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now().checked_add(timeout);
    let mut drain = ReadyFrameDrain::new(None, ReadyFrameDrainBudget::standard());
    loop {
        let polled = std::future::poll_fn(|cx| {
            std::task::Poll::Ready(drain.poll_next(
                transport,
                cx,
                deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline),
            ))
        })
        .await;
        let ReadyFrameDrainPoll::Frame {
            frame,
            budget_reached,
        } = polled
        else {
            match polled {
                ReadyFrameDrainPoll::ReceiveFailed(receive_error) => {
                    debug!(%receive_error, "ready-frame drain stopped at receive failure after send failure");
                }
                ReadyFrameDrainPoll::DeadlineReached => {
                    debug!("ready-frame drain reached the shutdown deadline after send failure");
                }
                ReadyFrameDrainPoll::Pending | ReadyFrameDrainPoll::Closed => {}
                ReadyFrameDrainPoll::Frame { .. } => {}
            }
            break;
        };

        let outcome = lock_core(state).process_frame(frame);
        let protocol_stop = outcome.disconnect;
        if !emit_event_batch(event_tx, shutdown, deadline, outcome.events).await {
            debug!("terminal event delivery was preempted mid-batch after send failure");
            break;
        }
        if protocol_stop || budget_reached {
            break;
        }
    }

    let reason = peer_close_reason(transport).or_else(|| Some(error.to_string()));
    let disconnected = lock_core(state).disconnect(reason);
    // A preempted batch means the sticky signal was observed or the shared
    // deadline has passed, so this bounded wait always collapses within one
    // poll instead of parking beside the already-spent budget.
    let deliver_disconnected = async {
        if !emit_terminal_event(event_tx, shutdown, deadline, disconnected.clone()).await {
            let _ = event_tx.try_send(disconnected);
        }
    };
    let close = finish_send_and_close_bounded(
        transport,
        pending_send,
        state,
        remaining_shutdown_budget(timeout, deadline),
    );
    let ((), ()) = tokio::join!(deliver_disconnected, close);
}

/// Background transport loop that multiplexes send/receive via `tokio::select!`.
///
/// Exits when:
/// - The command channel closes (client handle dropped or shutdown called)
/// - The transport returns `None` (server closed connection)
/// - A transport error occurs
#[cfg(feature = "tokio-runtime")]
async fn transport_loop(
    mut transport: impl Transport + Send + 'static,
    mut cmd_rx: mpsc::Receiver<ClientCommand>,
    event_tx: mpsc::Sender<SignalFishEvent>,
    state: Arc<Mutex<ClientCore>>,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    shutdown_timeout: Duration,
) {
    debug!("transport loop started");

    let mut pending_send = None;
    let mut connected_emitted = false;
    let mut shutdown = ShutdownSignal::new(shutdown_rx);

    loop {
        if !connected_emitted && transport.is_ready() && lock_core(&state).mark_transport_ready() {
            connected_emitted = true;
            if matches!(
                emit_event_or_shutdown(&event_tx, &mut shutdown, SignalFishEvent::Connected).await,
                EmitOutcome::ShutdownRequested
            ) {
                finish_core_shutdown(
                    &mut transport,
                    &mut pending_send,
                    &event_tx,
                    &state,
                    shutdown_timeout,
                )
                .await;
                break;
            }
        }
        tokio::select! {
            command = cmd_rx.recv(), if pending_send.is_none() => {
                let Some(command) = command else {
                    emit_core_disconnected_or_shutdown(
                        &mut transport,
                        &mut pending_send,
                        &event_tx,
                        &mut shutdown,
                        &state,
                        TerminalTeardown::starting(
                            shutdown_timeout,
                            Some("client shut down".into()),
                        ),
                    ).await;
                    break;
                };
                let (frame, is_game_data) = match command {
                    ClientCommand::Message(message) => match serialize_client_message(&message) {
                        Ok(json) => (
                            Some(TransportFrame::Text(json)),
                            matches!(message, ClientMessage::GameData { .. }),
                        ),
                        Err(error) => {
                            error!("failed to serialize ClientMessage: {error}");
                            lock_core(&state).dequeue_serialization_failed(&message);
                            (None, false)
                        }
                    },
                    ClientCommand::Binary(payload) => {
                        (Some(TransportFrame::Binary(payload)), true)
                    }
                };
                if let Some(frame) = frame {
                    pending_send = Some(PendingSend {
                        frame: Some(frame),
                        is_game_data,
                    });
                }
            }
            _ = shutdown.fired() => {
                finish_core_shutdown(
                    &mut transport,
                    &mut pending_send,
                    &event_tx,
                    &state,
                    shutdown_timeout,
                ).await;
                break;
            }
            io = poll_transport_io(
                &mut transport,
                &mut pending_send,
                &state,
                !connected_emitted,
            ) => {
                if !connected_emitted
                    && transport.is_ready()
                    && lock_core(&state).mark_transport_ready()
                {
                    connected_emitted = true;
                    if matches!(
                        emit_event_or_shutdown(&event_tx, &mut shutdown, SignalFishEvent::Connected)
                            .await,
                        EmitOutcome::ShutdownRequested
                    ) {
                        finish_core_shutdown(
                            &mut transport,
                            &mut pending_send,
                            &event_tx,
                            &state,
                            shutdown_timeout,
                        ).await;
                        break;
                    }
                }
                if !connected_emitted
                    && matches!(&io, TransportIo::Received(Some(Ok(_))))
                {
                    emit_core_disconnected_or_shutdown(
                        &mut transport,
                        &mut pending_send,
                        &event_tx,
                        &mut shutdown,
                        &state,
                        TerminalTeardown::starting(
                            shutdown_timeout,
                            Some("transport received a protocol frame before readiness".into()),
                        ),
                    ).await;
                    break;
                }
                match io {
                    TransportIo::Ready | TransportIo::Sent => {}
                    TransportIo::SendFailed(error) => {
                        // The transport has made outbound I/O terminal. Freeze
                        // admission before processing any already-ready
                        // inbound farewell frames so no concurrent caller can
                        // enqueue work that will never be attempted.
                        lock_core(&state).freeze_admission();
                        cmd_rx.close();
                        finish_send_failure(
                            &mut transport,
                            &mut pending_send,
                            &event_tx,
                            &mut shutdown,
                            &state,
                            error,
                            shutdown_timeout,
                        ).await;
                        break;
                    }
                    TransportIo::Received(Some(Ok(frame))) => {
                        let outcome = lock_core(&state).process_frame(frame);
                        let disconnect = outcome.disconnect;
                        // A policy-driven disconnect terminates the session,
                        // so its batch shares one shutdown budget with the
                        // farewell delivery and close: a wedged consumer
                        // cannot park reliable senders past the budget. This
                        // mirrors the send-failure teardown; ordinary frames
                        // keep unbounded backpressure.
                        let violation_teardown = disconnect.then(|| {
                            TerminalTeardown::starting(
                                shutdown_timeout,
                                Some("protocol violation".into()),
                            )
                        });
                        let delivered = emit_event_batch(
                            &event_tx,
                            &mut shutdown,
                            violation_teardown.as_ref().and_then(|t| t.deadline),
                            outcome.events,
                        ).await;
                        if !delivered && !disconnect {
                            finish_core_shutdown(
                                &mut transport,
                                &mut pending_send,
                                &event_tx,
                                &state,
                                shutdown_timeout,
                            ).await;
                            break;
                        }
                        if let Some(teardown) = violation_teardown {
                            // A preempted batch means the sticky shutdown
                            // signal was observed or the shared deadline has
                            // passed, so the farewell's bounded wait always
                            // collapses within one poll — exactly like the
                            // send-failure teardown — and never parks beside
                            // the already-spent budget.
                            let TerminalTeardown {
                                reason,
                                deadline,
                                timeout,
                            } = teardown;
                            let event = lock_core(&state).disconnect(reason);
                            let deliver_farewell = async {
                                if !emit_terminal_event(
                                    &event_tx,
                                    &mut shutdown,
                                    deadline,
                                    event.clone(),
                                )
                                .await
                                {
                                    let _ = event_tx.try_send(event);
                                }
                            };
                            let close = finish_send_and_close_bounded(
                                &mut transport,
                                &mut pending_send,
                                &state,
                                remaining_shutdown_budget(timeout, deadline),
                            );
                            let ((), ()) = tokio::join!(deliver_farewell, close);
                            break;
                        }
                    }
                    TransportIo::Received(Some(Err(error))) => {
                        emit_core_disconnected_or_shutdown(
                            &mut transport,
                            &mut pending_send,
                            &event_tx,
                            &mut shutdown,
                            &state,
                            TerminalTeardown::starting(shutdown_timeout, Some(error.to_string())),
                        ).await;
                        break;
                    }
                    TransportIo::Received(None) => {
                        let reason = close_reason(&transport);
                        emit_core_disconnected_or_shutdown(
                            &mut transport,
                            &mut pending_send,
                            &event_tx,
                            &mut shutdown,
                            &state,
                            TerminalTeardown::starting(shutdown_timeout, reason),
                        ).await;
                        break;
                    }
                }
            }
        }
    }
    debug!("transport loop exited");
}

/// Result of racing an event delivery against the shutdown signal.
#[cfg(feature = "tokio-runtime")]
enum EmitOutcome {
    /// The event was handed to the channel (or the receiver is gone — the
    /// loop keeps running either way, matching pre-0.7.0 behavior).
    Delivered,
    /// The shutdown signal fired while the delivery was still waiting for
    /// channel capacity; the in-flight event is abandoned.
    ShutdownRequested,
}

/// Explicitly tracked terminal-shutdown signal (issue #148).
///
/// A completed [`tokio::sync::oneshot::Receiver`] **panics if re-polled**
/// ("called after complete"). Racing the raw receiver is therefore safe only
/// while every call site *infers* consumption from delivery outcomes — a
/// fragile precondition: any future edit that delivers again after a
/// shutdown-arm observation would panic the transport task mid-teardown,
/// closing both channels and failing every parked reliable sender with
/// `NotConnected` instead of completing graceful teardown. This wrapper
/// removes the precondition instead of trusting it. Consumption is recorded
/// once — fired or canceled sender — and every later poll consults the
/// sticky flag without touching the receiver again, so a re-poll degrades to
/// an immediate observation rather than a panic.
///
/// Callers keep their existing nonblocking fallbacks and budget semantics;
/// documented contracts are unchanged.
#[cfg(feature = "tokio-runtime")]
struct ShutdownSignal {
    rx: Option<tokio::sync::oneshot::Receiver<()>>,
    observed: bool,
}

#[cfg(feature = "tokio-runtime")]
impl ShutdownSignal {
    fn new(rx: tokio::sync::oneshot::Receiver<()>) -> Self {
        Self {
            rx: Some(rx),
            observed: false,
        }
    }

    /// Poll whether shutdown has been requested. Sticky: once observed,
    /// later polls answer `true` without re-polling the receiver.
    fn poll_fired(&mut self, cx: &mut std::task::Context<'_>) -> bool {
        if !self.observed {
            if let Some(rx) = self.rx.as_mut() {
                match std::future::Future::poll(std::pin::Pin::new(rx), cx) {
                    // Fired or canceled (sender dropped) both end the wait,
                    // exactly like racing the raw receiver did.
                    std::task::Poll::Ready(_) => {
                        self.observed = true;
                        self.rx = None;
                    }
                    std::task::Poll::Pending => {}
                }
            }
        }
        debug_assert_eq!(
            self.rx.is_none(),
            self.observed,
            "ShutdownSignal state desynchronized"
        );
        self.observed
    }

    /// Future racing this signal inside `tokio::select!`.
    fn fired(&mut self) -> ShutdownFired<'_> {
        ShutdownFired(self)
    }
}

/// Polls [`ShutdownSignal::poll_fired`] to completion.
#[cfg(feature = "tokio-runtime")]
struct ShutdownFired<'a>(&'a mut ShutdownSignal);

#[cfg(feature = "tokio-runtime")]
impl std::future::Future for ShutdownFired<'_> {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if self.0.poll_fired(cx) {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    }
}

/// Emit an event with backpressure, but let a shutdown request preempt the
/// wait.
///
/// `biased` polls the delivery arm first, so when both are ready the event is
/// still delivered; only a genuinely blocked delivery (consumer not draining)
/// lets shutdown win. On [`EmitOutcome::ShutdownRequested`] exactly the one
/// in-flight event is abandoned — the caller must then run
/// [`finish_core_shutdown`] and exit the loop promptly. The shutdown signal's
/// sticky tracking makes any later observation immediate instead of
/// re-polling a completed `oneshot::Receiver` (which panics). Batch callers
/// route through [`emit_event_batch`], which attempts the nonblocking
/// fallback for the batch's remaining events before abandoning them.
#[cfg(feature = "tokio-runtime")]
async fn emit_event_or_shutdown(
    event_tx: &mpsc::Sender<SignalFishEvent>,
    shutdown: &mut ShutdownSignal,
    event: SignalFishEvent,
) -> EmitOutcome {
    tokio::select! {
        biased;
        res = event_tx.send(event) => {
            if res.is_err() {
                debug!("event channel closed, receiver dropped");
            }
            EmitOutcome::Delivered
        }
        _ = shutdown.fired() => EmitOutcome::ShutdownRequested,
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(all(test, feature = "tokio-runtime"))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
mod tests {
    use super::*;
    use crate::protocol::{
        LobbyState, RateLimitInfo, RoomJoinedPayload, ROOM_OPERATION_IDS_CAPABILITY,
    };
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Barrier, Mutex as StdMutex};
    use std::task::{Context, Poll, Waker};

    #[test]
    fn snapshot_debug_redacts_reconnection_token() {
        let snapshot = ClientSnapshot {
            reconnection_token: Some("top-secret-token".into()),
            ..ClientSnapshot::default()
        };
        let debug = format!("{snapshot:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("top-secret-token"));
    }

    // ── Mock transport ──────────────────────────────────────────────

    /// A mock transport that records sent messages and replays scripted responses.
    type SharedIncoming =
        Arc<StdMutex<VecDeque<Option<std::result::Result<String, SignalFishError>>>>>;

    /// Handles for steering a running [`MockTransport`] from a test.
    struct IncomingControls {
        incoming: SharedIncoming,
        waker: Arc<StdMutex<Option<Waker>>>,
    }

    impl IncomingControls {
        fn close_peer(&self) {
            self.incoming.lock().unwrap().push_back(None);
            if let Some(waker) = self.waker.lock().unwrap().take() {
                waker.wake();
            }
        }
    }

    struct MockTransport {
        /// Messages that `poll_recv` will yield in order.
        incoming: SharedIncoming,
        /// Recorded outgoing messages.
        sent: Arc<StdMutex<Vec<String>>>,
        /// Whether `poll_close` was called.
        closed: Arc<AtomicBool>,
        delivered_room_responses: [usize; 5],
        /// Waker registered whenever `poll_recv` has nothing scripted.
        recv_waker: Arc<StdMutex<Option<Waker>>>,
        /// When present, outbound sends block until these permits are
        /// available, keeping queued commands stranded in the client.
        send_gate: Option<Arc<tokio::sync::Semaphore>>,
        held_frame: Option<TransportFrame>,
        permit_wait:
            Option<Pin<Box<dyn Future<Output = tokio::sync::OwnedSemaphorePermit> + Send>>>,
        /// Count of outbound frames the gate has taken ownership of.
        frames_taken: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl MockTransport {
        fn new(
            incoming: Vec<Option<std::result::Result<String, SignalFishError>>>,
        ) -> (Self, Arc<StdMutex<Vec<String>>>, Arc<AtomicBool>) {
            let (transport, sent, closed, _controls) = Self::new_shared(incoming);
            (transport, sent, closed)
        }

        #[allow(clippy::type_complexity)]
        fn new_shared(
            incoming: Vec<Option<std::result::Result<String, SignalFishError>>>,
        ) -> (
            Self,
            Arc<StdMutex<Vec<String>>>,
            Arc<AtomicBool>,
            IncomingControls,
        ) {
            let sent = Arc::new(StdMutex::new(Vec::new()));
            let closed = Arc::new(AtomicBool::new(false));
            let incoming = Arc::new(StdMutex::new(VecDeque::from(incoming)));
            let recv_waker = Arc::new(StdMutex::new(None));
            let transport = Self {
                incoming: Arc::clone(&incoming),
                sent: Arc::clone(&sent),
                closed: Arc::clone(&closed),
                delivered_room_responses: [0; 5],
                recv_waker: Arc::clone(&recv_waker),
                send_gate: None,
                held_frame: None,
                permit_wait: None,
                frames_taken: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            };
            (
                transport,
                sent,
                closed,
                IncomingControls {
                    incoming,
                    waker: recv_waker,
                },
            )
        }

        /// A mock whose outbound sends block on a semaphore after the given
        /// number of initial permits are consumed.
        #[allow(clippy::type_complexity)]
        fn new_send_gated(
            incoming: Vec<Option<std::result::Result<String, SignalFishError>>>,
            initial_permits: usize,
        ) -> (
            Self,
            Arc<StdMutex<Vec<String>>>,
            Arc<AtomicBool>,
            IncomingControls,
            Arc<tokio::sync::Semaphore>,
            Arc<std::sync::atomic::AtomicUsize>,
        ) {
            let (mut transport, sent, closed, controls) = Self::new_shared(incoming);
            let gate = Arc::new(tokio::sync::Semaphore::new(initial_permits));
            transport.send_gate = Some(Arc::clone(&gate));
            let frames_taken = Arc::clone(&transport.frames_taken);
            (transport, sent, closed, controls, gate, frames_taken)
        }
    }

    impl Transport for MockTransport {
        fn abort(&mut self) {
            // Aborting is teardown too: tests observe either graceful close
            // or abort through the same flag.
            self.closed.store(true, Ordering::Relaxed);
            self.held_frame = None;
            self.permit_wait = None;
        }

        fn poll_send(
            &mut self,
            cx: &mut Context<'_>,
            frame: &mut Option<TransportFrame>,
        ) -> Poll<std::result::Result<(), SignalFishError>> {
            let Some(gate) = self.send_gate.as_ref() else {
                return match frame.take() {
                    Some(TransportFrame::Text(message)) => {
                        self.sent.lock().unwrap().push(message);
                        Poll::Ready(Ok(()))
                    }
                    Some(TransportFrame::Binary(_)) => Poll::Ready(Err(
                        SignalFishError::TransportSend("mock expected a text frame".into()),
                    )),
                    None => Poll::Ready(Ok(())),
                };
            };
            if self.held_frame.is_none() {
                let Some(accepted) = frame.take() else {
                    return Poll::Ready(Ok(()));
                };
                self.held_frame = Some(accepted);
                self.frames_taken.fetch_add(1, Ordering::Release);
                let gate = Arc::clone(gate);
                self.permit_wait = Some(Box::pin(async move {
                    gate.acquire_owned().await.expect("send gate never closed")
                }));
            }
            let Some(permit_wait) = self.permit_wait.as_mut() else {
                return Poll::Ready(Err(SignalFishError::TransportClosed));
            };
            let permit = match permit_wait.as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(permit) => permit,
            };
            // Consume the permit permanently so later sends stay blocked.
            permit.forget();
            self.permit_wait = None;
            if let Some(TransportFrame::Text(message)) = self.held_frame.take() {
                self.sent.lock().unwrap().push(message);
            }
            Poll::Ready(Ok(()))
        }

        fn poll_recv(
            &mut self,
            cx: &mut Context<'_>,
        ) -> Poll<Option<std::result::Result<TransportFrame, SignalFishError>>> {
            let response_kind = self.incoming.lock().unwrap().front().and_then(|item| {
                let Some(Ok(json)) = item else {
                    return None;
                };
                let message_type = serde_json::from_str::<serde_json::Value>(json)
                    .ok()?
                    .get("type")?
                    .as_str()?
                    .to_owned();
                match message_type.as_str() {
                    "RoomJoined" | "RoomJoinFailed" => Some(0),
                    "RoomLeft" => Some(1),
                    "Reconnected" | "ReconnectionFailed" => Some(2),
                    "SpectatorJoined" | "SpectatorJoinFailed" => Some(3),
                    "SpectatorLeft" => Some(4),
                    _ => None,
                }
            });
            if let Some(kind) = response_kind {
                let sent_count = self
                    .sent
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|json| {
                        serde_json::from_str::<ClientMessage>(json).is_ok_and(
                            |message| match kind {
                                0 => matches!(message, ClientMessage::JoinRoom { .. }),
                                1 => matches!(message, ClientMessage::LeaveRoom),
                                2 => matches!(message, ClientMessage::Reconnect { .. }),
                                3 => matches!(message, ClientMessage::JoinAsSpectator { .. }),
                                4 => matches!(message, ClientMessage::LeaveSpectator),
                                _ => false,
                            },
                        )
                    })
                    .count();
                if sent_count <= self.delivered_room_responses[kind] {
                    *self.recv_waker.lock().unwrap() = Some(cx.waker().clone());
                    return Poll::Pending;
                }
            }
            if let Some(item) = self.incoming.lock().unwrap().pop_front() {
                if let Some(kind) = response_kind {
                    self.delivered_room_responses[kind] += 1;
                }
                // An explicit `None` entry signals a clean transport close;
                // `Some(result)` delivers the scripted message or error.
                Poll::Ready(item.map(|result| result.map(TransportFrame::Text)))
            } else {
                // All scripted messages have been delivered. Register the
                // waker so tests can push further frames (or a peer close)
                // into the running queue.
                *self.recv_waker.lock().unwrap() = Some(cx.waker().clone());
                Poll::Pending
            }
        }

        fn poll_close(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<std::result::Result<(), SignalFishError>> {
            self.closed.store(true, Ordering::Relaxed);
            Poll::Ready(Ok(()))
        }
    }

    #[derive(Clone)]
    struct DeferredReadyControls {
        ready: Arc<AtomicBool>,
        waker: Arc<StdMutex<Option<Waker>>>,
    }

    impl DeferredReadyControls {
        fn set_ready(&self, ready: bool) {
            self.ready.store(ready, Ordering::Release);
            if ready {
                if let Some(waker) = self.waker.lock().unwrap().take() {
                    waker.wake();
                }
            }
        }
    }

    struct DeferredReadyTransport {
        controls: DeferredReadyControls,
        sent: Arc<StdMutex<Vec<TransportFrame>>>,
    }

    impl DeferredReadyTransport {
        fn new() -> (
            Self,
            DeferredReadyControls,
            Arc<StdMutex<Vec<TransportFrame>>>,
        ) {
            let controls = DeferredReadyControls {
                ready: Arc::new(AtomicBool::new(false)),
                waker: Arc::new(StdMutex::new(None)),
            };
            let sent = Arc::new(StdMutex::new(Vec::new()));
            (
                Self {
                    controls: controls.clone(),
                    sent: Arc::clone(&sent),
                },
                controls,
                sent,
            )
        }
    }

    impl Transport for DeferredReadyTransport {
        fn abort(&mut self) {
            let _ = self.controls.waker.lock().unwrap().take();
        }

        fn poll_send(
            &mut self,
            cx: &mut Context<'_>,
            frame: &mut Option<TransportFrame>,
        ) -> Poll<std::result::Result<(), SignalFishError>> {
            if !self.is_ready() {
                *self.controls.waker.lock().unwrap() = Some(cx.waker().clone());
                return Poll::Pending;
            }
            if let Some(frame) = frame.take() {
                self.sent.lock().unwrap().push(frame);
            }
            Poll::Ready(Ok(()))
        }

        fn poll_recv(
            &mut self,
            cx: &mut Context<'_>,
        ) -> Poll<Option<std::result::Result<TransportFrame, SignalFishError>>> {
            *self.controls.waker.lock().unwrap() = Some(cx.waker().clone());
            Poll::Pending
        }

        fn poll_close(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<std::result::Result<(), SignalFishError>> {
            Poll::Ready(Ok(()))
        }

        fn is_ready(&self) -> bool {
            self.controls.ready.load(Ordering::Acquire)
        }
    }

    struct GameDataErrorTransport {
        take_before_error: bool,
    }

    impl Transport for GameDataErrorTransport {
        fn abort(&mut self) {}

        fn poll_send(
            &mut self,
            _cx: &mut Context<'_>,
            frame: &mut Option<TransportFrame>,
        ) -> Poll<std::result::Result<(), SignalFishError>> {
            let game_data = matches!(
                frame.as_ref(),
                Some(TransportFrame::Text(text)) if text.contains("\"type\":\"GameData\"")
            );
            if game_data {
                if self.take_before_error {
                    let _ = frame.take();
                }
                return Poll::Ready(Err(SignalFishError::TransportSend(
                    "scripted game-data failure".into(),
                )));
            }
            let _ = frame.take();
            Poll::Ready(Ok(()))
        }

        fn poll_recv(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<std::result::Result<TransportFrame, SignalFishError>>> {
            Poll::Pending
        }

        fn poll_close(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<std::result::Result<(), SignalFishError>> {
            Poll::Ready(Ok(()))
        }
    }

    #[derive(Clone)]
    struct PendingGameDataControls {
        accepted: Arc<AtomicBool>,
        outcome: Arc<std::sync::atomic::AtomicU8>,
        waker: Arc<StdMutex<Option<Waker>>>,
    }

    impl PendingGameDataControls {
        fn finish(&self, fail: bool) {
            self.outcome
                .store(if fail { 2 } else { 1 }, Ordering::Release);
            if let Some(waker) = self.waker.lock().unwrap().take() {
                waker.wake();
            }
        }
    }

    struct PendingGameDataTransport {
        retained: Option<TransportFrame>,
        controls: PendingGameDataControls,
    }

    impl PendingGameDataTransport {
        fn new() -> (Self, PendingGameDataControls) {
            let controls = PendingGameDataControls {
                accepted: Arc::new(AtomicBool::new(false)),
                outcome: Arc::new(std::sync::atomic::AtomicU8::new(0)),
                waker: Arc::new(StdMutex::new(None)),
            };
            (
                Self {
                    retained: None,
                    controls: controls.clone(),
                },
                controls,
            )
        }
    }

    impl Transport for PendingGameDataTransport {
        fn poll_send(
            &mut self,
            cx: &mut Context<'_>,
            frame: &mut Option<TransportFrame>,
        ) -> Poll<std::result::Result<(), SignalFishError>> {
            if self.retained.is_none() {
                let game_data = matches!(
                    frame.as_ref(),
                    Some(TransportFrame::Text(text)) if text.contains("\"type\":\"GameData\"")
                );
                if !game_data {
                    let _ = frame.take();
                    return Poll::Ready(Ok(()));
                }
                self.retained = frame.take();
                self.controls.accepted.store(true, Ordering::Release);
            }
            match self.controls.outcome.load(Ordering::Acquire) {
                1 => {
                    self.retained = None;
                    Poll::Ready(Ok(()))
                }
                2 => {
                    self.retained = None;
                    Poll::Ready(Err(SignalFishError::TransportSend(
                        "scripted completion failure".into(),
                    )))
                }
                _ => {
                    *self.controls.waker.lock().unwrap() = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        }

        fn poll_recv(
            &mut self,
            cx: &mut Context<'_>,
        ) -> Poll<Option<std::result::Result<TransportFrame, SignalFishError>>> {
            *self.controls.waker.lock().unwrap() = Some(cx.waker().clone());
            Poll::Pending
        }

        fn poll_close(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<std::result::Result<(), SignalFishError>> {
            Poll::Ready(Ok(()))
        }

        fn abort(&mut self) {
            self.retained = None;
            let _ = self.controls.waker.lock().unwrap().take();
        }
    }

    struct NeverReadyTerminalTransport {
        frame: Option<TransportFrame>,
    }

    impl Transport for NeverReadyTerminalTransport {
        fn abort(&mut self) {
            self.frame = None;
        }

        fn poll_send(
            &mut self,
            _cx: &mut Context<'_>,
            _frame: &mut Option<TransportFrame>,
        ) -> Poll<std::result::Result<(), SignalFishError>> {
            Poll::Pending
        }

        fn poll_recv(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<std::result::Result<TransportFrame, SignalFishError>>> {
            Poll::Ready(self.frame.take().map(Ok))
        }

        fn poll_close(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<std::result::Result<(), SignalFishError>> {
            Poll::Ready(Ok(()))
        }

        fn is_ready(&self) -> bool {
            false
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

    fn protocol_info_v2_json() -> String {
        serde_json::to_string(&ServerMessage::ProtocolInfo(
            crate::protocol::ProtocolInfoPayload {
                platform: None,
                sdk_version: None,
                minimum_version: None,
                recommended_version: None,
                capabilities: vec![],
                notes: None,
                game_data_formats: vec![GameDataEncoding::Json, GameDataEncoding::MessagePack],
                player_name_rules: None,
                protocol_version: None,
                min_protocol_version: None,
                max_protocol_version: None,
                transports: None,
                max_outbound_message_size: None,
            },
        ))
        .unwrap()
    }

    fn room_joined_json() -> String {
        let player = |id, name: &str| crate::protocol::PlayerInfo {
            id,
            name: name.into(),
            is_authority: id == uuid::Uuid::from_u128(42),
            is_ready: false,
            connected_at: "2026-01-01T00:00:00Z".into(),
            connection_info: None,
            epoch: None,
            seq: None,
        };
        let payload = RoomJoinedPayload {
            room_id: uuid::Uuid::nil(),
            room_code: "ABC123".into(),
            player_id: uuid::Uuid::from_u128(42),
            game_name: "test-game".into(),
            max_players: 4,
            supports_authority: true,
            current_players: vec![
                player(uuid::Uuid::from_u128(42), "local"),
                player(uuid::Uuid::from_u128(7), "peer-7"),
                player(uuid::Uuid::from_u128(9), "peer-9"),
            ],
            is_authority: true,
            lobby_state: LobbyState::Waiting,
            ready_players: vec![],
            relay_type: "auto".into(),
            current_spectators: vec![],
            ice_servers: vec![],
            reconnection_token: None,
        };
        serde_json::to_string(&ServerMessage::RoomJoined(Box::new(payload))).unwrap()
    }

    fn prime_player_room(client: &SignalFishClient) {
        let mut core = lock_core(&client.state);
        let _ = core.process_frame(TransportFrame::Text(authenticated_json()));
        let _ = core.process_frame(TransportFrame::Text(protocol_info_v2_json()));
        core.record_admission(ClientCore::admission_for(&ClientOperation::JoinRoom(
            JoinRoomParams::new("test-game", "local"),
        )));
        let _ = core.process_frame(TransportFrame::Text(room_joined_json()));
    }

    async fn enter_scripted_player_room(
        client: &mut SignalFishClient,
        events: &mut mpsc::Receiver<SignalFishEvent>,
    ) {
        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Connected)
        ));
        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Authenticated { .. })
        ));
        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::ProtocolInfo(_))
        ));
        client
            .join_room(JoinRoomParams::new("test-game", "local"))
            .expect("scripted player join should be admitted");
        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::RoomJoined { .. })
        ));
    }

    async fn enter_scripted_spectator_room(
        client: &mut SignalFishClient,
        events: &mut mpsc::Receiver<SignalFishEvent>,
    ) {
        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Connected)
        ));
        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Authenticated { .. })
        ));
        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::ProtocolInfo(_))
        ));
        client
            .join_as_spectator("spec-game".into(), "SPEC1".into(), "local".into())
            .expect("scripted spectator join should be admitted");
        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::SpectatorJoined { .. })
        ));
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
            assert!(val["data"].get("requested_capabilities").is_none());
        }

        client.shutdown().await;
    }

    #[tokio::test]
    async fn connected_and_snapshot_wait_for_async_transport_readiness() {
        let (transport, controls, sent) = DeferredReadyTransport::new();
        let (mut client, mut events) =
            SignalFishClient::start(transport, SignalFishConfig::new("app"));

        assert_eq!(
            (
                client.is_connected(),
                client.is_transport_ready(),
                client.is_authenticated(),
                client.room_role(),
            ),
            (true, false, false, None)
        );
        tokio::task::yield_now().await;
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        assert!(sent.lock().unwrap().is_empty());

        controls.set_ready(true);
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), events.recv()).await,
            Ok(Some(SignalFishEvent::Connected))
        ));
        wait_until(|| !sent.lock().unwrap().is_empty()).await;
        assert!(client.is_transport_ready());

        controls.set_ready(false);
        tokio::task::yield_now().await;
        assert!(client.is_transport_ready(), "observed readiness is sticky");

        client.shutdown().await;
        assert_eq!(
            (
                client.is_connected(),
                client.is_transport_ready(),
                client.is_authenticated(),
                client.room_role(),
            ),
            (false, false, false, None)
        );
    }

    #[tokio::test]
    async fn send_error_counts_only_frames_the_transport_took() {
        for (take_before_error, expected_sent) in [(false, 0), (true, 1)] {
            let transport = GameDataErrorTransport { take_before_error };
            let (mut client, mut events) =
                SignalFishClient::start(transport, SignalFishConfig::new("app"));
            assert!(matches!(
                events.recv().await,
                Some(SignalFishEvent::Connected)
            ));
            prime_player_room(&client);
            client
                .send_game_data(serde_json::json!({"frame": 1}))
                .expect("primed player may queue game data");

            let terminal = tokio::time::timeout(Duration::from_secs(1), events.recv())
                .await
                .expect("send failure should become terminal")
                .expect("Disconnected event should be delivered");

            assert!(matches!(terminal, SignalFishEvent::Disconnected { .. }));
            assert_eq!(
                client.stats().game_data_sent,
                expected_sent,
                "take_before_error={take_before_error}"
            );
            client.shutdown().await;
        }
    }

    #[tokio::test]
    async fn accepted_pending_game_data_counts_before_completion_without_double_counting() {
        for fail_completion in [false, true] {
            let (transport, controls) = PendingGameDataTransport::new();
            let (mut client, mut events) =
                SignalFishClient::start(transport, SignalFishConfig::new("app"));
            assert!(matches!(
                events.recv().await,
                Some(SignalFishEvent::Connected)
            ));
            prime_player_room(&client);
            client
                .send_game_data(serde_json::json!({"frame": 1}))
                .expect("primed player may queue game data");
            wait_until(|| controls.accepted.load(Ordering::Acquire)).await;
            assert_eq!(
                client.stats().game_data_sent,
                1,
                "ownership transfer counts while completion is pending"
            );

            controls.finish(fail_completion);
            if fail_completion {
                let terminal = tokio::time::timeout(Duration::from_secs(1), events.recv())
                    .await
                    .expect("failed accepted send should become terminal")
                    .expect("Disconnected event should be delivered");
                assert!(matches!(terminal, SignalFishEvent::Disconnected { .. }));
            } else {
                tokio::task::yield_now().await;
            }
            assert_eq!(client.stats().game_data_sent, 1);
            client.shutdown().await;
            assert_eq!(
                client.stats().game_data_sent,
                1,
                "completion and shutdown must not double-count acceptance"
            );
        }
    }

    #[tokio::test]
    async fn terminal_before_readiness_never_emits_connected() {
        for frame in [None, Some(TransportFrame::Text("{}".into()))] {
            let transport = NeverReadyTerminalTransport { frame };
            let (mut client, mut events) =
                SignalFishClient::start(transport, SignalFishConfig::new("app"));

            let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
                .await
                .expect("terminal transport should wake the driver")
                .expect("Disconnected should be delivered");

            assert!(matches!(event, SignalFishEvent::Disconnected { .. }));
            assert!(matches!(
                events.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                    | Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)
            ));
            let snapshot = client.snapshot();
            assert_eq!(
                (
                    snapshot.connected,
                    snapshot.transport_ready,
                    snapshot.authenticated,
                    snapshot.room_role,
                ),
                (false, false, false, None)
            );
            client.shutdown().await;
        }
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
            assert_eq!(
                val["data"]["requested_capabilities"],
                serde_json::json!([ROOM_OPERATION_IDS_CAPABILITY])
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
            Some(Ok(protocol_info_v2_json())),
            Some(Ok(room_joined_json())),
        ]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        enter_scripted_player_room(&mut client, &mut events).await;

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

    #[test]
    fn room_operation_capability_request_intent_matches_authentication_wire() {
        let cases = [
            ("default", SignalFishConfig::new("app"), false),
            (
                "explicit-v2",
                SignalFishConfig::new("app")
                    .with_protocol_version(2)
                    .with_transports([TransportKind::Relay]),
                false,
            ),
            (
                "explicit-v3",
                SignalFishConfig::new("app").enable_v3(),
                true,
            ),
            (
                "future-version",
                SignalFishConfig::new("app").with_protocol_version(4),
                true,
            ),
            (
                "endpoint-default-v3-shape",
                SignalFishConfig::new("app").with_transports([TransportKind::Relay]),
                true,
            ),
        ];

        for (name, config, expected) in cases {
            assert_eq!(config.requests_room_operation_ids(), expected, "{name}");
            let ClientCommand::Message(ClientMessage::Authenticate {
                requested_capabilities,
                ..
            }) = ClientCore::authenticate(&config)
            else {
                panic!("authenticate helper must return Authenticate")
            };
            assert_eq!(
                requested_capabilities,
                expected.then(|| vec![ROOM_OPERATION_IDS_CAPABILITY.to_string()]),
                "{name}"
            );
        }
    }

    #[tokio::test]
    #[cfg(feature = "mesh")]
    async fn controller_mesh_configuration_preserves_compatible_choices() {
        let cases = [
            (
                "default",
                SignalFishConfig::new("app"),
                crate::PROTOCOL_VERSION.max(3),
                vec![TransportKind::WebRtc, TransportKind::Relay],
                vec![Topology::Mesh, Topology::Host, Topology::Relay],
            ),
            (
                "relay-v3",
                SignalFishConfig::new("app").enable_v3(),
                crate::PROTOCOL_VERSION,
                vec![TransportKind::Relay, TransportKind::WebRtc],
                vec![Topology::Relay, Topology::Mesh],
            ),
            (
                "future-direct-host",
                SignalFishConfig::new("app")
                    .with_protocol_version(4)
                    .with_transports([TransportKind::Direct])
                    .with_topologies([Topology::Host]),
                4,
                vec![TransportKind::Direct, TransportKind::WebRtc],
                vec![Topology::Host],
            ),
            (
                "existing-webrtc-missing-p2p-topology",
                SignalFishConfig::new("app")
                    .with_protocol_version(3)
                    .with_transports([TransportKind::WebRtc, TransportKind::Relay])
                    .with_topologies([Topology::Relay]),
                3,
                vec![TransportKind::WebRtc, TransportKind::Relay],
                vec![Topology::Relay, Topology::Mesh],
            ),
            (
                "compatible-existing-webrtc-host",
                SignalFishConfig::new("app")
                    .with_protocol_version(3)
                    .with_transports([TransportKind::Direct, TransportKind::WebRtc])
                    .with_topologies([Topology::Host, Topology::Relay]),
                3,
                vec![TransportKind::Direct, TransportKind::WebRtc],
                vec![Topology::Host, Topology::Relay],
            ),
            (
                "pre-v3-custom",
                SignalFishConfig::new("app")
                    .with_protocol_version(2)
                    .with_transports([TransportKind::Relay])
                    .with_topologies([Topology::Mesh, Topology::Relay]),
                crate::PROTOCOL_VERSION.max(3),
                vec![TransportKind::Relay, TransportKind::WebRtc],
                vec![Topology::Mesh, Topology::Relay],
            ),
        ];

        for (name, config, protocol, transports, topologies) in cases {
            let config = config.enable_controller_mesh();
            assert_eq!(config.protocol_version, Some(protocol), "{name}");
            assert_eq!(config.supported_transports, Some(transports), "{name}");
            assert_eq!(config.supported_topologies, Some(topologies), "{name}");
            assert!(config.advertises_mesh_capability(), "{name}");
        }
    }

    #[tokio::test]
    async fn mesh_capability_requires_webrtc_and_a_p2p_topology() {
        let cases = [
            (vec![TransportKind::WebRtc], vec![Topology::Mesh], true),
            (vec![TransportKind::WebRtc], vec![Topology::Host], true),
            (vec![TransportKind::WebRtc], vec![Topology::Relay], false),
            (vec![TransportKind::Relay], vec![Topology::Mesh], false),
            (vec![], vec![Topology::Mesh], false),
            (vec![TransportKind::WebRtc], vec![], false),
        ];

        for (transports, topologies, expected) in cases {
            let config = SignalFishConfig::new("app")
                .with_transports(transports)
                .with_topologies(topologies);
            assert_eq!(config.advertises_mesh_capability(), expected);
        }
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
        incoming.push(Some(Ok(protocol_info_v2_json())));
        incoming.push(Some(Ok(room_joined_json())));
        let pong_json = serde_json::to_string(&ServerMessage::Pong).unwrap();
        for _ in 0..20 {
            incoming.push(Some(Ok(pong_json.clone())));
        }
        incoming.push(None);

        let (transport, _sent, _closed) = MockTransport::new(incoming);

        let config = SignalFishConfig::new("mb_test").with_event_channel_capacity(1);
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let mut received = Vec::new();
        for _ in 0..3 {
            received.push(
                events
                    .recv()
                    .await
                    .expect("connection prelude should remain lossless"),
            );
        }
        client
            .join_room(JoinRoomParams::new("test-game", "local"))
            .expect("scripted player join should be admitted");
        while let Some(event) = events.recv().await {
            received.push(event);
        }
        // Connection/room baseline + 20 Pongs + Disconnected — nothing dropped.
        assert_eq!(
            received.len(),
            25,
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
        incoming.push(Some(Ok(protocol_info_v2_json())));
        incoming.push(Some(Ok(room_joined_json())));
        for seq in 0..MESSAGES {
            let msg = ServerMessage::GameData {
                from_player: uuid::Uuid::from_u128(7),
                data: serde_json::json!({ "seq": seq }),
                seq: None,
                epoch: None,
                class: None,
                key: None,
            };
            incoming.push(Some(Ok(serde_json::to_string(&msg).unwrap())));
        }
        incoming.push(None);

        let (transport, _sent, _closed) = MockTransport::new(incoming);

        // Tiny event buffer: correctness must not depend on channel capacity.
        let config = SignalFishConfig::new("mb_test").with_event_channel_capacity(2);
        let (mut client, mut events) = SignalFishClient::start(transport, config);
        enter_scripted_player_room(&mut client, &mut events).await;

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
            seq: None,
            epoch: None,
            class: None,
            key: None,
        })
        .unwrap();
        let (transport, sent, _closed) = MockTransport::new(vec![
            Some(Ok(authenticated_json())),
            Some(Ok(protocol_info_v2_json())),
            Some(Ok(room_joined_json())),
            Some(Ok(game_data_json)),
        ]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        enter_scripted_player_room(&mut client, &mut events).await;

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
                seq: None,
                epoch: None,
                class: None,
                key: None,
            })
            .unwrap()
        };
        let (transport, sent, _closed) = MockTransport::new(vec![
            Some(Ok(authenticated_json())),
            Some(Ok(protocol_info_v2_json())),
            Some(Ok(room_joined_json())),
            Some(Ok(game_data_json(0))),
            Some(Ok(game_data_json(1))),
        ]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        assert_eq!(client.stats(), ClientStats::default());

        enter_scripted_player_room(&mut client, &mut events).await;
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
                messages_undecodable: 0,
            }
        );

        client.shutdown().await;
    }

    // ── Send-side backpressure (issue #47, item 2) ──────────────────

    /// Transport whose `poll_send` requires a semaphore permit per message, so
    /// tests can stall the outgoing path deterministically.
    type PermitWait = Pin<
        Box<
            dyn Future<
                    Output = std::result::Result<
                        tokio::sync::OwnedSemaphorePermit,
                        tokio::sync::AcquireError,
                    >,
                > + Send,
        >,
    >;

    struct GatedSendTransport {
        entered_send: Arc<AtomicBool>,
        permits: Arc<tokio::sync::Semaphore>,
        sent: Arc<StdMutex<Vec<String>>>,
        pending_frame: Option<TransportFrame>,
        permit_wait: Option<PermitWait>,
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
                    pending_frame: None,
                    permit_wait: None,
                },
                entered_send,
                permits,
                sent,
            )
        }
    }

    impl Transport for GatedSendTransport {
        fn abort(&mut self) {
            self.pending_frame = None;
            self.permit_wait = None;
        }

        fn poll_send(
            &mut self,
            cx: &mut Context<'_>,
            frame: &mut Option<TransportFrame>,
        ) -> Poll<std::result::Result<(), SignalFishError>> {
            if self.pending_frame.is_none() {
                let Some(accepted) = frame.take() else {
                    return Poll::Ready(Ok(()));
                };
                self.entered_send.store(true, Ordering::Release);
                self.pending_frame = Some(accepted);
                self.permit_wait = Some(Box::pin(Arc::clone(&self.permits).acquire_owned()));
            }

            let Some(permit_wait) = self.permit_wait.as_mut() else {
                return Poll::Ready(Err(SignalFishError::TransportClosed));
            };
            let permit = match permit_wait.as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(permit)) => permit,
                Poll::Ready(Err(_)) => {
                    self.pending_frame = None;
                    self.permit_wait = None;
                    return Poll::Ready(Err(SignalFishError::TransportClosed));
                }
            };
            permit.forget();
            self.permit_wait = None;
            match self.pending_frame.take() {
                Some(TransportFrame::Text(message)) => {
                    self.sent.lock().unwrap().push(message);
                    Poll::Ready(Ok(()))
                }
                Some(TransportFrame::Binary(_)) => Poll::Ready(Err(
                    SignalFishError::TransportSend("gated mock expected a text frame".into()),
                )),
                None => Poll::Ready(Ok(())),
            }
        }

        fn poll_recv(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<std::result::Result<TransportFrame, SignalFishError>>> {
            // No scripted messages and no registered waker: preserve the old
            // never-completing recv until shutdown aborts the loop.
            Poll::Pending
        }

        fn poll_close(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<std::result::Result<(), SignalFishError>> {
            Poll::Ready(Ok(()))
        }
    }

    #[derive(Clone)]
    struct PendingSendControls {
        entered_send: Arc<AtomicBool>,
        complete_send: Arc<AtomicBool>,
        peer_closed: Arc<AtomicBool>,
        close_called: Arc<AtomicBool>,
        abort_called: Arc<AtomicBool>,
        send_waker: Arc<StdMutex<Option<Waker>>>,
        recv_waker: Arc<StdMutex<Option<Waker>>>,
    }

    impl PendingSendControls {
        fn complete_send(&self) {
            self.complete_send.store(true, Ordering::Release);
            if let Some(waker) = self.send_waker.lock().unwrap().take() {
                waker.wake();
            }
        }

        fn close_peer(&self) {
            self.peer_closed.store(true, Ordering::Release);
            if let Some(waker) = self.recv_waker.lock().unwrap().take() {
                waker.wake();
            }
        }
    }

    /// A transport that retains an accepted frame until independently woken.
    /// Receive readiness is controlled separately, proving bidirectional
    /// progress while the backend owns an incomplete send.
    struct PendingSendTransport {
        retained_frame: Option<TransportFrame>,
        controls: PendingSendControls,
    }

    impl PendingSendTransport {
        fn new() -> (Self, PendingSendControls) {
            let controls = PendingSendControls {
                entered_send: Arc::new(AtomicBool::new(false)),
                complete_send: Arc::new(AtomicBool::new(false)),
                peer_closed: Arc::new(AtomicBool::new(false)),
                close_called: Arc::new(AtomicBool::new(false)),
                abort_called: Arc::new(AtomicBool::new(false)),
                send_waker: Arc::new(StdMutex::new(None)),
                recv_waker: Arc::new(StdMutex::new(None)),
            };
            (
                Self {
                    retained_frame: None,
                    controls: controls.clone(),
                },
                controls,
            )
        }
    }

    impl Transport for PendingSendTransport {
        fn poll_send(
            &mut self,
            cx: &mut Context<'_>,
            frame: &mut Option<TransportFrame>,
        ) -> Poll<std::result::Result<(), SignalFishError>> {
            if self.retained_frame.is_none() {
                self.retained_frame = frame.take();
                self.controls.entered_send.store(true, Ordering::Release);
            }
            if self.controls.complete_send.load(Ordering::Acquire) {
                self.retained_frame = None;
                Poll::Ready(Ok(()))
            } else {
                *self.controls.send_waker.lock().unwrap() = Some(cx.waker().clone());
                Poll::Pending
            }
        }

        fn poll_recv(
            &mut self,
            cx: &mut Context<'_>,
        ) -> Poll<Option<std::result::Result<TransportFrame, SignalFishError>>> {
            if self.controls.peer_closed.load(Ordering::Acquire) {
                Poll::Ready(None)
            } else {
                *self.controls.recv_waker.lock().unwrap() = Some(cx.waker().clone());
                Poll::Pending
            }
        }

        fn poll_close(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<std::result::Result<(), SignalFishError>> {
            assert!(
                self.retained_frame.is_none(),
                "backend-owned send must complete before close"
            );
            self.controls.close_called.store(true, Ordering::Release);
            Poll::Ready(Ok(()))
        }

        fn abort(&mut self) {
            self.retained_frame = None;
            let _ = self.controls.send_waker.lock().unwrap().take();
            let _ = self.controls.recv_waker.lock().unwrap().take();
            self.controls.abort_called.store(true, Ordering::Release);
        }
    }

    /// Models a WebSocket peer close discovered while an accepted send is
    /// still flushing. The close response needs another poll, so receive first
    /// returns `Pending`; meanwhile further sends are terminally refused.
    struct PeerCloseDuringSendTransport {
        send_accepted: bool,
        peer_close_observed: bool,
        close_called: Arc<AtomicBool>,
    }

    impl PeerCloseDuringSendTransport {
        fn new() -> (Self, Arc<AtomicBool>) {
            let close_called = Arc::new(AtomicBool::new(false));
            (
                Self {
                    send_accepted: false,
                    peer_close_observed: false,
                    close_called: Arc::clone(&close_called),
                },
                close_called,
            )
        }
    }

    impl Transport for PeerCloseDuringSendTransport {
        fn abort(&mut self) {
            self.send_accepted = false;
            self.peer_close_observed = true;
        }

        fn poll_send(
            &mut self,
            _cx: &mut Context<'_>,
            frame: &mut Option<TransportFrame>,
        ) -> Poll<std::result::Result<(), SignalFishError>> {
            if self.peer_close_observed {
                return Poll::Ready(Err(SignalFishError::TransportClosed));
            }
            if !self.send_accepted {
                assert!(frame.take().is_some());
                self.send_accepted = true;
            }
            Poll::Pending
        }

        fn poll_recv(
            &mut self,
            cx: &mut Context<'_>,
        ) -> Poll<Option<std::result::Result<TransportFrame, SignalFishError>>> {
            if self.send_accepted && !self.peer_close_observed {
                self.peer_close_observed = true;
                cx.waker().wake_by_ref();
            }
            Poll::Pending
        }

        fn poll_close(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<std::result::Result<(), SignalFishError>> {
            self.close_called.store(true, Ordering::Release);
            Poll::Ready(Ok(()))
        }

        fn close_info(&self) -> Option<crate::transport::TransportCloseInfo> {
            self.peer_close_observed
                .then(|| crate::transport::TransportCloseInfo {
                    code: Some(1000),
                    reason: Some("normal closure".into()),
                    clean: Some(true),
                    initiated_by_peer: true,
                })
        }
    }

    #[tokio::test]
    async fn peer_close_during_pending_send_uses_close_metadata() {
        let (transport, close_called) = PeerCloseDuringSendTransport::new();
        let (mut client, mut events) =
            SignalFishClient::start(transport, SignalFishConfig::new("app"));

        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Connected)
        ));
        let terminal = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("peer close must surface")
            .expect("terminal event channel must remain open");
        assert!(matches!(
            terminal,
            SignalFishEvent::Disconnected { reason, .. }
                if reason.as_deref()
                    == Some("closed by server: code=Some(1000), reason=Some(\"normal closure\")")
        ));
        wait_until(|| close_called.load(Ordering::Acquire)).await;

        client.shutdown().await;
    }

    #[tokio::test]
    async fn pending_send_does_not_hide_peer_close() {
        let (transport, controls) = PendingSendTransport::new();
        let (mut client, mut events) =
            SignalFishClient::start(transport, SignalFishConfig::new("app"));

        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Connected)
        ));
        wait_until(|| controls.entered_send.load(Ordering::Acquire)).await;
        controls.close_peer();
        let terminal = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("peer close must surface while send remains pending")
            .expect("terminal event channel must remain open");
        assert!(matches!(terminal, SignalFishEvent::Disconnected { .. }));
        assert!(!client.is_connected());
        assert!(!controls.close_called.load(Ordering::Acquire));

        controls.complete_send();
        wait_until(|| controls.close_called.load(Ordering::Acquire)).await;

        client.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_preempts_pending_send_and_attempts_close() {
        let (transport, controls) = PendingSendTransport::new();
        let config = SignalFishConfig::new("app").with_shutdown_timeout(Duration::from_secs(1));
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Connected)
        ));
        wait_until(|| controls.entered_send.load(Ordering::Acquire)).await;
        let completion = controls.clone();
        let complete = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            completion.complete_send();
        });
        tokio::time::timeout(Duration::from_millis(250), client.shutdown())
            .await
            .expect("shutdown must preempt a pending send");
        complete.await.unwrap();

        assert!(controls.close_called.load(Ordering::Acquire));
        assert!(!controls.abort_called.load(Ordering::Acquire));
        assert!(!client.is_connected());
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
        prime_player_room(&client);
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
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        wait_until(|| entered_send.load(Ordering::Acquire)).await;
        prime_player_room(&client);

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

        // All three messages reach the wire: Authenticate + both game data payloads.
        wait_for_sent_len(&sent, 3).await;

        let mut client = Arc::into_inner(client).expect("all clones dropped");
        client.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reliable_send_revalidates_admission_after_reserving_capacity() {
        let (transport, entered_send, permits, _sent) = GatedSendTransport::new(0);
        let config = SignalFishConfig::new("mb_test").with_command_channel_capacity(1);
        let (client, mut events) = SignalFishClient::start(transport, config);

        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Connected)
        ));
        wait_until(|| entered_send.load(Ordering::Acquire)).await;
        prime_player_room(&client);

        let reserved = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let client = Arc::new(client);
        let sender = Arc::clone(&client);
        let reserved_by_sender = Arc::clone(&reserved);
        let release_sender = Arc::clone(&release);
        let send = tokio::spawn(async move {
            sender
                .send_operation_reliable_after_reserve(
                    ClientOperation::GameData(
                        serde_json::json!({ "reserved": true }),
                        GameDataDelivery::Reliable,
                    ),
                    || {
                        reserved_by_sender.wait();
                        release_sender.wait();
                    },
                )
                .await
        });

        reserved.wait();
        lock_core(&client.state).freeze_admission();
        release.wait();
        let result = send.await.expect("reliable-send task must not panic");
        assert!(matches!(result, Err(SignalFishError::NotConnected)));

        permits.add_permits(1);
        let mut client = Arc::into_inner(client).expect("all client clones must be dropped");
        client.shutdown().await;
    }

    #[tokio::test]
    async fn reliable_game_data_reports_disconnect_after_waiting_for_capacity() {
        let (transport, entered_send, permits, _) = GatedSendTransport::new(0);
        let config = SignalFishConfig::new("mb_test").with_command_channel_capacity(1);
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Connected)
        ));
        wait_until(|| entered_send.load(Ordering::Acquire)).await;
        prime_player_room(&client);

        client
            .send_game_data(serde_json::json!({ "fills": "queue" }))
            .expect("capacity-1 queue should accept one filler command");
        let client = Arc::new(client);
        let sender = Arc::clone(&client);
        let mut reliable = tokio::spawn(async move {
            sender
                .send_game_data_reliable(serde_json::json!({ "waited": true }))
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut reliable)
                .await
                .is_err(),
            "reliable game data should wait for queue capacity"
        );

        lock_core(&client.state).disconnect(None);
        permits.add_permits(16);
        let result = tokio::time::timeout(Duration::from_secs(1), reliable)
            .await
            .expect("reliable game data should finish after capacity opens")
            .expect("reliable game data task should not panic");
        assert!(
            matches!(result, Err(SignalFishError::NotConnected)),
            "disconnect should remain authoritative over room binding: {result:?}"
        );

        let mut client = Arc::into_inner(client).expect("all client clones should be dropped");
        client.shutdown().await;
    }

    #[tokio::test]
    async fn reliable_json_game_data_does_not_cross_room_membership() {
        assert_reliable_game_data_rejected_after_room_change(false, true).await;
    }

    /// Pins the cancellation contract of the waiting sends: dropping a future
    /// parked on queue capacity leaves no state mutation, no statistics
    /// change, and no leaked queue permit, so an identical command still
    /// completes afterwards.
    #[tokio::test]
    async fn dropping_a_parked_reliable_send_leaves_no_trace() {
        let (transport, entered_send, permits, sent) = GatedSendTransport::new(0);
        let config = SignalFishConfig::new("mb_test").with_command_channel_capacity(1);
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Connected)
        ));
        wait_until(|| entered_send.load(Ordering::Acquire)).await;
        prime_player_room(&client);

        client
            .send_game_data(serde_json::json!({ "fills": "queue" }))
            .expect("capacity-1 queue should accept one filler command");
        let client = Arc::new(client);
        let sender = Arc::clone(&client);
        let mut parked = Box::pin(async move {
            sender
                .send_game_data_reliable(serde_json::json!({ "cancelled": true }))
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), parked.as_mut())
                .await
                .is_err(),
            "the reliable send should be parked on queue capacity"
        );

        // Drop mid-wait: nothing may change.
        let stats_before = client.stats();
        let snapshot_before = client.snapshot();
        drop(parked);
        assert_eq!(client.stats(), stats_before, "stats must not move");
        assert_eq!(client.snapshot(), snapshot_before, "snapshot must not move");
        assert_eq!(client.send_capacity(), 0, "the filler still owns the slot");

        // The same command still works once capacity is available again.
        permits.add_permits(16);
        wait_for_sent_len(&sent, 2).await;
        assert_eq!(
            client.snapshot(),
            snapshot_before,
            "draining the filler must not disturb membership state"
        );
        client
            .send_game_data_reliable(serde_json::json!({ "identical": true }))
            .await
            .expect("an identical reliable send must complete after cancellation");
        wait_for_sent_len(&sent, 3).await;
        {
            let payloads = sent.lock().unwrap();
            assert!(
                !payloads.iter().any(|json| json.contains("\"cancelled\"")),
                "the cancelled payload must not reach the wire: {payloads:?}"
            );
            assert!(
                payloads.iter().any(|json| json.contains("\"identical\"")),
                "the identical payload must reach the wire: {payloads:?}"
            );
        }

        let mut client = Arc::into_inner(client).expect("all client clones should be dropped");
        client.shutdown().await;
    }

    #[tokio::test]
    async fn reliable_binary_game_data_does_not_cross_room_membership() {
        assert_reliable_game_data_rejected_after_room_change(true, true).await;
    }

    #[tokio::test]
    async fn reliable_json_game_data_does_not_cross_initial_room_join() {
        assert_reliable_game_data_rejected_after_room_change(false, false).await;
    }

    #[tokio::test]
    async fn reliable_binary_game_data_does_not_cross_initial_room_join() {
        assert_reliable_game_data_rejected_after_room_change(true, false).await;
    }

    async fn assert_reliable_game_data_rejected_after_room_change(
        binary: bool,
        initially_in_room: bool,
    ) {
        let (transport, entered_send, permits, sent) = GatedSendTransport::new(0);
        let mut config = SignalFishConfig::new("mb_test")
            .enable_v3()
            .with_command_channel_capacity(1);
        if binary {
            config.game_data_format = Some(GameDataEncoding::MessagePack);
        }
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Connected)
        ));
        wait_until(|| entered_send.load(Ordering::Acquire)).await;

        let joined = |room_id, player_id| {
            ServerMessage::RoomJoined(Box::new(RoomJoinedPayload {
                room_id,
                room_code: "ROOM".into(),
                player_id,
                game_name: "game".into(),
                max_players: 2,
                supports_authority: false,
                current_players: vec![crate::protocol::PlayerInfo {
                    id: player_id,
                    name: "local".into(),
                    is_authority: false,
                    is_ready: false,
                    connected_at: "2026-01-01T00:00:00Z".into(),
                    connection_info: None,
                    epoch: Some(1),
                    seq: Some(0),
                }],
                is_authority: false,
                lobby_state: LobbyState::Waiting,
                ready_players: vec![],
                relay_type: "relay".into(),
                current_spectators: vec![],
                ice_servers: vec![],
                reconnection_token: None,
            }))
        };
        {
            let mut core = lock_core(&client.state);
            let _ = core.process_frame(TransportFrame::Text(authenticated_json()));
            let _ = core.process_frame(TransportFrame::Text(protocol_info_v3_json()));
            if initially_in_room {
                core.record_admission(ClientCore::admission_for(&ClientOperation::JoinRoom(
                    JoinRoomParams::new("game", "local"),
                )));
                let room_a = serde_json::to_string(&joined(
                    uuid::Uuid::from_u128(100),
                    uuid::Uuid::from_u128(101),
                ))
                .unwrap();
                let _ = core.process_frame(TransportFrame::Text(room_a));
            }
        }

        if !initially_in_room {
            let result = if binary {
                client
                    .send_binary_game_data_reliable(b"pre-room-binary".to_vec())
                    .await
            } else {
                client
                    .send_game_data_reliable(serde_json::json!({ "value": "pre-room-json" }))
                    .await
            };
            assert!(
                matches!(result, Err(SignalFishError::NotInRoom)),
                "pre-room game data must fail locally: {result:?}"
            );
            permits.add_permits(16);
            wait_for_sent_len(&sent, 1).await;
            assert_eq!(sent.lock().unwrap().len(), 1);
            client.shutdown().await;
            return;
        }

        client
            .send_game_data(serde_json::json!({ "fills": "queue" }))
            .expect("capacity-1 queue should accept one filler command");
        let client = Arc::new(client);
        let sender = Arc::clone(&client);
        let mut reliable = tokio::spawn(async move {
            if binary {
                sender
                    .send_binary_game_data_reliable(b"stale-room-binary".to_vec())
                    .await
            } else {
                sender
                    .send_game_data_reliable(serde_json::json!({
                        "value": "stale-room-json"
                    }))
                    .await
            }
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut reliable)
                .await
                .is_err(),
            "reliable game data should wait for queue capacity"
        );

        {
            let mut core = lock_core(&client.state);
            if initially_in_room {
                core.record_admission(ClientCore::admission_for(&ClientOperation::LeaveRoom));
                let room_left = serde_json::to_string(&ServerMessage::RoomLeft).unwrap();
                let _ = core.process_frame(TransportFrame::Text(room_left));
            }
            core.record_admission(ClientCore::admission_for(&ClientOperation::JoinRoom(
                JoinRoomParams::new("game", "local"),
            )));
            let room_b = serde_json::to_string(&joined(
                uuid::Uuid::from_u128(200),
                uuid::Uuid::from_u128(201),
            ))
            .unwrap();
            let _ = core.process_frame(TransportFrame::Text(room_b));
        }
        permits.add_permits(16);
        let result = tokio::time::timeout(Duration::from_secs(1), reliable)
            .await
            .expect("reliable game data should finish after capacity opens")
            .expect("reliable game data task should not panic");
        assert!(
            matches!(result, Err(SignalFishError::NotInRoom)),
            "room-A data must be rejected after joining room B: {result:?}"
        );
        wait_for_sent_len(&sent, 2).await;
        {
            let messages = sent.lock().unwrap();
            assert!(
                messages.iter().all(|message| {
                    !message.contains("stale-room-json") && !message.contains("stale-room-binary")
                }),
                "stale room data must never reach the transport"
            );
        }

        let mut client = Arc::into_inner(client).expect("all client clones should be dropped");
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
            game_data_formats: vec![GameDataEncoding::Json, GameDataEncoding::MessagePack],
            player_name_rules: None,
            protocol_version: Some(3),
            min_protocol_version: Some(2),
            max_protocol_version: Some(3),
            transports: Some(vec![crate::protocol::MessageTransport::Websocket]),
            max_outbound_message_size: Some(8 * 1024 * 1024),
        }))
        .unwrap()
    }

    fn session_plan_v3_json(peer: PlayerId) -> String {
        use crate::protocol::{SessionPeer, SessionPlanPayload, Topology, TransportKind};
        serde_json::to_string(&ServerMessage::SessionPlan(Box::new(SessionPlanPayload {
            generation: Some(uuid::Uuid::from_u128(12)),
            topology: Topology::Mesh,
            transport: TransportKind::WebRtc,
            host: None,
            direct_endpoint: None,
            peers: vec![SessionPeer {
                player_id: peer,
                player_name: "peer".into(),
                is_authority: false,
                initiate: true,
            }],
            ice_servers: vec![],
            fallback: TransportKind::Relay,
        })))
        .unwrap()
    }

    fn finalized_room_v3_json(peer: PlayerId) -> String {
        let local = uuid::Uuid::from_u128(42);
        let player = |id, name: &str| crate::protocol::PlayerInfo {
            id,
            name: name.into(),
            is_authority: id == local,
            is_ready: true,
            connected_at: "2026-01-01T00:00:00Z".into(),
            connection_info: None,
            epoch: Some(1),
            seq: Some(0),
        };
        serde_json::to_string(&ServerMessage::RoomJoined(Box::new(RoomJoinedPayload {
            room_id: uuid::Uuid::from_u128(41),
            room_code: "V3ROOM".into(),
            player_id: local,
            game_name: "test-game".into(),
            max_players: 4,
            supports_authority: true,
            current_players: vec![player(local, "local"), player(peer, "peer")],
            is_authority: true,
            lobby_state: LobbyState::Finalized,
            ready_players: vec![local, peer],
            relay_type: "websocket".into(),
            current_spectators: vec![],
            ice_servers: vec![],
            reconnection_token: None,
        })))
        .unwrap()
    }

    #[tokio::test]
    async fn async_binary_send_requires_a_negotiated_binary_format() {
        let (transport, _sent, _closed) = MockTransport::new(vec![
            Some(Ok(authenticated_json())),
            Some(Ok(protocol_info_v3_json())),
            Some(Ok(finalized_room_v3_json(uuid::Uuid::from_u128(7)))),
        ]);
        let (mut client, mut events) =
            SignalFishClient::start(transport, SignalFishConfig::new("mb_test").enable_v3());
        enter_scripted_player_room(&mut client, &mut events).await;

        assert!(matches!(
            client.send_binary_game_data(vec![1, 2, 3]),
            Err(SignalFishError::BinaryFormatNotNegotiated)
        ));
        client.shutdown().await;
    }

    #[tokio::test]
    async fn async_quarantine_suppresses_invalid_lifecycle_event() {
        let peer = uuid::Uuid::from_u128(400);
        let room = ServerMessage::RoomJoined(Box::new(RoomJoinedPayload {
            room_id: uuid::Uuid::from_u128(401),
            room_code: "ROOM".into(),
            player_id: uuid::Uuid::from_u128(402),
            game_name: "test".into(),
            max_players: 2,
            supports_authority: false,
            current_players: vec![
                crate::protocol::PlayerInfo {
                    id: peer,
                    name: "peer".into(),
                    is_authority: false,
                    is_ready: false,
                    connected_at: "2026-01-01T00:00:00Z".into(),
                    connection_info: None,
                    epoch: Some(1),
                    seq: Some(0),
                },
                crate::protocol::PlayerInfo {
                    id: uuid::Uuid::from_u128(402),
                    name: "local".into(),
                    is_authority: false,
                    is_ready: false,
                    connected_at: "2026-01-01T00:00:00Z".into(),
                    connection_info: None,
                    epoch: Some(1),
                    seq: Some(0),
                },
            ],
            is_authority: false,
            lobby_state: LobbyState::Lobby,
            ready_players: vec![],
            relay_type: "websocket".into(),
            current_spectators: vec![],
            ice_servers: vec![],
            reconnection_token: None,
        }));
        let invalid = ServerMessage::PlayerLeft {
            player_id: peer,
            epoch: None,
            final_seq: None,
        };
        let (transport, _sent, _closed) = MockTransport::new(vec![
            Some(Ok(authenticated_json())),
            Some(Ok(protocol_info_v3_json())),
            Some(Ok(serde_json::to_string(&room).unwrap())),
            Some(Ok(serde_json::to_string(&invalid).unwrap())),
        ]);
        let (mut client, mut events) =
            SignalFishClient::start(transport, SignalFishConfig::new("mb_test").enable_v3());
        enter_scripted_player_room(&mut client, &mut events).await;
        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::ProtocolViolation { .. })
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(25), events.recv())
                .await
                .is_err()
        );
        assert!(client.snapshot().quarantined);
        client.shutdown().await;
    }

    #[tokio::test]
    async fn send_signal_reliable_fails_fast_outside_room_even_when_queue_full() {
        // Saturate the capacity-1 command queue behind a stalled transport.
        let (transport, entered_send, permits, _sent) = GatedSendTransport::new(0);
        let config = SignalFishConfig::new("mb_test").with_command_channel_capacity(1);
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        wait_until(|| entered_send.load(Ordering::Acquire)).await;
        client.ping().unwrap();
        assert_eq!(client.send_capacity(), 0);

        // Membership must be evaluated BEFORE waiting for queue capacity:
        // outside a room this returns immediately (nothing is queued)
        // instead of blocking on the full queue.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client.send_signal_reliable(uuid::Uuid::from_u128(5), PeerSignal::Offer("sdp".into())),
        )
        .await
        .expect("guard must fail fast, not wait for capacity");
        assert!(
            matches!(result, Err(SignalFishError::NotInRoom)),
            "expected NotInRoom, got {result:?}"
        );

        permits.add_permits(16);
        client.shutdown().await;
    }

    #[tokio::test]
    async fn reliable_signal_revalidates_generation_after_waiting_for_capacity() {
        assert_waiting_signal_rejected_after_replan(
            Some(uuid::Uuid::from_u128(12)),
            Some(uuid::Uuid::from_u128(13)),
            true,
            false,
        )
        .await;
    }

    #[tokio::test]
    async fn reliable_signal_revalidates_generationless_plan_revision() {
        assert_waiting_signal_rejected_after_replan(None, None, true, false).await;
    }

    #[tokio::test]
    async fn reliable_signal_accepts_reasserted_generation() {
        let generation = Some(uuid::Uuid::from_u128(12));
        assert_waiting_signal_rejected_after_replan(generation, generation, false, false).await;
    }

    #[tokio::test]
    async fn reliable_signal_revalidates_peer_departure_while_waiting_for_capacity() {
        let generation = Some(uuid::Uuid::from_u128(12));
        assert_waiting_signal_rejected_after_replan(generation, generation, true, true).await;
    }

    async fn assert_waiting_signal_rejected_after_replan(
        original_generation: Option<SessionGeneration>,
        replacement_generation: Option<SessionGeneration>,
        should_reject: bool,
        remove_peer: bool,
    ) {
        use crate::protocol::{SessionPeer, SessionPlanPayload, Topology, TransportKind};

        let peer = uuid::Uuid::from_u128(5);
        let (transport, entered_send, permits, sent) = GatedSendTransport::new(0);
        let config = SignalFishConfig::new("mb_test")
            .enable_mesh()
            .with_command_channel_capacity(1);
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Connected)
        ));
        wait_until(|| entered_send.load(Ordering::Acquire)).await;

        let plan = |generation| {
            ServerMessage::SessionPlan(Box::new(SessionPlanPayload {
                generation,
                topology: Topology::Mesh,
                transport: TransportKind::WebRtc,
                host: None,
                direct_endpoint: None,
                fallback: TransportKind::Relay,
                peers: vec![SessionPeer {
                    player_id: peer,
                    player_name: "peer".into(),
                    is_authority: false,
                    initiate: true,
                }],
                ice_servers: vec![],
            }))
        };
        {
            let mut core = lock_core(&client.state);
            let _ = core.process_frame(TransportFrame::Text(authenticated_json()));
            let _ = core.process_frame(TransportFrame::Text(protocol_info_v3_json()));
            core.record_admission(ClientCore::admission_for(&ClientOperation::JoinRoom(
                JoinRoomParams::new("test-game", "local"),
            )));
            let _ = core.process_frame(TransportFrame::Text(finalized_room_v3_json(peer)));
            let original = serde_json::to_string(&plan(original_generation))
                .expect("original SessionPlan should serialize");
            let _ = core.process_frame(TransportFrame::Text(original));
        }

        client
            .send_game_data(serde_json::json!({ "fills": "queue" }))
            .expect("capacity-1 queue should accept one filler command");
        let client = Arc::new(client);
        let sender = Arc::clone(&client);
        let mut reliable = tokio::spawn(async move {
            sender
                .send_signal_reliable(peer, PeerSignal::Offer("fresh-sdp".into()))
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut reliable)
                .await
                .is_err(),
            "reliable signal should still be waiting for queue capacity"
        );

        {
            let mut core = lock_core(&client.state);
            if remove_peer {
                let left = serde_json::to_string(&ServerMessage::PlayerLeft {
                    player_id: peer,
                    epoch: Some(1),
                    final_seq: Some(0),
                })
                .expect("PlayerLeft should serialize");
                let _ = core.process_frame(TransportFrame::Text(left));
            } else {
                let replacement = serde_json::to_string(&plan(replacement_generation))
                    .expect("replacement SessionPlan should serialize");
                let _ = core.process_frame(TransportFrame::Text(replacement));
            }
        }
        permits.add_permits(16);
        let result = tokio::time::timeout(Duration::from_secs(1), reliable)
            .await
            .expect("reliable signal should finish after capacity is available")
            .expect("reliable signal task should not panic");
        if remove_peer {
            assert!(
                matches!(result, Err(SignalFishError::SessionPlanUnavailable)),
                "revalidation should reject a departed peer: {result:?}"
            );
        } else if should_reject {
            assert!(
                matches!(
                    result,
                    Err(SignalFishError::StaleSessionGeneration {
                        attempted,
                        current,
                    }) if attempted == original_generation && current == replacement_generation
                ),
                "revalidation should report the captured and replacement generations: {result:?}"
            );
        } else {
            assert!(
                result.is_ok(),
                "an idempotent plan reassertion is valid: {result:?}"
            );
        }
        let expected_messages = if should_reject { 2 } else { 3 };
        wait_for_sent_len(&sent, expected_messages).await;

        let signal_count = {
            let messages = sent.lock().unwrap();
            messages
                .iter()
                .filter(|message| {
                    matches!(
                        serde_json::from_str::<ClientMessage>(message),
                        Ok(ClientMessage::Signal { .. })
                    )
                })
                .count()
        };
        assert_eq!(
            signal_count,
            usize::from(!should_reject),
            "only a signal for the current logical plan may reach the wire"
        );

        let mut client = Arc::into_inner(client).expect("all client clones should be dropped");
        client.shutdown().await;
    }

    #[tokio::test]
    async fn send_signal_reliable_reaches_wire_after_v3() {
        let (transport, sent, _closed) = MockTransport::new(vec![
            Some(Ok(authenticated_json())),
            Some(Ok(protocol_info_v3_json())),
            Some(Ok(finalized_room_v3_json(uuid::Uuid::from_u128(5)))),
            Some(Ok(session_plan_v3_json(uuid::Uuid::from_u128(5)))),
        ]);

        let config = SignalFishConfig::new("mb_test").enable_mesh();
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        enter_scripted_player_room(&mut client, &mut events).await;
        let _ = events.recv().await; // SessionPlan (establishes generation)

        client
            .send_signal_reliable(uuid::Uuid::from_u128(5), PeerSignal::Offer("sdp".into()))
            .await
            .unwrap();

        wait_for_sent_len(&sent, 3).await;
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

    /// Transport whose `poll_close` remains pending so deadline abort can be tested.
    struct HangingCloseTransport {
        incoming: VecDeque<Option<std::result::Result<String, SignalFishError>>>,
        close_called: Arc<AtomicBool>,
        abort_called: Arc<AtomicBool>,
        dropped: Arc<AtomicBool>,
    }

    impl HangingCloseTransport {
        fn new() -> (Self, Arc<AtomicBool>, Arc<AtomicBool>, Arc<AtomicBool>) {
            Self::with_incoming(Vec::new())
        }

        fn with_incoming(
            incoming: Vec<Option<std::result::Result<String, SignalFishError>>>,
        ) -> (Self, Arc<AtomicBool>, Arc<AtomicBool>, Arc<AtomicBool>) {
            let close_called = Arc::new(AtomicBool::new(false));
            let abort_called = Arc::new(AtomicBool::new(false));
            let dropped = Arc::new(AtomicBool::new(false));
            (
                Self {
                    incoming: VecDeque::from(incoming),
                    close_called: Arc::clone(&close_called),
                    abort_called: Arc::clone(&abort_called),
                    dropped: Arc::clone(&dropped),
                },
                close_called,
                abort_called,
                dropped,
            )
        }
    }

    impl Drop for HangingCloseTransport {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }

    impl Transport for HangingCloseTransport {
        fn poll_send(
            &mut self,
            _cx: &mut Context<'_>,
            frame: &mut Option<TransportFrame>,
        ) -> Poll<std::result::Result<(), SignalFishError>> {
            let _ = frame.take();
            Poll::Ready(Ok(()))
        }

        fn poll_recv(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<std::result::Result<TransportFrame, SignalFishError>>> {
            if let Some(item) = self.incoming.pop_front() {
                Poll::Ready(item.map(|result| result.map(TransportFrame::Text)))
            } else {
                // No scripted messages and no registered waker: preserve the
                // pending receive until shutdown preempts it and closes.
                Poll::Pending
            }
        }

        fn poll_close(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<std::result::Result<(), SignalFishError>> {
            self.close_called.store(true, Ordering::Release);
            // Simulate a close that never completes, so the
            // shutdown timeout/abort path can be exercised.
            Poll::Pending
        }

        fn abort(&mut self) {
            self.abort_called.store(true, Ordering::Release);
        }
    }

    struct DirectDeadlineTransport {
        hang_accepted_send: bool,
        close_called: Arc<AtomicBool>,
        abort_called: Arc<AtomicBool>,
    }

    impl Transport for DirectDeadlineTransport {
        fn poll_send(
            &mut self,
            _cx: &mut Context<'_>,
            frame: &mut Option<TransportFrame>,
        ) -> Poll<std::result::Result<(), SignalFishError>> {
            assert!(frame.is_none(), "the send must already be backend-owned");
            if self.hang_accepted_send {
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        }

        fn poll_recv(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<std::result::Result<TransportFrame, SignalFishError>>> {
            Poll::Pending
        }

        fn poll_close(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<std::result::Result<(), SignalFishError>> {
            self.close_called.store(true, Ordering::Release);
            Poll::Pending
        }

        fn abort(&mut self) {
            self.abort_called.store(true, Ordering::Release);
        }
    }

    #[tokio::test]
    async fn configured_deadline_aborts_before_transport_guard_drop() {
        for hang_accepted_send in [true, false] {
            let close_called = Arc::new(AtomicBool::new(false));
            let abort_called = Arc::new(AtomicBool::new(false));
            let mut transport = AbortOnDropTransport::new(DirectDeadlineTransport {
                hang_accepted_send,
                close_called: Arc::clone(&close_called),
                abort_called: Arc::clone(&abort_called),
            });
            let mut pending_send = hang_accepted_send.then_some(PendingSend {
                frame: None,
                is_game_data: false,
            });
            let state = Arc::new(Mutex::new(ClientCore::new(
                None,
                ProtocolViolationPolicy::Quarantine,
                false,
            )));

            tokio::time::timeout(
                Duration::from_millis(100),
                finish_send_and_close_bounded(
                    &mut transport,
                    &mut pending_send,
                    &state,
                    Duration::from_millis(5),
                ),
            )
            .await
            .expect("the configured inner deadline must finish before the test watchdog");

            assert!(
                abort_called.load(Ordering::Acquire),
                "the helper itself must abort before its owning guard is dropped"
            );
            assert_eq!(
                close_called.load(Ordering::Acquire),
                !hang_accepted_send,
                "close starts only after a backend-owned send completes"
            );
            assert!(pending_send.is_none());
            assert!(!transport.armed);
        }
    }

    #[tokio::test]
    async fn shutdown_timeout_aborts_stuck_transport() {
        let (transport, close_called, abort_called, dropped) = HangingCloseTransport::new();
        let config = SignalFishConfig::new("mb_test")
            .with_shutdown_timeout(std::time::Duration::from_millis(20));
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        // Drain Connected so the channel remains uncongested.
        let event = events.recv().await.unwrap();
        assert!(matches!(event, SignalFishEvent::Connected));

        client.shutdown().await;

        assert!(
            close_called.load(Ordering::Acquire),
            "transport.poll_close should have been attempted during graceful shutdown"
        );
        assert!(
            abort_called.load(Ordering::Acquire),
            "the transport loop should invoke abort after its close deadline"
        );
        assert!(
            dropped.load(Ordering::Acquire),
            "deadline-aborted shutdown must drop the transport when its loop returns"
        );
        assert!(!client.is_connected());
    }

    #[tokio::test]
    async fn terminal_event_precedes_a_hanging_transport_close() {
        let (transport, close_called, abort_called, dropped) =
            HangingCloseTransport::with_incoming(vec![None]);
        let config = SignalFishConfig::new("mb_test")
            .with_shutdown_timeout(std::time::Duration::from_millis(100));
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Connected)
        ));
        let terminal = tokio::time::timeout(Duration::from_millis(50), events.recv())
            .await
            .expect("terminal event must not wait for graceful close")
            .expect("terminal event channel must remain open");
        assert!(matches!(terminal, SignalFishEvent::Disconnected { .. }));
        assert!(!client.is_connected());
        assert!(close_called.load(Ordering::Acquire));

        client.shutdown().await;
        assert!(abort_called.load(Ordering::Acquire));
        assert!(dropped.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn terminal_close_progresses_while_event_channel_is_full() {
        let (transport, _sent, closed) = MockTransport::new(vec![None]);
        let config = SignalFishConfig::new("mb_test").with_event_channel_capacity(1);
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        // Connected occupies the only event slot. The terminal event cannot be
        // admitted yet, but transport close must still progress independently.
        wait_until(|| closed.load(Ordering::Acquire)).await;
        assert!(!client.is_connected());
        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Connected)
        ));
        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Disconnected { .. })
        ));

        client.shutdown().await;
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
            Some(Ok(protocol_info_v2_json())),
            Some(Ok(room_joined_json())),
            Some(Ok(room_left_json)),
        ]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        enter_scripted_player_room(&mut client, &mut events).await;
        client
            .leave_room()
            .expect("scripted player leave should be admitted");
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
        if let SignalFishEvent::Disconnected { reason, .. } = event {
            assert!(reason.unwrap().contains("boom"));
        }

        client.shutdown().await;
    }

    #[tokio::test]
    async fn leave_room_sends_message() {
        let (transport, sent, _closed) = MockTransport::new(vec![
            Some(Ok(authenticated_json())),
            Some(Ok(protocol_info_v2_json())),
            Some(Ok(room_joined_json())),
        ]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        enter_scripted_player_room(&mut client, &mut events).await;
        client.leave_room().unwrap();

        wait_for_sent_len(&sent, 3).await;

        {
            let messages = sent.lock().unwrap();
            let last: ClientMessage = serde_json::from_str(messages.last().unwrap()).unwrap();
            assert!(matches!(last, ClientMessage::LeaveRoom));
        }

        client.shutdown().await;
    }

    #[tokio::test]
    async fn set_ready_sends_player_ready() {
        let (transport, sent, _closed) = MockTransport::new(vec![
            Some(Ok(authenticated_json())),
            Some(Ok(protocol_info_v2_json())),
            Some(Ok(room_joined_json())),
        ]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        enter_scripted_player_room(&mut client, &mut events).await;
        client.set_ready().unwrap();

        wait_for_sent_len(&sent, 3).await;

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
        incoming.push(Some(Ok(protocol_info_v2_json())));
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
        // Connected + Authenticated + ProtocolInfo + pongs + Disconnected.
        assert_eq!(
            count,
            pongs + 4,
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
        if let SignalFishEvent::Disconnected { reason, .. } = event {
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
        let (transport, sent, _closed) = MockTransport::new(vec![
            Some(Ok(authenticated_json())),
            Some(Ok(protocol_info_v2_json())),
            Some(Ok(room_joined_json())),
        ]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        enter_scripted_player_room(&mut client, &mut events).await;

        let data = serde_json::json!({ "action": "move", "x": 10, "y": 20 });
        client.send_game_data(data.clone()).unwrap();

        wait_for_sent_len(&sent, 3).await;

        {
            let messages = sent.lock().unwrap();
            assert!(messages.len() >= 2);
            let last: ClientMessage = serde_json::from_str(messages.last().unwrap()).unwrap();
            if let ClientMessage::GameData {
                data: sent_data, ..
            } = last
            {
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
            current_players: vec![crate::protocol::PlayerInfo {
                id: uuid::Uuid::from_u128(200),
                name: "local".into(),
                is_authority: true,
                is_ready: false,
                connected_at: "2026-01-01T00:00:00Z".into(),
                connection_info: None,
                epoch: None,
                seq: None,
            }],
            is_authority: true,
            lobby_state: LobbyState::Waiting,
            ready_players: vec![],
            relay_type: "tcp".into(),
            current_spectators: vec![],
            ice_servers: vec![],
            missed_events: vec![],
            replay: None,
            sender_watermarks: vec![],
            reconnection_token: None,
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
            Some(Ok(protocol_info_v2_json())),
            Some(Ok(reconnected_json())),
        ]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated
        let _ = events.recv().await; // ProtocolInfo
        client
            .reconnect(
                uuid::Uuid::from_u128(200),
                uuid::Uuid::from_u128(100),
                "submitted-token".into(),
            )
            .unwrap();
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
            Some(Ok(protocol_info_v2_json())),
            Some(Ok(spectator_joined_json())),
        ]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        enter_scripted_spectator_room(&mut client, &mut events).await;

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
            Some(Ok(protocol_info_v2_json())),
            Some(Ok(spectator_joined_json())),
            Some(Ok(spectator_left_json())),
        ]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        enter_scripted_spectator_room(&mut client, &mut events).await;
        client
            .leave_spectator()
            .expect("scripted spectator leave should be admitted");
        let ev = events.recv().await.unwrap(); // SpectatorLeft
        assert!(matches!(ev, SignalFishEvent::SpectatorLeft { .. }));

        assert!(client.current_room_id().await.is_none());
        assert!(client.current_room_code().await.is_none());

        client.shutdown().await;
    }

    #[tokio::test]
    async fn room_operations_are_refused_before_authentication_completes() {
        // The server never confirms authentication. A caller that skips the
        // documented wait for `Authenticated` must get an immediate typed
        // refusal instead of arming an operation fence that the inbound
        // lifecycle gates would never release.
        let (transport, sent, _closed) = MockTransport::new(vec![]);
        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);
        let _ = events.recv().await; // Connected

        let result = client.join_room(JoinRoomParams::new("test-game", "local"));
        assert!(
            matches!(result, Err(SignalFishError::NotAuthenticated)),
            "expected NotAuthenticated, got {result:?}"
        );
        assert!(!client.is_authenticated());
        assert_eq!(
            client.send_capacity(),
            client.max_send_capacity(),
            "a refused room operation must not consume queue capacity"
        );

        // The fence stayed unarmed: once authentication completes, the same
        // operation is admitted immediately.
        {
            let mut core = lock_core(&client.state);
            let _ = core.process_frame(TransportFrame::Text(authenticated_json()));
        }
        assert!(client.is_authenticated());
        client
            .join_room(JoinRoomParams::new("test-game", "local"))
            .expect("join must be admitted once authenticated");

        // Exactly two commands reached the wire: Authenticate and JoinRoom.
        wait_for_sent_len(&sent, 2).await;
        let join_json = sent.lock().unwrap()[1].clone();
        let join: serde_json::Value = serde_json::from_str(&join_json).unwrap();
        assert_eq!(join["type"], "JoinRoom");

        client.shutdown().await;
    }

    /// Validates the documented best-effort delivery guarantee: deadline
    /// abandonment does not make the terminal `Disconnected` event mandatory.
    #[tokio::test]
    async fn shutdown_deadline_may_skip_disconnected_event() {
        let (transport, _close_called, _abort_called, _dropped) = HangingCloseTransport::new();
        let config = SignalFishConfig::new("mb_test")
            .with_shutdown_timeout(std::time::Duration::from_millis(1));
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        // Drain the initial Connected event so the channel is not congested.
        let event = events.recv().await.unwrap();
        assert!(matches!(event, SignalFishEvent::Connected));

        // The configured deadline expires because poll_close remains pending.
        client.shutdown().await;

        // Terminal event delivery is best-effort on the deadline path.
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(50), events.recv()).await;

        match result {
            Ok(Some(SignalFishEvent::Disconnected { .. })) => {
                // Disconnected was delivered before deadline abandonment.
            }
            Ok(None) => {
                // Channel closed without a Disconnected event — acceptable.
            }
            Err(_) => {
                // Timed out waiting; no Disconnected event was delivered — acceptable.
            }
            Ok(Some(other)) => {
                panic!("unexpected event after shutdown deadline: {other:?}");
            }
        }

        assert!(!client.is_connected());
    }

    #[tokio::test]
    async fn shutdown_abort_clears_auth_and_room_state() {
        let (transport, _close_called, _abort_called, _dropped) =
            HangingCloseTransport::with_incoming(vec![
                Some(Ok(authenticated_json())),
                Some(Ok(protocol_info_v2_json())),
                Some(Ok(room_joined_json())),
            ]);
        let config = SignalFishConfig::new("mb_test")
            .with_shutdown_timeout(std::time::Duration::from_millis(1));
        let (mut client, mut events) = SignalFishClient::start(transport, config);
        lock_core(&client.state).record_admission(ClientCore::admission_for(
            &ClientOperation::JoinRoom(JoinRoomParams::new("test-game", "local")),
        ));

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated
        let _ = events.recv().await; // ProtocolInfo
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

    // ── Bounded terminal delivery ───────────────────────────────────

    /// A consumer that never drains must not keep the transport loop (and
    /// every sender parked on the command queue) alive past the configured
    /// shutdown budget when the peer closes the connection without any
    /// `shutdown()` call.
    #[tokio::test]
    async fn peer_close_with_wedged_consumer_terminates_loop_and_releases_parked_senders() {
        // Two permits let Authenticate and JoinRoom reach the wire so the
        // gated room baseline is released; every later send stalls inside the
        // transport while the command queue backs up behind it.
        let (transport, _sent, closed, controls, _permits, frames_taken) =
            MockTransport::new_send_gated(
                vec![
                    Some(Ok(authenticated_json())),
                    Some(Ok(protocol_info_v2_json())),
                    Some(Ok(room_joined_json())),
                ],
                2,
            );
        let config = SignalFishConfig::new("mb_test")
            .with_event_channel_capacity(4)
            .with_command_channel_capacity(1)
            .with_shutdown_timeout(std::time::Duration::from_millis(100));
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        // Reach membership without draining events: Connected..RoomJoined
        // then exactly fill the capacity-4 channel, so the terminal
        // Disconnected delivery wedges against a permanently full channel.
        let give_up = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while !client.is_authenticated() {
            assert!(
                tokio::time::Instant::now() < give_up,
                "the scripted handshake never authenticated"
            );
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        client
            .join_room(JoinRoomParams::new("test-game", "local"))
            .expect("join must be admitted once authenticated");
        while client.current_room_id().await.is_none() {
            assert!(
                tokio::time::Instant::now() < give_up,
                "the scripted room baseline never landed"
            );
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }

        // Strand one command inside the stalled transport, fill the single
        // command queue slot behind it, then park a reliable sender on
        // reserve.
        client
            .ping()
            .expect("the first ping is taken by the stalled transport");
        // Authenticate + JoinRoom + our first ping: three gated takes.
        wait_until(|| frames_taken.load(Ordering::Acquire) >= 3).await;
        client
            .ping()
            .expect("one command fits the capacity-1 queue");
        let client = Arc::new(client);
        let sender = Arc::clone(&client);
        let mut parked = tokio::spawn(async move {
            sender
                .send_game_data_reliable(serde_json::json!({ "late": true }))
                .await
        });
        match tokio::time::timeout(std::time::Duration::from_millis(50), &mut parked).await {
            Err(_) => {}
            Ok(outcome) => panic!(
                "reliable send finished early: {outcome:?} (cap={}, role={:?})",
                client.send_capacity(),
                client.room_role(),
            ),
        }

        // The peer closes now. Connected..RoomJoined hold every event slot,
        // so the terminal Disconnected delivery also wedges until its budget
        // expires while the loop stops servicing the command queue entirely.
        controls.close_peer();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // The budget expires on the wedged Disconnected delivery; loop exit
        // drops the command receiver and resolves the parked reserve.
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), parked)
            .await
            .expect("loop termination must release parked reliable senders")
            .expect("reliable sender task should not panic");
        assert!(
            matches!(result, Err(SignalFishError::NotConnected)),
            "a reliable send interrupted by terminal close must fail cleanly: {result:?}"
        );
        assert!(
            closed.load(Ordering::Relaxed),
            "peer-close teardown must close or abort the transport"
        );
        {
            let client = Arc::clone(&client);
            assert!(!client.is_connected());
        }

        // Exactly the pre-close events were delivered, in order, and the
        // loop's exit closes the stream.
        let expected = ["Connected", "Authenticated", "ProtocolInfo", "RoomJoined"];
        let mut observed = Vec::new();
        while let Ok(Some(event)) =
            tokio::time::timeout(std::time::Duration::from_millis(200), events.recv()).await
        {
            observed.push(format!("{event:?}"));
            if observed.len() == expected.len() {
                break;
            }
        }
        let names: Vec<String> = observed
            .iter()
            .map(|debug| {
                debug
                    .split(['{', '('])
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect();
        assert_eq!(names, expected, "wedged delivery must preserve order");
    }

    /// A receive error wedges terminal delivery exactly like a clean close:
    /// with no `shutdown()` and an uncompletable `poll_close`, teardown must
    /// come from the budget-expiry abort, never hang the loop.
    #[tokio::test]
    async fn terminal_receive_error_aborts_only_after_the_shutdown_budget() {
        // Connected and Authenticated fill the capacity-2 channel, so the
        // terminal Disconnected delivery wedges with no `shutdown()` call.
        // The transport's `poll_close` never completes, so graceful close is
        // impossible: teardown can only come from the budget expiry aborting
        // the transport. A regression that restores unbounded delivery keeps
        // this spin alive forever instead of passing vacuously.
        let (transport, _close_called, abort_called, _dropped) =
            HangingCloseTransport::with_incoming(vec![
                Some(Ok(authenticated_json())),
                Some(Err(SignalFishError::TransportReceive(
                    "network failure".into(),
                ))),
            ]);
        let config = SignalFishConfig::new("mb_test")
            .with_event_channel_capacity(2)
            .with_shutdown_timeout(std::time::Duration::from_millis(100));
        let (client, mut events) = SignalFishClient::start(transport, config);

        let started = std::time::Instant::now();
        while !abort_called.load(Ordering::Acquire) {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(2),
                "the wedged terminal delivery must abort at its budget without shutdown()"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(!client.is_connected());

        // Loop exit drops the sender: draining surfaces the buffered prefix,
        // then closes the stream authoritatively.
        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Connected)
        ));
        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Authenticated { .. })
        ));
        let closed_stream =
            tokio::time::timeout(std::time::Duration::from_millis(500), events.recv())
                .await
                .expect("the event stream must end promptly after teardown");
        assert!(
            closed_stream.is_none(),
            "unexpected extra events after abort: {closed_stream:?}"
        );
    }

    /// A policy-driven disconnect wedges on its violation batch exactly like
    /// a peer close: with no `shutdown()` and a full channel, teardown must
    /// come from the shared shutdown budget, never hang past it.
    #[tokio::test]
    async fn policy_disconnect_violation_batch_is_bounded_by_the_shutdown_budget() {
        // Connected, Authenticated, and ProtocolInfo fill the capacity-3
        // channel, so the violation batch that precedes the Disconnect-policy
        // teardown wedges with no `shutdown()` call. The transport's
        // `poll_close` never completes, so teardown can only come from the
        // budget expiry aborting the transport. A regression that restores
        // unbounded delivery keeps this spin alive forever instead of passing
        // vacuously.
        let room_left_json = serde_json::to_string(&ServerMessage::RoomLeft).unwrap();
        let (transport, _close_called, abort_called, _dropped) =
            HangingCloseTransport::with_incoming(vec![
                Some(Ok(authenticated_json())),
                Some(Ok(protocol_info_v2_json())),
                Some(Ok(room_left_json)),
            ]);
        let config = SignalFishConfig::new("mb_test")
            .with_event_channel_capacity(3)
            .with_protocol_violation_policy(ProtocolViolationPolicy::Disconnect)
            .with_shutdown_timeout(std::time::Duration::from_millis(200));
        let (client, mut events) = SignalFishClient::start(transport, config);

        let started = std::time::Instant::now();
        while !abort_called.load(Ordering::Acquire) {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(2),
                "the wedged policy-disconnect delivery must abort at its budget without shutdown()"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        // The batch and the close share one budget: a regression that lets
        // the close restart a fresh window after the batch consumed it
        // observes roughly twice the configured timeout here.
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(300),
            "teardown took {elapsed:?}; the batch and close must share one budget"
        );
        assert!(!client.is_connected());

        // Loop exit drops the sender: draining surfaces the buffered prefix
        // (the wedged violation batch and farewell were abandoned by their
        // nonblocking fallbacks), then closes the stream authoritatively.
        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Connected)
        ));
        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Authenticated { .. })
        ));
        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::ProtocolInfo(_))
        ));
        let closed_stream =
            tokio::time::timeout(std::time::Duration::from_millis(500), events.recv())
                .await
                .expect("the event stream must end promptly after teardown");
        assert!(
            closed_stream.is_none(),
            "unexpected extra events after abort: {closed_stream:?}"
        );
    }

    /// `shutdown()` preempting a wedged policy-disconnect batch must not
    /// re-poll the already-consumed shutdown signal: the farewell falls back
    /// to a nonblocking attempt and the graceful close still runs.
    #[tokio::test]
    async fn shutdown_preempting_policy_disconnect_batch_skips_bounded_wait() {
        let room_left_json = serde_json::to_string(&ServerMessage::RoomLeft).unwrap();
        let (transport, close_called, _abort_called, _dropped) =
            HangingCloseTransport::with_incoming(vec![
                Some(Ok(authenticated_json())),
                Some(Ok(protocol_info_v2_json())),
                Some(Ok(room_left_json)),
            ]);
        // A long budget keeps the wedged batch blocked until the explicit
        // `shutdown()` below preempts it; the hanging close then bounds how
        // long `shutdown()` itself takes.
        let config = SignalFishConfig::new("mb_test")
            .with_event_channel_capacity(3)
            .with_protocol_violation_policy(ProtocolViolationPolicy::Disconnect)
            .with_shutdown_timeout(std::time::Duration::from_millis(300));
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        // Let the loop wedge on the full channel before shutting down.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        tokio::time::timeout(std::time::Duration::from_secs(2), client.shutdown())
            .await
            .expect("shutdown must complete promptly");

        // A regression that re-polled the consumed signal panicked the task
        // before any close was attempted.
        assert!(
            close_called.load(Ordering::Acquire),
            "shutdown must still attempt the graceful transport close"
        );

        // Loop exit drops the sender: draining surfaces the buffered prefix,
        // then closes the stream authoritatively.
        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Connected)
        ));
        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::Authenticated { .. })
        ));
        assert!(matches!(
            events.recv().await,
            Some(SignalFishEvent::ProtocolInfo(_))
        ));
        let closed_stream =
            tokio::time::timeout(std::time::Duration::from_millis(500), events.recv())
                .await
                .expect("the event stream must end promptly after teardown");
        assert!(
            closed_stream.is_none(),
            "unexpected extra events after shutdown: {closed_stream:?}"
        );
    }

    /// When a batch delivery is preempted, the remaining batch events get one
    /// nonblocking attempt instead of being discarded sight unseen.
    #[tokio::test]
    async fn expired_budget_preempts_blocked_batch_delivers_without_corruption() {
        let (tx, mut rx) = mpsc::channel(4);
        let (_shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let mut shutdown = ShutdownSignal::new(shutdown_rx);

        // A budget that expired before the call makes every blocked delivery
        // deterministic. The channel starts full, so both batch events are
        // preempted instead of delivered or queued.
        for _ in 0..4 {
            tx.try_send(SignalFishEvent::Pong).expect("filler fits");
        }
        let stale_deadline = tokio::time::Instant::now();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let batch = vec![SignalFishEvent::Connected, SignalFishEvent::Connected];
        let delivered = emit_event_batch(&tx, &mut shutdown, Some(stale_deadline), batch).await;
        assert!(!delivered, "an expired budget must report preemption");

        drop(tx);
        let mut observed = Vec::new();
        while let Some(event) = rx.recv().await {
            observed.push(event);
        }
        assert_eq!(
            observed.len(),
            4,
            "preemption must not corrupt buffered events: {observed:?}"
        );
        assert!(
            observed
                .iter()
                .all(|event| matches!(event, SignalFishEvent::Pong)),
            "only the original buffered events remain: {observed:?}"
        );
    }

    /// Issue #148: once the shutdown signal is observed, its tracking must be
    /// sticky — a later poll consults the flag instead of re-polling the
    /// completed `oneshot::Receiver`, which would panic ("called after
    /// complete").
    #[test]
    fn shutdown_signal_observation_is_sticky_across_polls() {
        let mut cx = Context::from_waker(Waker::noop());
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let mut signal = ShutdownSignal::new(rx);

        assert!(
            !signal.poll_fired(&mut cx),
            "an unfired signal must stay pending"
        );

        tx.send(()).expect("the receiver is still held");
        assert!(signal.poll_fired(&mut cx), "firing must be observed");
        // The exact re-poll that panicked before explicit tracking:
        assert!(
            signal.poll_fired(&mut cx),
            "observation must stay sticky after consumption"
        );
    }

    /// A signal whose sender was dropped without firing ends the wait the
    /// same way a fired one does, and stays sticky afterwards.
    #[test]
    fn shutdown_signal_treats_sender_drop_as_fired() {
        let mut cx = Context::from_waker(Waker::noop());
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let mut signal = ShutdownSignal::new(rx);
        drop(tx);

        assert!(
            signal.poll_fired(&mut cx),
            "a dropped sender must count as fired"
        );
        assert!(
            signal.poll_fired(&mut cx),
            "a canceled observation must also stay sticky"
        );
    }

    /// Issue #148: after one delivery consumes the shutdown signal, later
    /// deliveries on the same signal must observe it immediately instead of
    /// re-polling the completed oneshot (which panics) or blocking forever.
    #[tokio::test]
    async fn deliveries_after_consumed_shutdown_stay_nonblocking() {
        let (tx, _rx) = mpsc::channel(1);
        tx.try_send(SignalFishEvent::Pong).expect("filler fits");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let mut shutdown = ShutdownSignal::new(shutdown_rx);
        shutdown_tx.send(()).expect("the receiver is still held");

        // The channel is full and shutdown has fired: the first delivery
        // preempts and consumes the signal.
        assert!(matches!(
            emit_event_or_shutdown(&tx, &mut shutdown, SignalFishEvent::Connected).await,
            EmitOutcome::ShutdownRequested
        ));
        // The second delivery on the SAME signal panicked before explicit
        // tracking; now it observes the sticky flag.
        assert!(matches!(
            emit_event_or_shutdown(&tx, &mut shutdown, SignalFishEvent::Connected).await,
            EmitOutcome::ShutdownRequested
        ));
        // The terminal variant must answer immediately too — with no deadline,
        // only the consumed-signal observation can end the wait.
        assert!(
            !emit_terminal_event(&tx, &mut shutdown, None, SignalFishEvent::Connected).await,
            "a consumed signal must preempt a terminal delivery"
        );
    }
}
