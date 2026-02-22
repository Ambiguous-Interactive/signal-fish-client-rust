#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing
)]
//! Shared test utilities for Signal Fish Client integration tests.
//!
//! Provides a channel-based [`MockTransport`] and helper functions for
//! constructing common server response JSON strings.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use signal_fish_client::protocol::{
    LobbyState, PlayerInfo, RateLimitInfo, ReconnectedPayload, RoomJoinedPayload, ServerMessage,
    SpectatorJoinedPayload,
};
use signal_fish_client::{SignalFishError, Transport};

// ── MockTransport ───────────────────────────────────────────────────

/// A channel-based mock transport for integration testing.
///
/// Scripted server responses are consumed in order by `recv()`.
/// All messages sent by the client are recorded in `sent`.
pub struct MockTransport {
    /// Scripted server responses (consumed in order by `recv`).
    incoming: VecDeque<Option<Result<String, SignalFishError>>>,
    /// Recorded outgoing messages from the client.
    pub sent: Arc<StdMutex<Vec<String>>>,
    /// Whether `close()` has been called.
    pub closed: Arc<AtomicBool>,
}

impl MockTransport {
    /// Create a new mock transport with the given scripted incoming messages.
    ///
    /// Returns the transport plus shared handles for inspecting sent messages
    /// and whether close was called.
    pub fn new(
        incoming: Vec<Option<Result<String, SignalFishError>>>,
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
    async fn send(&mut self, message: String) -> Result<(), SignalFishError> {
        self.sent.lock().unwrap().push(message);
        Ok(())
    }

    async fn recv(&mut self) -> Option<Result<String, SignalFishError>> {
        if let Some(item) = self.incoming.pop_front() {
            item
        } else {
            // No more scripted messages — hang forever so the transport loop
            // stays alive until shutdown is called.
            std::future::pending().await
        }
    }

    async fn close(&mut self) -> Result<(), SignalFishError> {
        self.closed.store(true, Ordering::Relaxed);
        Ok(())
    }
}

// ── JSON helper functions ───────────────────────────────────────────

/// Returns the JSON string for a successful `Authenticated` server message.
pub fn authenticated_json() -> String {
    serde_json::to_string(&ServerMessage::Authenticated {
        app_name: "test-app".into(),
        organization: None,
        rate_limits: RateLimitInfo {
            per_minute: 60,
            per_hour: 1000,
            per_day: 10000,
        },
    })
    .expect("authenticated_json serialization")
}

/// Returns the JSON string for a `RoomJoined` server message with default values.
pub fn room_joined_json() -> String {
    room_joined_json_with("ABC123", "test-game", uuid::Uuid::from_u128(42))
}

/// Returns the JSON string for a `RoomJoined` server message with custom values.
pub fn room_joined_json_with(room_code: &str, game_name: &str, player_id: uuid::Uuid) -> String {
    let payload = RoomJoinedPayload {
        room_id: uuid::Uuid::nil(),
        room_code: room_code.into(),
        player_id,
        game_name: game_name.into(),
        max_players: 4,
        supports_authority: true,
        current_players: vec![],
        is_authority: false,
        lobby_state: LobbyState::Waiting,
        ready_players: vec![],
        relay_type: "auto".into(),
        current_spectators: vec![],
    };
    serde_json::to_string(&ServerMessage::RoomJoined(Box::new(payload)))
        .expect("room_joined_json serialization")
}

/// Returns the JSON string for a `RoomLeft` server message.
pub fn room_left_json() -> String {
    serde_json::to_string(&ServerMessage::RoomLeft).expect("room_left_json serialization")
}

/// Returns the JSON string for a `Reconnected` server message.
pub fn reconnected_json() -> String {
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
    serde_json::to_string(&ServerMessage::Reconnected(Box::new(payload)))
        .expect("reconnected_json serialization")
}

/// Returns the JSON string for a `SpectatorJoined` server message.
pub fn spectator_joined_json() -> String {
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
    serde_json::to_string(&ServerMessage::SpectatorJoined(Box::new(payload)))
        .expect("spectator_joined_json serialization")
}

/// Returns the JSON string for a `SpectatorLeft` server message.
pub fn spectator_left_json() -> String {
    serde_json::to_string(&ServerMessage::SpectatorLeft {
        room_id: Some(uuid::Uuid::from_u128(300)),
        room_code: Some("SPEC1".into()),
        reason: None,
        current_spectators: vec![],
    })
    .expect("spectator_left_json serialization")
}

/// Returns the JSON string for a `Pong` server message.
pub fn pong_json() -> String {
    serde_json::to_string(&ServerMessage::Pong).expect("pong_json serialization")
}

/// Returns the JSON string for a `PlayerJoined` server message.
pub fn player_joined_json(name: &str, player_id: uuid::Uuid) -> String {
    serde_json::to_string(&ServerMessage::PlayerJoined {
        player: PlayerInfo {
            id: player_id,
            name: name.into(),
            is_authority: false,
            is_ready: false,
            connected_at: "2026-01-01T00:00:00Z".into(),
            connection_info: None,
        },
    })
    .expect("player_joined_json serialization")
}

/// Returns the JSON string for a `PlayerLeft` server message.
pub fn player_left_json(player_id: uuid::Uuid) -> String {
    serde_json::to_string(&ServerMessage::PlayerLeft { player_id })
        .expect("player_left_json serialization")
}

/// Returns the JSON string for a server `Error` message.
pub fn error_json(message: &str, error_code: Option<signal_fish_client::ErrorCode>) -> String {
    serde_json::to_string(&ServerMessage::Error {
        message: message.into(),
        error_code,
    })
    .expect("error_json serialization")
}

/// Returns the JSON string for an `AuthorityResponse` server message.
pub fn authority_response_json(granted: bool, reason: Option<&str>) -> String {
    serde_json::to_string(&ServerMessage::AuthorityResponse {
        granted,
        reason: reason.map(Into::into),
        error_code: None,
    })
    .expect("authority_response_json serialization")
}

/// Returns the JSON string for a `GameData` server message.
pub fn game_data_json(from_player: uuid::Uuid, data: serde_json::Value) -> String {
    serde_json::to_string(&ServerMessage::GameData { from_player, data })
        .expect("game_data_json serialization")
}

/// Returns the JSON string for a `GameDataBinary` server message.
pub fn game_data_binary_json(
    from_player: uuid::Uuid,
    encoding: signal_fish_client::protocol::GameDataEncoding,
    payload: Vec<u8>,
) -> String {
    serde_json::to_string(&ServerMessage::GameDataBinary {
        from_player,
        encoding,
        payload,
    })
    .expect("game_data_binary_json serialization")
}
