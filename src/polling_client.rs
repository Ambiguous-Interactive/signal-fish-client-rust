//! Synchronous, polling-based client for the Signal Fish signaling protocol.
//!
//! [`SignalFishPollingClient`] is designed for environments without a
//! continuously driven async runtime: frame-driven game engines (Godot,
//! Unity via FFI, Bevy without tokio), `wasm32` targets (e.g. Godot web
//! builds via gdext on `wasm32-unknown-emscripten`), and any application
//! that would otherwise "tick" a tokio runtime once per frame — a pattern
//! that starves [`SignalFishClient`](crate::SignalFishClient)'s spawned
//! transport loop. The caller drives the client by calling
//! [`poll()`](SignalFishPollingClient::poll) once per frame from the game
//! loop; no background task or runtime is required.
//!
//! Delivery guarantees match the async client: incoming events are returned
//! from `poll()` without loss, and outgoing commands go through a bounded
//! queue that fails fast with
//! [`SendBufferFull`](crate::error::SignalFishError::SendBufferFull) instead
//! of growing without bound when the transport is congested.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use tracing::{debug, error};

use crate::client::{ClientSnapshot, GameDataDelivery, JoinRoomParams, SignalFishConfig};
use crate::client_core::{ClientCore, ClientOperation, CoreCommand as PollingCommand};
use crate::error::{Result, SignalFishError};
use crate::event::SignalFishEvent;
#[cfg(test)]
use crate::protocol::GameDataEncoding;
use crate::protocol::{ClientMessage, ConnectionInfo, PlayerId, RoomId, TransportKind};
use crate::signal::PeerSignal;
use crate::transport::{Transport, TransportDiagnostics, TransportFrame};

const DEFAULT_POLL_FRAMES: usize = 64;
const DEFAULT_POLL_BYTES: usize = 64 * 1024;

/// Maximum send and receive work performed by one [`poll`](SignalFishPollingClient::poll).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollingWorkBudget {
    /// Maximum outbound ownership transfers per poll.
    pub send_frames: usize,
    /// Maximum outbound payload bytes begun per poll.
    pub send_bytes: usize,
    /// Maximum inbound frames processed per poll.
    pub receive_frames: usize,
    /// Maximum inbound payload bytes processed per poll.
    pub receive_bytes: usize,
}

impl Default for PollingWorkBudget {
    fn default() -> Self {
        Self {
            send_frames: DEFAULT_POLL_FRAMES,
            send_bytes: DEFAULT_POLL_BYTES,
            receive_frames: DEFAULT_POLL_FRAMES,
            receive_bytes: DEFAULT_POLL_BYTES,
        }
    }
}

impl PollingWorkBudget {
    fn clamped(self) -> Self {
        Self {
            send_frames: self.send_frames.max(1),
            send_bytes: self.send_bytes.max(1),
            receive_frames: self.receive_frames.max(1),
            receive_bytes: self.receive_bytes.max(1),
        }
    }
}

/// Behavior for commands queued when [`close`](SignalFishPollingClient::close) is called.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PollingClosePolicy {
    /// Abandon client-owned queued commands and start the transport close immediately.
    #[default]
    Abandon,
    /// Offer already-queued commands to the backend under the normal poll budget first.
    Flush,
}

/// Construction options for [`SignalFishPollingClient`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PollingClientOptions {
    /// Per-poll outbound and inbound work limits.
    pub work_budget: PollingWorkBudget,
    /// Treatment of commands already queued at close time.
    pub close_policy: PollingClosePolicy,
}

/// Cumulative polling-driver scheduling and close diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PollingStats {
    /// Current client-owned outbound command/frame depth.
    pub current_queue_depth: u64,
    /// Highest observed client-owned outbound depth.
    pub peak_queue_depth: u64,
    /// Polls that stopped because the send frame or byte budget was exhausted.
    pub send_budget_exhaustions: u64,
    /// Polls that stopped because the receive frame or byte budget was exhausted.
    pub receive_budget_exhaustions: u64,
    /// Commands abandoned by close policy or close-deadline expiry.
    pub abandoned_commands: u64,
    /// Flush/close lifecycles aborted after the configured deadline.
    pub close_deadline_expirations: u64,
}

/// Sampled age of the oldest client-owned outbound command or frame.
///
/// Authentication and other setup commands contribute to these values until
/// [`SignalFishPollingClient::reset_queue_age_peak`] is called. The age stops
/// when the transport accepts ownership of a frame; transport acceptance is
/// not peer delivery, and backend-owned buffering is reported separately by
/// [`SignalFishPollingClient::transport_diagnostics`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PollingQueueAgeStats {
    /// Age of the oldest client-owned item at the latest sample.
    pub current_oldest_queue_age: Duration,
    /// Highest sampled age of the oldest client-owned item since construction
    /// or the latest peak reset.
    pub peak_oldest_queue_age: Duration,
}

#[derive(Debug)]
struct QueuedCommand {
    command: PollingCommand,
    enqueued_at: Instant,
}

#[derive(Debug, Clone, Copy)]
enum ClosePhase {
    Open,
    Flushing { started_at: Instant },
    Closing { started_at: Instant },
    Closed,
}

// ── Public client ───────────────────────────────────────────────────

/// A synchronous, polling-based client for the Signal Fish signaling protocol.
///
/// Unlike [`SignalFishClient`](crate::SignalFishClient), this client does **not**
/// spawn a background task or require a tokio runtime. Instead, the caller drives
/// the client by calling [`poll()`](Self::poll) on each frame (e.g., from Godot's
/// `_process(delta)` method).
///
/// # Example (Godot via gdext)
///
/// ```rust,ignore
/// use signal_fish_client::{
///     GodotWebSocketTransport, SignalFishPollingClient,
///     SignalFishConfig, JoinRoomParams, SignalFishEvent,
/// };
///
/// struct MyNode {
///     client: Option<SignalFishPollingClient<GodotWebSocketTransport>>,
/// }
///
/// impl MyNode {
///     fn ready(&mut self) {
///         let transport = GodotWebSocketTransport::connect("wss://server/ws")
///             .expect("failed to create WebSocket");
///         let config = SignalFishConfig::new("my_app_id");
///         self.client = Some(SignalFishPollingClient::new(transport, config));
///     }
///
///     fn process(&mut self, _delta: f64) {
///         let Some(client) = &mut self.client else { return };
///         for event in client.poll() {
///             match event {
///                 SignalFishEvent::Authenticated { .. } => {
///                     client.join_room(JoinRoomParams::new("game", "Player1"))
///                         .ok();
///                 }
///                 SignalFishEvent::Disconnected { .. } => {
///                     self.client = None;
///                     return;
///                 }
///                 _ => {}
///             }
///         }
///     }
/// }
/// ```
pub struct SignalFishPollingClient<T: Transport> {
    transport: T,
    cmd_queue: VecDeque<QueuedCommand>,
    /// Maximum number of queued commands before sends fail fast with
    /// [`SignalFishError::SendBufferFull`]. Mirrors the async client's
    /// bounded command channel.
    command_capacity: usize,
    core: ClientCore,
    options: PollingClientOptions,
    polling_stats: PollingStats,
    queue_age_stats: PollingQueueAgeStats,
    shutdown_timeout: Duration,
    started: bool,
    pending_frame: Option<TransportFrame>,
    pending_frame_enqueued_at: Option<Instant>,
    /// The transport accepted the current frame and is still completing it.
    /// While true, poll with `None` and do not dequeue a replacement frame.
    send_in_flight: bool,
    pending_frame_is_game_data: bool,
    in_flight_is_game_data: bool,
    pending_inbound: Option<TransportFrame>,
    close_phase: ClosePhase,
}

impl<T: Transport> SignalFishPollingClient<T> {
    /// Create a new polling client with the given transport and configuration.
    ///
    /// Immediately queues an [`Authenticate`](ClientMessage::Authenticate) message.
    /// A synthetic [`Connected`](SignalFishEvent::Connected) event will be emitted
    /// once [`poll()`](Self::poll) observes that the transport is ready (see
    /// [`Transport::is_ready()`](crate::Transport::is_ready)).
    #[must_use]
    pub fn new(transport: T, config: SignalFishConfig) -> Self {
        Self::new_with_options(transport, config, PollingClientOptions::default())
    }

    /// Create a polling client with explicit work-budget and close behavior.
    #[must_use]
    pub fn new_with_options(
        transport: T,
        config: SignalFishConfig,
        mut options: PollingClientOptions,
    ) -> Self {
        options.work_budget = options.work_budget.clamped();
        let requested_game_data_encoding = config.game_data_format.unwrap_or_default();
        let mesh_enabled = config
            .supported_transports
            .as_ref()
            .is_some_and(|transports| transports.contains(&TransportKind::WebRtc));
        let auth_msg = ClientCore::authenticate(&config);

        let now = Instant::now();
        let mut cmd_queue = VecDeque::new();
        cmd_queue.push_back(QueuedCommand {
            command: auth_msg,
            enqueued_at: now,
        });

        let shutdown_timeout = config.shutdown_timeout;
        let mut client = Self {
            transport,
            cmd_queue,
            command_capacity: config.command_channel_capacity.max(1),
            core: ClientCore::new(
                requested_game_data_encoding,
                config.protocol_violation_policy,
                mesh_enabled,
            ),
            options,
            polling_stats: PollingStats {
                current_queue_depth: 1,
                peak_queue_depth: 1,
                ..PollingStats::default()
            },
            queue_age_stats: PollingQueueAgeStats::default(),
            shutdown_timeout,
            started: false,
            pending_frame: None,
            pending_frame_enqueued_at: None,
            send_in_flight: false,
            pending_frame_is_game_data: false,
            in_flight_is_game_data: false,
            pending_inbound: None,
            close_phase: ClosePhase::Open,
        };
        client.refresh_queue_diagnostics_at(now);
        client
    }

    // ── Core polling method ─────────────────────────────────────────

    /// Drive the client for one frame.
    ///
    /// Transfers queued outgoing commands and processes incoming messages up to
    /// the configured frame and byte budgets. Returns a `Vec` of events that
    /// occurred during this poll cycle; retained work remains FIFO for the next
    /// call. One individually oversized frame may consume a poll by itself.
    ///
    /// Call this method once per frame from your game loop.
    ///
    /// # Connection timing
    ///
    /// The first time `poll()` observes that
    /// [`Transport::is_ready()`](crate::Transport::is_ready) returns `true`,
    /// it emits a synthetic [`SignalFishEvent::Connected`] event at position 0
    /// in the returned `Vec`.
    ///
    /// For transports that are already connected at construction time (e.g.,
    /// [`WebSocketTransport`](crate::WebSocketTransport)), `Connected` is
    /// emitted on the very first call to `poll()`. For transports with
    /// asynchronous handshakes (e.g., `GodotWebSocketTransport`),
    /// `Connected` is deferred until the transport reports that its connection
    /// handshake has completed.
    ///
    /// Commands queued before `Connected` (including the automatic
    /// [`Authenticate`](crate::protocol::ClientMessage::Authenticate) message)
    /// are offered on every `poll()` call regardless of readiness. A transport
    /// that is still connecting returns `Pending`, leaving the exact frame
    /// caller-owned for a later poll.
    pub fn poll(&mut self) -> Vec<SignalFishEvent> {
        self.poll_at(Instant::now())
    }

    fn poll_at(&mut self, now: Instant) -> Vec<SignalFishEvent> {
        let mut events = Vec::new();
        self.refresh_queue_diagnostics_at(now);

        // Create a noop waker to poll transport futures synchronously.
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        self.transport.begin_poll_cycle();

        if !matches!(self.close_phase, ClosePhase::Open) {
            self.drive_close_at(now, &mut cx);
            return events;
        }

        if !self.core.is_connected() {
            return events;
        }

        if let Err(error) = self.drive_outbound(&mut cx, now) {
            error!(%error, "transport send failed");
            self.handle_disconnect_at(
                &mut events,
                Some(format!("transport send error: {error}")),
                &mut cx,
                now,
            );
            return events;
        }

        let budget = self.options.work_budget;
        let mut received_frames = 0usize;
        let mut received_bytes = 0usize;
        loop {
            let frame = if let Some(frame) = self.pending_inbound.take() {
                frame
            } else {
                match self.transport.poll_recv(&mut cx) {
                    std::task::Poll::Ready(Some(Ok(frame))) => frame,
                    std::task::Poll::Ready(Some(Err(e))) => {
                        error!("transport receive error: {e}");
                        self.handle_disconnect_at(
                            &mut events,
                            Some(format!("transport receive error: {e}")),
                            &mut cx,
                            now,
                        );
                        break;
                    }
                    std::task::Poll::Ready(None) => {
                        debug!("transport closed by server");
                        let reason = self.transport.close_info().map(|info| {
                            format!(
                                "closed by server: code={:?}, reason={:?}",
                                info.code, info.reason
                            )
                        });
                        self.handle_disconnect_at(&mut events, reason, &mut cx, now);
                        break;
                    }
                    std::task::Poll::Pending => {
                        break;
                    }
                }
            };

            let frame_bytes = frame_payload_len(&frame);
            let next_bytes = received_bytes.checked_add(frame_bytes);
            if received_frames > 0
                && (received_frames >= budget.receive_frames
                    || next_bytes.is_none_or(|bytes| bytes > budget.receive_bytes))
            {
                self.pending_inbound = Some(frame);
                self.polling_stats.receive_budget_exhaustions = self
                    .polling_stats
                    .receive_budget_exhaustions
                    .saturating_add(1);
                break;
            }

            received_frames = received_frames.saturating_add(1);
            received_bytes = next_bytes.unwrap_or(usize::MAX);
            let outcome = self.core.process_frame(frame);
            events.extend(outcome.events);
            if outcome.disconnect {
                self.handle_disconnect_at(
                    &mut events,
                    Some("protocol accountability violation".into()),
                    &mut cx,
                    now,
                );
                return events;
            }
        }

        // Emit Connected once the transport signals readiness.
        // This is placed after the recv drain so that transports with
        // asynchronous handshakes (e.g., EmscriptenWebSocketTransport)
        // have a chance to process their connection-open event before
        // we check is_ready(). Connected is inserted at position 0 to
        // guarantee it is always the first event in the batch.
        if !self.started && self.transport.is_ready() {
            self.started = true;
            events.insert(0, SignalFishEvent::Connected);
        }

        events
    }

    // ── Public API methods (mirror SignalFishClient) ────────────────

    /// Join or create a room with the given parameters.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn join_room(&mut self, params: JoinRoomParams) -> Result<()> {
        self.queue_operation(ClientOperation::JoinRoom(params))
    }

    /// Leave the current room.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn leave_room(&mut self) -> Result<()> {
        self.queue_operation(ClientOperation::LeaveRoom)
    }

    /// Send game data to other players in the room.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn send_game_data(&mut self, data: serde_json::Value) -> Result<()> {
        self.queue_operation(ClientOperation::GameData(data, GameDataDelivery::Reliable))
    }

    /// Send JSON game data with an explicit protocol-v3 delivery policy.
    pub fn send_game_data_with_delivery(
        &mut self,
        data: serde_json::Value,
        delivery: GameDataDelivery,
    ) -> Result<()> {
        self.queue_operation(ClientOperation::GameData(data, delivery))
    }

    /// Queue opaque binary game data for the negotiated protocol-v3 relay.
    pub fn send_binary_game_data(&mut self, payload: Vec<u8>) -> Result<()> {
        self.queue_operation(ClientOperation::Binary(payload))
    }

    /// Signal readiness to start the game.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn set_ready(&mut self) -> Result<()> {
        self.queue_operation(ClientOperation::SetReady)
    }

    /// Request or relinquish authority status.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn request_authority(&mut self, become_authority: bool) -> Result<()> {
        self.queue_operation(ClientOperation::RequestAuthority(become_authority))
    }

    /// Provide connection info for P2P establishment.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn provide_connection_info(&mut self, connection_info: ConnectionInfo) -> Result<()> {
        self.queue_operation(ClientOperation::ProvideConnectionInfo(connection_info))
    }

    /// Reconnect to a room after disconnection.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn reconnect(
        &mut self,
        player_id: PlayerId,
        room_id: RoomId,
        auth_token: String,
    ) -> Result<()> {
        self.queue_operation(ClientOperation::Reconnect(player_id, room_id, auth_token))
    }

    /// Join a room as a spectator (read-only observer).
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn join_as_spectator(
        &mut self,
        game_name: String,
        room_code: String,
        spectator_name: String,
    ) -> Result<()> {
        self.queue_operation(ClientOperation::JoinAsSpectator(
            game_name,
            room_code,
            spectator_name,
        ))
    }

    /// Leave spectator mode.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn leave_spectator(&mut self) -> Result<()> {
        self.queue_operation(ClientOperation::LeaveSpectator)
    }

    /// Send a heartbeat ping.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn ping(&mut self) -> Result<()> {
        self.queue_operation(ClientOperation::Ping)
    }

    // ── Game start (protocol v2) ────────────────────────────────────

    /// Request that the server start the game (protocol v2).
    ///
    /// See [`SignalFishClient::start_game`](crate::SignalFishClient::start_game)
    /// for the full semantics (explicit start, all-ready + authority gating).
    /// Available on every connection, not gated behind the mesh opt-in.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed,
    /// or [`SignalFishError::SendBufferFull`] if the outgoing command queue
    /// is full (the message is **not** queued; nothing is silently dropped).
    pub fn start_game(&mut self) -> Result<()> {
        self.queue_operation(ClientOperation::StartGame)
    }

    // ── Mesh signaling (protocol v3) ────────────────────────────────

    /// Send a typed WebRTC signal to a single peer.
    ///
    /// **Protocol v3 only.** Fails fast on a relay-floor connection (see Errors).
    ///
    /// See [`SignalFishClient::send_signal`](crate::SignalFishClient::send_signal).
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::ProtocolUnsupported`] if the connection has not
    /// negotiated protocol v3, [`SignalFishError::NotConnected`] if the
    /// transport has closed, or [`SignalFishError::SendBufferFull`] if the
    /// outgoing command queue is full.
    pub fn send_signal(&mut self, to: PlayerId, signal: impl Into<PeerSignal>) -> Result<()> {
        self.queue_operation(ClientOperation::Signal(to, signal.into()))
    }

    /// Send an SDP offer to a peer. **Protocol v3 only.**
    ///
    /// # Errors
    ///
    /// See [`send_signal`](Self::send_signal).
    pub fn send_offer(&mut self, to: PlayerId, sdp: impl Into<String>) -> Result<()> {
        self.send_signal(to, PeerSignal::Offer(sdp.into()))
    }

    /// Send an SDP answer to a peer. **Protocol v3 only.**
    ///
    /// # Errors
    ///
    /// See [`send_signal`](Self::send_signal).
    pub fn send_answer(&mut self, to: PlayerId, sdp: impl Into<String>) -> Result<()> {
        self.send_signal(to, PeerSignal::Answer(sdp.into()))
    }

    /// Send a single trickle ICE candidate to a peer. **Protocol v3 only.**
    ///
    /// # Errors
    ///
    /// See [`send_signal`](Self::send_signal).
    pub fn send_ice_candidate(&mut self, to: PlayerId, candidate: impl Into<String>) -> Result<()> {
        self.send_signal(to, PeerSignal::IceCandidate(candidate.into()))
    }

    /// Raw escape hatch: relay an un-modeled signal shape. **Protocol v3 only.**
    ///
    /// Still gated on a negotiated v3 session — the escape hatch bypasses the
    /// typing, not the negotiation guard.
    ///
    /// # Errors
    ///
    /// See [`send_signal`](Self::send_signal).
    pub fn send_raw_signal(&mut self, to: PlayerId, signal: serde_json::Value) -> Result<()> {
        self.queue_operation(ClientOperation::RawSignal(to, signal))
    }

    /// Report whether a data-path transport is established. **Protocol v3 only.**
    ///
    /// See [`SignalFishClient::report_transport_status`](crate::SignalFishClient::report_transport_status).
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::ProtocolUnsupported`] if the connection has not
    /// negotiated protocol v3, [`SignalFishError::NotConnected`] if the
    /// transport has closed, or [`SignalFishError::SendBufferFull`] if the
    /// outgoing command queue is full.
    pub fn report_transport_status(
        &mut self,
        transport: TransportKind,
        connected: bool,
    ) -> Result<()> {
        self.queue_operation(ClientOperation::TransportStatus(transport, connected))
    }

    // ── State accessors ─────────────────────────────────────────────

    /// The protocol version negotiated with the server, or `None` if not yet
    /// negotiated or negotiated as v2 (the relay floor).
    pub fn negotiated_protocol_version(&self) -> Option<u16> {
        self.core.negotiated_protocol_version()
    }

    /// Returns `true` once the connection has negotiated protocol v3 and this
    /// client advertised WebRTC support through [`SignalFishConfig::enable_mesh`].
    /// This is the "am I in mesh mode?" check.
    pub fn supports_mesh(&self) -> bool {
        self.core.supports_mesh()
    }

    /// Whether the transport connection is still alive.
    pub fn is_connected(&self) -> bool {
        self.core.is_connected()
    }

    /// Whether a transport close handshake still needs to be driven by
    /// [`poll()`](Self::poll).
    pub fn is_closing(&self) -> bool {
        matches!(
            self.close_phase,
            ClosePhase::Flushing { .. } | ClosePhase::Closing { .. }
        )
    }

    /// Whether the client has received an `Authenticated` response.
    pub fn is_authenticated(&self) -> bool {
        self.core.is_authenticated()
    }

    /// The local player's ID, set after joining a room.
    pub fn current_player_id(&self) -> Option<PlayerId> {
        self.core.current_player_id()
    }

    /// The current room ID, set after joining a room.
    pub fn current_room_id(&self) -> Option<RoomId> {
        self.core.current_room_id()
    }

    /// The current room code, set after joining a room.
    pub fn current_room_code(&self) -> Option<&str> {
        self.core.current_room_code()
    }

    /// Number of messages that can currently be queued before send methods
    /// return [`SignalFishError::SendBufferFull`].
    ///
    /// The queue drains on every [`poll()`](Self::poll) while the transport
    /// accepts writes, so a shrinking value means the transport is congested.
    pub fn send_capacity(&self) -> usize {
        self.command_capacity.saturating_sub(self.cmd_queue.len())
    }

    /// Configured capacity of the outgoing command queue
    /// (see [`SignalFishConfig::command_channel_capacity`]).
    pub fn max_send_capacity(&self) -> usize {
        self.command_capacity
    }

    /// Cumulative game-data traffic counters
    /// (see [`ClientStats`](crate::client::ClientStats)).
    pub fn stats(&self) -> crate::client::ClientStats {
        self.core.stats()
    }

    /// Return polling-driver queue, budget, and close diagnostics.
    pub fn polling_stats(&self) -> PollingStats {
        self.polling_stats
    }

    /// Return the latest sampled client-owned queue-age diagnostics.
    ///
    /// Queue age is sampled on every poll and queue mutation. Empty queues
    /// report zero. Backend acceptance ends client ownership immediately; use
    /// [`transport_diagnostics`](Self::transport_diagnostics) for buffering
    /// after that boundary.
    pub fn queue_age_stats(&self) -> PollingQueueAgeStats {
        self.queue_age_stats
    }

    /// Refresh the current oldest client-owned queue age and reset its peak to
    /// that sampled value.
    pub fn reset_queue_age_peak(&mut self) {
        self.reset_queue_age_peak_at(Instant::now());
    }

    /// Return backend-owned transport buffering and admission diagnostics.
    pub fn transport_diagnostics(&self) -> TransportDiagnostics {
        self.transport.diagnostics()
    }

    /// Borrow the owned transport for transport-specific read-only diagnostics.
    ///
    /// Protocol and I/O progress must still be driven through [`poll`](Self::poll).
    #[must_use]
    pub const fn transport(&self) -> &T {
        &self.transport
    }

    /// Return a coherent synchronous snapshot of connection and room state.
    pub fn snapshot(&self) -> ClientSnapshot {
        self.core.snapshot()
    }

    // ── Close ───────────────────────────────────────────────────────

    /// Close the transport and mark the client as disconnected.
    ///
    /// New commands are rejected immediately. The configured
    /// [`PollingClosePolicy`] either abandons client-owned work or flushes it
    /// under the normal work budget before starting the transport close.
    /// [`SignalFishConfig::shutdown_timeout`] bounds the complete operation.
    pub fn close(&mut self) {
        self.close_at(Instant::now());
    }

    fn close_at(&mut self, now: Instant) {
        if !matches!(self.close_phase, ClosePhase::Open) {
            return;
        }
        if !self.core.is_connected() {
            self.close_phase = ClosePhase::Closed;
            return;
        }
        let _ = self.core.disconnect(Some("client closed".into()));
        self.close_phase = match self.options.close_policy {
            PollingClosePolicy::Abandon => {
                self.abandon_client_owned(false, now);
                if self.send_in_flight {
                    ClosePhase::Flushing { started_at: now }
                } else {
                    ClosePhase::Closing { started_at: now }
                }
            }
            PollingClosePolicy::Flush => ClosePhase::Flushing { started_at: now },
        };
        debug!(policy = ?self.options.close_policy, "polling client close started");
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        self.transport.begin_poll_cycle();
        self.drive_close_at(now, &mut cx);
    }

    // ── Private helpers ─────────────────────────────────────────────

    fn queue_operation(&mut self, operation: ClientOperation) -> Result<()> {
        let command = self.core.prepare(operation)?;
        self.queue_command_at(command, Instant::now())
    }

    fn queue_command_at(&mut self, command: PollingCommand, now: Instant) -> Result<()> {
        if !self.core.is_connected() {
            return Err(SignalFishError::NotConnected);
        }
        // The queue only backs up while the transport reports Pending across
        // polls; refusing (rather than growing without bound) surfaces the
        // congestion to the caller, mirroring the async client's bounded
        // command channel.
        if self.cmd_queue.len() >= self.command_capacity {
            return Err(SignalFishError::SendBufferFull {
                capacity: self.command_capacity,
            });
        }
        self.cmd_queue.push_back(QueuedCommand {
            command,
            enqueued_at: now,
        });
        self.refresh_queue_diagnostics_at(now);
        Ok(())
    }

    fn drive_outbound(
        &mut self,
        cx: &mut std::task::Context<'_>,
        now: Instant,
    ) -> std::result::Result<(), SignalFishError> {
        if self.send_in_flight {
            let mut no_frame = None;
            match self.transport.poll_send(cx, &mut no_frame) {
                std::task::Poll::Ready(Ok(())) => {
                    if self.in_flight_is_game_data {
                        self.core.record_game_data_sent();
                    }
                    self.send_in_flight = false;
                    self.in_flight_is_game_data = false;
                }
                std::task::Poll::Ready(Err(error)) => {
                    self.send_in_flight = false;
                    self.in_flight_is_game_data = false;
                    return Err(error);
                }
                std::task::Poll::Pending => return Ok(()),
            }
        }

        let budget = self.options.work_budget;
        let mut sent_frames = 0usize;
        let mut sent_bytes = 0usize;
        loop {
            if self.pending_frame.is_none() {
                let Some(queued) = self.cmd_queue.pop_front() else {
                    break;
                };
                self.pending_frame_enqueued_at = Some(queued.enqueued_at);
                match queued.command {
                    PollingCommand::Message(message) => {
                        let Some(json) =
                            self.finish_serialization_at(serde_json::to_string(&message), now)
                        else {
                            continue;
                        };
                        self.pending_frame_is_game_data =
                            matches!(message, ClientMessage::GameData { .. });
                        self.pending_frame = Some(TransportFrame::Text(json));
                    }
                    PollingCommand::Binary(payload) => {
                        self.pending_frame_is_game_data = true;
                        self.pending_frame = Some(TransportFrame::Binary(payload));
                    }
                }
                self.refresh_queue_diagnostics_at(now);
            }

            let frame_bytes = self
                .pending_frame
                .as_ref()
                .map(frame_payload_len)
                .unwrap_or(0);
            let next_bytes = sent_bytes.checked_add(frame_bytes);
            if sent_frames >= budget.send_frames
                || (sent_frames > 0 && next_bytes.is_none_or(|bytes| bytes > budget.send_bytes))
            {
                self.polling_stats.send_budget_exhaustions =
                    self.polling_stats.send_budget_exhaustions.saturating_add(1);
                break;
            }

            let result = self.transport.poll_send(cx, &mut self.pending_frame);
            let transferred = self.pending_frame.is_none();
            if transferred {
                self.pending_frame_enqueued_at = None;
                sent_frames = sent_frames.saturating_add(1);
                sent_bytes = next_bytes.unwrap_or(usize::MAX);
                self.refresh_queue_diagnostics_at(now);
            }
            match result {
                std::task::Poll::Ready(Ok(())) => {
                    if !transferred {
                        break;
                    }
                    if self.pending_frame_is_game_data {
                        self.core.record_game_data_sent();
                    }
                    self.pending_frame_is_game_data = false;
                }
                std::task::Poll::Ready(Err(error)) => {
                    if transferred {
                        self.pending_frame_is_game_data = false;
                    }
                    return Err(error);
                }
                std::task::Poll::Pending => {
                    if transferred {
                        self.send_in_flight = true;
                        self.in_flight_is_game_data = self.pending_frame_is_game_data;
                        self.pending_frame_is_game_data = false;
                    }
                    break;
                }
            }

            if (sent_frames >= budget.send_frames || sent_bytes >= budget.send_bytes)
                && self.has_outbound_work()
            {
                self.polling_stats.send_budget_exhaustions =
                    self.polling_stats.send_budget_exhaustions.saturating_add(1);
                break;
            }
        }
        Ok(())
    }

    fn drive_close_at(&mut self, now: Instant, cx: &mut std::task::Context<'_>) {
        let started_at = match self.close_phase {
            ClosePhase::Flushing { started_at } | ClosePhase::Closing { started_at } => started_at,
            ClosePhase::Open | ClosePhase::Closed => return,
        };
        if now.saturating_duration_since(started_at) >= self.shutdown_timeout {
            self.abandon_client_owned(true, now);
            self.transport.abort();
            self.polling_stats.close_deadline_expirations = self
                .polling_stats
                .close_deadline_expirations
                .saturating_add(1);
            self.close_phase = ClosePhase::Closed;
            tracing::warn!("polling client close deadline expired; transport aborted");
            return;
        }

        self.drain_closing_inbound(cx);

        if matches!(self.close_phase, ClosePhase::Flushing { .. }) {
            if let Err(error) = self.drive_outbound(cx, now) {
                error!(%error, "queued send failed while flushing close");
                self.abandon_client_owned(false, now);
                self.close_phase = ClosePhase::Closing { started_at };
            } else if !self.has_outbound_work() {
                self.close_phase = ClosePhase::Closing { started_at };
                debug!("polling client queued work transferred before close");
            } else {
                return;
            }
        }

        if matches!(self.close_phase, ClosePhase::Closing { .. }) {
            match self.transport.poll_close(cx) {
                std::task::Poll::Ready(Ok(())) => {
                    self.close_phase = ClosePhase::Closed;
                    debug!("polling client transport close completed");
                }
                std::task::Poll::Ready(Err(error)) => {
                    error!(%error, "polling client transport close failed");
                    self.close_phase = ClosePhase::Closed;
                }
                std::task::Poll::Pending => {}
            }
        }
    }

    fn drain_closing_inbound(&mut self, cx: &mut std::task::Context<'_>) {
        let budget = self.options.work_budget;
        let mut received_frames = 0usize;
        let mut received_bytes = 0usize;
        loop {
            let frame = if let Some(frame) = self.pending_inbound.take() {
                frame
            } else {
                match self.transport.poll_recv(cx) {
                    std::task::Poll::Ready(Some(Ok(frame))) => frame,
                    std::task::Poll::Ready(Some(Err(error))) => {
                        error!(%error, "transport receive failed while closing");
                        break;
                    }
                    std::task::Poll::Ready(None) | std::task::Poll::Pending => break,
                }
            };

            let frame_bytes = frame_payload_len(&frame);
            let next_bytes = received_bytes.checked_add(frame_bytes);
            if received_frames > 0
                && (received_frames >= budget.receive_frames
                    || next_bytes.is_none_or(|bytes| bytes > budget.receive_bytes))
            {
                self.pending_inbound = Some(frame);
                self.polling_stats.receive_budget_exhaustions = self
                    .polling_stats
                    .receive_budget_exhaustions
                    .saturating_add(1);
                break;
            }

            received_frames = received_frames.saturating_add(1);
            received_bytes = next_bytes.unwrap_or(usize::MAX);
        }
    }

    fn handle_disconnect_at(
        &mut self,
        events: &mut Vec<SignalFishEvent>,
        reason: Option<String>,
        cx: &mut std::task::Context<'_>,
        now: Instant,
    ) {
        self.abandon_client_owned(false, now);
        self.pending_inbound = None;
        self.close_phase = if self.send_in_flight {
            ClosePhase::Flushing { started_at: now }
        } else {
            ClosePhase::Closing { started_at: now }
        };
        events.push(self.core.disconnect(reason));
        self.drive_close_at(now, cx);
    }

    fn has_outbound_work(&self) -> bool {
        self.send_in_flight || self.pending_frame.is_some() || !self.cmd_queue.is_empty()
    }

    fn abandon_client_owned(&mut self, include_in_flight: bool, now: Instant) {
        let mut abandoned = self.cmd_queue.len();
        abandoned = abandoned.saturating_add(usize::from(self.pending_frame.is_some()));
        if include_in_flight {
            abandoned = abandoned.saturating_add(usize::from(self.send_in_flight));
            self.send_in_flight = false;
            self.in_flight_is_game_data = false;
        }
        self.cmd_queue.clear();
        self.pending_frame = None;
        self.pending_frame_enqueued_at = None;
        self.pending_frame_is_game_data = false;
        self.polling_stats.abandoned_commands = self
            .polling_stats
            .abandoned_commands
            .saturating_add(u64::try_from(abandoned).unwrap_or(u64::MAX));
        self.refresh_queue_diagnostics_at(now);
    }

    fn record_serialization_failure_at(&mut self, now: Instant) {
        self.polling_stats.abandoned_commands =
            self.polling_stats.abandoned_commands.saturating_add(1);
        self.pending_frame_enqueued_at = None;
        self.refresh_queue_diagnostics_at(now);
    }

    fn finish_serialization_at(
        &mut self,
        result: serde_json::Result<String>,
        now: Instant,
    ) -> Option<String> {
        match result {
            Ok(json) => Some(json),
            Err(error) => {
                error!(%error, "failed to serialize ClientMessage");
                self.record_serialization_failure_at(now);
                None
            }
        }
    }

    fn refresh_queue_diagnostics_at(&mut self, now: Instant) {
        let depth = self
            .cmd_queue
            .len()
            .saturating_add(usize::from(self.pending_frame.is_some()));
        self.polling_stats.current_queue_depth = u64::try_from(depth).unwrap_or(u64::MAX);
        self.polling_stats.peak_queue_depth = self
            .polling_stats
            .peak_queue_depth
            .max(self.polling_stats.current_queue_depth);

        let oldest = self
            .pending_frame_enqueued_at
            .or_else(|| self.cmd_queue.front().map(|queued| queued.enqueued_at));
        let current = oldest.map_or(Duration::ZERO, |enqueued_at| {
            now.saturating_duration_since(enqueued_at)
        });
        self.queue_age_stats.current_oldest_queue_age = current;
        self.queue_age_stats.peak_oldest_queue_age =
            self.queue_age_stats.peak_oldest_queue_age.max(current);
    }

    fn reset_queue_age_peak_at(&mut self, now: Instant) {
        self.refresh_queue_diagnostics_at(now);
        self.queue_age_stats.peak_oldest_queue_age = self.queue_age_stats.current_oldest_queue_age;
    }
}

fn frame_payload_len(frame: &TransportFrame) -> usize {
    match frame {
        TransportFrame::Text(text) => text.len(),
        TransportFrame::Binary(bytes) => bytes.len(),
    }
}

// ── Debug ───────────────────────────────────────────────────────────

impl<T: Transport> std::fmt::Debug for SignalFishPollingClient<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalFishPollingClient")
            .field("connected", &self.core.is_connected())
            .field("authenticated", &self.core.is_authenticated())
            .field("started", &self.started)
            .field("queued_commands", &self.cmd_queue.len())
            .finish()
    }
}

impl<T: Transport> crate::client_api::SignalFishClientApi for SignalFishPollingClient<T> {
    fn join_room(&mut self, params: JoinRoomParams) -> Result<()> {
        SignalFishPollingClient::join_room(self, params)
    }

    fn leave_room(&mut self) -> Result<()> {
        SignalFishPollingClient::leave_room(self)
    }

    fn send_game_data(&mut self, data: serde_json::Value) -> Result<()> {
        SignalFishPollingClient::send_game_data(self, data)
    }

    fn send_game_data_with_delivery(
        &mut self,
        data: serde_json::Value,
        delivery: GameDataDelivery,
    ) -> Result<()> {
        SignalFishPollingClient::send_game_data_with_delivery(self, data, delivery)
    }

    fn send_binary_game_data(&mut self, payload: Vec<u8>) -> Result<()> {
        SignalFishPollingClient::send_binary_game_data(self, payload)
    }

    fn set_ready(&mut self) -> Result<()> {
        SignalFishPollingClient::set_ready(self)
    }

    fn start_game(&mut self) -> Result<()> {
        SignalFishPollingClient::start_game(self)
    }

    fn request_authority(&mut self, become_authority: bool) -> Result<()> {
        SignalFishPollingClient::request_authority(self, become_authority)
    }

    fn provide_connection_info(&mut self, connection_info: ConnectionInfo) -> Result<()> {
        SignalFishPollingClient::provide_connection_info(self, connection_info)
    }

    fn reconnect(
        &mut self,
        player_id: PlayerId,
        room_id: RoomId,
        auth_token: String,
    ) -> Result<()> {
        SignalFishPollingClient::reconnect(self, player_id, room_id, auth_token)
    }

    fn join_as_spectator(
        &mut self,
        game_name: String,
        room_code: String,
        spectator_name: String,
    ) -> Result<()> {
        SignalFishPollingClient::join_as_spectator(self, game_name, room_code, spectator_name)
    }

    fn leave_spectator(&mut self) -> Result<()> {
        SignalFishPollingClient::leave_spectator(self)
    }

    fn ping(&mut self) -> Result<()> {
        SignalFishPollingClient::ping(self)
    }

    fn send_signal(&mut self, to: PlayerId, signal: PeerSignal) -> Result<()> {
        SignalFishPollingClient::send_signal(self, to, signal)
    }

    fn send_raw_signal(&mut self, to: PlayerId, signal: serde_json::Value) -> Result<()> {
        SignalFishPollingClient::send_raw_signal(self, to, signal)
    }

    fn report_transport_status(&mut self, transport: TransportKind, connected: bool) -> Result<()> {
        SignalFishPollingClient::report_transport_status(self, transport, connected)
    }

    fn send_capacity(&self) -> usize {
        SignalFishPollingClient::send_capacity(self)
    }

    fn max_send_capacity(&self) -> usize {
        SignalFishPollingClient::max_send_capacity(self)
    }

    fn stats(&self) -> crate::client::ClientStats {
        SignalFishPollingClient::stats(self)
    }

    fn snapshot(&self) -> ClientSnapshot {
        SignalFishPollingClient::snapshot(self)
    }

    fn supports_mesh(&self) -> bool {
        SignalFishPollingClient::supports_mesh(self)
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing
)]
mod tests {
    use std::collections::VecDeque;

    use proptest::{prop_assert, prop_assert_eq};

    use super::*;
    use crate::protocol::ServerMessage;
    use crate::transport::TransportFrame;

    // ── Mock transport ──────────────────────────────────────────────

    struct MockTransport {
        incoming: VecDeque<Option<std::result::Result<TransportFrame, SignalFishError>>>,
        sent: Vec<String>,
        _sent_binary: Vec<Vec<u8>>,
        closed: bool,
    }

    impl MockTransport {
        fn new() -> Self {
            Self {
                incoming: VecDeque::new(),
                sent: Vec::new(),
                _sent_binary: Vec::new(),
                closed: false,
            }
        }

        fn with_incoming(
            mut self,
            msgs: Vec<Option<std::result::Result<String, SignalFishError>>>,
        ) -> Self {
            self.incoming = msgs
                .into_iter()
                .map(|item| item.map(|result| result.map(TransportFrame::Text)))
                .collect();
            self
        }

        fn with_frames(mut self, frames: impl IntoIterator<Item = TransportFrame>) -> Self {
            self.incoming = frames.into_iter().map(|frame| Some(Ok(frame))).collect();
            self
        }
    }

    impl Transport for MockTransport {
        fn poll_send(
            &mut self,
            _cx: &mut std::task::Context<'_>,
            frame: &mut Option<TransportFrame>,
        ) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
            if let Some(frame) = frame.take() {
                match frame {
                    TransportFrame::Text(text) => self.sent.push(text),
                    TransportFrame::Binary(bytes) => self._sent_binary.push(bytes),
                }
            }
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_recv(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<std::result::Result<TransportFrame, SignalFishError>>> {
            if let Some(item) = self.incoming.pop_front() {
                std::task::Poll::Ready(item)
            } else {
                std::task::Poll::Pending
            }
        }

        fn poll_close(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
            self.closed = true;
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// A mock transport that can simulate an asynchronous connection handshake.
    /// `is_ready()` returns `self.ready`, which starts as `false`.
    struct NotReadyTransport {
        ready: bool,
        incoming: VecDeque<Option<std::result::Result<String, SignalFishError>>>,
        sent: Vec<String>,
        _sent_binary: Vec<Vec<u8>>,
        /// When true, `recv()` sets `ready = true` before returning,
        /// simulating a transport that becomes ready during the recv drain.
        ready_after_recv: bool,
    }

    impl NotReadyTransport {
        fn new() -> Self {
            Self {
                ready: false,
                incoming: VecDeque::new(),
                sent: Vec::new(),
                _sent_binary: Vec::new(),
                ready_after_recv: false,
            }
        }

        fn with_incoming_and_ready_after_recv(
            msgs: Vec<Option<std::result::Result<String, SignalFishError>>>,
        ) -> Self {
            Self {
                ready: false,
                incoming: msgs.into(),
                sent: Vec::new(),
                _sent_binary: Vec::new(),
                ready_after_recv: true,
            }
        }
    }

    impl Transport for NotReadyTransport {
        fn poll_send(
            &mut self,
            _cx: &mut std::task::Context<'_>,
            frame: &mut Option<TransportFrame>,
        ) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
            if let Some(frame) = frame.take() {
                match frame {
                    TransportFrame::Text(text) => self.sent.push(text),
                    TransportFrame::Binary(bytes) => self._sent_binary.push(bytes),
                }
            }
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_recv(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<std::result::Result<TransportFrame, SignalFishError>>> {
            if let Some(item) = self.incoming.pop_front() {
                if self.ready_after_recv {
                    self.ready = true;
                }
                std::task::Poll::Ready(item.map(|result| result.map(TransportFrame::Text)))
            } else {
                std::task::Poll::Pending
            }
        }

        fn poll_close(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn is_ready(&self) -> bool {
            self.ready
        }
    }

    fn default_config() -> SignalFishConfig {
        SignalFishConfig::new("test_app_id")
    }

    fn enqueue_direct<T: Transport>(
        client: &mut SignalFishPollingClient<T>,
        command: PollingCommand,
    ) {
        let now = Instant::now();
        client.cmd_queue.push_back(QueuedCommand {
            command,
            enqueued_at: now,
        });
        client.refresh_queue_diagnostics_at(now);
    }

    fn accountability_prefix(player_id: PlayerId) -> Vec<TransportFrame> {
        let protocol_info = ServerMessage::ProtocolInfo(protocol_info_v3());
        let room_joined = ServerMessage::RoomJoined(Box::new(crate::protocol::RoomJoinedPayload {
            room_id: uuid::Uuid::from_u128(200),
            room_code: "V3ROOM".into(),
            player_id: uuid::Uuid::from_u128(100),
            game_name: "accountability-test".into(),
            max_players: 4,
            supports_authority: false,
            current_players: vec![crate::protocol::PlayerInfo {
                id: player_id,
                name: "sender".into(),
                is_authority: false,
                is_ready: false,
                connected_at: "2026-01-01T00:00:00Z".into(),
                connection_info: None,
                epoch: Some(1),
                seq: Some(0),
            }],
            is_authority: false,
            lobby_state: crate::protocol::LobbyState::Lobby,
            ready_players: vec![],
            relay_type: "matchbox".into(),
            current_spectators: vec![],
            ice_servers: vec![],
            reconnection_token: Some("rotating-token".into()),
        }));
        [protocol_info, room_joined]
            .into_iter()
            .map(|message| {
                TransportFrame::Text(
                    serde_json::to_string(&message).expect("serialize accountability fixture"),
                )
            })
            .collect()
    }

    #[test]
    fn accountability_policies_apply_to_text_game_data() {
        let player_id = uuid::Uuid::from_u128(300);
        for (policy, expected_data, expected_connected, expected_quarantine) in [
            (
                crate::client::ProtocolViolationPolicy::Quarantine,
                0,
                true,
                true,
            ),
            (
                crate::client::ProtocolViolationPolicy::Disconnect,
                0,
                false,
                false,
            ),
            (
                crate::client::ProtocolViolationPolicy::Observe,
                2,
                true,
                false,
            ),
        ] {
            let mut frames = accountability_prefix(player_id);
            for seq in [2, 1] {
                let message = ServerMessage::GameData {
                    from_player: player_id,
                    data: serde_json::json!({ "seq": seq }),
                    seq: Some(seq),
                    epoch: Some(1),
                    class: Some(crate::protocol::DeliveryClass::Reliable),
                    key: None,
                };
                frames.push(TransportFrame::Text(
                    serde_json::to_string(&message).expect("serialize GameData fixture"),
                ));
            }
            let transport = MockTransport::new().with_frames(frames);
            let config = default_config()
                .enable_v3()
                .with_protocol_violation_policy(policy);
            let mut client = SignalFishPollingClient::new(transport, config);

            let events = client.poll();
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, SignalFishEvent::ProtocolViolation { .. }))
                    .count(),
                1,
                "policy {policy:?}: {events:?}"
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, SignalFishEvent::GameData { .. }))
                    .count(),
                expected_data,
                "policy {policy:?}: {events:?}"
            );
            assert_eq!(
                client.is_connected(),
                expected_connected,
                "policy {policy:?}"
            );
            if policy == crate::client::ProtocolViolationPolicy::Disconnect {
                assert!(
                    client.transport.closed,
                    "Disconnect policy must close the physical transport"
                );
            }
            assert_eq!(
                client.snapshot().quarantined,
                expected_quarantine,
                "policy {policy:?}"
            );
        }
    }

    #[test]
    fn binary_game_data_uses_the_same_accountability_policy() {
        let player_id = uuid::Uuid::from_u128(301);
        let mut frames = accountability_prefix(player_id);
        let invalid_gap = crate::protocol::V3BinaryGameDataFrame {
            from_player: player_id,
            encoding: crate::protocol::GameDataEncoding::MessagePack,
            payload: vec![0xca, 0xfe],
            seq: 2,
            epoch: 1,
        };
        frames.push(TransportFrame::Binary(
            rmp_serde::to_vec_named(&invalid_gap).expect("serialize binary fixture"),
        ));
        let transport = MockTransport::new().with_frames(frames);
        let mut config = default_config().enable_v3();
        config.game_data_format = Some(crate::protocol::GameDataEncoding::MessagePack);
        let mut client = SignalFishPollingClient::new(transport, config);

        let events = client.poll();
        assert!(events
            .iter()
            .any(|event| matches!(event, SignalFishEvent::ProtocolViolation { .. })));
        assert!(!events
            .iter()
            .any(|event| matches!(event, SignalFishEvent::GameDataBinary { .. })));
        assert!(client.snapshot().quarantined);
    }

    #[test]
    fn binary_send_requires_a_negotiated_binary_format() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config().enable_v3());
        let protocol_info = serde_json::to_string(&ServerMessage::ProtocolInfo(protocol_info_v3()))
            .expect("serialize protocol negotiation fixture");
        let _ = client
            .core
            .process_frame(TransportFrame::Text(protocol_info));
        assert!(matches!(
            client.send_binary_game_data(vec![1, 2, 3]),
            Err(SignalFishError::BinaryFormatNotNegotiated)
        ));

        let transport = MockTransport::new();
        let mut config = default_config().enable_v3();
        config.game_data_format = Some(GameDataEncoding::MessagePack);
        let mut client = SignalFishPollingClient::new(transport, config);
        let protocol_info = serde_json::to_string(&ServerMessage::ProtocolInfo(protocol_info_v3()))
            .expect("serialize protocol negotiation fixture");
        let _ = client
            .core
            .process_frame(TransportFrame::Text(protocol_info));
        client
            .send_binary_game_data(vec![1, 2, 3])
            .expect("MessagePack negotiation permits binary sends");
    }

    #[test]
    fn text_binary_envelope_is_rejected_in_json_mode() {
        let from = uuid::Uuid::from_u128(302);
        let message = ServerMessage::GameDataBinary {
            from_player: from,
            encoding: GameDataEncoding::MessagePack,
            payload: vec![1, 2, 3],
            seq: None,
            epoch: None,
        };
        let transport = MockTransport::new().with_frames(vec![TransportFrame::Text(
            serde_json::to_string(&message).expect("serialize fixture"),
        )]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();
        assert!(events
            .iter()
            .any(|event| matches!(event, SignalFishEvent::ProtocolViolation { .. })));
        assert!(!events
            .iter()
            .any(|event| matches!(event, SignalFishEvent::GameDataBinary { .. })));
    }

    #[test]
    fn v2_message_pack_binary_envelope_is_delivered() {
        let from = uuid::Uuid::from_u128(303);
        let frame = crate::protocol::V2BinaryGameDataFrame {
            from_player: from,
            encoding: GameDataEncoding::MessagePack,
            payload: vec![1, 2, 3],
        };
        let transport = MockTransport::new().with_frames(vec![
            TransportFrame::Text(PROTOCOL_INFO_V2.into()),
            TransportFrame::Binary(
                rmp_serde::to_vec_named(&frame).expect("serialize v2 binary fixture"),
            ),
        ]);
        let mut config = default_config();
        config.game_data_format = Some(GameDataEncoding::MessagePack);
        let mut client = SignalFishPollingClient::new(transport, config);
        let events = client.poll();
        assert!(events.iter().any(|event| matches!(
            event,
            SignalFishEvent::GameDataBinary {
                from_player,
                seq: None,
                epoch: None,
                ..
            } if *from_player == from
        )));
    }

    #[test]
    fn json_mode_rejects_physical_binary_before_decode_for_every_policy() {
        for (policy, connected, quarantined, closed) in [
            (
                crate::client::ProtocolViolationPolicy::Quarantine,
                true,
                true,
                false,
            ),
            (
                crate::client::ProtocolViolationPolicy::Disconnect,
                false,
                false,
                true,
            ),
            (
                crate::client::ProtocolViolationPolicy::Observe,
                true,
                false,
                false,
            ),
        ] {
            let transport =
                MockTransport::new().with_frames(vec![TransportFrame::Binary(vec![0xff, 0x00])]);
            let config = default_config().with_protocol_violation_policy(policy);
            let mut client = SignalFishPollingClient::new(transport, config);
            let events = client.poll();
            assert!(events
                .iter()
                .any(|event| matches!(event, SignalFishEvent::ProtocolViolation { .. })));
            assert_eq!(
                events
                    .iter()
                    .any(|event| matches!(event, SignalFishEvent::DecodeFailed { .. })),
                policy == crate::client::ProtocolViolationPolicy::Observe,
                "Observe continues decoding for diagnostics; enforcing policies stop first"
            );
            assert_eq!(client.is_connected(), connected);
            assert_eq!(client.snapshot().quarantined, quarantined);
            assert_eq!(client.transport.closed, closed);
        }
    }

    #[test]
    fn observe_delivers_valid_wrong_representation_and_advances_sequence() {
        let player_id = uuid::Uuid::from_u128(305);
        let mut frames = accountability_prefix(player_id);
        let binary = crate::protocol::V3BinaryGameDataFrame {
            from_player: player_id,
            encoding: GameDataEncoding::MessagePack,
            payload: vec![1, 2, 3],
            seq: 1,
            epoch: 1,
        };
        frames.push(TransportFrame::Binary(
            rmp_serde::to_vec_named(&binary).expect("serialize binary fixture"),
        ));
        frames.push(TransportFrame::Text(
            serde_json::to_string(&ServerMessage::GameData {
                from_player: player_id,
                data: serde_json::json!({"seq": 2}),
                seq: Some(2),
                epoch: Some(1),
                class: Some(crate::protocol::DeliveryClass::Reliable),
                key: None,
            })
            .expect("serialize following JSON fixture"),
        ));
        let transport = MockTransport::new().with_frames(frames);
        let config = default_config()
            .enable_v3()
            .with_protocol_violation_policy(crate::client::ProtocolViolationPolicy::Observe);
        let mut client = SignalFishPollingClient::new(transport, config);
        let events = client.poll();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, SignalFishEvent::ProtocolViolation { .. }))
                .count(),
            1,
            "representation mismatch should produce exactly one violation: {events:?}"
        );
        assert!(events
            .iter()
            .any(|event| matches!(event, SignalFishEvent::GameDataBinary { seq: Some(1), .. })));
        assert!(events
            .iter()
            .any(|event| matches!(event, SignalFishEvent::GameData { seq: Some(2), .. })));
    }

    #[test]
    fn quarantine_suppresses_invalid_lifecycle_and_duplicate_protocol_info_is_ignored() {
        let player_id = uuid::Uuid::from_u128(304);
        let mut invalid_frames = accountability_prefix(player_id);
        invalid_frames.push(TransportFrame::Text(
            serde_json::to_string(&ServerMessage::PlayerLeft {
                player_id,
                epoch: None,
                final_seq: None,
            })
            .expect("serialize invalid lifecycle fixture"),
        ));
        let transport = MockTransport::new().with_frames(invalid_frames);
        let mut client = SignalFishPollingClient::new(transport, default_config().enable_v3());
        let events = client.poll();
        assert!(events
            .iter()
            .any(|event| matches!(event, SignalFishEvent::ProtocolViolation { .. })));
        assert!(!events
            .iter()
            .any(|event| matches!(event, SignalFishEvent::PlayerLeft { .. })));

        let mut duplicate_frames = accountability_prefix(player_id);
        duplicate_frames.push(TransportFrame::Text(PROTOCOL_INFO_V3.into()));
        duplicate_frames.push(TransportFrame::Text(
            serde_json::to_string(&ServerMessage::GameData {
                from_player: player_id,
                data: serde_json::json!({"valid": true}),
                seq: Some(1),
                epoch: Some(1),
                class: Some(crate::protocol::DeliveryClass::Reliable),
                key: None,
            })
            .expect("serialize valid data fixture"),
        ));
        let transport = MockTransport::new().with_frames(duplicate_frames);
        let mut client = SignalFishPollingClient::new(transport, default_config().enable_v3());
        let events = client.poll();
        assert!(!events
            .iter()
            .any(|event| matches!(event, SignalFishEvent::ProtocolViolation { .. })));
        assert!(events
            .iter()
            .any(|event| matches!(event, SignalFishEvent::GameData { .. })));
    }

    // ── Test cases ──────────────────────────────────────────────────

    #[test]
    fn poll_emits_connected_on_first_call() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());

        let events = client.poll();

        assert!(!events.is_empty());
        assert!(
            matches!(events[0], SignalFishEvent::Connected),
            "first event should be Connected, got: {:?}",
            events[0]
        );

        // Second poll must NOT re-emit Connected.
        let events2 = client.poll();
        assert!(
            !events2
                .iter()
                .any(|e| matches!(e, SignalFishEvent::Connected)),
            "second poll should not contain Connected, got: {events2:?}"
        );
    }

    #[test]
    fn poll_sends_authenticate_on_first_poll() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());

        client.poll();

        // The authenticate message should have been sent via the transport.
        assert!(
            !client.transport.sent.is_empty(),
            "expected at least one sent message"
        );
        let sent_json: serde_json::Value = serde_json::from_str(&client.transport.sent[0])
            .expect("first sent message must be valid JSON");
        assert_eq!(sent_json["type"], "Authenticate");
        assert_eq!(sent_json["data"]["app_id"], "test_app_id");
        // Relay floor on the client-produced path (polling client): no v3 keys.
        assert!(sent_json["data"].get("protocol_version").is_none());
        assert!(sent_json["data"].get("supported_transports").is_none());
        assert!(sent_json["data"].get("supported_topologies").is_none());
    }

    #[test]
    fn poll_with_enable_mesh_advertises_v3_on_the_wire() {
        let transport = MockTransport::new();
        let config = SignalFishConfig::new("mb_mesh").enable_mesh();
        let mut client = SignalFishPollingClient::new(transport, config);

        client.poll();

        let sent_json: serde_json::Value = serde_json::from_str(&client.transport.sent[0])
            .expect("first sent message must be valid JSON");
        assert_eq!(sent_json["data"]["protocol_version"], 3);
        assert_eq!(
            sent_json["data"]["supported_transports"],
            serde_json::json!(["webrtc", "relay"])
        );
        assert_eq!(
            sent_json["data"]["supported_topologies"],
            serde_json::json!(["mesh", "host", "relay"])
        );
    }

    // ── Protocol v2/v3: start_game, signaling, negotiation guard ────

    const PROTOCOL_INFO_V3: &str = r#"{"type":"ProtocolInfo","data":{"capabilities":[],"game_data_formats":[],"protocol_version":3,"min_protocol_version":2,"max_protocol_version":3}}"#;
    // A v2 negotiation omits the version fields entirely (so the bytes stay
    // identical to a v2 server), which deserializes to `protocol_version: None`.
    const PROTOCOL_INFO_V2: &str =
        r#"{"type":"ProtocolInfo","data":{"capabilities":[],"game_data_formats":[]}}"#;
    const PEER_UUID: &str = "00000000-0000-0000-0000-000000000007";

    /// Parse every queued outgoing frame into a `ClientMessage`.
    ///
    /// Each frame MUST deserialize cleanly: silently dropping unparsable
    /// frames would let assertions like "no `Signal` reached the wire" pass
    /// against a malformed shape they never saw. A parse failure is a real
    /// client bug, so surface it loudly.
    fn last_sent(client: &SignalFishPollingClient<MockTransport>) -> Vec<ClientMessage> {
        client
            .transport
            .sent
            .iter()
            .map(|m| {
                serde_json::from_str::<ClientMessage>(m).unwrap_or_else(|e| {
                    panic!("outgoing client message must deserialize: {e}\n{m}")
                })
            })
            .collect()
    }

    #[test]
    fn start_game_queues_start_game_message() {
        let transport = MockTransport::new()
            .with_incoming(vec![Some(Ok(authenticated_json_str().to_string()))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll();
        client.start_game().expect("start_game");
        client.poll();
        assert!(last_sent(&client)
            .iter()
            .any(|m| matches!(m, ClientMessage::StartGame)));
    }

    #[test]
    fn send_signal_before_v3_returns_protocol_unsupported() {
        let transport = MockTransport::new()
            .with_incoming(vec![Some(Ok(authenticated_json_str().to_string()))]);
        let mut client = SignalFishPollingClient::new(transport, default_config().enable_mesh());
        client.poll();
        assert!(client.negotiated_protocol_version().is_none());
        assert!(!client.supports_mesh());
        let peer: PlayerId = PEER_UUID.parse().unwrap();
        // Authenticated but no `ProtocolInfo` yet → negotiation in flight, so
        // the guard reports "pre-negotiation" (NOT "relay-only", which requires
        // an observed v2 `ProtocolInfo` — see `v2_protocol_info_is_relay_only`).
        assert!(matches!(
            client.send_offer(peer, "sdp"),
            Err(SignalFishError::ProtocolUnsupported {
                mode: "pre-negotiation"
            })
        ));
        // Nothing v3 reached the wire (the guard runs before enqueue).
        client.poll();
        assert!(last_sent(&client)
            .iter()
            .all(|m| !matches!(m, ClientMessage::Signal { .. })));
    }

    #[test]
    fn v2_protocol_info_is_relay_only() {
        // A v2 `ProtocolInfo` (no version field) is a terminal relay floor: the
        // guard reports "relay-only", distinct from the "pre-negotiation" state
        // before any `ProtocolInfo` arrives.
        let transport = MockTransport::new().with_incoming(vec![
            Some(Ok(authenticated_json_str().to_string())),
            Some(Ok(PROTOCOL_INFO_V2.to_string())),
        ]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll();
        assert!(client.negotiated_protocol_version().is_none());
        let peer: PlayerId = PEER_UUID.parse().unwrap();
        assert!(matches!(
            client.send_offer(peer, "sdp"),
            Err(SignalFishError::ProtocolUnsupported { mode: "relay-only" })
        ));
    }

    #[test]
    fn disconnect_resets_negotiation_and_commands_return_not_connected() {
        // A closed connection takes precedence over protocol guards while the
        // cleared snapshot still proves negotiation state was reset.
        let transport = MockTransport::new().with_incoming(vec![
            Some(Ok(authenticated_json_str().to_string())),
            Some(Ok(PROTOCOL_INFO_V3.to_string())),
        ]);
        let mut client = SignalFishPollingClient::new(transport, default_config().enable_mesh());
        client.poll();
        assert!(client.supports_mesh());
        client.close();
        assert!(client.negotiated_protocol_version().is_none());
        let peer: PlayerId = PEER_UUID.parse().unwrap();
        assert!(matches!(
            client.send_offer(peer, "sdp"),
            Err(SignalFishError::NotConnected)
        ));
    }

    #[test]
    fn send_signal_before_authentication_is_pre_negotiation() {
        // The `mode: "pre-negotiation"` branch: no poll/auth before the send.
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let peer: PlayerId = PEER_UUID.parse().unwrap();
        assert!(matches!(
            client.send_offer(peer, "sdp"),
            Err(SignalFishError::ProtocolUnsupported {
                mode: "pre-negotiation"
            })
        ));
    }

    #[test]
    fn send_signal_after_v3_is_queued() {
        let transport = MockTransport::new().with_incoming(vec![
            Some(Ok(authenticated_json_str().to_string())),
            Some(Ok(PROTOCOL_INFO_V3.to_string())),
        ]);
        let mut client = SignalFishPollingClient::new(transport, default_config().enable_mesh());
        client.poll();
        assert_eq!(client.negotiated_protocol_version(), Some(3));
        assert!(client.supports_mesh());
        let peer: PlayerId = PEER_UUID.parse().unwrap();
        client.send_offer(peer, "the-sdp").expect("send_offer");
        client.poll();
        let signal = last_sent(&client).into_iter().find_map(|m| match m {
            ClientMessage::Signal { to, signal } if to == peer => Some(signal),
            _ => None,
        });
        assert_eq!(signal, Some(serde_json::json!({ "Offer": "the-sdp" })));
    }

    #[test]
    fn send_answer_ice_and_raw_signal_wire_shapes() {
        let transport = MockTransport::new().with_incoming(vec![
            Some(Ok(authenticated_json_str().to_string())),
            Some(Ok(PROTOCOL_INFO_V3.to_string())),
        ]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll();
        let peer: PlayerId = PEER_UUID.parse().unwrap();
        client.send_answer(peer, "ans").expect("send_answer");
        client.send_ice_candidate(peer, "cand").expect("send_ice");
        client
            .send_raw_signal(peer, serde_json::json!({ "Renegotiate": true }))
            .expect("send_raw_signal");
        client.poll();
        let signals: Vec<serde_json::Value> = last_sent(&client)
            .into_iter()
            .filter_map(|m| match m {
                ClientMessage::Signal { to, signal } if to == peer => Some(signal),
                _ => None,
            })
            .collect();
        assert!(signals.contains(&serde_json::json!({ "Answer": "ans" })));
        assert!(signals.contains(&serde_json::json!({ "IceCandidate": "cand" })));
        assert!(signals.contains(&serde_json::json!({ "Renegotiate": true })));
    }

    #[test]
    fn report_transport_status_after_v3_is_queued() {
        let transport = MockTransport::new().with_incoming(vec![
            Some(Ok(authenticated_json_str().to_string())),
            Some(Ok(PROTOCOL_INFO_V3.to_string())),
        ]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll();
        client
            .report_transport_status(TransportKind::WebRtc, true)
            .expect("report_transport_status");
        client.poll();
        assert!(last_sent(&client).iter().any(|m| matches!(
            m,
            ClientMessage::TransportStatus {
                transport: TransportKind::WebRtc,
                connected: true
            }
        )));
    }

    #[test]
    fn negotiated_version_resets_on_disconnect() {
        let transport = MockTransport::new().with_incoming(vec![
            Some(Ok(authenticated_json_str().to_string())),
            Some(Ok(PROTOCOL_INFO_V3.to_string())),
        ]);
        let mut client = SignalFishPollingClient::new(transport, default_config().enable_mesh());
        client.poll();
        assert!(client.supports_mesh());
        client.close();
        assert_eq!(client.negotiated_protocol_version(), None);
        assert!(!client.supports_mesh());
    }

    #[test]
    fn reconnect_restores_negotiated_version_from_missed_events() {
        use crate::protocol::{LobbyState, ReconnectedPayload};
        let payload = ReconnectedPayload {
            room_id: uuid::Uuid::from_u128(100),
            room_code: "R".into(),
            player_id: uuid::Uuid::from_u128(200),
            game_name: "g".into(),
            max_players: 4,
            supports_authority: false,
            current_players: vec![],
            is_authority: false,
            lobby_state: LobbyState::Waiting,
            ready_players: vec![],
            relay_type: "tcp".into(),
            current_spectators: vec![],
            ice_servers: vec![],
            missed_events: vec![ServerMessage::ProtocolInfo(protocol_info_v3())],
            replay: None,
            sender_watermarks: vec![],
            reconnection_token: None,
        };
        let reconnected =
            serde_json::to_string(&ServerMessage::Reconnected(Box::new(payload))).unwrap();
        let transport = MockTransport::new().with_incoming(vec![
            Some(Ok(authenticated_json_str().to_string())),
            Some(Ok(reconnected)),
        ]);
        let mut client = SignalFishPollingClient::new(transport, default_config().enable_mesh());
        client.poll();
        assert_eq!(client.negotiated_protocol_version(), Some(3));
        assert!(client.supports_mesh());
        // A v3 send now succeeds after the reconnect.
        let peer: PlayerId = PEER_UUID.parse().unwrap();
        client
            .send_offer(peer, "sdp")
            .expect("send after reconnect");
        client.poll();
        assert!(last_sent(&client)
            .iter()
            .any(|m| matches!(m, ClientMessage::Signal { .. })));
    }

    #[test]
    fn v4_negotiation_still_enables_mesh() {
        // `>= 3` (not `== 3`): a future v4 negotiation must still enable mesh.
        let pi_v4 = r#"{"type":"ProtocolInfo","data":{"capabilities":[],"game_data_formats":[],"protocol_version":4,"min_protocol_version":2,"max_protocol_version":4}}"#;
        let transport = MockTransport::new().with_incoming(vec![
            Some(Ok(authenticated_json_str().to_string())),
            Some(Ok(pi_v4.to_string())),
        ]);
        let mut client = SignalFishPollingClient::new(transport, default_config().enable_mesh());
        client.poll();
        assert_eq!(client.negotiated_protocol_version(), Some(4));
        assert!(client.supports_mesh());
        let peer: PlayerId = PEER_UUID.parse().unwrap();
        client.send_offer(peer, "sdp").expect("v4 must enable mesh");
    }

    /// A `ProtocolInfoPayload` negotiating v3 (for missed_events fixtures).
    fn protocol_info_v3() -> crate::protocol::ProtocolInfoPayload {
        crate::protocol::ProtocolInfoPayload {
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
            transports: None,
        }
    }

    #[test]
    fn poll_emits_session_plan_and_signal_events() {
        let session_plan = format!(
            r#"{{"type":"SessionPlan","data":{{"topology":"mesh","transport":"webrtc","peers":[{{"player_id":"{PEER_UUID}","player_name":"P","is_authority":false,"initiate":true}}],"fallback":"relay"}}}}"#
        );
        let signal = format!(
            r#"{{"type":"Signal","data":{{"from":"{PEER_UUID}","signal":{{"Offer":"remote-sdp"}}}}}}"#
        );
        let transport = MockTransport::new().with_incoming(vec![
            Some(Ok(authenticated_json_str().to_string())),
            Some(Ok(PROTOCOL_INFO_V3.to_string())),
            Some(Ok(session_plan)),
            Some(Ok(signal)),
        ]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        let peer: PlayerId = PEER_UUID.parse().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            SignalFishEvent::SessionPlan { peers, .. }
                if peers.len() == 1 && peers[0].player_id == peer && peers[0].initiate
        )));
        // Decode the received signal payload (not just match the variant).
        let received = events.iter().find_map(|e| match e {
            SignalFishEvent::SignalReceived { from, signal } if *from == peer => {
                Some(PeerSignal::try_from(signal).expect("typed signal"))
            }
            _ => None,
        });
        assert_eq!(received, Some(PeerSignal::Offer("remote-sdp".into())));
    }

    #[test]
    fn poll_emits_new_peer_and_peer_transport_status_events() {
        let new_peer = format!(
            r#"{{"type":"NewPeer","data":{{"peer_id":"{PEER_UUID}","you_initiate":true}}}}"#
        );
        let status = format!(
            r#"{{"type":"PeerTransportStatus","data":{{"peer_id":"{PEER_UUID}","transport":"webrtc","connected":true}}}}"#
        );
        let transport = MockTransport::new().with_incoming(vec![
            Some(Ok(authenticated_json_str().to_string())),
            Some(Ok(new_peer)),
            Some(Ok(status)),
        ]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        let peer: PlayerId = PEER_UUID.parse().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            SignalFishEvent::NewPeer { peer_id, you_initiate: true } if *peer_id == peer
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            SignalFishEvent::PeerTransportStatus { peer_id, transport: TransportKind::WebRtc, connected: true }
                if *peer_id == peer
        )));
    }

    #[test]
    fn poll_surfaces_unknown_message_type_then_next_arrives() {
        // A well-formed but unknown `type` surfaces as DecodeFailed carrying
        // the wire tag (distinct from malformed JSON, whose tag is None), and
        // the following valid message still arrives.
        let transport = MockTransport::new().with_incoming(vec![
            Some(Ok(authenticated_json_str().to_string())),
            Some(Ok(r#"{"type":"SomeFutureV4Message","data":{}}"#.to_string())),
            Some(Ok(r#"{"type":"Pong"}"#.to_string())),
        ]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();
        let decode_failed_at = events.iter().position(|e| {
            matches!(
                e,
                SignalFishEvent::DecodeFailed { message_type, .. }
                    if message_type.as_deref() == Some("SomeFutureV4Message")
            )
        });
        let pong_at = events
            .iter()
            .position(|e| matches!(e, SignalFishEvent::Pong));
        assert!(
            decode_failed_at.is_some(),
            "expected DecodeFailed with the wire tag, got: {events:?}"
        );
        assert!(pong_at.is_some(), "expected Pong, got: {events:?}");
        assert!(
            decode_failed_at < pong_at,
            "DecodeFailed must precede the following Pong"
        );
        assert_eq!(client.stats().messages_undecodable, 1);
        assert!(client.is_connected());
    }

    fn authenticated_json_str() -> &'static str {
        r#"{"type":"Authenticated","data":{"app_name":"test","rate_limits":{"per_minute":60,"per_hour":1000,"per_day":10000}}}"#
    }

    #[test]
    fn stats_count_game_data_sent_and_received() {
        let game_data_json = |seq: u64| {
            serde_json::to_string(&ServerMessage::GameData {
                from_player: uuid::Uuid::from_u128(9),
                data: serde_json::json!({ "seq": seq }),
                seq: None,
                epoch: None,
                class: None,
                key: None,
            })
            .expect("GameData serializes")
        };
        let transport = MockTransport::new().with_incoming(vec![
            Some(Ok(authenticated_json_str().to_string())),
            Some(Ok(game_data_json(0))),
            Some(Ok(game_data_json(1))),
        ]);
        let mut client = SignalFishPollingClient::new(transport, default_config());

        assert_eq!(client.stats(), crate::client::ClientStats::default());

        for seq in 0..3 {
            client
                .send_game_data(serde_json::json!({ "seq": seq }))
                .unwrap();
        }
        let _ = client.poll();

        // Authenticate + 3 GameData flushed; only GameData counts as sent.
        assert_eq!(
            client.stats(),
            crate::client::ClientStats {
                game_data_sent: 3,
                game_data_received: 2,
                messages_undecodable: 0,
            }
        );
    }

    #[test]
    fn poll_receives_and_deserializes_messages() {
        let authenticated_json = r#"{"type":"Authenticated","data":{"app_name":"test","rate_limits":{"per_minute":60,"per_hour":1000,"per_day":10000}}}"#;
        let room_joined_json = r#"{"type":"RoomJoined","data":{"room_id":"00000000-0000-0000-0000-000000000001","room_code":"ABC123","player_id":"00000000-0000-0000-0000-000000000002","game_name":"test-game","max_players":4,"supports_authority":false,"current_players":[],"is_authority":false,"lobby_state":"waiting","ready_players":[],"relay_type":"websocket","current_spectators":[]}}"#;

        let transport = MockTransport::new().with_incoming(vec![
            Some(Ok(authenticated_json.to_string())),
            Some(Ok(room_joined_json.to_string())),
        ]);
        let mut client = SignalFishPollingClient::new(transport, default_config());

        let events = client.poll();

        // Should contain: Connected, Authenticated, RoomJoined
        assert_eq!(events.len(), 3, "expected 3 events, got: {events:?}");
        assert!(matches!(events[0], SignalFishEvent::Connected));
        assert!(matches!(events[1], SignalFishEvent::Authenticated { .. }));
        assert!(matches!(events[2], SignalFishEvent::RoomJoined { .. }));
    }

    #[test]
    fn state_updates_on_authenticated() {
        let authenticated_json = r#"{"type":"Authenticated","data":{"app_name":"test","rate_limits":{"per_minute":60,"per_hour":1000,"per_day":10000}}}"#;

        let transport =
            MockTransport::new().with_incoming(vec![Some(Ok(authenticated_json.to_string()))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());

        assert!(!client.is_authenticated());
        client.poll();
        assert!(client.is_authenticated());
    }

    #[test]
    fn state_updates_on_room_joined() {
        let room_joined_json = r#"{"type":"RoomJoined","data":{"room_id":"00000000-0000-0000-0000-000000000001","room_code":"ABC123","player_id":"00000000-0000-0000-0000-000000000002","game_name":"test-game","max_players":4,"supports_authority":false,"current_players":[],"is_authority":false,"lobby_state":"waiting","ready_players":[],"relay_type":"websocket","current_spectators":[]}}"#;

        let transport =
            MockTransport::new().with_incoming(vec![Some(Ok(room_joined_json.to_string()))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());

        assert!(client.current_room_id().is_none());
        assert!(client.current_room_code().is_none());

        client.poll();

        let expected_room_id: uuid::Uuid = "00000000-0000-0000-0000-000000000001"
            .parse()
            .expect("test room_id UUID must parse");
        let expected_player_id: uuid::Uuid = "00000000-0000-0000-0000-000000000002"
            .parse()
            .expect("test player_id UUID must parse");
        assert_eq!(client.current_room_id(), Some(expected_room_id));
        assert_eq!(client.current_room_code(), Some("ABC123"));
        assert_eq!(client.current_player_id(), Some(expected_player_id));
    }

    #[test]
    fn join_room_queues_command() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());

        // First poll to send the authenticate message and emit Connected.
        client.poll();

        // Now join a room.
        client
            .join_room(JoinRoomParams::new("test-game", "Alice"))
            .expect("join_room must succeed on connected client");

        // Poll again to flush the join_room command.
        client.poll();

        // The last sent message should be a JoinRoom command.
        let last_sent = client
            .transport
            .sent
            .last()
            .expect("transport must have at least one sent message");
        let sent_json: serde_json::Value =
            serde_json::from_str(last_sent).expect("sent message must be valid JSON");
        assert_eq!(sent_json["type"], "JoinRoom");
        assert_eq!(sent_json["data"]["game_name"], "test-game");
        assert_eq!(sent_json["data"]["player_name"], "Alice");
    }

    #[test]
    fn send_fails_when_disconnected() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());

        client.close();

        let result = client.join_room(JoinRoomParams::new("test-game", "Alice"));
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), SignalFishError::NotConnected),
            "expected NotConnected error"
        );
    }

    #[test]
    fn poll_handles_transport_close() {
        // Transport returns None (clean close).
        let transport = MockTransport::new().with_incoming(vec![None]);
        let mut client = SignalFishPollingClient::new(transport, default_config());

        let events = client.poll();

        // Should contain Connected + Disconnected.
        let disconnected = events
            .iter()
            .any(|e| matches!(e, SignalFishEvent::Disconnected { .. }));
        assert!(disconnected, "expected Disconnected event, got: {events:?}");
        assert!(!client.is_connected());
        assert!(!client.is_authenticated());
    }

    #[test]
    fn poll_handles_transport_error() {
        // Transport returns an error.
        let transport = MockTransport::new().with_incoming(vec![Some(Err(
            SignalFishError::TransportReceive("connection reset".into()),
        ))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());

        let events = client.poll();

        let disconnected = events
            .iter()
            .any(|e| matches!(e, SignalFishEvent::Disconnected { .. }));
        assert!(disconnected, "expected Disconnected event, got: {events:?}");
        assert!(!client.is_connected());
        assert!(!client.is_authenticated());
    }

    #[test]
    fn connected_is_always_first_event_even_with_immediate_messages() {
        // Verifies that the synthetic Connected event is always the first event
        // in the first poll() result, even when the transport has messages
        // already buffered. This is important because Connected is emitted
        // before the recv drain loop, and callers may rely on this ordering.
        let authenticated_json = r#"{"type":"Authenticated","data":{"app_name":"test","rate_limits":{"per_minute":60,"per_hour":1000,"per_day":10000}}}"#;

        let transport =
            MockTransport::new().with_incoming(vec![Some(Ok(authenticated_json.to_string()))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());

        let events = client.poll();

        assert!(
            events.len() >= 2,
            "expected at least 2 events, got: {events:?}"
        );
        assert!(
            matches!(events[0], SignalFishEvent::Connected),
            "first event must always be Connected, got: {:?}",
            events[0]
        );
        // Connected must come before any server-derived events.
        for (i, event) in events.iter().enumerate().skip(1) {
            assert!(
                !matches!(event, SignalFishEvent::Connected),
                "Connected must only appear once and at index 0, but found at index {i}"
            );
        }
    }

    #[test]
    fn poll_defers_connected_when_transport_not_ready() {
        // A transport that reports not-ready should cause poll() to
        // defer the Connected event until is_ready() returns true.
        let transport = NotReadyTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());

        // First poll: transport is not ready, so no Connected.
        let events = client.poll();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SignalFishEvent::Connected)),
            "Connected should not be emitted when transport is not ready, got: {events:?}"
        );

        // Mark transport as ready.
        client.transport.ready = true;

        // Next poll: transport is now ready, Connected should appear.
        let events = client.poll();
        assert!(
            matches!(events.first(), Some(SignalFishEvent::Connected)),
            "Connected should be emitted once transport becomes ready, got: {events:?}"
        );

        // Subsequent poll: no duplicate Connected.
        let events = client.poll();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SignalFishEvent::Connected)),
            "Connected should not be re-emitted, got: {events:?}"
        );
    }

    #[test]
    fn poll_emits_connected_at_position_zero_after_recv() {
        // When the transport becomes ready during the recv drain (simulated
        // by a transport that becomes ready after recv is called), Connected
        // should still be at position 0, before any server messages.
        let authenticated_json = r#"{"type":"Authenticated","data":{"app_name":"test","rate_limits":{"per_minute":60,"per_hour":1000,"per_day":10000}}}"#;

        let transport = NotReadyTransport::with_incoming_and_ready_after_recv(vec![Some(Ok(
            authenticated_json.to_string(),
        ))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());

        let events = client.poll();

        assert!(
            events.len() >= 2,
            "expected at least 2 events, got: {events:?}"
        );
        assert!(
            matches!(events[0], SignalFishEvent::Connected),
            "Connected should be at index 0, got: {:?}",
            events[0]
        );
        assert!(
            matches!(events[1], SignalFishEvent::Authenticated { .. }),
            "Authenticated should follow Connected, got: {:?}",
            events[1]
        );
    }

    #[test]
    fn close_is_idempotent() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());

        client.close();
        assert!(!client.is_connected());
        assert!(client.transport.closed);

        // Second close should not panic.
        client.close();
        assert!(!client.is_connected());
    }

    #[test]
    fn state_updates_on_room_left() {
        let room_joined_json = r#"{"type":"RoomJoined","data":{"room_id":"00000000-0000-0000-0000-000000000001","room_code":"ABC123","player_id":"00000000-0000-0000-0000-000000000002","game_name":"test-game","max_players":4,"supports_authority":false,"current_players":[],"is_authority":false,"lobby_state":"waiting","ready_players":[],"relay_type":"websocket","current_spectators":[]}}"#;
        let room_left_json = r#"{"type":"RoomLeft"}"#;

        let transport = MockTransport::new().with_incoming(vec![
            Some(Ok(room_joined_json.to_string())),
            Some(Ok(room_left_json.to_string())),
        ]);
        let mut client = SignalFishPollingClient::new(transport, default_config());

        client.poll();

        assert!(client.current_room_id().is_none());
        assert!(client.current_room_code().is_none());
        assert!(client.current_player_id().is_some());
    }

    #[test]
    fn state_updates_on_reconnected() {
        let reconnected_json = r#"{"type":"Reconnected","data":{"room_id":"00000000-0000-0000-0000-000000000001","room_code":"RECON1","player_id":"00000000-0000-0000-0000-000000000003","game_name":"test-game","max_players":4,"supports_authority":false,"current_players":[],"is_authority":false,"lobby_state":"waiting","ready_players":[],"relay_type":"websocket","current_spectators":[],"missed_events":[]}}"#;

        let transport =
            MockTransport::new().with_incoming(vec![Some(Ok(reconnected_json.to_string()))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());

        let events = client.poll();

        // Verify the Reconnected event is emitted.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SignalFishEvent::Reconnected { .. })),
            "expected Reconnected event, got: {events:?}"
        );

        let expected_player_id: uuid::Uuid = "00000000-0000-0000-0000-000000000003"
            .parse()
            .expect("test player_id UUID must parse");
        let expected_room_id: uuid::Uuid = "00000000-0000-0000-0000-000000000001"
            .parse()
            .expect("test room_id UUID must parse");
        assert_eq!(client.current_player_id(), Some(expected_player_id));
        assert_eq!(client.current_room_id(), Some(expected_room_id));
        assert_eq!(client.current_room_code(), Some("RECON1"));
    }

    #[test]
    fn state_updates_on_spectator_joined() {
        let spectator_joined_json = r#"{"type":"SpectatorJoined","data":{"room_id":"00000000-0000-0000-0000-000000000001","room_code":"SPEC1","spectator_id":"00000000-0000-0000-0000-000000000004","game_name":"test-game","current_players":[],"current_spectators":[],"lobby_state":"waiting"}}"#;

        let transport =
            MockTransport::new().with_incoming(vec![Some(Ok(spectator_joined_json.to_string()))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());

        client.poll();

        let expected_player_id: uuid::Uuid = "00000000-0000-0000-0000-000000000004"
            .parse()
            .expect("test spectator_id UUID must parse");
        let expected_room_id: uuid::Uuid = "00000000-0000-0000-0000-000000000001"
            .parse()
            .expect("test room_id UUID must parse");
        assert_eq!(client.current_player_id(), Some(expected_player_id));
        assert_eq!(client.current_room_id(), Some(expected_room_id));
        assert_eq!(client.current_room_code(), Some("SPEC1"));
    }

    #[test]
    fn state_updates_on_spectator_left() {
        let spectator_joined_json = r#"{"type":"SpectatorJoined","data":{"room_id":"00000000-0000-0000-0000-000000000001","room_code":"SPEC1","spectator_id":"00000000-0000-0000-0000-000000000004","game_name":"test-game","current_players":[],"current_spectators":[],"lobby_state":"waiting"}}"#;
        let spectator_left_json = r#"{"type":"SpectatorLeft","data":{"current_spectators":[]}}"#;

        let transport = MockTransport::new().with_incoming(vec![
            Some(Ok(spectator_joined_json.to_string())),
            Some(Ok(spectator_left_json.to_string())),
        ]);
        let mut client = SignalFishPollingClient::new(transport, default_config());

        let events = client.poll();

        // Verify the SpectatorLeft event is emitted.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SignalFishEvent::SpectatorLeft { .. })),
            "expected SpectatorLeft event, got: {events:?}"
        );

        assert!(client.current_room_id().is_none());
        assert!(client.current_room_code().is_none());
        assert!(client.current_player_id().is_some());
    }

    #[test]
    fn poll_handles_malformed_json() {
        let malformed_json = "not valid json {{{";
        let authenticated_json = r#"{"type":"Authenticated","data":{"app_name":"test","rate_limits":{"per_minute":60,"per_hour":1000,"per_day":10000}}}"#;

        let transport = MockTransport::new().with_incoming(vec![
            Some(Ok(malformed_json.to_string())),
            Some(Ok(authenticated_json.to_string())),
        ]);
        let mut client = SignalFishPollingClient::new(transport, default_config());

        let events = client.poll();

        // Connected, then the malformed frame surfaced as DecodeFailed (never
        // a silent skip), then Authenticated.
        assert_eq!(events.len(), 3, "expected 3 events, got: {events:?}");
        assert!(matches!(events[0], SignalFishEvent::Connected));
        match &events[1] {
            SignalFishEvent::DecodeFailed {
                message_type,
                raw_prefix,
                ..
            } => {
                // Not valid JSON at all → no wire `type` tag recoverable.
                assert_eq!(message_type.as_deref(), None);
                assert_eq!(raw_prefix, malformed_json);
            }
            other => panic!("expected DecodeFailed, got: {other:?}"),
        }
        assert!(matches!(events[2], SignalFishEvent::Authenticated { .. }));
        assert_eq!(client.stats().messages_undecodable, 1);

        // Should NOT contain a Disconnected event.
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, SignalFishEvent::Disconnected { .. })),
            "malformed JSON should not cause Disconnected, got: {events:?}"
        );

        // Client should remain connected after malformed JSON.
        assert!(client.is_connected());
    }

    // ── Additional mock transports ─────────────────────────────────

    /// A transport whose `send()` always returns an error.
    struct ErrorOnSendTransport;

    impl Transport for ErrorOnSendTransport {
        fn poll_send(
            &mut self,
            _cx: &mut std::task::Context<'_>,
            frame: &mut Option<TransportFrame>,
        ) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
            let _ = frame.take();
            std::task::Poll::Ready(Err(SignalFishError::TransportSend("write failed".into())))
        }

        fn poll_recv(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<std::result::Result<TransportFrame, SignalFishError>>> {
            std::task::Poll::Pending
        }

        fn poll_close(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// A transport whose `send()` always returns `Pending`.
    struct PendingOnSendTransport;

    impl PendingOnSendTransport {
        fn new() -> Self {
            Self
        }
    }

    /// Accepts a frame before returning `Pending`, then completes it on the
    /// next poll. A replacement frame during completion violates `Transport`.
    struct AcceptedPendingSendTransport {
        retained: Option<TransportFrame>,
        sent: Vec<TransportFrame>,
        replacement_seen: bool,
        peer_closes_on_recv: bool,
    }

    /// Accepts an initial FIFO burst, then permanently refuses caller-owned
    /// frames and never completes close. This models a backend that transferred
    /// multiple frames before becoming non-draining.
    struct AcceptThenStallTransport {
        accept_limit: usize,
        accepted: Vec<TransportFrame>,
        close_calls: usize,
        abort_calls: usize,
    }

    impl Transport for AcceptThenStallTransport {
        fn poll_send(
            &mut self,
            _cx: &mut std::task::Context<'_>,
            frame: &mut Option<TransportFrame>,
        ) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
            if self.accepted.len() >= self.accept_limit {
                return std::task::Poll::Pending;
            }
            if let Some(frame) = frame.take() {
                self.accepted.push(frame);
            }
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_recv(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<std::result::Result<TransportFrame, SignalFishError>>> {
            std::task::Poll::Pending
        }

        fn poll_close(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
            self.close_calls = self.close_calls.saturating_add(1);
            std::task::Poll::Pending
        }

        fn abort(&mut self) {
            self.abort_calls = self.abort_calls.saturating_add(1);
        }
    }

    impl Transport for AcceptedPendingSendTransport {
        fn poll_send(
            &mut self,
            _cx: &mut std::task::Context<'_>,
            frame: &mut Option<TransportFrame>,
        ) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
            if let Some(retained) = self.retained.take() {
                self.replacement_seen |= frame.is_some();
                self.sent.push(retained);
                return std::task::Poll::Ready(Ok(()));
            }
            if let Some(accepted) = frame.take() {
                self.retained = Some(accepted);
                return std::task::Poll::Pending;
            }
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_recv(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<std::result::Result<TransportFrame, SignalFishError>>> {
            if self.peer_closes_on_recv {
                self.peer_closes_on_recv = false;
                std::task::Poll::Ready(None)
            } else {
                std::task::Poll::Pending
            }
        }

        fn poll_close(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    impl Transport for PendingOnSendTransport {
        fn poll_send(
            &mut self,
            _cx: &mut std::task::Context<'_>,
            _frame: &mut Option<TransportFrame>,
        ) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
            std::task::Poll::Pending
        }

        fn poll_recv(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<std::result::Result<TransportFrame, SignalFishError>>> {
            std::task::Poll::Pending
        }

        fn poll_close(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// A transport whose `close()` always returns `Pending` (never completes).
    /// `send()` and `recv()` behave normally (Ready).
    struct PendingCloseTransport;

    impl Transport for PendingCloseTransport {
        fn poll_send(
            &mut self,
            _cx: &mut std::task::Context<'_>,
            frame: &mut Option<TransportFrame>,
        ) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
            let _ = frame.take();
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_recv(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<std::result::Result<TransportFrame, SignalFishError>>> {
            std::task::Poll::Pending
        }

        fn poll_close(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
            std::task::Poll::Pending
        }
    }

    #[derive(Default)]
    struct RecordingFrameTransport {
        sent: Vec<TransportFrame>,
        incoming: VecDeque<TransportFrame>,
        close_calls: usize,
        abort_calls: usize,
        send_pending: bool,
        close_pending: bool,
    }

    impl Transport for RecordingFrameTransport {
        fn poll_send(
            &mut self,
            _cx: &mut std::task::Context<'_>,
            frame: &mut Option<TransportFrame>,
        ) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
            if self.send_pending {
                return std::task::Poll::Pending;
            }
            if let Some(frame) = frame.take() {
                self.sent.push(frame);
            }
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_recv(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<std::result::Result<TransportFrame, SignalFishError>>> {
            self.incoming
                .pop_front()
                .map(|frame| std::task::Poll::Ready(Some(Ok(frame))))
                .unwrap_or(std::task::Poll::Pending)
        }

        fn poll_close(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
            self.close_calls += 1;
            if self.close_pending {
                std::task::Poll::Pending
            } else {
                std::task::Poll::Ready(Ok(()))
            }
        }

        fn abort(&mut self) {
            self.abort_calls += 1;
        }
    }

    // ── A. Command Queuing Tests ──────────────────────────────────

    #[test]
    fn leave_room_queues_command() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll(); // flush auth

        client
            .leave_room()
            .expect("leave_room must succeed on connected client");
        client.poll();

        let last_sent = client
            .transport
            .sent
            .last()
            .expect("transport must have at least one sent message");
        let sent_json: serde_json::Value =
            serde_json::from_str(last_sent).expect("sent message must be valid JSON");
        assert_eq!(sent_json["type"], "LeaveRoom");
    }

    #[test]
    fn send_game_data_queues_command() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll(); // flush auth

        client
            .send_game_data(serde_json::json!({"score": 42}))
            .expect("send_game_data must succeed on connected client");
        client.poll();

        let last_sent = client
            .transport
            .sent
            .last()
            .expect("transport must have at least one sent message");
        let sent_json: serde_json::Value =
            serde_json::from_str(last_sent).expect("sent message must be valid JSON");
        assert_eq!(sent_json["type"], "GameData");
        assert_eq!(sent_json["data"]["data"]["score"], 42);
    }

    #[test]
    fn set_ready_queues_command() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll(); // flush auth

        client
            .set_ready()
            .expect("set_ready must succeed on connected client");
        client.poll();

        let last_sent = client
            .transport
            .sent
            .last()
            .expect("transport must have at least one sent message");
        let sent_json: serde_json::Value =
            serde_json::from_str(last_sent).expect("sent message must be valid JSON");
        assert_eq!(sent_json["type"], "PlayerReady");
    }

    #[test]
    fn request_authority_queues_command() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll(); // flush auth

        client
            .request_authority(true)
            .expect("request_authority must succeed on connected client");
        client.poll();

        let last_sent = client
            .transport
            .sent
            .last()
            .expect("transport must have at least one sent message");
        let sent_json: serde_json::Value =
            serde_json::from_str(last_sent).expect("sent message must be valid JSON");
        assert_eq!(sent_json["type"], "AuthorityRequest");
        assert_eq!(sent_json["data"]["become_authority"], true);
    }

    #[test]
    fn provide_connection_info_queues_command() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll(); // flush auth

        client
            .provide_connection_info(ConnectionInfo::Direct {
                host: "127.0.0.1".into(),
                port: 7777,
            })
            .expect("provide_connection_info must succeed on connected client");
        client.poll();

        let last_sent = client
            .transport
            .sent
            .last()
            .expect("transport must have at least one sent message");
        let sent_json: serde_json::Value =
            serde_json::from_str(last_sent).expect("sent message must be valid JSON");
        assert_eq!(sent_json["type"], "ProvideConnectionInfo");
        assert_eq!(sent_json["data"]["connection_info"]["host"], "127.0.0.1");
        assert_eq!(sent_json["data"]["connection_info"]["port"], 7777);
    }

    #[test]
    fn provide_relay_connection_info_queues_command() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll(); // flush auth

        client
            .provide_connection_info(ConnectionInfo::Relay {
                host: "relay.example.com".into(),
                port: 9999,
                transport: crate::protocol::RelayTransport::Tcp,
                allocation_id: "alloc-42".into(),
                token: "secret-token".into(),
                client_id: Some(7),
            })
            .expect("provide_connection_info (relay) must succeed on connected client");
        client.poll();

        let last_sent = client
            .transport
            .sent
            .last()
            .expect("transport must have at least one sent message");
        let sent_json: serde_json::Value =
            serde_json::from_str(last_sent).expect("sent message must be valid JSON");
        assert_eq!(sent_json["type"], "ProvideConnectionInfo");
        let info = &sent_json["data"]["connection_info"];
        assert_eq!(info["host"], "relay.example.com");
        assert_eq!(info["port"], 9999);
        assert_eq!(info["transport"], "tcp");
        assert_eq!(info["allocation_id"], "alloc-42");
        assert_eq!(info["token"], "secret-token");
        assert_eq!(info["client_id"], 7);
    }

    #[test]
    fn reconnect_queues_command() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll(); // flush auth

        let player_id = uuid::Uuid::from_u128(1);
        let room_id = uuid::Uuid::from_u128(2);
        client
            .reconnect(player_id, room_id, "token123".into())
            .expect("reconnect must succeed on connected client");
        client.poll();

        let last_sent = client
            .transport
            .sent
            .last()
            .expect("transport must have at least one sent message");
        let sent_json: serde_json::Value =
            serde_json::from_str(last_sent).expect("sent message must be valid JSON");
        assert_eq!(sent_json["type"], "Reconnect");
        assert_eq!(sent_json["data"]["player_id"], player_id.to_string());
        assert_eq!(sent_json["data"]["room_id"], room_id.to_string());
        assert_eq!(sent_json["data"]["auth_token"], "token123");
    }

    #[test]
    fn join_as_spectator_queues_command() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll(); // flush auth

        client
            .join_as_spectator("my-game".into(), "ROOM1".into(), "Spectator1".into())
            .expect("join_as_spectator must succeed on connected client");
        client.poll();

        let last_sent = client
            .transport
            .sent
            .last()
            .expect("transport must have at least one sent message");
        let sent_json: serde_json::Value =
            serde_json::from_str(last_sent).expect("sent message must be valid JSON");
        assert_eq!(sent_json["type"], "JoinAsSpectator");
        assert_eq!(sent_json["data"]["game_name"], "my-game");
        assert_eq!(sent_json["data"]["room_code"], "ROOM1");
        assert_eq!(sent_json["data"]["spectator_name"], "Spectator1");
    }

    #[test]
    fn leave_spectator_queues_command() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll(); // flush auth

        client
            .leave_spectator()
            .expect("leave_spectator must succeed on connected client");
        client.poll();

        let last_sent = client
            .transport
            .sent
            .last()
            .expect("transport must have at least one sent message");
        let sent_json: serde_json::Value =
            serde_json::from_str(last_sent).expect("sent message must be valid JSON");
        assert_eq!(sent_json["type"], "LeaveSpectator");
    }

    #[test]
    fn ping_queues_command() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll(); // flush auth

        client
            .ping()
            .expect("ping must succeed on connected client");
        client.poll();

        let last_sent = client
            .transport
            .sent
            .last()
            .expect("transport must have at least one sent message");
        let sent_json: serde_json::Value =
            serde_json::from_str(last_sent).expect("sent message must be valid JSON");
        assert_eq!(sent_json["type"], "Ping");
    }

    #[test]
    fn join_room_with_all_options_queues_command() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll(); // flush auth

        let params = JoinRoomParams::new("strategy-game", "Bob")
            .with_max_players(8)
            .with_supports_authority(true)
            .with_relay_transport(crate::protocol::RelayTransport::Tcp)
            .with_room_code("CUSTOM1");
        client
            .join_room(params)
            .expect("join_room must succeed on connected client");
        client.poll();

        let last_sent = client
            .transport
            .sent
            .last()
            .expect("transport must have at least one sent message");
        let sent_json: serde_json::Value =
            serde_json::from_str(last_sent).expect("sent message must be valid JSON");
        assert_eq!(sent_json["type"], "JoinRoom");
        assert_eq!(sent_json["data"]["game_name"], "strategy-game");
        assert_eq!(sent_json["data"]["player_name"], "Bob");
        assert_eq!(sent_json["data"]["max_players"], 8);
        assert_eq!(sent_json["data"]["supports_authority"], true);
        assert_eq!(sent_json["data"]["relay_transport"], "tcp");
        assert_eq!(sent_json["data"]["room_code"], "CUSTOM1");
    }

    // ── B. All Commands Fail When Disconnected ─────────────────────

    #[test]
    fn all_commands_fail_when_disconnected() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.close();

        assert!(matches!(
            client.leave_room().unwrap_err(),
            SignalFishError::NotConnected
        ));
        assert!(matches!(
            client.send_game_data(serde_json::json!({})).unwrap_err(),
            SignalFishError::NotConnected
        ));
        assert!(matches!(
            client.set_ready().unwrap_err(),
            SignalFishError::NotConnected
        ));
        assert!(matches!(
            client.request_authority(false).unwrap_err(),
            SignalFishError::NotConnected
        ));
        assert!(matches!(
            client
                .provide_connection_info(ConnectionInfo::Direct {
                    host: "127.0.0.1".into(),
                    port: 7777,
                })
                .unwrap_err(),
            SignalFishError::NotConnected
        ));
        assert!(matches!(
            client
                .reconnect(
                    uuid::Uuid::from_u128(1),
                    uuid::Uuid::from_u128(2),
                    "tok".into()
                )
                .unwrap_err(),
            SignalFishError::NotConnected
        ));
        assert!(matches!(
            client
                .join_as_spectator("g".into(), "r".into(), "s".into())
                .unwrap_err(),
            SignalFishError::NotConnected
        ));
        assert!(matches!(
            client.leave_spectator().unwrap_err(),
            SignalFishError::NotConnected
        ));
        assert!(matches!(
            client.ping().unwrap_err(),
            SignalFishError::NotConnected
        ));
    }

    // ── C. Server Event Reception ──────────────────────────────────

    #[test]
    fn poll_receives_player_joined_event() {
        let player_id = uuid::Uuid::from_u128(10);
        let json = serde_json::to_string(&ServerMessage::PlayerJoined {
            player: crate::protocol::PlayerInfo {
                id: player_id,
                name: "NewPlayer".into(),
                is_authority: false,
                is_ready: false,
                connected_at: "2025-01-01T00:00:00Z".into(),
                connection_info: None,
                epoch: None,
                seq: None,
            },
        })
        .expect("PlayerJoined ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        assert!(events.iter().any(|e| matches!(
            e,
            SignalFishEvent::PlayerJoined { player } if player.id == player_id
        )));
    }

    #[test]
    fn poll_receives_player_left_event() {
        let player_id = uuid::Uuid::from_u128(11);
        let json = serde_json::to_string(&ServerMessage::PlayerLeft {
            player_id,
            epoch: None,
            final_seq: None,
        })
        .expect("PlayerLeft ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        assert!(events.iter().any(|e| matches!(
            e,
            SignalFishEvent::PlayerLeft {
                player_id: pid,
                ..
            } if *pid == player_id
        )));
    }

    #[test]
    fn poll_receives_game_data_event() {
        let from = uuid::Uuid::from_u128(12);
        let json = serde_json::to_string(&ServerMessage::GameData {
            from_player: from,
            data: serde_json::json!({"hp": 100}),
            seq: None,
            epoch: None,
            class: None,
            key: None,
        })
        .expect("GameData ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        let gd = events
            .iter()
            .find(|e| matches!(e, SignalFishEvent::GameData { .. }));
        assert!(gd.is_some(), "expected GameData event, got: {events:?}");
        if let SignalFishEvent::GameData {
            from_player, data, ..
        } = gd.expect("GameData event must exist (verified by preceding assert)")
        {
            assert_eq!(*from_player, from);
            assert_eq!(data["hp"], 100);
        }
    }

    #[test]
    fn poll_receives_game_data_binary_event() {
        let from = uuid::Uuid::from_u128(13);
        let mut frames = accountability_prefix(from);
        let binary = crate::protocol::V3BinaryGameDataFrame {
            from_player: from,
            encoding: crate::protocol::GameDataEncoding::MessagePack,
            payload: vec![0xCA, 0xFE],
            seq: 1,
            epoch: 1,
        };
        frames.push(TransportFrame::Binary(
            rmp_serde::to_vec_named(&binary).expect("serialize binary fixture"),
        ));

        let transport = MockTransport::new().with_frames(frames);
        let mut config = default_config().enable_v3();
        config.game_data_format = Some(crate::protocol::GameDataEncoding::MessagePack);
        let mut client = SignalFishPollingClient::new(transport, config);
        let events = client.poll();

        let gdb = events
            .iter()
            .find(|e| matches!(e, SignalFishEvent::GameDataBinary { .. }));
        assert!(
            gdb.is_some(),
            "expected GameDataBinary event, got: {events:?}"
        );
        if let SignalFishEvent::GameDataBinary {
            from_player,
            encoding,
            payload,
            seq,
            epoch,
        } = gdb.expect("GameDataBinary event must exist (verified by preceding assert)")
        {
            assert_eq!(*from_player, from);
            assert!(matches!(
                encoding,
                crate::protocol::GameDataEncoding::MessagePack
            ));
            assert_eq!(payload, &[0xCA, 0xFE]);
            assert_eq!(*seq, Some(1));
            assert_eq!(*epoch, Some(1));
        }
    }

    #[test]
    fn poll_receives_authority_changed_event() {
        let auth_player = uuid::Uuid::from_u128(14);
        let json = serde_json::to_string(&ServerMessage::AuthorityChanged {
            authority_player: Some(auth_player),
            you_are_authority: true,
        })
        .expect("AuthorityChanged ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        assert!(events.iter().any(|e| matches!(
            e,
            SignalFishEvent::AuthorityChanged {
                authority_player: Some(pid),
                you_are_authority: true,
            } if *pid == auth_player
        )));
    }

    #[test]
    fn poll_receives_authority_response_event() {
        let json = serde_json::to_string(&ServerMessage::AuthorityResponse {
            granted: true,
            reason: None,
            error_code: None,
        })
        .expect("AuthorityResponse ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        assert!(events.iter().any(|e| matches!(
            e,
            SignalFishEvent::AuthorityResponse {
                granted: true,
                reason: None,
                error_code: None,
            }
        )));
    }

    #[test]
    fn poll_receives_authority_response_denied() {
        let json = serde_json::to_string(&ServerMessage::AuthorityResponse {
            granted: false,
            reason: Some("already assigned".into()),
            error_code: None,
        })
        .expect("AuthorityResponse (denied) ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        let ar = events
            .iter()
            .find(|e| matches!(e, SignalFishEvent::AuthorityResponse { .. }));
        assert!(
            ar.is_some(),
            "expected AuthorityResponse event, got: {events:?}"
        );
        if let SignalFishEvent::AuthorityResponse {
            granted,
            reason,
            error_code,
        } = ar.expect("AuthorityResponse event must exist (verified by preceding assert)")
        {
            assert!(!granted, "expected granted to be false");
            assert_eq!(reason.as_deref(), Some("already assigned"));
            assert!(error_code.is_none());
        }
    }

    #[test]
    fn poll_receives_lobby_state_changed_event() {
        let player_id = uuid::Uuid::from_u128(15);
        let json = serde_json::to_string(&ServerMessage::LobbyStateChanged {
            lobby_state: crate::protocol::LobbyState::Finalized,
            ready_players: vec![player_id],
            all_ready: true,
        })
        .expect("LobbyStateChanged ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        assert!(events.iter().any(|e| matches!(
            e,
            SignalFishEvent::LobbyStateChanged {
                all_ready: true,
                ..
            }
        )));
    }

    #[test]
    fn poll_receives_game_starting_event() {
        let json = serde_json::to_string(&ServerMessage::GameStarting {
            peer_connections: vec![],
        })
        .expect("GameStarting ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        assert!(events
            .iter()
            .any(|e| matches!(e, SignalFishEvent::GameStarting { .. })));
    }

    #[test]
    fn poll_receives_pong_event() {
        let json = serde_json::to_string(&ServerMessage::Pong)
            .expect("Pong ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        assert!(events.iter().any(|e| matches!(e, SignalFishEvent::Pong)));
    }

    #[test]
    fn poll_receives_error_event() {
        let json = serde_json::to_string(&ServerMessage::Error {
            message: "something went wrong".into(),
            error_code: Some(crate::error_codes::ErrorCode::InternalError),
        })
        .expect("Error ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        let err = events
            .iter()
            .find(|e| matches!(e, SignalFishEvent::Error { .. }));
        assert!(err.is_some(), "expected Error event, got: {events:?}");
        if let SignalFishEvent::Error {
            message,
            error_code,
        } = err.expect("Error event must exist (verified by preceding assert)")
        {
            assert_eq!(message, "something went wrong");
            assert_eq!(
                *error_code,
                Some(crate::error_codes::ErrorCode::InternalError)
            );
        }
    }

    #[test]
    fn poll_receives_error_event_without_code() {
        let json = serde_json::to_string(&ServerMessage::Error {
            message: "minor issue".into(),
            error_code: None,
        })
        .expect("Error (no code) ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        let err = events
            .iter()
            .find(|e| matches!(e, SignalFishEvent::Error { .. }));
        assert!(err.is_some(), "expected Error event, got: {events:?}");
        if let SignalFishEvent::Error {
            message,
            error_code,
        } = err.expect("Error event must exist (verified by preceding assert)")
        {
            assert_eq!(message, "minor issue");
            assert!(error_code.is_none());
        }
    }

    #[test]
    fn poll_receives_authentication_error_event() {
        let json = serde_json::to_string(&ServerMessage::AuthenticationError {
            error: "bad app id".into(),
            error_code: crate::error_codes::ErrorCode::InvalidAppId,
        })
        .expect("AuthenticationError ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        assert!(events
            .iter()
            .any(|e| matches!(e, SignalFishEvent::AuthenticationError { .. })));
    }

    #[test]
    fn poll_receives_room_join_failed_event() {
        let json = serde_json::to_string(&ServerMessage::RoomJoinFailed {
            reason: "room full".into(),
            error_code: Some(crate::error_codes::ErrorCode::RoomFull),
        })
        .expect("RoomJoinFailed ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        let rjf = events
            .iter()
            .find(|e| matches!(e, SignalFishEvent::RoomJoinFailed { .. }));
        assert!(
            rjf.is_some(),
            "expected RoomJoinFailed event, got: {events:?}"
        );
        if let SignalFishEvent::RoomJoinFailed { reason, error_code } =
            rjf.expect("RoomJoinFailed event must exist (verified by preceding assert)")
        {
            assert_eq!(reason, "room full");
            assert_eq!(*error_code, Some(crate::error_codes::ErrorCode::RoomFull));
        }
    }

    #[test]
    fn poll_receives_spectator_join_failed_event() {
        let json = serde_json::to_string(&ServerMessage::SpectatorJoinFailed {
            reason: "not allowed".into(),
            error_code: Some(crate::error_codes::ErrorCode::SpectatorNotAllowed),
        })
        .expect("SpectatorJoinFailed ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        assert!(events
            .iter()
            .any(|e| matches!(e, SignalFishEvent::SpectatorJoinFailed { .. })));
    }

    #[test]
    fn poll_receives_reconnection_failed_event() {
        let json = serde_json::to_string(&ServerMessage::ReconnectionFailed {
            reason: "expired".into(),
            error_code: crate::error_codes::ErrorCode::ReconnectionExpired,
        })
        .expect("ReconnectionFailed ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        assert!(events
            .iter()
            .any(|e| matches!(e, SignalFishEvent::ReconnectionFailed { .. })));
    }

    #[test]
    fn poll_receives_player_reconnected_event() {
        let player_id = uuid::Uuid::from_u128(20);
        let json = serde_json::to_string(&ServerMessage::PlayerReconnected {
            player_id,
            epoch: None,
        })
        .expect("PlayerReconnected ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        assert!(events.iter().any(|e| matches!(
            e,
            SignalFishEvent::PlayerReconnected {
                player_id: pid,
                ..
            } if *pid == player_id
        )));
    }

    #[test]
    fn poll_receives_new_spectator_joined_event() {
        let spec_id = uuid::Uuid::from_u128(21);
        let json = serde_json::to_string(&ServerMessage::NewSpectatorJoined {
            spectator: crate::protocol::SpectatorInfo {
                id: spec_id,
                name: "Watcher".into(),
                connected_at: "2025-01-01T00:00:00Z".into(),
            },
            current_spectators: vec![],
            reason: Some(crate::protocol::SpectatorStateChangeReason::Joined),
        })
        .expect("NewSpectatorJoined ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        assert!(events
            .iter()
            .any(|e| matches!(e, SignalFishEvent::NewSpectatorJoined { .. })));
    }

    #[test]
    fn poll_receives_spectator_disconnected_event() {
        let spec_id = uuid::Uuid::from_u128(22);
        let json = serde_json::to_string(&ServerMessage::SpectatorDisconnected {
            spectator_id: spec_id,
            reason: Some(crate::protocol::SpectatorStateChangeReason::Disconnected),
            current_spectators: vec![],
        })
        .expect("SpectatorDisconnected ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        assert!(events.iter().any(|e| matches!(
            e,
            SignalFishEvent::SpectatorDisconnected {
                spectator_id: sid,
                ..
            } if *sid == spec_id
        )));
    }

    #[test]
    fn poll_receives_protocol_info_event() {
        let json = serde_json::to_string(&ServerMessage::ProtocolInfo(
            crate::protocol::ProtocolInfoPayload {
                platform: Some("rust".into()),
                sdk_version: Some("0.1.0".into()),
                minimum_version: None,
                recommended_version: None,
                capabilities: vec!["binary_data".into()],
                notes: None,
                game_data_formats: vec![crate::protocol::GameDataEncoding::Json],
                player_name_rules: None,
                protocol_version: None,
                min_protocol_version: None,
                max_protocol_version: None,
                transports: None,
            },
        ))
        .expect("ProtocolInfo ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        assert!(events
            .iter()
            .any(|e| matches!(e, SignalFishEvent::ProtocolInfo(_))));
    }

    // ── D. Transport Error Paths ───────────────────────────────────

    #[test]
    fn poll_handles_send_error() {
        let transport = ErrorOnSendTransport;
        let mut client = SignalFishPollingClient::new(transport, default_config());

        // First poll will try to send the queued Authenticate message
        // and encounter the send error.
        let events = client.poll();

        let disconnected = events.iter().find(|e| {
            matches!(
                e,
                SignalFishEvent::Disconnected {
                    reason: Some(_),
                    ..
                }
            )
        });
        assert!(
            disconnected.is_some(),
            "expected Disconnected event, got: {events:?}"
        );
        if let SignalFishEvent::Disconnected {
            reason: Some(r), ..
        } = disconnected.expect("Disconnected event must exist (verified by preceding assert)")
        {
            assert!(
                r.contains("transport send error"),
                "expected reason to contain 'transport send error', got: {r}"
            );
        }
        assert!(!client.is_connected());
    }

    /// Transport whose `send()` stays `Pending` until `allow` is set, so tests
    /// can saturate and then drain the bounded command queue deterministically.
    struct TogglePendingSendTransport {
        allow: std::sync::Arc<std::sync::atomic::AtomicBool>,
        sent: Vec<String>,
        _sent_binary: Vec<Vec<u8>>,
    }

    impl Transport for TogglePendingSendTransport {
        fn poll_send(
            &mut self,
            _cx: &mut std::task::Context<'_>,
            frame: &mut Option<TransportFrame>,
        ) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
            if !self.allow.load(std::sync::atomic::Ordering::Acquire) {
                return std::task::Poll::Pending;
            }
            if let Some(frame) = frame.take() {
                match frame {
                    TransportFrame::Text(text) => self.sent.push(text),
                    TransportFrame::Binary(bytes) => self._sent_binary.push(bytes),
                }
            }
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_recv(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<std::result::Result<TransportFrame, SignalFishError>>> {
            std::task::Poll::Pending
        }

        fn poll_close(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn queue_cmd_fails_fast_when_command_queue_is_full() {
        let allow = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let transport = TogglePendingSendTransport {
            allow: std::sync::Arc::clone(&allow),
            sent: Vec::new(),
            _sent_binary: Vec::new(),
        };
        let config = default_config().with_command_channel_capacity(3);
        let mut client = SignalFishPollingClient::new(transport, config);

        // Authenticate occupies one of the three slots.
        assert_eq!(client.max_send_capacity(), 3);
        assert_eq!(client.send_capacity(), 2);

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
            matches!(err, SignalFishError::SendBufferFull { capacity: 3 }),
            "expected SendBufferFull, got {err:?}"
        );

        // Once Authenticate leaves the queue for the transport, that queue
        // slot is free, matching the async mpsc driver's capacity semantics.
        let _ = client.poll();
        assert_eq!(client.send_capacity(), 1);
        assert!(client.is_connected());
        client
            .send_game_data(serde_json::json!({ "seq": 2 }))
            .expect("one queue slot should be free behind the pending send");
        assert_eq!(client.send_capacity(), 0);

        // Once the transport accepts writes again, one poll drains everything.
        allow.store(true, std::sync::atomic::Ordering::Release);
        let _ = client.poll();
        assert_eq!(client.send_capacity(), 3);
        assert_eq!(client.transport.sent.len(), 4);
        client
            .send_game_data(serde_json::json!({ "seq": 3 }))
            .unwrap();
    }

    #[test]
    fn poll_retries_pending_send_next_frame() {
        let transport = PendingOnSendTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());

        // First poll: send returns Pending, so the accepted Authenticate frame
        // remains owned by the driver's pending slot.
        let events = client.poll();

        // Connected should still be emitted before the send attempt.
        assert!(events
            .iter()
            .any(|e| matches!(e, SignalFishEvent::Connected)));

        // Client should still be connected (not disconnected).
        assert!(client.is_connected());

        assert!(
            client.pending_frame.is_some(),
            "expected pending frame to remain retained"
        );
    }

    #[test]
    fn constant_depth_can_have_positive_oldest_queue_age_slope() {
        let allow = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let transport = TogglePendingSendTransport {
            allow,
            sent: Vec::new(),
            _sent_binary: Vec::new(),
        };
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let base = Instant::now();
        client
            .cmd_queue
            .front_mut()
            .expect("Authenticate should be queued")
            .enqueued_at = base;

        let _ = client.poll_at(base);
        let first_depth = client.polling_stats().current_queue_depth;
        let first_age = client.queue_age_stats().current_oldest_queue_age;
        let _ = client.poll_at(base + Duration::from_millis(40));
        let second_depth = client.polling_stats().current_queue_depth;
        let second_age = client.queue_age_stats().current_oldest_queue_age;

        assert_eq!(second_depth, first_depth);
        assert!(second_age > first_age);
        assert_eq!(second_age, Duration::from_millis(40));
    }

    #[test]
    fn refused_frame_retains_fifo_identity_and_ages_until_acceptance() {
        let allow = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let transport = TogglePendingSendTransport {
            allow: std::sync::Arc::clone(&allow),
            sent: Vec::new(),
            _sent_binary: Vec::new(),
        };
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let base = Instant::now();
        client
            .cmd_queue
            .front_mut()
            .expect("Authenticate should be queued")
            .enqueued_at = base;

        let _ = client.poll_at(base + Duration::from_millis(10));
        let refused = client
            .pending_frame
            .clone()
            .expect("refused frame should remain client-owned");
        let refused_at = client.pending_frame_enqueued_at;
        let _ = client.poll_at(base + Duration::from_millis(25));

        assert_eq!(client.pending_frame.as_ref(), Some(&refused));
        assert_eq!(client.pending_frame_enqueued_at, refused_at);
        assert_eq!(
            client.queue_age_stats().current_oldest_queue_age,
            Duration::from_millis(25)
        );

        allow.store(true, std::sync::atomic::Ordering::Release);
        let _ = client.poll_at(base + Duration::from_millis(30));

        assert!(client.pending_frame.is_none());
        assert!(client.pending_frame_enqueued_at.is_none());
        assert_eq!(
            client.queue_age_stats().current_oldest_queue_age,
            Duration::ZERO
        );
        assert_eq!(
            client.queue_age_stats().peak_oldest_queue_age,
            Duration::from_millis(30)
        );
        assert_eq!(client.transport.sent.len(), 1);
    }

    #[test]
    fn queue_age_peak_reset_clock_regression_and_close_preserve_invariants() {
        let allow = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let transport = TogglePendingSendTransport {
            allow,
            sent: Vec::new(),
            _sent_binary: Vec::new(),
        };
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let base = Instant::now();
        client
            .cmd_queue
            .front_mut()
            .expect("Authenticate should be queued")
            .enqueued_at = base;

        let _ = client.poll_at(base + Duration::from_millis(50));
        client.reset_queue_age_peak_at(base + Duration::from_millis(20));
        let regressed = client.queue_age_stats();
        assert_eq!(
            regressed.current_oldest_queue_age,
            Duration::from_millis(20)
        );
        assert_eq!(
            regressed.peak_oldest_queue_age,
            regressed.current_oldest_queue_age
        );

        let _ = client.poll_at(base - Duration::from_millis(1));
        let regressed = client.queue_age_stats();
        assert_eq!(regressed.current_oldest_queue_age, Duration::ZERO);
        assert_eq!(regressed.peak_oldest_queue_age, Duration::from_millis(20));

        client.close_at(base + Duration::from_millis(60));
        let drained = client.queue_age_stats();
        assert_eq!(drained.current_oldest_queue_age, Duration::ZERO);
        assert_eq!(drained.peak_oldest_queue_age, Duration::from_millis(20));
    }

    #[test]
    fn serialization_failure_cleanup_stops_queue_age_and_preserves_peak() {
        let mut client = SignalFishPollingClient::new(MockTransport::new(), default_config());
        let base = Instant::now();
        let queued = client
            .cmd_queue
            .pop_front()
            .expect("Authenticate should be queued");
        let _ = queued;
        client.pending_frame_enqueued_at = Some(base);
        client.refresh_queue_diagnostics_at(base + Duration::from_millis(10));

        let serialization_error = serde_json::from_str::<serde_json::Value>("{")
            .expect_err("the injected malformed JSON should fail");
        assert!(client
            .finish_serialization_at(Err(serialization_error), base + Duration::from_millis(20),)
            .is_none());

        let age = client.queue_age_stats();
        assert_eq!(age.current_oldest_queue_age, Duration::ZERO);
        assert_eq!(age.peak_oldest_queue_age, Duration::from_millis(10));
        assert_eq!(client.polling_stats().abandoned_commands, 1);

        // `ClientMessage` contains only JSON-representable SDK and
        // `serde_json::Value` fields, so safe public inputs cannot construct a
        // failing message. Inject an error into the same result transition
        // used by `drive_outbound` instead.
    }

    #[test]
    fn backend_accepted_pending_frame_stops_contributing_to_queue_age() {
        let transport = AcceptedPendingSendTransport {
            retained: None,
            sent: Vec::new(),
            replacement_seen: false,
            peer_closes_on_recv: false,
        };
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let base = Instant::now();
        client
            .cmd_queue
            .front_mut()
            .expect("Authenticate should be queued")
            .enqueued_at = base;

        let _ = client.poll_at(base + Duration::from_millis(25));

        assert!(client.send_in_flight, "backend should retain Authenticate");
        assert!(client.pending_frame.is_none());
        assert_eq!(
            client.queue_age_stats().current_oldest_queue_age,
            Duration::ZERO,
            "backend ownership must stop client queue age even while completion is pending"
        );
        assert_eq!(
            client.queue_age_stats().peak_oldest_queue_age,
            Duration::from_millis(25)
        );
    }

    #[test]
    fn accepted_pending_send_completes_before_dequeuing_replacement() {
        let transport = AcceptedPendingSendTransport {
            retained: None,
            sent: Vec::new(),
            replacement_seen: false,
            peer_closes_on_recv: false,
        };
        let mut client = SignalFishPollingClient::new(
            transport,
            default_config().with_command_channel_capacity(2),
        );

        let _ = client.poll();
        assert!(client.send_in_flight, "Authenticate should be in flight");
        client
            .send_game_data(serde_json::json!({"frame": 1}))
            .expect("one queued command should fit behind the in-flight send");

        let _ = client.poll();
        assert!(client.send_in_flight, "game data should now be in flight");
        let _ = client.poll();

        assert!(!client.transport.replacement_seen);
        assert_eq!(client.transport.sent.len(), 2);
        assert!(matches!(
            client.transport.sent.first(),
            Some(TransportFrame::Text(text)) if text.contains("Authenticate")
        ));
        assert!(matches!(
            client.transport.sent.get(1),
            Some(TransportFrame::Text(text)) if text.contains("GameData")
        ));
        assert_eq!(client.stats().game_data_sent, 1);
    }

    #[test]
    fn one_poll_transfers_multiple_text_and_binary_frames() {
        let transport = RecordingFrameTransport::default();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        enqueue_direct(&mut client, PollingCommand::Message(ClientMessage::Ping));
        enqueue_direct(&mut client, PollingCommand::Binary(vec![1, 2, 3]));
        enqueue_direct(&mut client, PollingCommand::Message(ClientMessage::Ping));
        enqueue_direct(&mut client, PollingCommand::Binary(vec![4]));

        let _ = client.poll();

        assert_eq!(client.transport.sent.len(), 5);
        assert!(matches!(
            client.transport.sent.first(),
            Some(TransportFrame::Text(text)) if text.contains("Authenticate")
        ));
        assert!(matches!(
            client.transport.sent.get(1),
            Some(TransportFrame::Text(text)) if text.contains("Ping")
        ));
        assert_eq!(
            client.transport.sent.get(2),
            Some(&TransportFrame::Binary(vec![1, 2, 3]))
        );
        assert_eq!(client.polling_stats().current_queue_depth, 0);
    }

    #[test]
    fn send_frame_budget_stops_exactly_and_preserves_fifo() {
        let options = PollingClientOptions {
            work_budget: PollingWorkBudget {
                send_frames: 3,
                send_bytes: usize::MAX,
                receive_frames: 64,
                receive_bytes: 64 * 1024,
            },
            close_policy: PollingClosePolicy::Abandon,
        };
        let transport = RecordingFrameTransport::default();
        let mut client =
            SignalFishPollingClient::new_with_options(transport, default_config(), options);
        for _ in 0..5 {
            enqueue_direct(&mut client, PollingCommand::Message(ClientMessage::Ping));
        }

        let _ = client.poll();
        assert_eq!(client.transport.sent.len(), 3);
        assert_eq!(client.polling_stats().current_queue_depth, 3);
        assert_eq!(client.polling_stats().send_budget_exhaustions, 1);

        let _ = client.poll();
        assert_eq!(client.transport.sent.len(), 6);
        assert_eq!(client.polling_stats().current_queue_depth, 0);
    }

    #[test]
    fn oversized_outbound_frame_gets_single_frame_escape() {
        let options = PollingClientOptions {
            work_budget: PollingWorkBudget {
                send_frames: 8,
                send_bytes: 4,
                receive_frames: 8,
                receive_bytes: 8,
            },
            close_policy: PollingClosePolicy::Abandon,
        };
        let transport = RecordingFrameTransport::default();
        let mut client =
            SignalFishPollingClient::new_with_options(transport, default_config(), options);
        let _ = client.poll();
        enqueue_direct(&mut client, PollingCommand::Binary(vec![7; 32]));

        let _ = client.poll();
        assert_eq!(client.transport.sent.len(), 2);
        assert_eq!(
            client.transport.sent.last(),
            Some(&TransportFrame::Binary(vec![7; 32]))
        );
    }

    #[test]
    fn send_byte_budget_retains_fifo_work_for_later_polls() {
        let options = PollingClientOptions {
            work_budget: PollingWorkBudget {
                send_frames: 8,
                send_bytes: 7,
                receive_frames: 8,
                receive_bytes: 8,
            },
            close_policy: PollingClosePolicy::Abandon,
        };
        let transport = RecordingFrameTransport::default();
        let mut client =
            SignalFishPollingClient::new_with_options(transport, default_config(), options);
        let _ = client.poll();
        for byte in [1, 2, 3] {
            enqueue_direct(&mut client, PollingCommand::Binary(vec![byte; 4]));
        }

        let _ = client.poll();
        assert_eq!(client.transport.sent.len(), 2);
        assert_eq!(client.polling_stats().current_queue_depth, 2);
        assert_eq!(client.polling_stats().send_budget_exhaustions, 1);
        let _ = client.poll();
        let _ = client.poll();

        assert_eq!(
            &client.transport.sent[1..],
            &[
                TransportFrame::Binary(vec![1; 4]),
                TransportFrame::Binary(vec![2; 4]),
                TransportFrame::Binary(vec![3; 4]),
            ]
        );
        assert_eq!(client.polling_stats().current_queue_depth, 0);
    }

    #[test]
    fn receive_budget_retains_backlog_without_loss_or_reordering() {
        let pong = TransportFrame::Text(
            serde_json::to_string(&ServerMessage::Pong).expect("Pong should serialize"),
        );
        let transport = RecordingFrameTransport {
            incoming: [pong.clone(), pong.clone(), pong.clone(), pong]
                .into_iter()
                .collect(),
            ..RecordingFrameTransport::default()
        };
        let options = PollingClientOptions {
            work_budget: PollingWorkBudget {
                send_frames: 64,
                send_bytes: 64 * 1024,
                receive_frames: 2,
                receive_bytes: 64 * 1024,
            },
            close_policy: PollingClosePolicy::Abandon,
        };
        let mut client =
            SignalFishPollingClient::new_with_options(transport, default_config(), options);

        let first = client.poll();
        assert_eq!(
            first
                .iter()
                .filter(|event| matches!(event, SignalFishEvent::Pong))
                .count(),
            2
        );
        let second = client.poll();
        assert_eq!(
            second
                .iter()
                .filter(|event| matches!(event, SignalFishEvent::Pong))
                .count(),
            2
        );
        assert_eq!(client.polling_stats().receive_budget_exhaustions, 1);
        assert!(client.pending_inbound.is_none());
    }

    #[test]
    fn exact_receive_budget_without_backlog_is_not_an_exhaustion() {
        let pong = TransportFrame::Text(
            serde_json::to_string(&ServerMessage::Pong).expect("Pong should serialize"),
        );
        let transport = RecordingFrameTransport {
            incoming: VecDeque::from([pong]),
            ..RecordingFrameTransport::default()
        };
        let options = PollingClientOptions {
            work_budget: PollingWorkBudget {
                receive_frames: 1,
                ..PollingWorkBudget::default()
            },
            ..PollingClientOptions::default()
        };
        let mut client =
            SignalFishPollingClient::new_with_options(transport, default_config(), options);

        let events = client.poll();

        assert!(events
            .iter()
            .any(|event| matches!(event, SignalFishEvent::Pong)));
        assert_eq!(client.polling_stats().receive_budget_exhaustions, 0);
    }

    #[test]
    fn receive_byte_budget_retains_distinct_frames_in_order() {
        let from = uuid::Uuid::from_u128(77);
        let frame = |id| {
            TransportFrame::Text(
                serde_json::to_string(&ServerMessage::GameData {
                    from_player: from,
                    data: serde_json::json!({"id": id, "padding": "abcdefgh"}),
                    seq: None,
                    epoch: None,
                    class: None,
                    key: None,
                })
                .expect("GameData should serialize"),
            )
        };
        let first = frame(1);
        let second = frame(2);
        let first_len = frame_payload_len(&first);
        let transport = RecordingFrameTransport {
            incoming: [first, second].into_iter().collect(),
            ..RecordingFrameTransport::default()
        };
        let options = PollingClientOptions {
            work_budget: PollingWorkBudget {
                send_frames: 8,
                send_bytes: usize::MAX,
                receive_frames: 8,
                receive_bytes: first_len,
            },
            close_policy: PollingClosePolicy::Abandon,
        };
        let mut client =
            SignalFishPollingClient::new_with_options(transport, default_config(), options);

        let first_events = client.poll();
        let second_events = client.poll();
        let ids = first_events
            .iter()
            .chain(&second_events)
            .filter_map(|event| match event {
                SignalFishEvent::GameData { data, .. } => data["id"].as_u64(),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(ids, vec![1, 2]);
        assert!(client.polling_stats().receive_budget_exhaustions >= 1);
        assert!(client.pending_inbound.is_none());
    }

    #[test]
    fn zero_work_budgets_clamp_to_one() {
        let options = PollingClientOptions {
            work_budget: PollingWorkBudget {
                send_frames: 0,
                send_bytes: 0,
                receive_frames: 0,
                receive_bytes: 0,
            },
            close_policy: PollingClosePolicy::Abandon,
        };
        let client = SignalFishPollingClient::new_with_options(
            RecordingFrameTransport::default(),
            default_config(),
            options,
        );
        assert_eq!(
            client.options.work_budget,
            PollingWorkBudget {
                send_frames: 1,
                send_bytes: 1,
                receive_frames: 1,
                receive_bytes: 1,
            }
        );
    }

    #[test]
    fn advertised_polling_defaults_are_exact() {
        assert_eq!(
            PollingWorkBudget::default(),
            PollingWorkBudget {
                send_frames: 64,
                send_bytes: 64 * 1024,
                receive_frames: 64,
                receive_bytes: 64 * 1024,
            }
        );
        assert_eq!(PollingClosePolicy::default(), PollingClosePolicy::Abandon);
        assert_eq!(
            PollingClientOptions::default(),
            PollingClientOptions {
                work_budget: PollingWorkBudget::default(),
                close_policy: PollingClosePolicy::Abandon,
            }
        );
    }

    #[test]
    fn transport_accessor_is_read_only_and_exposes_specific_diagnostics() {
        let client = SignalFishPollingClient::new(MockTransport::new(), default_config());

        assert!(!client.transport().closed);
        assert!(client.transport().sent.is_empty());
    }

    #[test]
    fn abandon_close_discards_queued_work_and_starts_close() {
        let transport = RecordingFrameTransport::default();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        enqueue_direct(&mut client, PollingCommand::Message(ClientMessage::Ping));
        let base = Instant::now();
        for queued in &mut client.cmd_queue {
            queued.enqueued_at = base;
        }
        client.refresh_queue_diagnostics_at(base + Duration::from_millis(15));

        client.close_at(base + Duration::from_millis(20));

        assert_eq!(client.transport.sent.len(), 0);
        assert_eq!(client.transport.close_calls, 1);
        assert_eq!(client.polling_stats().abandoned_commands, 2);
        assert_eq!(
            client.queue_age_stats(),
            PollingQueueAgeStats {
                current_oldest_queue_age: Duration::ZERO,
                peak_oldest_queue_age: Duration::from_millis(15),
            }
        );
        assert!(!client.is_closing());
        assert!(matches!(client.ping(), Err(SignalFishError::NotConnected)));
    }

    #[test]
    fn abandon_close_discards_a_transport_refused_frame() {
        let transport = RecordingFrameTransport {
            send_pending: true,
            ..RecordingFrameTransport::default()
        };
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let _ = client.poll();
        assert!(client.pending_frame.is_some());

        client.close();

        assert!(client.pending_frame.is_none());
        assert_eq!(client.polling_stats().abandoned_commands, 1);
        assert_eq!(
            client.queue_age_stats().current_oldest_queue_age,
            Duration::ZERO
        );
        assert!(
            client.queue_age_stats().peak_oldest_queue_age
                >= client.queue_age_stats().current_oldest_queue_age
        );
        assert_eq!(client.transport.close_calls, 1);
        assert!(!client.is_closing());
    }

    #[test]
    fn flush_close_transfers_queued_work_in_fifo_order_before_close() {
        let options = PollingClientOptions {
            close_policy: PollingClosePolicy::Flush,
            ..PollingClientOptions::default()
        };
        let transport = RecordingFrameTransport::default();
        let mut client =
            SignalFishPollingClient::new_with_options(transport, default_config(), options);
        enqueue_direct(&mut client, PollingCommand::Message(ClientMessage::Ping));

        client.close();

        assert_eq!(client.transport.sent.len(), 2);
        assert!(matches!(
            client.transport.sent.first(),
            Some(TransportFrame::Text(text)) if text.contains("Authenticate")
        ));
        assert!(matches!(
            client.transport.sent.get(1),
            Some(TransportFrame::Text(text)) if text.contains("Ping")
        ));
        assert_eq!(client.transport.close_calls, 1);
        assert_eq!(client.polling_stats().abandoned_commands, 0);
        assert!(!client.is_closing());
    }

    #[test]
    fn both_close_policies_finish_backend_owned_send_before_close() {
        for close_policy in [PollingClosePolicy::Abandon, PollingClosePolicy::Flush] {
            let transport = AcceptedPendingSendTransport {
                retained: None,
                sent: Vec::new(),
                replacement_seen: false,
                peer_closes_on_recv: false,
            };
            let options = PollingClientOptions {
                close_policy,
                ..PollingClientOptions::default()
            };
            let mut client =
                SignalFishPollingClient::new_with_options(transport, default_config(), options);
            let _ = client.poll();
            assert!(client.send_in_flight);

            client.close();

            assert!(!client.is_closing(), "policy {close_policy:?}");
            assert!(!client.send_in_flight, "policy {close_policy:?}");
            assert_eq!(client.transport.sent.len(), 1, "policy {close_policy:?}");
            assert!(matches!(
                client.transport.sent.first(),
                Some(TransportFrame::Text(text)) if text.contains("Authenticate")
            ));
        }
    }

    #[test]
    fn accepted_burst_plus_retained_fifo_is_bounded_under_both_close_policies() {
        for close_policy in [PollingClosePolicy::Abandon, PollingClosePolicy::Flush] {
            let transport = AcceptThenStallTransport {
                accept_limit: 2,
                accepted: Vec::new(),
                close_calls: 0,
                abort_calls: 0,
            };
            let options = PollingClientOptions {
                close_policy,
                ..PollingClientOptions::default()
            };
            let config = default_config().with_shutdown_timeout(Duration::from_millis(10));
            let mut client = SignalFishPollingClient::new_with_options(transport, config, options);
            for _ in 0..4 {
                enqueue_direct(&mut client, PollingCommand::Message(ClientMessage::Ping));
            }
            let started_at = Instant::now();

            let _ = client.poll_at(started_at);

            assert_eq!(
                client.transport.accepted.len(),
                2,
                "policy {close_policy:?}"
            );
            assert!(matches!(
                client.transport.accepted.first(),
                Some(TransportFrame::Text(text)) if text.contains("Authenticate")
            ));
            assert!(matches!(
                client.transport.accepted.get(1),
                Some(TransportFrame::Text(text)) if text.contains("Ping")
            ));
            assert_eq!(client.polling_stats().current_queue_depth, 3);

            client.close_at(started_at);

            let expected_retained = match close_policy {
                PollingClosePolicy::Abandon => 0,
                PollingClosePolicy::Flush => 3,
            };
            assert_eq!(
                client.polling_stats().current_queue_depth,
                expected_retained,
                "policy {close_policy:?}"
            );
            assert_eq!(
                client.polling_stats().abandoned_commands,
                3 - expected_retained,
                "policy {close_policy:?}"
            );
            assert_eq!(
                client.transport.close_calls,
                usize::from(close_policy == PollingClosePolicy::Abandon),
                "policy {close_policy:?}"
            );
            assert!(client.is_closing(), "policy {close_policy:?}");

            let _ = client.poll_at(started_at + Duration::from_millis(10));

            assert!(!client.is_closing(), "policy {close_policy:?}");
            assert_eq!(client.transport.abort_calls, 1, "policy {close_policy:?}");
            assert_eq!(
                client.transport.accepted.len(),
                2,
                "policy {close_policy:?}"
            );
            assert_eq!(client.polling_stats().current_queue_depth, 0);
            assert_eq!(client.polling_stats().abandoned_commands, 3);
            assert_eq!(client.polling_stats().close_deadline_expirations, 1);
        }
    }

    #[test]
    fn peer_disconnect_finishes_a_backend_owned_send_before_close() {
        let transport = AcceptedPendingSendTransport {
            retained: None,
            sent: Vec::new(),
            replacement_seen: false,
            peer_closes_on_recv: true,
        };
        let mut client = SignalFishPollingClient::new(transport, default_config());

        let events = client.poll();

        assert!(events
            .iter()
            .any(|event| matches!(event, SignalFishEvent::Disconnected { .. })));
        assert!(!client.send_in_flight);
        assert_eq!(client.transport.sent.len(), 1);
        assert!(matches!(
            client.transport.sent.first(),
            Some(TransportFrame::Text(text)) if text.contains("Authenticate")
        ));
        assert!(!client.is_closing());
    }

    #[test]
    fn close_deadline_aborts_without_sleeping() {
        for close_policy in [PollingClosePolicy::Abandon, PollingClosePolicy::Flush] {
            let transport = RecordingFrameTransport {
                send_pending: true,
                close_pending: true,
                ..RecordingFrameTransport::default()
            };
            let options = PollingClientOptions {
                close_policy,
                ..PollingClientOptions::default()
            };
            let config = default_config().with_shutdown_timeout(Duration::from_millis(10));
            let mut client = SignalFishPollingClient::new_with_options(transport, config, options);
            let start = Instant::now();

            client.close_at(start);
            assert!(client.is_closing(), "policy {close_policy:?}");
            let _ = client.poll_at(start + Duration::from_millis(10));

            assert!(!client.is_closing(), "policy {close_policy:?}");
            assert_eq!(client.transport.abort_calls, 1, "policy {close_policy:?}");
            assert_eq!(
                client.polling_stats().close_deadline_expirations,
                1,
                "policy {close_policy:?}"
            );
            assert_eq!(
                client.polling_stats().abandoned_commands,
                1,
                "policy {close_policy:?}"
            );
        }
    }

    #[test]
    fn close_drains_inbound_under_the_normal_receive_budget() {
        let transport = RecordingFrameTransport {
            incoming: VecDeque::from([
                TransportFrame::Text("first late frame".to_string()),
                TransportFrame::Binary(vec![1, 2, 3]),
            ]),
            close_pending: true,
            ..RecordingFrameTransport::default()
        };
        let options = PollingClientOptions {
            work_budget: PollingWorkBudget {
                receive_frames: 1,
                ..PollingWorkBudget::default()
            },
            ..PollingClientOptions::default()
        };
        let mut client =
            SignalFishPollingClient::new_with_options(transport, default_config(), options);

        client.close();
        assert!(client.transport.incoming.is_empty());
        assert!(client.pending_inbound.is_some());
        assert!(client.is_closing());

        assert!(client.poll().is_empty());
        assert!(client.transport.incoming.is_empty());
        assert!(client.pending_inbound.is_none());
        assert_eq!(client.polling_stats().receive_budget_exhaustions, 1);
        assert!(client.is_closing());
    }

    #[test]
    fn zero_close_timeout_aborts_immediately() {
        for close_policy in [PollingClosePolicy::Abandon, PollingClosePolicy::Flush] {
            let transport = RecordingFrameTransport {
                close_pending: true,
                ..RecordingFrameTransport::default()
            };
            let config = default_config().with_shutdown_timeout(Duration::ZERO);
            let options = PollingClientOptions {
                close_policy,
                ..PollingClientOptions::default()
            };
            let mut client = SignalFishPollingClient::new_with_options(transport, config, options);

            client.close();

            assert!(!client.is_closing(), "policy {close_policy:?}");
            assert_eq!(client.transport.close_calls, 0, "policy {close_policy:?}");
            assert_eq!(client.transport.abort_calls, 1, "policy {close_policy:?}");
            assert_eq!(
                client.polling_stats().close_deadline_expirations,
                1,
                "policy {close_policy:?}"
            );
        }
    }

    #[test]
    fn peer_closed_is_terminal_under_both_close_policies() {
        for close_policy in [PollingClosePolicy::Abandon, PollingClosePolicy::Flush] {
            let transport = MockTransport::new().with_incoming(vec![None]);
            let options = PollingClientOptions {
                close_policy,
                ..PollingClientOptions::default()
            };
            let mut client =
                SignalFishPollingClient::new_with_options(transport, default_config(), options);

            let events = client.poll();
            client.close();

            assert!(events
                .iter()
                .any(|event| matches!(event, SignalFishEvent::Disconnected { .. })));
            assert!(!client.is_connected(), "policy {close_policy:?}");
            assert!(!client.is_closing(), "policy {close_policy:?}");
            assert!(client.transport.closed, "policy {close_policy:?}");
        }
    }

    #[test]
    fn huge_close_timeout_does_not_create_an_unbounded_deadline_sentinel() {
        let transport = RecordingFrameTransport {
            close_pending: true,
            ..RecordingFrameTransport::default()
        };
        let config = default_config().with_shutdown_timeout(Duration::MAX);
        let mut client = SignalFishPollingClient::new(transport, config);
        let start = Instant::now();

        client.close_at(start);
        assert!(matches!(
            client.close_phase,
            ClosePhase::Closing { started_at } if started_at == start
        ));
        let _ = client.poll_at(start + Duration::from_secs(1));
        assert!(client.is_closing());
        assert_eq!(client.polling_stats().close_deadline_expirations, 0);
    }

    // ── E. Integration Scenarios ───────────────────────────────────

    #[test]
    fn auth_then_join_room_flow() {
        let player_id = uuid::Uuid::from_u128(100);
        let room_id = uuid::Uuid::from_u128(200);
        let authenticated_json = serde_json::to_string(&ServerMessage::Authenticated {
            app_name: "test-app".into(),
            organization: None,
            rate_limits: crate::protocol::RateLimitInfo {
                per_minute: 60,
                per_hour: 1000,
                per_day: 10000,
            },
        })
        .expect("Authenticated ServerMessage must serialize to JSON");
        let room_joined_json = serde_json::to_string(&ServerMessage::RoomJoined(Box::new(
            crate::protocol::RoomJoinedPayload {
                room_id,
                room_code: "FLOW1".into(),
                player_id,
                game_name: "test-game".into(),
                max_players: 4,
                supports_authority: false,
                current_players: vec![],
                is_authority: false,
                lobby_state: crate::protocol::LobbyState::Waiting,
                ready_players: vec![],
                relay_type: "websocket".into(),
                current_spectators: vec![],
                ice_servers: vec![],
                reconnection_token: None,
            },
        )))
        .expect("RoomJoined ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![
            Some(Ok(authenticated_json)),
            Some(Ok(room_joined_json)),
        ]);
        let mut client = SignalFishPollingClient::new(transport, default_config());

        let events = client.poll();

        // Should contain: Connected, Authenticated, RoomJoined
        assert_eq!(events.len(), 3, "expected 3 events, got: {events:?}");
        assert!(matches!(events[0], SignalFishEvent::Connected));
        assert!(matches!(events[1], SignalFishEvent::Authenticated { .. }));
        assert!(matches!(events[2], SignalFishEvent::RoomJoined { .. }));

        // State should be updated.
        assert!(client.is_authenticated());
        assert_eq!(client.current_room_id(), Some(room_id));
        assert_eq!(client.current_player_id(), Some(player_id));
    }

    #[test]
    fn room_join_leave_rejoin_flow() {
        let player_id1 = uuid::Uuid::from_u128(101);
        let room_id1 = uuid::Uuid::from_u128(201);
        let player_id2 = uuid::Uuid::from_u128(102);
        let room_id2 = uuid::Uuid::from_u128(202);

        let room_joined1 = serde_json::to_string(&ServerMessage::RoomJoined(Box::new(
            crate::protocol::RoomJoinedPayload {
                room_id: room_id1,
                room_code: "JOIN1".into(),
                player_id: player_id1,
                game_name: "test-game".into(),
                max_players: 4,
                supports_authority: false,
                current_players: vec![],
                is_authority: false,
                lobby_state: crate::protocol::LobbyState::Waiting,
                ready_players: vec![],
                relay_type: "websocket".into(),
                current_spectators: vec![],
                ice_servers: vec![],
                reconnection_token: None,
            },
        )))
        .expect("RoomJoined (first) ServerMessage must serialize to JSON");
        let room_left = serde_json::to_string(&ServerMessage::RoomLeft)
            .expect("RoomLeft ServerMessage must serialize to JSON");
        let room_joined2 = serde_json::to_string(&ServerMessage::RoomJoined(Box::new(
            crate::protocol::RoomJoinedPayload {
                room_id: room_id2,
                room_code: "JOIN2".into(),
                player_id: player_id2,
                game_name: "test-game-2".into(),
                max_players: 6,
                supports_authority: true,
                current_players: vec![],
                is_authority: true,
                lobby_state: crate::protocol::LobbyState::Lobby,
                ready_players: vec![],
                relay_type: "tcp".into(),
                current_spectators: vec![],
                ice_servers: vec![],
                reconnection_token: None,
            },
        )))
        .expect("RoomJoined (second) ServerMessage must serialize to JSON");

        let transport = MockTransport::new().with_incoming(vec![
            Some(Ok(room_joined1)),
            Some(Ok(room_left)),
            Some(Ok(room_joined2)),
        ]);
        let mut client = SignalFishPollingClient::new(transport, default_config());

        let events = client.poll();

        // After all messages: Connected, RoomJoined, RoomLeft, RoomJoined
        assert_eq!(events.len(), 4, "expected 4 events, got: {events:?}");
        assert!(matches!(events[0], SignalFishEvent::Connected));
        assert!(matches!(events[1], SignalFishEvent::RoomJoined { .. }));
        assert!(matches!(events[2], SignalFishEvent::RoomLeft));
        assert!(matches!(events[3], SignalFishEvent::RoomJoined { .. }));

        // Final state should reflect the second room.
        assert_eq!(client.current_room_id(), Some(room_id2));
        assert_eq!(client.current_player_id(), Some(player_id2));
        assert_eq!(client.current_room_code(), Some("JOIN2"));
    }

    #[test]
    fn multiple_commands_in_one_poll() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll(); // flush auth

        client
            .leave_room()
            .expect("leave_room must succeed on connected client");
        client
            .join_room(JoinRoomParams::new("game", "Player"))
            .expect("join_room must succeed on connected client");
        client
            .ping()
            .expect("ping must succeed on connected client");

        client.poll();

        // After the initial auth message (index 0), we should have 3 more.
        assert_eq!(
            client.transport.sent.len(),
            4,
            "expected 4 total sent messages (auth + 3 commands), got: {:?}",
            client.transport.sent
        );
        let leave: serde_json::Value = serde_json::from_str(&client.transport.sent[1])
            .expect("leave_room sent message must be valid JSON");
        let join: serde_json::Value = serde_json::from_str(&client.transport.sent[2])
            .expect("join_room sent message must be valid JSON");
        let ping: serde_json::Value = serde_json::from_str(&client.transport.sent[3])
            .expect("ping sent message must be valid JSON");
        assert_eq!(leave["type"], "LeaveRoom");
        assert_eq!(join["type"], "JoinRoom");
        assert_eq!(ping["type"], "Ping");
    }

    #[test]
    fn poll_returns_empty_when_disconnected() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.close();

        let events = client.poll();
        assert!(
            events.is_empty(),
            "expected empty events after disconnect, got: {events:?}"
        );
    }

    #[test]
    fn disconnect_clears_all_state() {
        let room_joined_json = r#"{"type":"RoomJoined","data":{"room_id":"00000000-0000-0000-0000-000000000001","room_code":"ABC123","player_id":"00000000-0000-0000-0000-000000000002","game_name":"test-game","max_players":4,"supports_authority":false,"current_players":[],"is_authority":false,"lobby_state":"waiting","ready_players":[],"relay_type":"websocket","current_spectators":[]}}"#;
        let authenticated_json = r#"{"type":"Authenticated","data":{"app_name":"test","rate_limits":{"per_minute":60,"per_hour":1000,"per_day":10000}}}"#;

        let transport = MockTransport::new().with_incoming(vec![
            Some(Ok(authenticated_json.to_string())),
            Some(Ok(room_joined_json.to_string())),
        ]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll();

        // Verify state is populated.
        assert!(client.is_connected());
        assert!(client.is_authenticated());
        assert!(client.current_player_id().is_some());
        assert!(client.current_room_id().is_some());
        assert!(client.current_room_code().is_some());

        // Disconnect.
        client.close();

        // Verify all state is cleared.
        assert!(!client.is_connected());
        assert!(!client.is_authenticated());
        assert!(client.current_player_id().is_none());
        assert!(client.current_room_id().is_none());
        assert!(client.current_room_code().is_none());
    }

    #[test]
    fn debug_impl_shows_expected_fields() {
        let transport = MockTransport::new();
        let client = SignalFishPollingClient::new(transport, default_config());

        let debug_output = format!("{client:?}");
        assert!(
            debug_output.contains("SignalFishPollingClient"),
            "Debug output should contain 'SignalFishPollingClient', got: {debug_output}"
        );
        assert!(
            debug_output.contains("connected"),
            "Debug output should contain 'connected', got: {debug_output}"
        );
        assert!(
            debug_output.contains("authenticated"),
            "Debug output should contain 'authenticated', got: {debug_output}"
        );
    }

    // ── F. Ping/Pong Flow ──────────────────────────────────────────

    #[test]
    fn ping_and_pong_flow() {
        let pong_json = serde_json::to_string(&ServerMessage::Pong)
            .expect("Pong ServerMessage must serialize to JSON");
        let transport = MockTransport::new().with_incoming(vec![Some(Ok(pong_json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());

        // First poll: sends auth, receives Connected + Pong.
        let events = client.poll();
        assert!(
            events.iter().any(|e| matches!(e, SignalFishEvent::Pong)),
            "expected Pong event in first poll, got: {events:?}"
        );

        // Queue a ping command.
        client
            .ping()
            .expect("ping must succeed on connected client");

        // Second poll: sends the ping.
        client.poll();

        // Verify the Ping message was sent.
        let ping_sent = client.transport.sent.iter().any(|s| {
            let v: serde_json::Value =
                serde_json::from_str(s).expect("sent message must be valid JSON");
            v["type"] == "Ping"
        });
        assert!(ping_sent, "expected Ping to be sent");
    }

    // ── G. Pending Close Regression ──────────────────────────────

    #[test]
    fn close_handles_pending_transport_gracefully() {
        let transport = PendingCloseTransport;
        let mut client = SignalFishPollingClient::new(transport, default_config());

        // Sanity: client starts connected.
        assert!(client.is_connected(), "client should start connected");

        // Close the client. The transport's close() returns Pending,
        // but the client should still mark itself as disconnected.
        client.close();

        // 1. Client must be disconnected even though transport.close() was Pending.
        assert!(
            !client.is_connected(),
            "client should be disconnected after close(), even when transport returns Pending"
        );

        // 2. Command methods must return NotConnected.
        let join_result = client.join_room(JoinRoomParams::new("game", "Player"));
        assert!(
            matches!(join_result, Err(SignalFishError::NotConnected)),
            "expected NotConnected after close(), got: {join_result:?}"
        );
    }

    // ── H. Model-based scheduler verification ─────────────────────

    #[derive(Debug, Clone)]
    enum SchedulerOperation {
        EnqueueText,
        EnqueueBinary,
        Poll,
        SetReady(bool),
        RefuseBeforeAcceptance,
        PendingAfterAcceptance(bool),
        CapacityRecovery,
        SetCloseReady(bool),
        ReceiveText,
        ReceiveBinary,
        AdvanceClock(u8),
        Close,
        Abort,
    }

    fn scheduler_operations() -> impl proptest::strategy::Strategy<Value = Vec<SchedulerOperation>>
    {
        use proptest::prelude::*;

        prop::collection::vec(
            prop_oneof![
                5 => Just(SchedulerOperation::EnqueueText),
                5 => Just(SchedulerOperation::EnqueueBinary),
                8 => Just(SchedulerOperation::Poll),
                1 => any::<bool>().prop_map(SchedulerOperation::SetReady),
                2 => Just(SchedulerOperation::RefuseBeforeAcceptance),
                2 => any::<bool>().prop_map(SchedulerOperation::PendingAfterAcceptance),
                2 => Just(SchedulerOperation::CapacityRecovery),
                1 => any::<bool>().prop_map(SchedulerOperation::SetCloseReady),
                2 => Just(SchedulerOperation::ReceiveText),
                2 => Just(SchedulerOperation::ReceiveBinary),
                3 => (0u8..=20).prop_map(SchedulerOperation::AdvanceClock),
                1 => Just(SchedulerOperation::Close),
                1 => Just(SchedulerOperation::Abort),
            ],
            1..96,
        )
        .prop_map(|mut operations| {
            // Every generated close/refusal/in-flight state gets an explicit
            // recovery suffix. The driver must remain stuck until these
            // transitions occur, then finish in finite polls.
            operations.push(SchedulerOperation::CapacityRecovery);
            operations.push(SchedulerOperation::PendingAfterAcceptance(false));
            operations.push(SchedulerOperation::SetCloseReady(true));
            operations.extend((0..8).map(|_| SchedulerOperation::Poll));
            operations
        })
    }

    fn scheduler_budgets() -> impl proptest::strategy::Strategy<Value = PollingWorkBudget> {
        use proptest::prelude::*;

        (1usize..=4, 1usize..=160, 1usize..=4, 1usize..=160).prop_map(
            |(send_frames, send_bytes, receive_frames, receive_bytes)| PollingWorkBudget {
                send_frames,
                send_bytes,
                receive_frames,
                receive_bytes,
            },
        )
    }

    #[derive(Default)]
    struct SchedulerTransport {
        ready: bool,
        accept: bool,
        pending_after_acceptance: bool,
        complete_retained: bool,
        close_ready: bool,
        retained: Option<TransportFrame>,
        accepted: Vec<TransportFrame>,
        incoming: VecDeque<TransportFrame>,
        replacement_seen: bool,
        close_calls: u64,
        abort_calls: u64,
    }

    impl Transport for SchedulerTransport {
        fn poll_send(
            &mut self,
            _cx: &mut std::task::Context<'_>,
            frame: &mut Option<TransportFrame>,
        ) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
            if self.retained.is_some() {
                self.replacement_seen |= frame.is_some();
                if !self.complete_retained {
                    return std::task::Poll::Pending;
                }
                let retained = self
                    .retained
                    .take()
                    .expect("retained frame was checked immediately above");
                let _ = retained;
                return std::task::Poll::Ready(Ok(()));
            }
            if !self.accept {
                return std::task::Poll::Pending;
            }
            let Some(accepted) = frame.take() else {
                return std::task::Poll::Ready(Ok(()));
            };
            self.accepted.push(accepted.clone());
            if self.pending_after_acceptance {
                self.retained = Some(accepted);
                std::task::Poll::Pending
            } else {
                std::task::Poll::Ready(Ok(()))
            }
        }

        fn poll_recv(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<std::result::Result<TransportFrame, SignalFishError>>> {
            self.incoming
                .pop_front()
                .map_or(std::task::Poll::Pending, |frame| {
                    std::task::Poll::Ready(Some(Ok(frame)))
                })
        }

        fn poll_close(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::result::Result<(), SignalFishError>> {
            self.close_calls = self.close_calls.saturating_add(1);
            if self.close_ready {
                std::task::Poll::Ready(Ok(()))
            } else {
                std::task::Poll::Pending
            }
        }

        fn abort(&mut self) {
            self.abort_calls = self.abort_calls.saturating_add(1);
            self.retained = None;
        }

        fn is_ready(&self) -> bool {
            self.ready
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SchedulerReceiveIdentity {
        Text(u64),
        Binary(u64),
    }

    #[derive(Debug, Clone)]
    struct SchedulerInbound {
        frame: TransportFrame,
        identity: SchedulerReceiveIdentity,
    }

    #[derive(Default)]
    struct SchedulerModel {
        commands: VecDeque<(TransportFrame, Instant)>,
        pending: Option<(TransportFrame, Instant)>,
        backend_in_flight: Option<TransportFrame>,
        accepted: Vec<TransportFrame>,
        incoming: VecDeque<SchedulerInbound>,
        current_age: Duration,
        peak_age: Duration,
        connected_emitted: bool,
        abandoned: u64,
        close_started_at: Option<Instant>,
        close_calls: u64,
        abort_calls: u64,
        close_deadline_expirations: u64,
        closed: bool,
    }

    impl SchedulerModel {
        fn sample_age(&mut self, now: Instant) {
            let oldest = self
                .pending
                .as_ref()
                .map(|(_, enqueued_at)| *enqueued_at)
                .or_else(|| self.commands.front().map(|(_, enqueued_at)| *enqueued_at));
            self.current_age = oldest.map_or(Duration::ZERO, |enqueued_at| {
                now.saturating_duration_since(enqueued_at)
            });
            self.peak_age = self.peak_age.max(self.current_age);
        }

        fn client_owned_depth(&self) -> usize {
            self.commands
                .len()
                .saturating_add(usize::from(self.pending.is_some()))
        }

        fn all_outbound_depth(&self) -> usize {
            self.client_owned_depth()
                .saturating_add(usize::from(self.backend_in_flight.is_some()))
        }

        fn abandon_client_owned(&mut self, include_in_flight: bool, now: Instant) -> usize {
            let mut abandoned = self.client_owned_depth();
            self.commands.clear();
            self.pending = None;
            if include_in_flight {
                abandoned =
                    abandoned.saturating_add(usize::from(self.backend_in_flight.take().is_some()));
            }
            self.sample_age(now);
            abandoned
        }

        fn stage_pending(&mut self, now: Instant) {
            if self.pending.is_none() {
                self.pending = self.commands.pop_front();
                if self.pending.is_some() {
                    self.sample_age(now);
                }
            }
        }

        fn is_open(&self) -> bool {
            self.close_started_at.is_none() && !self.closed
        }

        fn is_closing(&self) -> bool {
            self.close_started_at.is_some() && !self.closed
        }
    }

    fn scheduler_text(id: u64) -> (PollingCommand, TransportFrame) {
        let message = ClientMessage::GameData {
            data: serde_json::json!({ "scheduler_id": id }),
            class: None,
            key: None,
        };
        let frame = TransportFrame::Text(
            serde_json::to_string(&message).expect("scheduler text frame should serialize"),
        );
        (PollingCommand::Message(message), frame)
    }

    fn scheduler_binary(id: u64) -> (PollingCommand, TransportFrame) {
        let mut bytes = vec![0x42];
        bytes.extend_from_slice(&id.to_be_bytes());
        (
            PollingCommand::Binary(bytes.clone()),
            TransportFrame::Binary(bytes),
        )
    }

    fn scheduler_inbound(id: u64, binary: bool) -> SchedulerInbound {
        if binary {
            let frame = crate::protocol::V2BinaryGameDataFrame {
                from_player: uuid::Uuid::from_u128(0xfeed),
                encoding: GameDataEncoding::MessagePack,
                payload: id.to_be_bytes().to_vec(),
            };
            SchedulerInbound {
                frame: TransportFrame::Binary(
                    rmp_serde::to_vec_named(&frame)
                        .expect("scheduler binary receive frame should serialize"),
                ),
                identity: SchedulerReceiveIdentity::Binary(id),
            }
        } else {
            SchedulerInbound {
                frame: TransportFrame::Text(
                    serde_json::to_string(&ServerMessage::Error {
                        message: format!("scheduler-receive-{id}"),
                        error_code: None,
                    })
                    .expect("scheduler Error frame should serialize"),
                ),
                identity: SchedulerReceiveIdentity::Text(id),
            }
        }
    }

    fn scheduler_event_identity(event: &SignalFishEvent) -> Option<SchedulerReceiveIdentity> {
        match event {
            SignalFishEvent::Error { message, .. } => message
                .strip_prefix("scheduler-receive-")?
                .parse()
                .ok()
                .map(SchedulerReceiveIdentity::Text),
            SignalFishEvent::GameDataBinary { payload, .. } => {
                let bytes: [u8; 8] = payload.as_slice().try_into().ok()?;
                Some(SchedulerReceiveIdentity::Binary(u64::from_be_bytes(bytes)))
            }
            _ => None,
        }
    }

    fn declarative_admission_count(
        model: &SchedulerModel,
        accept: bool,
        pending_after_acceptance: bool,
        budget: PollingWorkBudget,
    ) -> usize {
        if !accept {
            return 0;
        }
        let candidates = model
            .pending
            .iter()
            .map(|(frame, _)| frame)
            .chain(model.commands.iter().map(|(frame, _)| frame));
        let mut count = 0usize;
        let mut bytes = 0usize;
        for frame in candidates {
            let next_bytes = bytes.saturating_add(frame_payload_len(frame));
            if count >= budget.send_frames || (count > 0 && next_bytes > budget.send_bytes) {
                break;
            }
            count = count.saturating_add(1);
            bytes = next_bytes;
            if pending_after_acceptance {
                break;
            }
        }
        count
    }

    fn drive_scheduler_model(
        model: &mut SchedulerModel,
        now: Instant,
        accept: bool,
        pending_after_acceptance: bool,
        complete_retained: bool,
        budget: PollingWorkBudget,
    ) -> Vec<TransportFrame> {
        if model.backend_in_flight.is_some() {
            if !complete_retained {
                return Vec::new();
            }
            model.backend_in_flight = None;
        }

        model.stage_pending(now);
        let admission_count =
            declarative_admission_count(model, accept, pending_after_acceptance, budget);
        let mut accepted = Vec::new();
        let mut bytes = 0usize;
        for index in 0..admission_count {
            let (frame, _) = model
                .pending
                .take()
                .expect("declarative admission selected an available frame");
            model.accepted.push(frame.clone());
            accepted.push(frame.clone());
            bytes = bytes.saturating_add(frame_payload_len(&frame));
            model.sample_age(now);
            if pending_after_acceptance {
                model.backend_in_flight = Some(frame);
            } else if index + 1 < admission_count {
                model.stage_pending(now);
            }
        }

        // The driver stages the next FIFO frame before discovering that it
        // would exceed a non-exact byte budget. Exact frame/byte exhaustion
        // stops at the queue boundary instead.
        if !pending_after_acceptance
            && accepted.len() < budget.send_frames
            && bytes < budget.send_bytes
        {
            model.stage_pending(now);
        }
        accepted
    }

    #[derive(Clone, Copy)]
    struct SchedulerTransportState {
        accept: bool,
        pending_after_acceptance: bool,
        complete_retained: bool,
        close_ready: bool,
    }

    fn scheduler_transport_state(
        client: &SignalFishPollingClient<SchedulerTransport>,
    ) -> SchedulerTransportState {
        SchedulerTransportState {
            accept: client.transport.accept,
            pending_after_acceptance: client.transport.pending_after_acceptance,
            complete_retained: client.transport.complete_retained,
            close_ready: client.transport.close_ready,
        }
    }

    fn drive_model_close_tick(
        model: &mut SchedulerModel,
        now: Instant,
        transport: SchedulerTransportState,
        budget: PollingWorkBudget,
    ) -> Vec<TransportFrame> {
        let accepted = drive_scheduler_model(
            model,
            now,
            transport.accept,
            transport.pending_after_acceptance,
            transport.complete_retained,
            budget,
        );
        if model.all_outbound_depth() == 0 {
            model.close_calls = model.close_calls.saturating_add(1);
            if transport.close_ready {
                model.closed = true;
            }
        }
        accepted
    }

    fn start_model_close(
        model: &mut SchedulerModel,
        now: Instant,
        flush_on_close: bool,
        transport: SchedulerTransportState,
        budget: PollingWorkBudget,
    ) -> Vec<TransportFrame> {
        if !model.is_open() {
            return Vec::new();
        }
        model.close_started_at = Some(now);
        if !flush_on_close {
            let abandoned = model.abandon_client_owned(false, now);
            model.abandoned = model
                .abandoned
                .saturating_add(u64::try_from(abandoned).unwrap_or(u64::MAX));
        }
        drive_model_close_tick(model, now, transport, budget)
    }

    fn poll_model_close(
        model: &mut SchedulerModel,
        now: Instant,
        transport: SchedulerTransportState,
        budget: PollingWorkBudget,
    ) -> Vec<TransportFrame> {
        model.sample_age(now);
        let started_at = model
            .close_started_at
            .expect("closing model should retain its start time");
        if now.saturating_duration_since(started_at) >= Duration::from_millis(50) {
            let abandoned = model.abandon_client_owned(true, now);
            model.abandoned = model
                .abandoned
                .saturating_add(u64::try_from(abandoned).unwrap_or(u64::MAX));
            model.abort_calls = model.abort_calls.saturating_add(1);
            model.close_deadline_expirations = model.close_deadline_expirations.saturating_add(1);
            model.closed = true;
            Vec::new()
        } else {
            drive_model_close_tick(model, now, transport, budget)
        }
    }

    fn drive_scheduler_receive_model(
        model: &mut SchedulerModel,
        budget: PollingWorkBudget,
    ) -> Vec<SchedulerInbound> {
        let mut processed = Vec::new();
        let mut frames = 0usize;
        let mut bytes = 0usize;
        while let Some(inbound) = model.incoming.front() {
            let next_bytes = bytes.checked_add(frame_payload_len(&inbound.frame));
            if frames > 0
                && (frames >= budget.receive_frames
                    || next_bytes.is_none_or(|next| next > budget.receive_bytes))
            {
                break;
            }
            let inbound = model
                .incoming
                .pop_front()
                .expect("model receive front was just checked");
            frames = frames.saturating_add(1);
            bytes = next_bytes.unwrap_or(usize::MAX);
            processed.push(inbound);
        }
        processed
    }

    fn scheduler_batch_within_budget(
        batch: &[TransportFrame],
        frame_limit: usize,
        byte_limit: usize,
    ) -> bool {
        let bytes = batch.iter().fold(0usize, |total, frame| {
            total.saturating_add(frame_payload_len(frame))
        });
        batch.len() <= frame_limit && (bytes <= byte_limit || batch.len() == 1)
    }

    fn scheduler_transition_matches(
        expected: &[TransportFrame],
        observed: &[TransportFrame],
        budget: PollingWorkBudget,
    ) -> bool {
        expected == observed
            && scheduler_batch_within_budget(observed, budget.send_frames, budget.send_bytes)
    }

    #[test]
    fn scheduler_oracle_rejects_stop_and_wait_and_duplication_models() {
        let budget = PollingWorkBudget {
            send_frames: 2,
            send_bytes: 8,
            ..PollingWorkBudget::default()
        };
        let now = Instant::now();
        let first = TransportFrame::Binary(vec![1; 4]);
        let second = TransportFrame::Binary(vec![2; 4]);
        let mut model = SchedulerModel::default();
        model.commands.push_back((first.clone(), now));
        model.commands.push_back((second.clone(), now));
        let expected = drive_scheduler_model(&mut model, now, true, false, true, budget);

        let stop_and_wait = vec![first.clone()];
        let duplication = vec![first.clone(), first, second];
        assert!(scheduler_transition_matches(&expected, &expected, budget));
        assert!(!scheduler_transition_matches(
            &expected,
            &stop_and_wait,
            budget
        ));
        assert!(!scheduler_transition_matches(
            &expected,
            &duplication,
            budget
        ));
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config {
            cases: 128,
            failure_persistence: Some(Box::new(
                proptest::test_runner::FileFailurePersistence::SourceParallel(
                    "proptest-regressions",
                ),
            )),
            ..proptest::test_runner::Config::default()
        })]

        #[test]
        fn polling_scheduler_matches_reference_model(
            operations in scheduler_operations(),
            flush_on_close in proptest::bool::ANY,
            budget in scheduler_budgets(),
            command_capacity in 1usize..=6,
        ) {
            let options = PollingClientOptions {
                work_budget: budget,
                close_policy: if flush_on_close {
                    PollingClosePolicy::Flush
                } else {
                    PollingClosePolicy::Abandon
                },
            };
            let mut config = default_config()
                .with_command_channel_capacity(command_capacity)
                .with_shutdown_timeout(Duration::from_millis(50));
            config.game_data_format = Some(GameDataEncoding::MessagePack);
            let transport = SchedulerTransport {
                ready: false,
                accept: true,
                complete_retained: true,
                close_ready: true,
                ..SchedulerTransport::default()
            };
            let mut client = SignalFishPollingClient::new_with_options(transport, config, options);
            let mut now = Instant::now();
            let startup_events = client.poll_at(now);
            prop_assert!(!startup_events
                .iter()
                .any(|event| matches!(event, SignalFishEvent::Connected)));
            client.transport.accepted.clear();
            client.reset_queue_age_peak_at(now);
            let mut model = SchedulerModel::default();
            let mut next_outbound_id = 1u64;
            let mut next_receive_id = 1u64;

            for operation in operations {
                if model.closed {
                    prop_assert!(matches!(
                        client.queue_command_at(PollingCommand::Binary(vec![0]), now),
                        Err(SignalFishError::NotConnected)
                    ));
                    continue;
                }
                match operation {
                    operation @ (SchedulerOperation::EnqueueText
                    | SchedulerOperation::EnqueueBinary) => {
                        let id = next_outbound_id;
                        next_outbound_id = next_outbound_id.saturating_add(1);
                        let (command, frame) = match operation {
                            SchedulerOperation::EnqueueText => scheduler_text(id),
                            SchedulerOperation::EnqueueBinary => scheduler_binary(id),
                            _ => unreachable!("enqueue alternatives were matched above"),
                        };
                        if model.is_closing() {
                            prop_assert!(matches!(
                                client.queue_command_at(command, now),
                                Err(SignalFishError::NotConnected)
                            ));
                        } else {
                            let expected_full = model.commands.len() >= command_capacity;
                            let result = client.queue_command_at(command, now);
                            if expected_full {
                                match result {
                                    Err(SignalFishError::SendBufferFull { capacity }) => {
                                        prop_assert_eq!(capacity, command_capacity);
                                    }
                                    other => prop_assert!(
                                        false,
                                        "full scheduler queue returned {other:?}"
                                    ),
                                }
                            } else {
                                prop_assert!(result.is_ok());
                                model.commands.push_back((frame, now));
                                model.sample_age(now);
                            }
                        }
                    }
                    SchedulerOperation::Poll => {
                        let was_open = model.is_open();
                        let transport_state = scheduler_transport_state(&client);
                        let accepted_before = client.transport.accepted.len();
                        let events = client.poll_at(now);
                        let expected_accepted = if was_open {
                            model.sample_age(now);
                            drive_scheduler_model(
                                &mut model,
                                now,
                                transport_state.accept,
                                transport_state.pending_after_acceptance,
                                transport_state.complete_retained,
                                budget,
                            )
                        } else {
                            poll_model_close(&mut model, now, transport_state, budget)
                        };
                        let actual_accepted = &client.transport.accepted[accepted_before..];
                        prop_assert!(scheduler_transition_matches(
                            &expected_accepted,
                            actual_accepted,
                            budget
                        ));

                        let should_connect =
                            was_open && client.transport.ready && !model.connected_emitted;
                        let connected_count = events
                            .iter()
                            .filter(|event| matches!(event, SignalFishEvent::Connected))
                            .count();
                        prop_assert_eq!(connected_count, usize::from(should_connect));
                        if should_connect {
                            model.connected_emitted = true;
                        }

                        let expected_inbound = if was_open {
                            drive_scheduler_receive_model(&mut model, budget)
                        } else {
                            Vec::new()
                        };
                        let actual_inbound = events
                            .iter()
                            .filter_map(scheduler_event_identity)
                            .collect::<Vec<_>>();
                        let expected_identities = expected_inbound
                            .iter()
                            .map(|inbound| inbound.identity)
                            .collect::<Vec<_>>();
                        prop_assert_eq!(&actual_inbound, &expected_identities);
                        let processed_frames = expected_inbound
                            .iter()
                            .map(|inbound| inbound.frame.clone())
                            .collect::<Vec<_>>();
                        prop_assert!(scheduler_batch_within_budget(
                            &processed_frames,
                            budget.receive_frames,
                            budget.receive_bytes,
                        ));
                    }
                    SchedulerOperation::SetReady(ready) => client.transport.ready = ready,
                    SchedulerOperation::RefuseBeforeAcceptance => client.transport.accept = false,
                    SchedulerOperation::PendingAfterAcceptance(pending) => {
                        client.transport.pending_after_acceptance = pending;
                        if pending {
                            client.transport.complete_retained = false;
                        }
                    }
                    SchedulerOperation::CapacityRecovery => {
                        client.transport.accept = true;
                        client.transport.complete_retained = true;
                    }
                    SchedulerOperation::SetCloseReady(ready) => {
                        client.transport.close_ready = ready;
                    }
                    operation @ (SchedulerOperation::ReceiveText
                    | SchedulerOperation::ReceiveBinary) => {
                        let inbound = scheduler_inbound(
                            next_receive_id,
                            matches!(operation, SchedulerOperation::ReceiveBinary),
                        );
                        next_receive_id = next_receive_id.saturating_add(1);
                        client.transport.incoming.push_back(inbound.frame.clone());
                        model.incoming.push_back(inbound);
                    }
                    SchedulerOperation::AdvanceClock(milliseconds) => {
                        now += Duration::from_millis(u64::from(milliseconds));
                    }
                    SchedulerOperation::Close => {
                        let transport_state = scheduler_transport_state(&client);
                        let accepted_before = client.transport.accepted.len();
                        let expected = start_model_close(
                            &mut model,
                            now,
                            flush_on_close,
                            transport_state,
                            budget,
                        );
                        client.close_at(now);
                        let actual = &client.transport.accepted[accepted_before..];
                        prop_assert!(scheduler_transition_matches(&expected, actual, budget));
                    }
                    SchedulerOperation::Abort => {
                        client.transport.accept = false;
                        client.transport.complete_retained = false;
                        client.transport.close_ready = false;
                        let transport_state = SchedulerTransportState {
                            accept: false,
                            pending_after_acceptance: client.transport.pending_after_acceptance,
                            complete_retained: false,
                            close_ready: false,
                        };
                        if model.is_open() {
                            let accepted_before = client.transport.accepted.len();
                            let expected = start_model_close(
                                &mut model,
                                now,
                                flush_on_close,
                                transport_state,
                                budget,
                            );
                            client.close_at(now);
                            let actual = &client.transport.accepted[accepted_before..];
                            prop_assert!(scheduler_transition_matches(&expected, actual, budget));
                        }
                        let deadline = model
                            .close_started_at
                            .expect("abort operation should start close")
                            + Duration::from_millis(50);
                        now = now.max(deadline);
                        let expected = poll_model_close(&mut model, now, transport_state, budget);
                        prop_assert!(expected.is_empty());
                        let _ = client.poll_at(now);
                    }
                }

                prop_assert!(client.cmd_queue.len() <= command_capacity);
                prop_assert!(!client.transport.replacement_seen);
                prop_assert_eq!(
                    client.polling_stats.current_queue_depth,
                    u64::try_from(model.client_owned_depth())
                        .unwrap_or(u64::MAX),
                );
                prop_assert_eq!(
                    client.queue_age_stats.current_oldest_queue_age,
                    model.current_age
                );
                prop_assert_eq!(
                    client.queue_age_stats.peak_oldest_queue_age,
                    model.peak_age
                );
                prop_assert_eq!(client.polling_stats.abandoned_commands, model.abandoned);
                prop_assert_eq!(client.transport.close_calls, model.close_calls);
                prop_assert_eq!(client.transport.abort_calls, model.abort_calls);
                prop_assert_eq!(
                    client.polling_stats.close_deadline_expirations,
                    model.close_deadline_expirations
                );
                prop_assert_eq!(client.is_closing(), model.is_closing());
                prop_assert_eq!(&client.transport.accepted, &model.accepted);
                if model.closed {
                    prop_assert!(!client.is_closing());
                    prop_assert_eq!(client.polling_stats.current_queue_depth, 0);
                    prop_assert_eq!(model.current_age, Duration::ZERO);
                    prop_assert!(matches!(
                        client.queue_command_at(PollingCommand::Binary(vec![0]), now),
                        Err(SignalFishError::NotConnected)
                    ));
                    break;
                }
            }
            prop_assert!(!model.is_closing(), "recovery suffix must finish close");
            prop_assert!(!client.is_closing(), "driver must finish after recovery suffix");
        }
    }
}
