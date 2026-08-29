#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
//! Shared test utilities for Signal Fish Client integration tests.
//!
//! Provides a channel-based [`MockTransport`] and helper functions for
//! constructing common server response JSON strings.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use signal_fish_client::protocol::SpectatorJoinedPayload;
use signal_fish_client::protocol::{
    GameDataEncoding, LobbyState, PlayerId, PlayerInfo, ProtocolInfoPayload, RateLimitInfo,
    ReconnectedPayload, ReplayStatus, RoomJoinedPayload, SenderWatermark, ServerMessage,
    SessionPeer, SessionPlanPayload, SpectatorStateChangeReason, Topology, TransportKind,
};
use signal_fish_client::transport::TransportFrame;
use signal_fish_client::{ClientMessage, SignalFishError, Transport};

// ── MockTransport ───────────────────────────────────────────────────

/// A channel-based mock transport for integration testing.
///
/// Scripted server responses are consumed in order by `recv()`.
/// All messages sent by the client are recorded in `sent`.
pub struct MockTransport {
    /// Scripted server responses (consumed in order by `recv`).
    incoming: VecDeque<Option<Result<TransportFrame, SignalFishError>>>,
    /// Recorded outgoing messages from the client.
    pub sent: Arc<StdMutex<Vec<String>>>,
    /// Whether `close()` has been called.
    pub closed: Arc<AtomicBool>,
    delivered_room_responses: [usize; 5],
    gate_room_responses: bool,
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
            incoming: incoming
                .into_iter()
                .map(|item| item.map(|result| result.map(TransportFrame::Text)))
                .collect(),
            sent: Arc::clone(&sent),
            closed: Arc::clone(&closed),
            delivered_room_responses: [0; 5],
            gate_room_responses: true,
        };
        (transport, sent, closed)
    }

    /// Create a transport that intentionally delivers terminal room responses
    /// without waiting for matching commands, for lifecycle-violation tests.
    #[allow(dead_code)]
    pub fn new_ungated(
        incoming: Vec<Option<Result<String, SignalFishError>>>,
    ) -> (Self, Arc<StdMutex<Vec<String>>>, Arc<AtomicBool>) {
        let (mut transport, sent, closed) = Self::new(incoming);
        transport.gate_room_responses = false;
        (transport, sent, closed)
    }

    /// Create a mock transport from physical text/binary frames.
    pub fn new_frames(
        incoming: Vec<Option<Result<TransportFrame, SignalFishError>>>,
    ) -> (Self, Arc<StdMutex<Vec<String>>>, Arc<AtomicBool>) {
        let sent = Arc::new(StdMutex::new(Vec::new()));
        let closed = Arc::new(AtomicBool::new(false));
        (
            Self {
                incoming: VecDeque::from(incoming),
                sent: Arc::clone(&sent),
                closed: Arc::clone(&closed),
                delivered_room_responses: [0; 5],
                gate_room_responses: true,
            },
            sent,
            closed,
        )
    }
}

impl Transport for MockTransport {
    fn abort(&mut self) {
        self.closed.store(true, Ordering::Relaxed);
        self.incoming.clear();
    }

    fn poll_send(
        &mut self,
        _cx: &mut std::task::Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        if let Some(frame) = frame.take() {
            let TransportFrame::Text(message) = frame else {
                panic!("test mock expected an outbound text frame");
            };
            self.sent.lock().unwrap().push(message);
        }
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_recv(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<TransportFrame, SignalFishError>>> {
        let response_kind = self.incoming.front().and_then(|item| {
            let Some(Ok(TransportFrame::Text(json))) = item else {
                return None;
            };
            let message = serde_json::from_str::<ServerMessage>(json).ok()?;
            match message {
                ServerMessage::RoomJoined(_) | ServerMessage::RoomJoinFailed { .. } => Some(0),
                ServerMessage::RoomLeft => Some(1),
                ServerMessage::Reconnected(_) | ServerMessage::ReconnectionFailed { .. } => Some(2),
                ServerMessage::SpectatorJoined(_) | ServerMessage::SpectatorJoinFailed { .. } => {
                    Some(3)
                }
                // Authoritative exits (removed, disconnected, room-closed)
                // are server-initiated and must be deliverable without a
                // matching client command; the command-answer faces (absent
                // reason, `voluntary_leave`, `joined`) stay gated. Mirrors
                // the async driver's test mock and the core's
                // `spectator_exit_is_authoritative` partition.
                ServerMessage::SpectatorLeft {
                    reason:
                        None
                        | Some(
                            SpectatorStateChangeReason::VoluntaryLeave
                            | SpectatorStateChangeReason::Joined,
                        ),
                    ..
                } => Some(4),
                _ => None,
            }
        });
        if self.gate_room_responses {
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
                    return std::task::Poll::Pending;
                }
            }
        }
        if let Some(item) = self.incoming.pop_front() {
            if let Some(kind) = response_kind {
                self.delivered_room_responses[kind] += 1;
            }
            std::task::Poll::Ready(item)
        } else {
            std::task::Poll::Pending
        }
    }

    fn poll_close(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        self.closed.store(true, Ordering::Relaxed);
        std::task::Poll::Ready(Ok(()))
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
        current_players: vec![PlayerInfo {
            id: player_id,
            name: "Alice".into(),
            is_authority: false,
            is_ready: false,
            connected_at: "2026-01-01T00:00:00Z".into(),
            connection_info: None,
            epoch: None,
            seq: None,
        }],
        is_authority: false,
        lobby_state: LobbyState::Waiting,
        ready_players: vec![],
        relay_type: "auto".into(),
        current_spectators: vec![],
        ice_servers: vec![],
        reconnection_token: None,
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
    let player_id = uuid::Uuid::from_u128(200);
    let payload = ReconnectedPayload {
        room_id: uuid::Uuid::from_u128(100),
        room_code: "RECON1".into(),
        player_id,
        game_name: "recon-game".into(),
        max_players: 6,
        supports_authority: false,
        current_players: vec![PlayerInfo {
            id: player_id,
            name: "Alice".into(),
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
    spectator_left_json_with_reason(None)
}

/// Returns the JSON string for a `SpectatorLeft` server message carrying the
/// given state-change reason (`None` models the voluntary/absent-reason face).
pub fn spectator_left_json_with_reason(reason: Option<SpectatorStateChangeReason>) -> String {
    serde_json::to_string(&ServerMessage::SpectatorLeft {
        room_id: Some(uuid::Uuid::from_u128(300)),
        room_code: Some("SPEC1".into()),
        reason,
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
            epoch: None,
            seq: None,
        },
    })
    .expect("player_joined_json serialization")
}

/// Returns the JSON string for a `PlayerLeft` server message.
pub fn player_left_json(player_id: uuid::Uuid) -> String {
    serde_json::to_string(&ServerMessage::PlayerLeft {
        player_id,
        epoch: None,
        final_seq: None,
    })
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
    serde_json::to_string(&ServerMessage::GameData {
        from_player,
        data,
        seq: None,
        epoch: None,
        class: None,
        key: None,
    })
    .expect("game_data_json serialization")
}

// ── Protocol v3 fixtures ────────────────────────────────────────────

/// Builds a `ProtocolInfoPayload` with the given negotiated version. Versions
/// `>= 3` stamp all five v3 fields; versions below 3 — like `None` — produce
/// the v2 shape with every v3 field (including `protocol_version` itself)
/// omitted, matching the vendored AsyncAPI ("absent on negotiated v2
/// connections") so every emitted shape is server-sendable.
pub fn protocol_info_payload(protocol_version: Option<u16>) -> ProtocolInfoPayload {
    let v3_fields = protocol_version.filter(|version| *version >= 3);
    ProtocolInfoPayload {
        platform: None,
        sdk_version: None,
        minimum_version: None,
        recommended_version: None,
        capabilities: vec![],
        notes: None,
        game_data_formats: vec![GameDataEncoding::Json, GameDataEncoding::MessagePack],
        player_name_rules: None,
        protocol_version: v3_fields,
        min_protocol_version: v3_fields.map(|_| 2),
        max_protocol_version: v3_fields.map(|version| version.max(3)),
        transports: v3_fields
            .map(|_| vec![signal_fish_client::protocol::MessageTransport::Websocket]),
        max_outbound_message_size: v3_fields.map(|_| 8 * 1024 * 1024),
    }
}

/// Returns the JSON for a `ProtocolInfo` message with the given negotiated
/// protocol version. Versioned fixtures advertise a coherent `2..=version`
/// range (with v3 as the minimum maximum); `None` is a v2 negotiation.
pub fn protocol_info_json(protocol_version: Option<u16>) -> String {
    serde_json::to_string(&ServerMessage::ProtocolInfo(protocol_info_payload(
        protocol_version,
    )))
    .expect("protocol_info_json serialization")
}

/// Returns a finalized `Reconnected` baseline with the local player and its
/// future plan peer in the roster. The authoritative `SessionPlan` is sent as
/// a separate room-ordered frame after this baseline.
pub fn finalized_reconnected_json() -> String {
    let player_id = uuid::Uuid::from_u128(200);
    let peer_id = uuid::Uuid::from_u128(2);
    let payload = ReconnectedPayload {
        room_id: uuid::Uuid::from_u128(100),
        room_code: "RECON1".into(),
        player_id,
        game_name: "recon-game".into(),
        max_players: 6,
        supports_authority: false,
        current_players: vec![
            PlayerInfo {
                id: player_id,
                name: "Alice".into(),
                is_authority: true,
                is_ready: true,
                connected_at: "2026-01-01T00:00:00Z".into(),
                connection_info: None,
                epoch: Some(1),
                seq: Some(0),
            },
            PlayerInfo {
                id: peer_id,
                name: "Peer".into(),
                is_authority: false,
                is_ready: true,
                connected_at: "2026-01-01T00:00:01Z".into(),
                connection_info: None,
                epoch: Some(1),
                seq: Some(0),
            },
        ],
        is_authority: true,
        lobby_state: LobbyState::Finalized,
        ready_players: vec![],
        relay_type: "tcp".into(),
        current_spectators: vec![],
        ice_servers: vec![],
        missed_events: vec![],
        replay: Some(ReplayStatus::Complete),
        sender_watermarks: vec![
            SenderWatermark {
                player_id,
                epoch: 1,
                seq: 0,
            },
            SenderWatermark {
                player_id: peer_id,
                epoch: 1,
                seq: 0,
            },
        ],
        reconnection_token: Some("rotated-token".into()),
    };
    serde_json::to_string(&ServerMessage::Reconnected(Box::new(payload)))
        .expect("finalized_reconnected_json serialization")
}

/// Wait until the mock transport records at least `expected_len` outgoing
/// messages. This avoids fixed sleeps when testing queued async sends.
pub async fn wait_for_sent_len(sent: &Arc<StdMutex<Vec<String>>>, expected_len: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
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

/// Wait until the async driver observes the server's `Authenticated`
/// confirmation without consuming any events, mirroring the documented
/// requirement to await `SignalFishEvent::Authenticated` before room
/// operations. Room operations refuse earlier with
/// [`SignalFishError::NotAuthenticated`].
pub async fn wait_for_authentication(client: &signal_fish_client::SignalFishClient) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !client.is_authenticated() {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("client must observe Authenticated before scripted room operations");
}

/// Returns the JSON for a `SessionPlan` (mesh + webrtc) naming a single peer.
pub fn session_plan_json(peer_id: PlayerId, initiate: bool) -> String {
    serde_json::to_string(&session_plan_message(peer_id, initiate))
        .expect("session_plan_json serialization")
}

fn session_plan_message(peer_id: PlayerId, initiate: bool) -> ServerMessage {
    let payload = SessionPlanPayload {
        generation: Some(uuid::Uuid::from_u128(12)),
        topology: Topology::Mesh,
        transport: TransportKind::WebRtc,
        host: None,
        direct_endpoint: None,
        peers: vec![SessionPeer {
            player_id: peer_id,
            player_name: "Peer".into(),
            is_authority: false,
            initiate,
        }],
        ice_servers: vec![],
        fallback: TransportKind::Relay,
    };
    ServerMessage::SessionPlan(Box::new(payload))
}

/// Returns the JSON for a server `Signal` relayed from `from`.
pub fn signal_json(from: PlayerId, signal: serde_json::Value) -> String {
    serde_json::to_string(&ServerMessage::Signal {
        from,
        generation: Some(uuid::Uuid::from_u128(12)),
        signal,
    })
    .expect("signal_json serialization")
}

/// Returns the JSON for a `NewPeer` (late-join) message.
pub fn new_peer_json(peer_id: PlayerId, you_initiate: bool) -> String {
    serde_json::to_string(&ServerMessage::NewPeer {
        peer_id,
        you_initiate,
    })
    .expect("new_peer_json serialization")
}

/// Returns the JSON for a `PeerTransportStatus` message.
pub fn peer_transport_status_json(peer_id: PlayerId, connected: bool) -> String {
    serde_json::to_string(&ServerMessage::PeerTransportStatus {
        peer_id,
        transport: TransportKind::WebRtc,
        connected,
    })
    .expect("peer_transport_status_json serialization")
}

/// Transport whose graceful close never completes, with per-path call
/// counters so tests can pin abort-vs-close teardown decisions.
#[derive(Clone, Default)]
pub struct RecordingCloseTransport {
    /// Times the driver attempted the graceful `poll_close` handshake.
    pub close_calls: Arc<AtomicUsize>,
    /// Times the driver invoked the abort fallback.
    pub abort_calls: Arc<AtomicUsize>,
}

impl Transport for RecordingCloseTransport {
    fn abort(&mut self) {
        self.abort_calls.fetch_add(1, Ordering::Relaxed);
    }

    fn poll_send(
        &mut self,
        _cx: &mut std::task::Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        // Accept ownership and drop; teardown decisions are what is pinned.
        let _accepted = frame.take();
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_recv(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<TransportFrame, SignalFishError>>> {
        std::task::Poll::Pending
    }

    fn poll_close(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        self.close_calls.fetch_add(1, Ordering::Relaxed);
        std::task::Poll::Pending
    }
}
