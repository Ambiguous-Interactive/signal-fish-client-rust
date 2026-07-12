//! Parity regression tests: `SignalFishPollingClient` (sync) must mirror
//! `SignalFishClient` (async) v3 behavior exactly.
//!
//! These drive BOTH clients through equivalent scenarios with the same scripted
//! server messages and assert identical observable behavior: negotiated-version
//! tracking, `ensure_v3` guard modes, reconnect replay restoration (no v2
//! downgrade), accessors, and relay-floor byte-identity. The polling client is a
//! primary WASM/Godot path, so any divergence is a silent bug for those users.
// Compares the async and polling clients side by side, so it needs both.
#![cfg(all(feature = "polling-client", feature = "tokio-runtime"))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::type_complexity
)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use signal_fish_client::client::{SignalFishClient, SignalFishConfig};
use signal_fish_client::error::SignalFishError;
use signal_fish_client::polling_client::SignalFishPollingClient;
use signal_fish_client::protocol::{
    ConnectionInfo, GameDataEncoding, LobbyState, PlayerId, PlayerInfo, ProtocolInfoPayload,
    ReconnectedPayload, RoomJoinedPayload, ServerMessage, TransportKind, V2BinaryGameDataFrame,
    V3BinaryGameDataFrame,
};
use signal_fish_client::transport::TransportFrame;
use signal_fish_client::ProtocolViolationPolicy;
use signal_fish_client::{
    GameDataDelivery, JoinRoomParams, PeerSignal, SignalFishClientApi, SignalFishEvent, Transport,
};

fn assert_common_api_is_object_safe(_client: &mut dyn signal_fish_client::SignalFishClientApi) {}

#[derive(Clone)]
struct FrameMock {
    incoming: Arc<Mutex<VecDeque<TransportFrame>>>,
    sent: Arc<Mutex<Vec<TransportFrame>>>,
}

#[derive(Clone)]
struct NeverSendMock {
    attempted: Arc<std::sync::atomic::AtomicBool>,
}

impl NeverSendMock {
    fn new() -> Self {
        Self {
            attempted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

impl Transport for NeverSendMock {
    fn poll_send(
        &mut self,
        _cx: &mut std::task::Context<'_>,
        _frame: &mut Option<TransportFrame>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        self.attempted
            .store(true, std::sync::atomic::Ordering::Release);
        std::task::Poll::Pending
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
        std::task::Poll::Ready(Ok(()))
    }
}

impl FrameMock {
    fn v3() -> Self {
        Self {
            incoming: Arc::new(Mutex::new(VecDeque::from([TransportFrame::Text(
                PI_V3.into(),
            )]))),
            sent: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl Transport for FrameMock {
    fn poll_send(
        &mut self,
        _cx: &mut std::task::Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        if let Some(frame) = frame.take() {
            self.sent.lock().unwrap().push(frame);
        }
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_recv(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<TransportFrame, SignalFishError>>> {
        match self.incoming.lock().unwrap().pop_front() {
            Some(frame) => std::task::Poll::Ready(Some(Ok(frame))),
            None => std::task::Poll::Pending,
        }
    }

    fn poll_close(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[derive(Clone, Copy, Debug)]
enum CommonCommandCase {
    JoinRoom,
    LeaveRoom,
    ReliableData,
    LatestData,
    VolatileData,
    BinaryData,
    SetReady,
    StartGame,
    RequestAuthority,
    ProvideConnectionInfo,
    Reconnect,
    JoinSpectator,
    LeaveSpectator,
    Ping,
    Signal,
    Offer,
    Answer,
    IceCandidate,
    RawSignal,
    TransportStatus,
}

impl CommonCommandCase {
    fn invoke(self, client: &mut dyn SignalFishClientApi) -> Result<(), SignalFishError> {
        let peer = uuid::Uuid::from_u128(7);
        match self {
            Self::JoinRoom => client.join_room(
                JoinRoomParams::new("game", "Alice")
                    .with_room_code("ROOM")
                    .with_max_players(4)
                    .with_supports_authority(true),
            ),
            Self::LeaveRoom => client.leave_room(),
            Self::ReliableData => client.send_game_data(serde_json::json!({"n": 1})),
            Self::LatestData => client.send_game_data_with_delivery(
                serde_json::json!({"n": 2}),
                GameDataDelivery::Latest { key: 9 },
            ),
            Self::VolatileData => client.send_game_data_with_delivery(
                serde_json::json!({"n": 3}),
                GameDataDelivery::Volatile,
            ),
            Self::BinaryData => client.send_binary_game_data(vec![1, 2, 3]),
            Self::SetReady => client.set_ready(),
            Self::StartGame => client.start_game(),
            Self::RequestAuthority => client.request_authority(true),
            Self::ProvideConnectionInfo => client.provide_connection_info(ConnectionInfo::Direct {
                host: "127.0.0.1".into(),
                port: 9000,
            }),
            Self::Reconnect => client.reconnect(peer, uuid::Uuid::from_u128(8), "token".into()),
            Self::JoinSpectator => {
                client.join_as_spectator("game".into(), "ROOM".into(), "Observer".into())
            }
            Self::LeaveSpectator => client.leave_spectator(),
            Self::Ping => client.ping(),
            Self::Signal => client.send_signal(peer, PeerSignal::Offer("sdp".into())),
            Self::Offer => client.send_offer(peer, "offer".into()),
            Self::Answer => client.send_answer(peer, "answer".into()),
            Self::IceCandidate => client.send_ice_candidate(peer, "candidate".into()),
            Self::RawSignal => client.send_raw_signal(peer, serde_json::json!({"Custom": 1})),
            Self::TransportStatus => client.report_transport_status(TransportKind::WebRtc, true),
        }
    }
}

const PEER_UUID: &str = "00000000-0000-0000-0000-000000000007";
const AUTH: &str = r#"{"type":"Authenticated","data":{"app_name":"test","rate_limits":{"per_minute":60,"per_hour":1000,"per_day":10000}}}"#;
const PI_V3: &str = r#"{"type":"ProtocolInfo","data":{"capabilities":[],"game_data_formats":[],"protocol_version":3,"min_protocol_version":2,"max_protocol_version":3}}"#;
// A v2 negotiation omits the version fields, so it deserializes to
// `protocol_version: None` — a terminal relay floor.
const PI_V2: &str = r#"{"type":"ProtocolInfo","data":{"capabilities":[],"game_data_formats":[]}}"#;

// ── Shared mock transport (works for both async + polling drivers) ────

#[derive(Clone)]
struct SharedMock {
    incoming: Arc<Mutex<VecDeque<Option<Result<String, SignalFishError>>>>>,
    sent: Arc<Mutex<Vec<String>>>,
}

impl SharedMock {
    fn new(msgs: Vec<&str>) -> Self {
        Self {
            incoming: Arc::new(Mutex::new(
                msgs.into_iter().map(|m| Some(Ok(m.to_string()))).collect(),
            )),
            sent: Arc::new(Mutex::new(Vec::new())),
        }
    }
    fn from_msgs(msgs: Vec<Option<Result<String, SignalFishError>>>) -> Self {
        Self {
            incoming: Arc::new(Mutex::new(msgs.into())),
            sent: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl Transport for SharedMock {
    fn poll_send(
        &mut self,
        _cx: &mut std::task::Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        if let Some(frame) = frame.take() {
            let TransportFrame::Text(message) = frame else {
                panic!("parity mock expected an outbound text frame");
            };
            self.sent.lock().unwrap().push(message);
        }
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_recv(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<TransportFrame, SignalFishError>>> {
        let item = self.incoming.lock().unwrap().pop_front();
        match item {
            Some(inner) => {
                std::task::Poll::Ready(inner.map(|result| result.map(TransportFrame::Text)))
            }
            None => std::task::Poll::Pending,
        }
    }
    fn poll_close(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        std::task::Poll::Ready(Ok(()))
    }
}

fn pi_v3_payload() -> ProtocolInfoPayload {
    ProtocolInfoPayload {
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

fn pi_v2_payload() -> ProtocolInfoPayload {
    let mut p = pi_v3_payload();
    p.protocol_version = None;
    p.min_protocol_version = None;
    p.max_protocol_version = None;
    p
}

fn reconnected_with_missed(missed: Vec<ServerMessage>) -> String {
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
        missed_events: missed,
        replay: None,
        sender_watermarks: vec![],
        reconnection_token: None,
    };
    serde_json::to_string(&ServerMessage::Reconnected(Box::new(payload))).unwrap()
}

async fn wait_for_sent_len(mock: &SharedMock, expected_len: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if mock.sent.lock().unwrap().len() >= expected_len {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "timed out waiting for {expected_len} sent message(s); got {}",
            mock.sent.lock().unwrap().len()
        )
    });
}

#[derive(Clone)]
struct TraceMock {
    incoming: Arc<Mutex<VecDeque<Option<Result<TransportFrame, SignalFishError>>>>>,
}

impl TraceMock {
    fn new(frames: Vec<TransportFrame>) -> Self {
        Self {
            incoming: Arc::new(Mutex::new(
                frames
                    .into_iter()
                    .map(|frame| Some(Ok(frame)))
                    .chain(std::iter::once(None))
                    .collect(),
            )),
        }
    }
}

impl Transport for TraceMock {
    fn poll_send(
        &mut self,
        _cx: &mut std::task::Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        let _ = frame.take();
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_recv(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<TransportFrame, SignalFishError>>> {
        match self.incoming.lock().unwrap().pop_front() {
            Some(item) => std::task::Poll::Ready(item),
            None => std::task::Poll::Pending,
        }
    }

    fn poll_close(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        std::task::Poll::Ready(Ok(()))
    }
}

fn canonical_event(event: &SignalFishEvent) -> String {
    use std::fmt::Write as _;

    macro_rules! event_fields {
        ($name:literal) => {
            String::from($name)
        };
        ($name:literal, $first:expr $(, $field:expr)*) => {{
            let mut result = String::from($name);
            write!(&mut result, "|{:?}", $first).expect("write event projection");
            $(write!(&mut result, "|{:?}", $field).expect("write event projection");)*
            result
        }};
    }

    match event {
        SignalFishEvent::Connected => event_fields!("Connected"),
        SignalFishEvent::Disconnected {
            reason,
            last_server_error,
        } => event_fields!("Disconnected", reason, last_server_error),
        SignalFishEvent::DecodeFailed {
            message_type,
            error,
            raw_prefix,
        } => event_fields!("DecodeFailed", message_type, error, raw_prefix),
        SignalFishEvent::ProtocolViolation { kind, diagnostic } => {
            event_fields!("ProtocolViolation", kind, diagnostic)
        }
        SignalFishEvent::Authenticated {
            app_name,
            organization,
            rate_limits,
        } => event_fields!("Authenticated", app_name, organization, rate_limits),
        SignalFishEvent::ProtocolInfo(payload) => event_fields!("ProtocolInfo", payload),
        SignalFishEvent::AuthenticationError { error, error_code } => {
            event_fields!("AuthenticationError", error, error_code)
        }
        SignalFishEvent::RoomJoined {
            room_id,
            room_code,
            player_id,
            game_name,
            max_players,
            supports_authority,
            current_players,
            is_authority,
            lobby_state,
            ready_players,
            relay_type,
            current_spectators,
            ice_servers,
            reconnection_token,
        } => event_fields!(
            "RoomJoined",
            room_id,
            room_code,
            player_id,
            game_name,
            max_players,
            supports_authority,
            current_players,
            is_authority,
            lobby_state,
            ready_players,
            relay_type,
            current_spectators,
            ice_servers,
            reconnection_token
        ),
        SignalFishEvent::RoomJoinFailed { reason, error_code } => {
            event_fields!("RoomJoinFailed", reason, error_code)
        }
        SignalFishEvent::RoomLeft => event_fields!("RoomLeft"),
        SignalFishEvent::PlayerJoined { player } => event_fields!("PlayerJoined", player),
        SignalFishEvent::PlayerLeft {
            player_id,
            epoch,
            final_seq,
        } => event_fields!("PlayerLeft", player_id, epoch, final_seq),
        SignalFishEvent::GameData {
            from_player,
            data,
            seq,
            epoch,
            class,
            key,
        } => event_fields!("GameData", from_player, data, seq, epoch, class, key),
        SignalFishEvent::GameDataBinary {
            from_player,
            encoding,
            payload,
            seq,
            epoch,
        } => event_fields!("GameDataBinary", from_player, encoding, payload, seq, epoch),
        SignalFishEvent::AuthorityChanged {
            authority_player,
            you_are_authority,
        } => event_fields!("AuthorityChanged", authority_player, you_are_authority),
        SignalFishEvent::AuthorityResponse {
            granted,
            reason,
            error_code,
        } => event_fields!("AuthorityResponse", granted, reason, error_code),
        SignalFishEvent::LobbyStateChanged {
            lobby_state,
            ready_players,
            all_ready,
        } => event_fields!("LobbyStateChanged", lobby_state, ready_players, all_ready),
        SignalFishEvent::GameStarting { peer_connections } => {
            event_fields!("GameStarting", peer_connections)
        }
        SignalFishEvent::SessionPlan {
            topology,
            transport,
            host,
            peers,
            ice_servers,
            fallback,
        } => event_fields!(
            "SessionPlan",
            topology,
            transport,
            host,
            peers,
            ice_servers,
            fallback
        ),
        SignalFishEvent::NewPeer {
            peer_id,
            you_initiate,
        } => event_fields!("NewPeer", peer_id, you_initiate),
        SignalFishEvent::SignalReceived { from, signal } => {
            event_fields!("SignalReceived", from, signal)
        }
        SignalFishEvent::PeerTransportStatus {
            peer_id,
            transport,
            connected,
        } => event_fields!("PeerTransportStatus", peer_id, transport, connected),
        SignalFishEvent::RelayStats {
            interval_ms,
            sent_to_you,
            dropped_for_you,
            backpressure_events,
        } => event_fields!(
            "RelayStats",
            interval_ms,
            sent_to_you,
            dropped_for_you,
            backpressure_events
        ),
        SignalFishEvent::GoingAway {
            deadline_ms,
            retry_after_secs,
        } => event_fields!("GoingAway", deadline_ms, retry_after_secs),
        SignalFishEvent::DeliveryReport(payload) => event_fields!("DeliveryReport", payload),
        SignalFishEvent::Pong => event_fields!("Pong"),
        SignalFishEvent::Reconnected {
            room_id,
            room_code,
            player_id,
            game_name,
            max_players,
            supports_authority,
            current_players,
            is_authority,
            lobby_state,
            ready_players,
            relay_type,
            current_spectators,
            ice_servers,
            missed_events,
            replay,
            sender_watermarks,
            reconnection_token,
        } => {
            let missed_events = missed_events
                .iter()
                .map(canonical_event)
                .collect::<Vec<_>>();
            event_fields!(
                "Reconnected",
                room_id,
                room_code,
                player_id,
                game_name,
                max_players,
                supports_authority,
                current_players,
                is_authority,
                lobby_state,
                ready_players,
                relay_type,
                current_spectators,
                ice_servers,
                missed_events,
                replay,
                sender_watermarks,
                reconnection_token
            )
        }
        SignalFishEvent::ReconnectionFailed { reason, error_code } => {
            event_fields!("ReconnectionFailed", reason, error_code)
        }
        SignalFishEvent::PlayerReconnected { player_id, epoch } => {
            event_fields!("PlayerReconnected", player_id, epoch)
        }
        SignalFishEvent::SpectatorJoined {
            room_id,
            room_code,
            spectator_id,
            game_name,
            current_players,
            current_spectators,
            lobby_state,
            reason,
        } => event_fields!(
            "SpectatorJoined",
            room_id,
            room_code,
            spectator_id,
            game_name,
            current_players,
            current_spectators,
            lobby_state,
            reason
        ),
        SignalFishEvent::SpectatorJoinFailed { reason, error_code } => {
            event_fields!("SpectatorJoinFailed", reason, error_code)
        }
        SignalFishEvent::SpectatorLeft {
            room_id,
            room_code,
            reason,
            current_spectators,
        } => event_fields!(
            "SpectatorLeft",
            room_id,
            room_code,
            reason,
            current_spectators
        ),
        SignalFishEvent::NewSpectatorJoined {
            spectator,
            current_spectators,
            reason,
        } => event_fields!("NewSpectatorJoined", spectator, current_spectators, reason),
        SignalFishEvent::SpectatorDisconnected {
            spectator_id,
            reason,
            current_spectators,
        } => event_fields!(
            "SpectatorDisconnected",
            spectator_id,
            reason,
            current_spectators
        ),
        SignalFishEvent::Error {
            message,
            error_code,
        } => event_fields!("Error", message, error_code),
    }
}

async fn assert_frame_trace_parity(frames: Vec<TransportFrame>, config: SignalFishConfig) {
    let make_mock = || TraceMock::new(frames.clone());

    let async_mock = make_mock();
    let (async_client, mut async_rx) = SignalFishClient::start(async_mock, config.clone());
    let mut async_events = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while let Some(event) = async_rx.recv().await {
            async_events.push(event);
        }
    })
    .await
    .expect("async server trace should terminate on scripted close");

    let polling_mock = make_mock();
    let mut polling_client = SignalFishPollingClient::new(polling_mock, config);
    let polling_events = polling_client.poll();

    let async_events = async_events
        .iter()
        .filter(|event| !matches!(event, SignalFishEvent::Connected))
        .map(canonical_event)
        .collect::<Vec<_>>();
    let polling_events = polling_events
        .iter()
        .filter(|event| !matches!(event, SignalFishEvent::Connected))
        .map(canonical_event)
        .collect::<Vec<_>>();
    assert_eq!(async_events, polling_events);
    assert_eq!(async_client.snapshot(), polling_client.snapshot());
    assert_eq!(async_client.stats(), polling_client.stats());
}

async fn assert_server_trace_parity(lines: &str, config: SignalFishConfig) {
    let frames = lines
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| TransportFrame::Text(line.to_owned()))
        .collect();
    assert_frame_trace_parity(frames, config).await;
}

// ── PARITY 1: relay-floor Authenticate byte-identity ─────────────────

#[tokio::test]
async fn parity_relay_floor_authenticate_is_byte_identical() {
    let async_mock = SharedMock::new(vec![]);
    let (_client, _events) =
        SignalFishClient::start(async_mock.clone(), SignalFishConfig::new("app"));
    wait_for_sent_len(&async_mock, 1).await;
    let async_sent = async_mock.sent.lock().unwrap().clone();

    let poll_mock = SharedMock::new(vec![]);
    let mut poll_client =
        SignalFishPollingClient::new(poll_mock.clone(), SignalFishConfig::new("app"));
    poll_client.poll();
    let poll_sent = poll_mock.sent.lock().unwrap().clone();

    assert!(!async_sent.is_empty());
    assert!(!poll_sent.is_empty());
    assert_eq!(
        async_sent[0], poll_sent[0],
        "Authenticate bytes must be byte-identical between clients"
    );
    let v: serde_json::Value = serde_json::from_str(&poll_sent[0]).unwrap();
    assert!(v["data"].get("protocol_version").is_none());
    assert!(v["data"].get("supported_transports").is_none());
    assert!(v["data"].get("supported_topologies").is_none());
}

#[tokio::test]
async fn both_drivers_implement_the_object_safe_common_api() {
    let async_mock = SharedMock::new(vec![]);
    let (mut async_client, _events) =
        SignalFishClient::start(async_mock, SignalFishConfig::new("app"));
    assert_common_api_is_object_safe(&mut async_client);

    let poll_mock = SharedMock::new(vec![]);
    let mut polling_client = SignalFishPollingClient::new(poll_mock, SignalFishConfig::new("app"));
    assert_common_api_is_object_safe(&mut polling_client);

    async_client.shutdown().await;
}

#[tokio::test]
async fn vendored_v2_and_v3_server_message_traces_have_complete_parity() {
    assert_server_trace_parity(
        include_str!("wire-samples/v2-server-messages.jsonl"),
        SignalFishConfig::new("app"),
    )
    .await;
    for policy in [
        ProtocolViolationPolicy::Quarantine,
        ProtocolViolationPolicy::Disconnect,
        ProtocolViolationPolicy::Observe,
    ] {
        assert_server_trace_parity(
            include_str!("wire-samples/v3-server-messages.jsonl"),
            SignalFishConfig::new("app")
                .enable_v3()
                .with_protocol_violation_policy(policy),
        )
        .await;
    }
}

fn binary_accountability_prefix(player_id: PlayerId) -> Vec<TransportFrame> {
    let room_joined = ServerMessage::RoomJoined(Box::new(RoomJoinedPayload {
        room_id: uuid::Uuid::from_u128(200),
        room_code: "BINARY".into(),
        player_id: uuid::Uuid::from_u128(100),
        game_name: "binary-parity".into(),
        max_players: 4,
        supports_authority: false,
        current_players: vec![PlayerInfo {
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
        lobby_state: LobbyState::Lobby,
        ready_players: vec![],
        relay_type: "websocket".into(),
        current_spectators: vec![],
        ice_servers: vec![],
        reconnection_token: Some("binary-parity-token".into()),
    }));
    vec![
        TransportFrame::Text(PI_V3.into()),
        TransportFrame::Text(serde_json::to_string(&room_joined).unwrap()),
    ]
}

#[tokio::test]
async fn inbound_binary_events_decode_failures_and_accountability_have_complete_parity() {
    let v2 = V2BinaryGameDataFrame {
        from_player: uuid::Uuid::from_u128(98),
        encoding: GameDataEncoding::MessagePack,
        payload: vec![4, 5, 6],
    };
    let mut v2_config = SignalFishConfig::new("app");
    v2_config.game_data_format = Some(GameDataEncoding::MessagePack);
    assert_frame_trace_parity(
        vec![
            TransportFrame::Text(PI_V2.into()),
            TransportFrame::Binary(rmp_serde::to_vec_named(&v2).unwrap()),
            TransportFrame::Binary(vec![0xc1]),
        ],
        v2_config,
    )
    .await;

    let player_id = uuid::Uuid::from_u128(301);
    for policy in [
        ProtocolViolationPolicy::Quarantine,
        ProtocolViolationPolicy::Disconnect,
        ProtocolViolationPolicy::Observe,
    ] {
        let mut frames = binary_accountability_prefix(player_id);
        for seq in [1, 3] {
            let frame = V3BinaryGameDataFrame {
                from_player: player_id,
                encoding: GameDataEncoding::MessagePack,
                payload: vec![seq as u8],
                seq,
                epoch: 1,
            };
            frames.push(TransportFrame::Binary(
                rmp_serde::to_vec_named(&frame).unwrap(),
            ));
        }
        let mut config = SignalFishConfig::new("app")
            .enable_v3()
            .with_protocol_violation_policy(policy);
        config.game_data_format = Some(GameDataEncoding::MessagePack);
        assert_frame_trace_parity(frames, config).await;
    }
}

#[tokio::test]
async fn every_common_command_produces_identical_physical_frames() {
    let cases = [
        CommonCommandCase::JoinRoom,
        CommonCommandCase::LeaveRoom,
        CommonCommandCase::ReliableData,
        CommonCommandCase::LatestData,
        CommonCommandCase::VolatileData,
        CommonCommandCase::BinaryData,
        CommonCommandCase::SetReady,
        CommonCommandCase::StartGame,
        CommonCommandCase::RequestAuthority,
        CommonCommandCase::ProvideConnectionInfo,
        CommonCommandCase::Reconnect,
        CommonCommandCase::JoinSpectator,
        CommonCommandCase::LeaveSpectator,
        CommonCommandCase::Ping,
        CommonCommandCase::Signal,
        CommonCommandCase::Offer,
        CommonCommandCase::Answer,
        CommonCommandCase::IceCandidate,
        CommonCommandCase::RawSignal,
        CommonCommandCase::TransportStatus,
    ];

    for case in cases {
        let mut config = SignalFishConfig::new("app").enable_v3();
        config.game_data_format = Some(GameDataEncoding::MessagePack);

        let async_mock = FrameMock::v3();
        let async_sent = Arc::clone(&async_mock.sent);
        let (mut async_client, mut async_events) =
            SignalFishClient::start(async_mock, config.clone());
        for _ in 0..2 {
            tokio::time::timeout(std::time::Duration::from_secs(1), async_events.recv())
                .await
                .unwrap_or_else(|_| panic!("{case:?}: async negotiation event timed out"));
        }
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while async_sent.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{case:?}: async Authenticate timed out"));
        async_sent.lock().unwrap().clear();
        case.invoke(&mut async_client)
            .unwrap_or_else(|error| panic!("{case:?}: async command failed: {error}"));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while async_sent.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{case:?}: async frame timed out"));
        let async_frames = async_sent.lock().unwrap().clone();

        let polling_mock = FrameMock::v3();
        let polling_sent = Arc::clone(&polling_mock.sent);
        let mut polling_client = SignalFishPollingClient::new(polling_mock, config);
        let _ = polling_client.poll();
        polling_sent.lock().unwrap().clear();
        case.invoke(&mut polling_client)
            .unwrap_or_else(|error| panic!("{case:?}: polling command failed: {error}"));
        let _ = polling_client.poll();
        let polling_frames = polling_sent.lock().unwrap().clone();

        assert_eq!(async_frames, polling_frames, "{case:?} frame drift");
        assert_eq!(
            SignalFishClientApi::snapshot(&async_client),
            SignalFishClientApi::snapshot(&polling_client),
            "{case:?} snapshot drift"
        );
        assert_eq!(
            SignalFishClientApi::stats(&async_client),
            SignalFishClientApi::stats(&polling_client),
            "{case:?} statistics drift"
        );
        async_client.shutdown().await;
    }
}

#[tokio::test]
async fn pending_transport_queue_capacity_and_errors_match() {
    let config = SignalFishConfig::new("app").with_command_channel_capacity(1);

    let async_mock = NeverSendMock::new();
    let attempted = Arc::clone(&async_mock.attempted);
    let (mut async_client, _events) = SignalFishClient::start(async_mock, config.clone());
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !attempted.load(std::sync::atomic::Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("async Authenticate should reach the stalled transport");

    let polling_mock = NeverSendMock::new();
    let mut polling_client = SignalFishPollingClient::new(polling_mock, config);
    let _ = polling_client.poll();

    assert_eq!(async_client.send_capacity(), 1);
    assert_eq!(polling_client.send_capacity(), 1);
    async_client
        .ping()
        .expect("one async queue slot should fit");
    polling_client
        .ping()
        .expect("one polling queue slot should fit");
    assert_eq!(async_client.send_capacity(), 0);
    assert_eq!(polling_client.send_capacity(), 0);

    let async_error = async_client.ping().expect_err("async queue must be full");
    let polling_error = polling_client
        .ping()
        .expect_err("polling queue must be full");
    assert_eq!(format!("{async_error:?}"), format!("{polling_error:?}"));
    async_client.shutdown().await;
}

#[tokio::test]
async fn disconnected_common_commands_consistently_return_not_connected() {
    let async_mock = SharedMock::new(vec![]);
    let (mut async_client, _events) =
        SignalFishClient::start(async_mock, SignalFishConfig::new("app"));
    async_client.shutdown().await;

    let polling_mock = SharedMock::new(vec![]);
    let mut polling_client =
        SignalFishPollingClient::new(polling_mock, SignalFishConfig::new("app"));
    polling_client.close();

    for case in [
        CommonCommandCase::JoinRoom,
        CommonCommandCase::LeaveRoom,
        CommonCommandCase::ReliableData,
        CommonCommandCase::LatestData,
        CommonCommandCase::VolatileData,
        CommonCommandCase::BinaryData,
        CommonCommandCase::SetReady,
        CommonCommandCase::StartGame,
        CommonCommandCase::RequestAuthority,
        CommonCommandCase::ProvideConnectionInfo,
        CommonCommandCase::Reconnect,
        CommonCommandCase::JoinSpectator,
        CommonCommandCase::LeaveSpectator,
        CommonCommandCase::Ping,
        CommonCommandCase::Signal,
        CommonCommandCase::Offer,
        CommonCommandCase::Answer,
        CommonCommandCase::IceCandidate,
        CommonCommandCase::RawSignal,
        CommonCommandCase::TransportStatus,
    ] {
        assert!(matches!(
            case.invoke(&mut async_client),
            Err(SignalFishError::NotConnected)
        ));
        assert!(matches!(
            case.invoke(&mut polling_client),
            Err(SignalFishError::NotConnected)
        ));
    }
}

// ── PARITY 2: ensure_v3 guard modes ──────────────────────────────────

#[tokio::test]
async fn parity_ensure_v3_pre_negotiation_mode() {
    let peer: PlayerId = PEER_UUID.parse().unwrap();

    let async_mock = SharedMock::new(vec![]);
    let (mut client, _events) = SignalFishClient::start(async_mock, SignalFishConfig::new("app"));
    let async_err = client.send_offer(peer, "sdp").unwrap_err();

    let poll_mock = SharedMock::new(vec![]);
    let mut poll_client =
        SignalFishPollingClient::new(poll_mock, SignalFishConfig::new("app").enable_mesh());
    let poll_err = poll_client.send_offer(peer, "sdp").unwrap_err();

    assert!(matches!(
        async_err,
        SignalFishError::ProtocolUnsupported {
            mode: "pre-negotiation"
        }
    ));
    assert!(matches!(
        poll_err,
        SignalFishError::ProtocolUnsupported {
            mode: "pre-negotiation"
        }
    ));
}

#[tokio::test]
async fn parity_ensure_v3_relay_only_mode_after_v2_negotiation() {
    // Both clients must report the terminal "relay-only" mode once a v2
    // `ProtocolInfo` has been observed — distinct from the "pre-negotiation"
    // state before any `ProtocolInfo` arrives (see the parity test above).
    let peer: PlayerId = PEER_UUID.parse().unwrap();

    let async_mock = SharedMock::new(vec![AUTH, PI_V2]);
    let (mut client, mut events) =
        SignalFishClient::start(async_mock, SignalFishConfig::new("app"));
    // Drain until the v2 ProtocolInfo has been processed into client state.
    loop {
        match events.recv().await {
            Some(SignalFishEvent::ProtocolInfo(_)) | None => break,
            _ => {}
        }
    }
    let async_err = client.send_offer(peer, "sdp").unwrap_err();

    let poll_mock = SharedMock::new(vec![AUTH, PI_V2]);
    let mut poll_client = SignalFishPollingClient::new(poll_mock, SignalFishConfig::new("app"));
    poll_client.poll();
    let poll_err = poll_client.send_offer(peer, "sdp").unwrap_err();

    assert!(matches!(
        async_err,
        SignalFishError::ProtocolUnsupported { mode: "relay-only" }
    ));
    assert!(matches!(
        poll_err,
        SignalFishError::ProtocolUnsupported { mode: "relay-only" }
    ));
}

// ── PARITY 3: relay-only v3 negotiation does not claim mesh ──────────

#[tokio::test]
async fn parity_negotiated_version_after_v3() {
    let async_mock = SharedMock::new(vec![AUTH, PI_V3]);
    let (client, mut events) = SignalFishClient::start(async_mock, SignalFishConfig::new("app"));
    for _ in 0..3 {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), events.recv()).await;
    }
    assert_eq!(client.negotiated_protocol_version(), Some(3));
    assert!(!client.supports_mesh());

    let poll_mock = SharedMock::new(vec![AUTH, PI_V3]);
    let mut poll_client = SignalFishPollingClient::new(poll_mock, SignalFishConfig::new("app"));
    poll_client.poll();
    assert_eq!(poll_client.negotiated_protocol_version(), Some(3));
    assert!(!poll_client.supports_mesh());
}

// ── PARITY 4: reconnect replay restores v3 (downgrade-risk hunt) ──────

#[tokio::test]
async fn parity_reconnect_replay_restores_v3_from_missed_events() {
    let recon = reconnected_with_missed(vec![ServerMessage::ProtocolInfo(pi_v3_payload())]);

    let async_mock = SharedMock::new(vec![AUTH, &recon]);
    let (client, mut events) =
        SignalFishClient::start(async_mock, SignalFishConfig::new("app").enable_mesh());
    for _ in 0..4 {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), events.recv()).await;
    }
    assert_eq!(
        client.negotiated_protocol_version(),
        Some(3),
        "async: reconnect must restore v3 from missed_events"
    );

    let poll_mock = SharedMock::new(vec![AUTH, &recon]);
    let mut poll_client = SignalFishPollingClient::new(poll_mock, SignalFishConfig::new("app"));
    poll_client.poll();
    assert_eq!(
        poll_client.negotiated_protocol_version(),
        Some(3),
        "polling: reconnect must restore v3 from missed_events"
    );
}

// ── PARITY 5: reconnect with v2 missed_events must NOT downgrade ──────

#[tokio::test]
async fn parity_reconnect_v2_missed_events_does_not_downgrade_active_v3() {
    let recon_v2 = reconnected_with_missed(vec![ServerMessage::ProtocolInfo(pi_v2_payload())]);

    let async_mock = SharedMock::new(vec![AUTH, PI_V3, &recon_v2]);
    let (client, mut events) = SignalFishClient::start(async_mock, SignalFishConfig::new("app"));
    for _ in 0..5 {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), events.recv()).await;
    }
    assert_eq!(
        client.negotiated_protocol_version(),
        Some(3),
        "async: a replayed v2 ProtocolInfo must not downgrade active v3"
    );

    let poll_mock = SharedMock::new(vec![AUTH, PI_V3, &recon_v2]);
    let mut poll_client = SignalFishPollingClient::new(poll_mock, SignalFishConfig::new("app"));
    poll_client.poll();
    assert_eq!(
        poll_client.negotiated_protocol_version(),
        Some(3),
        "polling: a replayed v2 ProtocolInfo must not downgrade active v3"
    );
}

// ── PARITY 6: enable_mesh advertises v3 identically ──────────────────

#[tokio::test]
async fn parity_enable_mesh_authenticate_is_byte_identical() {
    let async_mock = SharedMock::new(vec![]);
    let (_client, _events) = SignalFishClient::start(
        async_mock.clone(),
        SignalFishConfig::new("app").enable_mesh(),
    );
    wait_for_sent_len(&async_mock, 1).await;
    let async_sent = async_mock.sent.lock().unwrap().clone();

    let poll_mock = SharedMock::new(vec![]);
    let mut poll_client = SignalFishPollingClient::new(
        poll_mock.clone(),
        SignalFishConfig::new("app").enable_mesh(),
    );
    poll_client.poll();
    let poll_sent = poll_mock.sent.lock().unwrap().clone();

    assert_eq!(
        async_sent[0], poll_sent[0],
        "enable_mesh Authenticate must be byte-identical between clients"
    );
}

// ── PARITY 7: disconnect resets negotiated version in both ─────────────

#[tokio::test]
async fn parity_disconnect_resets_negotiated_version() {
    let poll_mock = SharedMock::new(vec![AUTH, PI_V3]);
    let mut poll_client =
        SignalFishPollingClient::new(poll_mock, SignalFishConfig::new("app").enable_mesh());
    poll_client.poll();
    assert!(poll_client.supports_mesh());
    poll_client.close();
    assert_eq!(poll_client.negotiated_protocol_version(), None);

    let async_mock = SharedMock::from_msgs(vec![
        Some(Ok(AUTH.to_string())),
        Some(Ok(PI_V3.to_string())),
        Some(Err(SignalFishError::TransportReceive("reset".into()))),
    ]);
    let (client, mut events) =
        SignalFishClient::start(async_mock, SignalFishConfig::new("app").enable_mesh());
    let mut saw_disconnect = false;
    for _ in 0..6 {
        match tokio::time::timeout(std::time::Duration::from_millis(150), events.recv()).await {
            Ok(Some(SignalFishEvent::Disconnected { .. })) => {
                saw_disconnect = true;
                break;
            }
            Ok(Some(_)) => {}
            _ => break,
        }
    }
    assert!(saw_disconnect, "async should have disconnected");
    assert_eq!(client.negotiated_protocol_version(), None);
}

// ── PARITY 9: DecodeFailed surfacing is identical ─────────────────────

#[tokio::test]
async fn parity_decode_failed_async_vs_polling() {
    const BAD_FRAME: &str =
        r#"{"type":"Error","data":{"message":"x","error_code":"FUTURE_CODE_XYZ"}}"#;

    // Async client.
    let async_mock = SharedMock::new(vec![AUTH, BAD_FRAME]);
    let (async_client, mut events) =
        SignalFishClient::start(async_mock, SignalFishConfig::new("app"));
    let mut async_decode_failed = None;
    for _ in 0..6 {
        match tokio::time::timeout(std::time::Duration::from_millis(150), events.recv()).await {
            Ok(Some(ev @ SignalFishEvent::DecodeFailed { .. })) => {
                async_decode_failed = Some(ev);
                break;
            }
            Ok(Some(_)) => {}
            _ => break,
        }
    }

    // Polling client.
    let poll_mock = SharedMock::new(vec![AUTH, BAD_FRAME]);
    let mut poll_client = SignalFishPollingClient::new(poll_mock, SignalFishConfig::new("app"));
    let poll_events = poll_client.poll();
    let poll_decode_failed = poll_events
        .into_iter()
        .find(|e| matches!(e, SignalFishEvent::DecodeFailed { .. }));

    // Both must surface the event, with identical fields.
    let (
        Some(SignalFishEvent::DecodeFailed {
            message_type: a_type,
            error: a_err,
            raw_prefix: a_raw,
        }),
        Some(SignalFishEvent::DecodeFailed {
            message_type: p_type,
            error: p_err,
            raw_prefix: p_raw,
        }),
    ) = (async_decode_failed, poll_decode_failed)
    else {
        panic!("both clients must surface DecodeFailed for the same frame");
    };
    assert_eq!(a_type, p_type);
    assert_eq!(a_err, p_err);
    assert_eq!(a_raw, p_raw);
    assert_eq!(a_type.as_deref(), Some("Error"));

    // And identical stats accounting.
    assert_eq!(async_client.stats().messages_undecodable, 1);
    assert_eq!(poll_client.stats().messages_undecodable, 1);
}

// ── PARITY 10: Disconnected carries last_server_error identically ─────

#[tokio::test]
async fn parity_disconnected_carries_last_server_error() {
    const FAREWELL: &str = r#"{"type":"Error","data":{"message":"Disconnected as a slow consumer","error_code":"SLOW_CONSUMER"}}"#;

    // Async: farewell then clean close.
    let async_mock = SharedMock::from_msgs(vec![
        Some(Ok(AUTH.to_string())),
        Some(Ok(FAREWELL.to_string())),
        None,
    ]);
    let (_client, mut events) = SignalFishClient::start(async_mock, SignalFishConfig::new("app"));
    let mut async_info = None;
    for _ in 0..8 {
        match tokio::time::timeout(std::time::Duration::from_millis(150), events.recv()).await {
            Ok(Some(SignalFishEvent::Disconnected {
                last_server_error, ..
            })) => {
                async_info = last_server_error;
                break;
            }
            Ok(Some(_)) => {}
            _ => break,
        }
    }

    // Polling: same script.
    let poll_mock = SharedMock::from_msgs(vec![
        Some(Ok(AUTH.to_string())),
        Some(Ok(FAREWELL.to_string())),
        None,
    ]);
    let mut poll_client = SignalFishPollingClient::new(poll_mock, SignalFishConfig::new("app"));
    let poll_events = poll_client.poll();
    let poll_info = poll_events.into_iter().find_map(|e| match e {
        SignalFishEvent::Disconnected {
            last_server_error, ..
        } => last_server_error,
        _ => None,
    });

    let async_info = async_info.expect("async Disconnected must carry the farewell");
    let poll_info = poll_info.expect("polling Disconnected must carry the farewell");
    assert_eq!(async_info, poll_info);
    assert_eq!(
        async_info.error_code,
        Some(signal_fish_client::ErrorCode::SlowConsumer)
    );
}
