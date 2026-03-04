//! Synchronous, polling-based client for the Signal Fish signaling protocol.
//!
//! [`SignalFishPollingClient`] is designed for environments without an async
//! runtime (e.g., Godot web builds via gdext on `wasm32-unknown-emscripten`).
//! The caller drives the client by calling [`poll()`](SignalFishPollingClient::poll)
//! once per frame from the game loop.

use std::collections::VecDeque;

use tracing::{debug, error, warn};

use crate::client::{JoinRoomParams, SignalFishConfig};
use crate::error::{Result, SignalFishError};
use crate::event::SignalFishEvent;
use crate::protocol::{ClientMessage, ConnectionInfo, PlayerId, RoomId, ServerMessage};
use crate::transport::Transport;

// ── Internal state ──────────────────────────────────────────────────

/// Internal state for the polling client. No `Arc`/`Mutex` needed — single-threaded.
struct PollingClientState {
    connected: bool,
    authenticated: bool,
    player_id: Option<PlayerId>,
    room_id: Option<RoomId>,
    room_code: Option<String>,
}

impl PollingClientState {
    fn new() -> Self {
        Self {
            connected: true,
            authenticated: false,
            player_id: None,
            room_id: None,
            room_code: None,
        }
    }

    fn clear_session(&mut self) {
        self.authenticated = false;
        self.player_id = None;
        self.room_id = None;
        self.room_code = None;
    }
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
///     EmscriptenWebSocketTransport, SignalFishPollingClient,
///     SignalFishConfig, JoinRoomParams, SignalFishEvent,
/// };
///
/// struct MyNode {
///     client: Option<SignalFishPollingClient<EmscriptenWebSocketTransport>>,
/// }
///
/// impl MyNode {
///     fn ready(&mut self) {
///         let transport = EmscriptenWebSocketTransport::connect("wss://server/ws")
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
    cmd_queue: VecDeque<ClientMessage>,
    state: PollingClientState,
    started: bool,
}

impl<T: Transport> SignalFishPollingClient<T> {
    /// Create a new polling client with the given transport and configuration.
    ///
    /// Immediately queues an [`Authenticate`](ClientMessage::Authenticate) message
    /// and a synthetic [`Connected`](SignalFishEvent::Connected) event, which will
    /// be delivered on the first call to [`poll()`](Self::poll).
    #[must_use]
    pub fn new(transport: T, config: SignalFishConfig) -> Self {
        let auth_msg = ClientMessage::Authenticate {
            app_id: config.app_id,
            sdk_version: config.sdk_version,
            platform: config.platform,
            game_data_format: config.game_data_format,
        };

        let mut cmd_queue = VecDeque::new();
        cmd_queue.push_back(auth_msg);

        Self {
            transport,
            cmd_queue,
            state: PollingClientState::new(),
            started: false,
        }
    }

    // ── Core polling method ─────────────────────────────────────────

    /// Drive the client for one frame.
    ///
    /// Flushes all queued outgoing commands, then reads all available incoming
    /// messages from the transport. Returns a `Vec` of events that occurred
    /// during this poll cycle.
    ///
    /// Call this method once per frame from your game loop.
    pub fn poll(&mut self) -> Vec<SignalFishEvent> {
        let mut events = Vec::new();

        if !self.state.connected {
            return events;
        }

        // Emit Connected on the very first poll.
        if !self.started {
            self.started = true;
            events.push(SignalFishEvent::Connected);
        }

        // Create a noop waker to poll transport futures synchronously.
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);

        // ── Flush outgoing commands ──
        while let Some(msg) = self.cmd_queue.pop_front() {
            let json = match serde_json::to_string(&msg) {
                Ok(json) => json,
                Err(e) => {
                    error!("failed to serialize ClientMessage: {e}");
                    continue;
                }
            };

            // Poll transport.send() and capture the result before using self again.
            let send_result = {
                let mut fut = std::pin::pin!(self.transport.send(json));
                std::future::Future::poll(fut.as_mut(), &mut cx)
            };

            match send_result {
                std::task::Poll::Ready(Ok(())) => {}
                std::task::Poll::Ready(Err(e)) => {
                    error!("transport send error: {e}");
                    self.handle_disconnect(&mut events, Some(format!("transport send error: {e}")));
                    return events;
                }
                std::task::Poll::Pending => {
                    // Put the message back at the front and try next frame.
                    warn!("transport send returned Pending, retrying next frame");
                    self.cmd_queue.push_front(msg);
                    break;
                }
            }
        }

        // ── Drain incoming messages ──
        loop {
            // Poll transport.recv() and capture the result before using self again.
            let recv_result = {
                let mut fut = std::pin::pin!(self.transport.recv());
                std::future::Future::poll(fut.as_mut(), &mut cx)
            };

            match recv_result {
                std::task::Poll::Ready(Some(Ok(text))) => {
                    match serde_json::from_str::<ServerMessage>(&text) {
                        Ok(server_msg) => {
                            self.update_state(&server_msg);
                            events.push(SignalFishEvent::from(server_msg));
                        }
                        Err(e) => {
                            warn!("failed to deserialize server message: {e} — raw: {text}");
                        }
                    }
                }
                std::task::Poll::Ready(Some(Err(e))) => {
                    error!("transport receive error: {e}");
                    self.handle_disconnect(
                        &mut events,
                        Some(format!("transport receive error: {e}")),
                    );
                    break;
                }
                std::task::Poll::Ready(None) => {
                    debug!("transport closed by server");
                    self.handle_disconnect(&mut events, None);
                    break;
                }
                std::task::Poll::Pending => {
                    // No more buffered messages this frame.
                    break;
                }
            }
        }

        events
    }

    // ── Public API methods (mirror SignalFishClient) ────────────────

    /// Join or create a room with the given parameters.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed.
    pub fn join_room(&mut self, params: JoinRoomParams) -> Result<()> {
        self.queue_cmd(ClientMessage::JoinRoom {
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
    pub fn leave_room(&mut self) -> Result<()> {
        self.queue_cmd(ClientMessage::LeaveRoom)
    }

    /// Send game data to other players in the room.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed.
    pub fn send_game_data(&mut self, data: serde_json::Value) -> Result<()> {
        self.queue_cmd(ClientMessage::GameData { data })
    }

    /// Signal readiness to start the game.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed.
    pub fn set_ready(&mut self) -> Result<()> {
        self.queue_cmd(ClientMessage::PlayerReady)
    }

    /// Request or relinquish authority status.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed.
    pub fn request_authority(&mut self, become_authority: bool) -> Result<()> {
        self.queue_cmd(ClientMessage::AuthorityRequest { become_authority })
    }

    /// Provide connection info for P2P establishment.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed.
    pub fn provide_connection_info(&mut self, connection_info: ConnectionInfo) -> Result<()> {
        self.queue_cmd(ClientMessage::ProvideConnectionInfo { connection_info })
    }

    /// Reconnect to a room after disconnection.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed.
    pub fn reconnect(
        &mut self,
        player_id: PlayerId,
        room_id: RoomId,
        auth_token: String,
    ) -> Result<()> {
        self.queue_cmd(ClientMessage::Reconnect {
            player_id,
            room_id,
            auth_token,
        })
    }

    /// Join a room as a spectator (read-only observer).
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed.
    pub fn join_as_spectator(
        &mut self,
        game_name: String,
        room_code: String,
        spectator_name: String,
    ) -> Result<()> {
        self.queue_cmd(ClientMessage::JoinAsSpectator {
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
    pub fn leave_spectator(&mut self) -> Result<()> {
        self.queue_cmd(ClientMessage::LeaveSpectator)
    }

    /// Send a heartbeat ping.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::NotConnected`] if the transport has closed.
    pub fn ping(&mut self) -> Result<()> {
        self.queue_cmd(ClientMessage::Ping)
    }

    // ── State accessors ─────────────────────────────────────────────

    /// Whether the transport connection is still alive.
    pub fn is_connected(&self) -> bool {
        self.state.connected
    }

    /// Whether the client has received an `Authenticated` response.
    pub fn is_authenticated(&self) -> bool {
        self.state.authenticated
    }

    /// The local player's ID, set after joining a room.
    pub fn current_player_id(&self) -> Option<PlayerId> {
        self.state.player_id
    }

    /// The current room ID, set after joining a room.
    pub fn current_room_id(&self) -> Option<RoomId> {
        self.state.room_id
    }

    /// The current room code, set after joining a room.
    pub fn current_room_code(&self) -> Option<&str> {
        self.state.room_code.as_deref()
    }

    // ── Close ───────────────────────────────────────────────────────

    /// Close the transport and mark the client as disconnected.
    pub fn close(&mut self) {
        if !self.state.connected {
            return;
        }
        self.state.connected = false;

        // Poll transport.close() in a separate scope to avoid borrow conflicts.
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let _ = {
            let mut fut = std::pin::pin!(self.transport.close());
            std::future::Future::poll(fut.as_mut(), &mut cx)
        };

        self.state.clear_session();
    }

    // ── Private helpers ─────────────────────────────────────────────

    fn queue_cmd(&mut self, msg: ClientMessage) -> Result<()> {
        if !self.state.connected {
            return Err(SignalFishError::NotConnected);
        }
        self.cmd_queue.push_back(msg);
        Ok(())
    }

    fn handle_disconnect(&mut self, events: &mut Vec<SignalFishEvent>, reason: Option<String>) {
        self.state.connected = false;
        self.state.clear_session();
        events.push(SignalFishEvent::Disconnected { reason });
    }

    fn update_state(&mut self, msg: &ServerMessage) {
        match msg {
            ServerMessage::Authenticated { .. } => {
                self.state.authenticated = true;
            }
            ServerMessage::RoomJoined(payload) => {
                self.state.player_id = Some(payload.player_id);
                self.state.room_id = Some(payload.room_id);
                self.state.room_code = Some(payload.room_code.clone());
            }
            ServerMessage::RoomLeft => {
                self.state.room_id = None;
                self.state.room_code = None;
            }
            ServerMessage::Reconnected(payload) => {
                self.state.player_id = Some(payload.player_id);
                self.state.room_id = Some(payload.room_id);
                self.state.room_code = Some(payload.room_code.clone());
            }
            ServerMessage::SpectatorJoined(payload) => {
                self.state.player_id = Some(payload.spectator_id);
                self.state.room_id = Some(payload.room_id);
                self.state.room_code = Some(payload.room_code.clone());
            }
            ServerMessage::SpectatorLeft { .. } => {
                self.state.room_id = None;
                self.state.room_code = None;
            }
            _ => {}
        }
    }
}

// ── Debug ───────────────────────────────────────────────────────────

impl<T: Transport> std::fmt::Debug for SignalFishPollingClient<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalFishPollingClient")
            .field("connected", &self.state.connected)
            .field("authenticated", &self.state.authenticated)
            .field("started", &self.started)
            .field("queued_commands", &self.cmd_queue.len())
            .finish()
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

    use async_trait::async_trait;

    use super::*;

    // ── Mock transport ──────────────────────────────────────────────

    struct MockTransport {
        incoming: VecDeque<Option<std::result::Result<String, SignalFishError>>>,
        sent: Vec<String>,
        closed: bool,
    }

    impl MockTransport {
        fn new() -> Self {
            Self {
                incoming: VecDeque::new(),
                sent: Vec::new(),
                closed: false,
            }
        }

        fn with_incoming(
            mut self,
            msgs: Vec<Option<std::result::Result<String, SignalFishError>>>,
        ) -> Self {
            self.incoming = msgs.into();
            self
        }
    }

    #[async_trait]
    impl Transport for MockTransport {
        async fn send(&mut self, message: String) -> std::result::Result<(), SignalFishError> {
            self.sent.push(message);
            Ok(())
        }

        async fn recv(&mut self) -> Option<std::result::Result<String, SignalFishError>> {
            if let Some(item) = self.incoming.pop_front() {
                item
            } else {
                std::future::pending().await
            }
        }

        async fn close(&mut self) -> std::result::Result<(), SignalFishError> {
            self.closed = true;
            Ok(())
        }
    }

    fn default_config() -> SignalFishConfig {
        SignalFishConfig::new("test_app_id")
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
        let sent_json: serde_json::Value = serde_json::from_str(&client.transport.sent[0]).unwrap();
        assert_eq!(sent_json["type"], "Authenticate");
        assert_eq!(sent_json["data"]["app_id"], "test_app_id");
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

        let expected_room_id: uuid::Uuid = "00000000-0000-0000-0000-000000000001".parse().unwrap();
        let expected_player_id: uuid::Uuid =
            "00000000-0000-0000-0000-000000000002".parse().unwrap();
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
            .unwrap();

        // Poll again to flush the join_room command.
        client.poll();

        // The last sent message should be a JoinRoom command.
        let last_sent = client.transport.sent.last().unwrap();
        let sent_json: serde_json::Value = serde_json::from_str(last_sent).unwrap();
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

        let expected_player_id: uuid::Uuid =
            "00000000-0000-0000-0000-000000000003".parse().unwrap();
        let expected_room_id: uuid::Uuid = "00000000-0000-0000-0000-000000000001".parse().unwrap();
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

        let expected_player_id: uuid::Uuid =
            "00000000-0000-0000-0000-000000000004".parse().unwrap();
        let expected_room_id: uuid::Uuid = "00000000-0000-0000-0000-000000000001".parse().unwrap();
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

        // Should contain Connected and Authenticated (malformed message skipped).
        assert_eq!(events.len(), 2, "expected 2 events, got: {events:?}");
        assert!(matches!(events[0], SignalFishEvent::Connected));
        assert!(matches!(events[1], SignalFishEvent::Authenticated { .. }));

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

    #[async_trait]
    impl Transport for ErrorOnSendTransport {
        async fn send(&mut self, _message: String) -> std::result::Result<(), SignalFishError> {
            Err(SignalFishError::TransportSend("write failed".into()))
        }

        async fn recv(&mut self) -> Option<std::result::Result<String, SignalFishError>> {
            std::future::pending().await
        }

        async fn close(&mut self) -> std::result::Result<(), SignalFishError> {
            Ok(())
        }
    }

    /// A transport whose `send()` always returns `Pending`.
    struct PendingOnSendTransport;

    impl PendingOnSendTransport {
        fn new() -> Self {
            Self
        }
    }

    #[async_trait]
    impl Transport for PendingOnSendTransport {
        async fn send(&mut self, _message: String) -> std::result::Result<(), SignalFishError> {
            std::future::pending().await
        }

        async fn recv(&mut self) -> Option<std::result::Result<String, SignalFishError>> {
            std::future::pending().await
        }

        async fn close(&mut self) -> std::result::Result<(), SignalFishError> {
            Ok(())
        }
    }

    // ── A. Command Queueing Tests ──────────────────────────────────

    #[test]
    fn leave_room_queues_command() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll(); // flush auth

        client.leave_room().unwrap();
        client.poll();

        let last_sent = client.transport.sent.last().unwrap();
        let sent_json: serde_json::Value = serde_json::from_str(last_sent).unwrap();
        assert_eq!(sent_json["type"], "LeaveRoom");
    }

    #[test]
    fn send_game_data_queues_command() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll(); // flush auth

        client
            .send_game_data(serde_json::json!({"score": 42}))
            .unwrap();
        client.poll();

        let last_sent = client.transport.sent.last().unwrap();
        let sent_json: serde_json::Value = serde_json::from_str(last_sent).unwrap();
        assert_eq!(sent_json["type"], "GameData");
        assert_eq!(sent_json["data"]["data"]["score"], 42);
    }

    #[test]
    fn set_ready_queues_command() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll(); // flush auth

        client.set_ready().unwrap();
        client.poll();

        let last_sent = client.transport.sent.last().unwrap();
        let sent_json: serde_json::Value = serde_json::from_str(last_sent).unwrap();
        assert_eq!(sent_json["type"], "PlayerReady");
    }

    #[test]
    fn request_authority_queues_command() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll(); // flush auth

        client.request_authority(true).unwrap();
        client.poll();

        let last_sent = client.transport.sent.last().unwrap();
        let sent_json: serde_json::Value = serde_json::from_str(last_sent).unwrap();
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
            .unwrap();
        client.poll();

        let last_sent = client.transport.sent.last().unwrap();
        let sent_json: serde_json::Value = serde_json::from_str(last_sent).unwrap();
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
            .unwrap();
        client.poll();

        let last_sent = client.transport.sent.last().unwrap();
        let sent_json: serde_json::Value = serde_json::from_str(last_sent).unwrap();
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
            .unwrap();
        client.poll();

        let last_sent = client.transport.sent.last().unwrap();
        let sent_json: serde_json::Value = serde_json::from_str(last_sent).unwrap();
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
            .unwrap();
        client.poll();

        let last_sent = client.transport.sent.last().unwrap();
        let sent_json: serde_json::Value = serde_json::from_str(last_sent).unwrap();
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

        client.leave_spectator().unwrap();
        client.poll();

        let last_sent = client.transport.sent.last().unwrap();
        let sent_json: serde_json::Value = serde_json::from_str(last_sent).unwrap();
        assert_eq!(sent_json["type"], "LeaveSpectator");
    }

    #[test]
    fn ping_queues_command() {
        let transport = MockTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());
        client.poll(); // flush auth

        client.ping().unwrap();
        client.poll();

        let last_sent = client.transport.sent.last().unwrap();
        let sent_json: serde_json::Value = serde_json::from_str(last_sent).unwrap();
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
        client.join_room(params).unwrap();
        client.poll();

        let last_sent = client.transport.sent.last().unwrap();
        let sent_json: serde_json::Value = serde_json::from_str(last_sent).unwrap();
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
            },
        })
        .unwrap();

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
        let json = serde_json::to_string(&ServerMessage::PlayerLeft { player_id }).unwrap();

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        assert!(events.iter().any(|e| matches!(
            e,
            SignalFishEvent::PlayerLeft { player_id: pid } if *pid == player_id
        )));
    }

    #[test]
    fn poll_receives_game_data_event() {
        let from = uuid::Uuid::from_u128(12);
        let json = serde_json::to_string(&ServerMessage::GameData {
            from_player: from,
            data: serde_json::json!({"hp": 100}),
        })
        .unwrap();

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        let gd = events
            .iter()
            .find(|e| matches!(e, SignalFishEvent::GameData { .. }));
        assert!(gd.is_some(), "expected GameData event, got: {events:?}");
        if let SignalFishEvent::GameData { from_player, data } = gd.unwrap() {
            assert_eq!(*from_player, from);
            assert_eq!(data["hp"], 100);
        }
    }

    #[test]
    fn poll_receives_game_data_binary_event() {
        let from = uuid::Uuid::from_u128(13);
        let json = serde_json::to_string(&ServerMessage::GameDataBinary {
            from_player: from,
            encoding: crate::protocol::GameDataEncoding::MessagePack,
            payload: vec![0xCA, 0xFE],
        })
        .unwrap();

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
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
        } = gdb.unwrap()
        {
            assert_eq!(*from_player, from);
            assert!(matches!(
                encoding,
                crate::protocol::GameDataEncoding::MessagePack
            ));
            assert_eq!(payload, &[0xCA, 0xFE]);
        }
    }

    #[test]
    fn poll_receives_authority_changed_event() {
        let auth_player = uuid::Uuid::from_u128(14);
        let json = serde_json::to_string(&ServerMessage::AuthorityChanged {
            authority_player: Some(auth_player),
            you_are_authority: true,
        })
        .unwrap();

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
        .unwrap();

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
        .unwrap();

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
        } = ar.unwrap()
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
        .unwrap();

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
        .unwrap();

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        assert!(events
            .iter()
            .any(|e| matches!(e, SignalFishEvent::GameStarting { .. })));
    }

    #[test]
    fn poll_receives_pong_event() {
        let json = serde_json::to_string(&ServerMessage::Pong).unwrap();

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
        .unwrap();

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
        } = err.unwrap()
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
        .unwrap();

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
        } = err.unwrap()
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
        .unwrap();

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
        .unwrap();

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
        if let SignalFishEvent::RoomJoinFailed { reason, error_code } = rjf.unwrap() {
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
        .unwrap();

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
        .unwrap();

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
        let json = serde_json::to_string(&ServerMessage::PlayerReconnected { player_id }).unwrap();

        let transport = MockTransport::new().with_incoming(vec![Some(Ok(json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());
        let events = client.poll();

        assert!(events.iter().any(|e| matches!(
            e,
            SignalFishEvent::PlayerReconnected { player_id: pid } if *pid == player_id
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
        .unwrap();

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
        .unwrap();

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
            },
        ))
        .unwrap();

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
        if let SignalFishEvent::Disconnected { reason: Some(r) } = disconnected.unwrap() {
            assert!(
                r.contains("transport send error"),
                "expected reason to contain 'transport send error', got: {r}"
            );
        }
        assert!(!client.is_connected());
    }

    #[test]
    fn poll_retries_pending_send_next_frame() {
        let transport = PendingOnSendTransport::new();
        let mut client = SignalFishPollingClient::new(transport, default_config());

        // First poll: send returns Pending, so the Authenticate message
        // should be put back in the queue.
        let events = client.poll();

        // Connected should still be emitted before the send attempt.
        assert!(events
            .iter()
            .any(|e| matches!(e, SignalFishEvent::Connected)));

        // Client should still be connected (not disconnected).
        assert!(client.is_connected());

        // The command queue should still have the Authenticate message.
        // We can verify by checking the cmd_queue length is at least 1.
        assert!(
            !client.cmd_queue.is_empty(),
            "expected pending message to stay in queue"
        );
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
        .unwrap();
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
            },
        )))
        .unwrap();

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
            },
        )))
        .unwrap();
        let room_left = serde_json::to_string(&ServerMessage::RoomLeft).unwrap();
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
            },
        )))
        .unwrap();

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

        client.leave_room().unwrap();
        client
            .join_room(JoinRoomParams::new("game", "Player"))
            .unwrap();
        client.ping().unwrap();

        client.poll();

        // After the initial auth message (index 0), we should have 3 more.
        assert_eq!(
            client.transport.sent.len(),
            4,
            "expected 4 total sent messages (auth + 3 commands), got: {:?}",
            client.transport.sent
        );
        let leave: serde_json::Value = serde_json::from_str(&client.transport.sent[1]).unwrap();
        let join: serde_json::Value = serde_json::from_str(&client.transport.sent[2]).unwrap();
        let ping: serde_json::Value = serde_json::from_str(&client.transport.sent[3]).unwrap();
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
        let pong_json = serde_json::to_string(&ServerMessage::Pong).unwrap();
        let transport = MockTransport::new().with_incoming(vec![Some(Ok(pong_json))]);
        let mut client = SignalFishPollingClient::new(transport, default_config());

        // First poll: sends auth, receives Connected + Pong.
        let events = client.poll();
        assert!(
            events.iter().any(|e| matches!(e, SignalFishEvent::Pong)),
            "expected Pong event in first poll, got: {events:?}"
        );

        // Queue a ping command.
        client.ping().unwrap();

        // Second poll: sends the ping.
        client.poll();

        // Verify the Ping message was sent.
        let ping_sent = client.transport.sent.iter().any(|s| {
            let v: serde_json::Value = serde_json::from_str(s).unwrap();
            v["type"] == "Ping"
        });
        assert!(ping_sent, "expected Ping to be sent");
    }
}
