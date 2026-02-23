//! Async client for the Signal Fish signaling protocol.
//!
//! [`SignalFishClient`] is a thin handle that communicates with a background
//! transport loop task via an unbounded MPSC channel. Events are emitted on a
//! bounded channel ([`tokio::sync::mpsc::Receiver<SignalFishEvent>`]) returned
//! from [`SignalFishClient::start`].
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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, warn};

use crate::error::{Result, SignalFishError};
use crate::event::SignalFishEvent;
use crate::protocol::{
    ClientMessage, ConnectionInfo, GameDataEncoding, PlayerId, RelayTransport, RoomId,
    ServerMessage,
};
use crate::transport::Transport;

/// Default capacity of the bounded event channel.
const DEFAULT_EVENT_CHANNEL_CAPACITY: usize = 256;

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
    /// Capacity of the bounded event channel.
    ///
    /// When the consumer cannot keep up with incoming server messages, events
    /// are dropped (with a warning logged) to avoid blocking the transport loop.
    /// The `Disconnected` event is always delivered regardless of capacity.
    ///
    /// Defaults to **256**. Values below 1 are clamped to 1.
    pub event_channel_capacity: usize,
    /// Timeout for the graceful shutdown.
    ///
    /// When [`SignalFishClient::shutdown`] is called, the background transport
    /// loop is given this much time to close the transport and emit a final
    /// `Disconnected` event. If the timeout expires the task is aborted.
    ///
    /// Defaults to **1 second**. A zero timeout aborts the transport loop
    /// immediately without waiting for graceful shutdown.
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
            event_channel_capacity: DEFAULT_EVENT_CHANNEL_CAPACITY,
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

    /// Set the timeout for the graceful shutdown.
    ///
    /// Defaults to **1 second**. A zero timeout aborts the transport loop
    /// immediately without waiting for graceful shutdown.
    #[must_use]
    pub fn with_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
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

// ── Shared state ────────────────────────────────────────────────────

/// Internal shared state between the client handle and the transport loop.
struct ClientState {
    connected: AtomicBool,
    authenticated: AtomicBool,
    player_id: Mutex<Option<PlayerId>>,
    room_id: Mutex<Option<RoomId>>,
    room_code: Mutex<Option<String>>,
}

impl ClientState {
    fn new() -> Self {
        Self {
            connected: AtomicBool::new(true),
            authenticated: AtomicBool::new(false),
            player_id: Mutex::new(None),
            room_id: Mutex::new(None),
            room_code: Mutex::new(None),
        }
    }
}

// ── Client handle ───────────────────────────────────────────────────

/// Async client handle for the Signal Fish signaling protocol.
///
/// Created via [`SignalFishClient::start`], which spawns a background transport
/// loop and returns this handle together with an event receiver.
///
/// All public methods serialize a [`ClientMessage`] and send it to the
/// transport loop over an unbounded channel. They return immediately once the
/// message is queued (no round-trip await).
pub struct SignalFishClient {
    /// Sender half of the command channel to the transport loop.
    cmd_tx: mpsc::UnboundedSender<ClientMessage>,
    /// Shared state updated by the transport loop.
    state: Arc<ClientState>,
    /// Handle to the background transport loop task.
    task: Option<tokio::task::JoinHandle<()>>,
    /// Oneshot sender to signal the transport loop to shut down gracefully.
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    /// Timeout for the graceful shutdown.
    shutdown_timeout: Duration,
}

impl SignalFishClient {
    /// Start the client transport loop and return a handle plus event receiver.
    ///
    /// The transport loop immediately sends an [`Authenticate`](ClientMessage::Authenticate)
    /// message using the provided [`SignalFishConfig`].
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
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<ClientMessage>();
        // Clamp capacity to at least 1 (tokio panics on 0).
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
        };
        // This cannot fail because we just created the channel.
        let _ = cmd_tx.send(auth_msg);

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

    // ── Public API methods ──────────────────────────────────────────

    /// Join or create a room with the given parameters.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed.
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
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed.
    pub fn leave_room(&self) -> Result<()> {
        self.send(ClientMessage::LeaveRoom)
    }

    /// Send arbitrary JSON game data to other players in the room.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed.
    pub fn send_game_data(&self, data: serde_json::Value) -> Result<()> {
        self.send(ClientMessage::GameData { data })
    }

    /// Signal readiness to start the game in the lobby.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed.
    pub fn set_ready(&self) -> Result<()> {
        self.send(ClientMessage::PlayerReady)
    }

    /// Request to become (or relinquish) authority.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed.
    pub fn request_authority(&self, become_authority: bool) -> Result<()> {
        self.send(ClientMessage::AuthorityRequest { become_authority })
    }

    /// Provide connection information for P2P establishment.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed.
    pub fn provide_connection_info(&self, connection_info: ConnectionInfo) -> Result<()> {
        self.send(ClientMessage::ProvideConnectionInfo { connection_info })
    }

    /// Reconnect to a room after a disconnection.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed.
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
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed.
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
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed.
    pub fn leave_spectator(&self) -> Result<()> {
        self.send(ClientMessage::LeaveSpectator)
    }

    /// Send a heartbeat ping to the server.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed.
    pub fn ping(&self) -> Result<()> {
        self.send(ClientMessage::Ping)
    }

    /// Shut down the client, closing the transport and stopping the background task.
    ///
    /// After calling this method, the event receiver will yield `None` once the
    /// transport loop exits.
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
    }

    // ── State accessors ─────────────────────────────────────────────

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

    // ── Internal helpers ────────────────────────────────────────────

    /// Queue a `ClientMessage` to the transport loop.
    fn send(&self, msg: ClientMessage) -> Result<()> {
        if !self.state.connected.load(Ordering::Acquire) {
            return Err(SignalFishError::NotConnected);
        }
        self.cmd_tx
            .send(msg)
            .map_err(|_| SignalFishError::NotConnected)
    }
}

impl std::fmt::Debug for SignalFishClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalFishClient")
            .field("connected", &self.is_connected())
            .field("authenticated", &self.is_authenticated())
            .field("has_task", &self.task.is_some())
            .finish()
    }
}

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
async fn transport_loop(
    mut transport: impl Transport,
    mut cmd_rx: mpsc::UnboundedReceiver<ClientMessage>,
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
async fn update_state(state: &ClientState, msg: &ServerMessage) {
    match msg {
        ServerMessage::Authenticated { .. } => {
            state.authenticated.store(true, Ordering::Release);
            debug!("state: authenticated");
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
        _ => {}
    }
}

/// Emit an event to the event channel. If the channel is full, log a warning
/// and drop the event to avoid blocking the transport loop.
async fn emit_event(event_tx: &mpsc::Sender<SignalFishEvent>, event: SignalFishEvent) {
    match event_tx.try_send(event) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(dropped)) => {
            warn!(
                "event channel full, dropping event: {:?}",
                std::mem::discriminant(&dropped)
            );
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            debug!("event channel closed, receiver dropped");
        }
    }
}

/// Emit a [`Disconnected`](SignalFishEvent::Disconnected) event and update state.
///
/// Uses `send().await` (blocking) instead of `try_send` because `Disconnected`
/// is always the last event on the channel and must never be silently dropped.
async fn emit_disconnected(
    event_tx: &mpsc::Sender<SignalFishEvent>,
    state: &ClientState,
    reason: Option<String>,
) {
    state.connected.store(false, Ordering::Release);
    state.authenticated.store(false, Ordering::Release);
    let event = SignalFishEvent::Disconnected { reason };
    if event_tx.send(event).await.is_err() {
        debug!("event channel closed, receiver dropped");
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
                // All scripted messages have been delivered — hang forever
                // so the transport loop stays alive until shutdown.
                std::future::pending().await
            }
        }

        async fn close(&mut self) -> std::result::Result<(), SignalFishError> {
            self.closed.store(true, Ordering::Relaxed);
            Ok(())
        }
    }

    // ── Helper ──────────────────────────────────────────────────────

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
            if let ClientMessage::Authenticate { app_id, .. } = first {
                assert_eq!(app_id, "mb_test_123");
            }
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

        // Give the loop a moment to process.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

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
    }

    #[tokio::test]
    async fn ping_sends_ping_message() {
        let (transport, sent, _closed) = MockTransport::new(vec![Some(Ok(authenticated_json()))]);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        let _ = events.recv().await; // Connected
        let _ = events.recv().await; // Authenticated
        client.ping().unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

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
        assert_eq!(config.event_channel_capacity, 256);
        assert_eq!(config.shutdown_timeout, std::time::Duration::from_secs(1));
    }

    #[tokio::test]
    async fn config_builder_methods() {
        let config = SignalFishConfig::new("mb_test")
            .with_event_channel_capacity(512)
            .with_shutdown_timeout(std::time::Duration::from_secs(5));
        assert_eq!(config.event_channel_capacity, 512);
        assert_eq!(config.shutdown_timeout, std::time::Duration::from_secs(5));
    }

    #[tokio::test]
    async fn event_channel_capacity_is_clamped_to_one() {
        let config = SignalFishConfig::new("mb_test").with_event_channel_capacity(0);
        assert_eq!(config.event_channel_capacity, 1);
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
    async fn small_event_channel_capacity_triggers_backpressure() {
        // Use a capacity of 1 and send multiple messages — events should be dropped.
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

        // Let the channel fill up and events get dropped.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let mut count = 0;
        while let Some(_event) = events.recv().await {
            count += 1;
        }
        // With capacity 1, we should receive fewer events than were sent.
        // At minimum we get Connected (first try_send succeeds) and Disconnected
        // (always delivered via blocking send().await). Authenticated and Pong
        // events may be dropped when the single-slot channel is full.
        assert!(count >= 2, "expected at least 2 events, got {count}");
        // But fewer than the total sent (2 synthetic + 1 auth + 20 pongs = 23 possible).
        assert!(
            count < 23,
            "expected backpressure to drop some events, but got all {count}"
        );

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
        close_called: Arc<AtomicBool>,
        dropped: Arc<AtomicBool>,
    }

    impl HangingCloseTransport {
        fn new() -> (Self, Arc<AtomicBool>, Arc<AtomicBool>) {
            let close_called = Arc::new(AtomicBool::new(false));
            let dropped = Arc::new(AtomicBool::new(false));
            (
                Self {
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
            std::future::pending().await
        }

        async fn close(&mut self) -> std::result::Result<(), SignalFishError> {
            self.close_called.store(true, Ordering::Release);
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

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

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

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

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
    async fn event_channel_backpressure_does_not_block() {
        // Create a transport with more messages than the event channel capacity.
        let mut incoming: Vec<Option<std::result::Result<String, SignalFishError>>> = Vec::new();
        incoming.push(Some(Ok(authenticated_json())));
        // Fill more than DEFAULT_EVENT_CHANNEL_CAPACITY pong messages.
        let pong_json = serde_json::to_string(&ServerMessage::Pong).unwrap();
        for _ in 0..(DEFAULT_EVENT_CHANNEL_CAPACITY + 50) {
            incoming.push(Some(Ok(pong_json.clone())));
        }
        // End with a clean close.
        incoming.push(None);

        let (transport, _sent, _closed) = MockTransport::new(incoming);

        let config = SignalFishConfig::new("mb_test");
        let (mut client, mut events) = SignalFishClient::start(transport, config);

        // Don't read events immediately — let the channel fill up.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Now drain events. The loop should have completed (possibly
        // dropping some events due to backpressure) without blocking.
        let mut count = 0;
        while let Some(_event) = events.recv().await {
            count += 1;
        }
        // We should have received at least some events.
        assert!(count > 0, "expected to receive at least some events");

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

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

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

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

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
}
