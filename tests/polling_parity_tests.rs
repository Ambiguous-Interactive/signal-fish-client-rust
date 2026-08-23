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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use signal_fish_client::client::{SignalFishClient, SignalFishConfig};
use signal_fish_client::error::SignalFishError;
use signal_fish_client::polling_client::{
    PollingClientOptions, PollingWorkBudget, SignalFishPollingClient,
};
use signal_fish_client::protocol::{
    ClientMessage, ConnectionInfo, DeliveryClass, DeliveryCountersByClass, DeliveryGap,
    DeliveryGapReason, DeliveryReportPayload, GameDataEncoding, LatestDeliveryCounters, LobbyState,
    PlayerId, PlayerInfo, ProtocolInfoPayload, ReconnectedPayload, ReliableDeliveryCounters,
    ReplayStatus, RoomJoinedPayload, RoomOperationRequest, RoomOperationResult, SenderWatermark,
    ServerMessage, SpectatorJoinedPayload, Topology, TransportKind, V2BinaryGameDataFrame,
    V3BinaryGameDataFrame,
};
use signal_fish_client::transport::TransportFrame;
use signal_fish_client::{ClientStats, ErrorCode, ProtocolViolationPolicy, RoomRole};
use signal_fish_client::{
    GameDataDelivery, JoinRoomParams, PeerSignal, SignalFishClientApi, SignalFishEvent, Transport,
};

fn assert_common_api_is_object_safe(_client: &mut dyn signal_fish_client::SignalFishClientApi) {}

fn selected_plan_through_common_api(
    client: &dyn SignalFishClientApi,
) -> (Option<Topology>, Option<TransportKind>, bool) {
    (
        client.session_topology(),
        client.session_transport(),
        client.is_p2p_active(),
    )
}

#[derive(Clone, Copy)]
enum InitialRoomOperation {
    JoinPlayer,
    JoinSpectator,
}

#[derive(Clone, Copy)]
#[repr(usize)]
enum RoomResponseKind {
    JoinPlayer,
    LeavePlayer,
    ReconnectPlayer,
    JoinSpectator,
    LeaveSpectator,
}

impl RoomResponseKind {
    fn index(self) -> usize {
        self as usize
    }

    fn matches_command(self, message: &ClientMessage) -> bool {
        if let ClientMessage::RoomOperation { operation, .. } = message {
            return matches!(
                (self, operation.as_ref()),
                (Self::JoinPlayer, RoomOperationRequest::JoinRoom { .. })
                    | (Self::LeavePlayer, RoomOperationRequest::LeaveRoom)
                    | (
                        Self::ReconnectPlayer,
                        RoomOperationRequest::Reconnect { .. }
                    )
                    | (
                        Self::JoinSpectator,
                        RoomOperationRequest::JoinAsSpectator { .. }
                    )
                    | (Self::LeaveSpectator, RoomOperationRequest::LeaveSpectator)
            );
        }
        matches!(
            (self, message),
            (Self::JoinPlayer, ClientMessage::JoinRoom { .. })
                | (Self::LeavePlayer, ClientMessage::LeaveRoom)
                | (Self::ReconnectPlayer, ClientMessage::Reconnect { .. })
                | (Self::JoinSpectator, ClientMessage::JoinAsSpectator { .. })
                | (Self::LeaveSpectator, ClientMessage::LeaveSpectator)
        )
    }
}

fn room_response_kind_json(json: &str) -> Option<RoomResponseKind> {
    match serde_json::from_str::<ServerMessage>(json).ok()? {
        ServerMessage::RoomJoined(_) | ServerMessage::RoomJoinFailed { .. } => {
            Some(RoomResponseKind::JoinPlayer)
        }
        ServerMessage::RoomLeft => Some(RoomResponseKind::LeavePlayer),
        ServerMessage::Reconnected(_) | ServerMessage::ReconnectionFailed { .. } => {
            Some(RoomResponseKind::ReconnectPlayer)
        }
        ServerMessage::SpectatorJoined(_) | ServerMessage::SpectatorJoinFailed { .. } => {
            Some(RoomResponseKind::JoinSpectator)
        }
        ServerMessage::SpectatorLeft {
            reason:
                None
                | Some(
                    signal_fish_client::protocol::SpectatorStateChangeReason::VoluntaryLeave
                    | signal_fish_client::protocol::SpectatorStateChangeReason::Joined,
                ),
            ..
        } => Some(RoomResponseKind::LeaveSpectator),
        _ => None,
    }
}

fn room_response_kind(frame: &TransportFrame) -> Option<RoomResponseKind> {
    let TransportFrame::Text(json) = frame else {
        return None;
    };
    room_response_kind_json(json)
}

fn text_matches_room_command(json: &str, kind: RoomResponseKind) -> bool {
    serde_json::from_str::<ClientMessage>(json).is_ok_and(|message| kind.matches_command(&message))
}

#[derive(Clone, Copy, Default)]
enum ScriptedRoomMembership {
    #[default]
    Outside,
    Player,
    Spectator,
}

struct RoomCommandRequirements {
    counts: [usize; 5],
    membership: ScriptedRoomMembership,
}

impl Default for RoomCommandRequirements {
    fn default() -> Self {
        Self {
            counts: [1; 5],
            membership: ScriptedRoomMembership::Outside,
        }
    }
}

fn advance_room_command_requirements(json: &str, gate: &mut RoomCommandRequirements) {
    let Ok(message) = serde_json::from_str::<ServerMessage>(json) else {
        return;
    };
    // Only accepted-looking lifecycle transitions advance the next command
    // ordinal. A raw duplicate exit while already outside must remain
    // deliverable without consuming the rejoin that is currently pending.
    let membership_result = match message {
        ServerMessage::RoomOperationResult { result, .. } => match *result {
            RoomOperationResult::RoomJoined(payload) => ServerMessage::RoomJoined(payload),
            RoomOperationResult::RoomLeft => ServerMessage::RoomLeft,
            RoomOperationResult::Reconnected(payload) => ServerMessage::Reconnected(payload),
            RoomOperationResult::SpectatorJoined(payload) => {
                ServerMessage::SpectatorJoined(payload)
            }
            RoomOperationResult::SpectatorLeft {
                room_id,
                room_code,
                reason,
                current_spectators,
            } => ServerMessage::SpectatorLeft {
                room_id,
                room_code,
                reason,
                current_spectators,
            },
            RoomOperationResult::RoomJoinFailed { .. }
            | RoomOperationResult::ReconnectionFailed { .. }
            | RoomOperationResult::SpectatorJoinFailed { .. }
            | RoomOperationResult::OperationFailed { .. } => return,
        },
        message => message,
    };
    match (gate.membership, membership_result) {
        (ScriptedRoomMembership::Outside, ServerMessage::RoomJoined(_)) => {
            gate.membership = ScriptedRoomMembership::Player;
            gate.counts[RoomResponseKind::LeavePlayer.index()] =
                gate.counts[RoomResponseKind::JoinPlayer.index()];
        }
        (ScriptedRoomMembership::Outside, ServerMessage::Reconnected(_)) => {
            gate.membership = ScriptedRoomMembership::Player;
        }
        (ScriptedRoomMembership::Player, ServerMessage::RoomLeft) => {
            gate.membership = ScriptedRoomMembership::Outside;
            gate.counts[RoomResponseKind::JoinPlayer.index()] += 1;
        }
        (ScriptedRoomMembership::Outside, ServerMessage::SpectatorJoined(_)) => {
            gate.membership = ScriptedRoomMembership::Spectator;
            gate.counts[RoomResponseKind::LeaveSpectator.index()] =
                gate.counts[RoomResponseKind::JoinSpectator.index()];
        }
        (ScriptedRoomMembership::Spectator, ServerMessage::SpectatorLeft { .. }) => {
            gate.membership = ScriptedRoomMembership::Outside;
            gate.counts[RoomResponseKind::JoinSpectator.index()] += 1;
        }
        _ => {}
    }
}

fn advance_frame_room_command_requirements(
    frame: &TransportFrame,
    gate: &mut RoomCommandRequirements,
) {
    if let TransportFrame::Text(json) = frame {
        advance_room_command_requirements(json, gate);
    }
}

fn initial_room_operation<'a>(
    frames: impl IntoIterator<Item = &'a TransportFrame>,
) -> Option<InitialRoomOperation> {
    frames
        .into_iter()
        .find_map(room_response_kind)
        .and_then(|kind| match kind {
            RoomResponseKind::JoinPlayer => Some(InitialRoomOperation::JoinPlayer),
            RoomResponseKind::JoinSpectator => Some(InitialRoomOperation::JoinSpectator),
            _ => None,
        })
}

fn room_response_counts(frames: &[TransportFrame]) -> [usize; 5] {
    let mut counts = [0; 5];
    for frame in frames {
        if let Some(kind) = room_response_kind(frame) {
            counts[kind.index()] += 1;
        }
    }
    counts
}

fn admit_initial_room_operation(
    client: &mut dyn SignalFishClientApi,
    operation: Option<InitialRoomOperation>,
) {
    match operation {
        Some(InitialRoomOperation::JoinPlayer) => client
            .join_room(JoinRoomParams::new("game", "local"))
            .expect("scripted player response must follow an admitted join"),
        Some(InitialRoomOperation::JoinSpectator) => client
            .join_as_spectator("game".into(), "ROOM".into(), "local".into())
            .expect("scripted spectator response must follow an admitted join"),
        None => {}
    }
}

#[derive(Clone)]
struct FrameMock {
    incoming: Arc<Mutex<VecDeque<TransportFrame>>>,
    sent: Arc<Mutex<Vec<TransportFrame>>>,
    required_room_commands: Arc<Mutex<RoomCommandRequirements>>,
}

#[derive(Clone)]
struct NeverSendMock {
    attempted: Arc<std::sync::atomic::AtomicBool>,
    allow: Arc<std::sync::atomic::AtomicBool>,
    waker: Arc<Mutex<Option<std::task::Waker>>>,
}

impl NeverSendMock {
    fn new() -> Self {
        Self {
            attempted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            allow: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            waker: Arc::new(Mutex::new(None)),
        }
    }

    fn release(&self) {
        self.allow.store(true, std::sync::atomic::Ordering::Release);
        if let Some(waker) = self.waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}

impl Transport for NeverSendMock {
    fn abort(&mut self) {
        let _ = self.waker.lock().unwrap().take();
    }

    fn poll_send(
        &mut self,
        cx: &mut std::task::Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        self.attempted
            .store(true, std::sync::atomic::Ordering::Release);
        if self.allow.load(std::sync::atomic::Ordering::Acquire) {
            let _ = frame.take();
            return std::task::Poll::Ready(Ok(()));
        }
        *self.waker.lock().unwrap() = Some(cx.waker().clone());
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

#[derive(Clone)]
struct CorrelationRaceMock {
    incoming: Arc<Mutex<VecDeque<TransportFrame>>>,
    sent: Arc<Mutex<Vec<String>>>,
    attempted_room_send: Arc<Mutex<Option<String>>>,
    allow_room_send: Arc<AtomicBool>,
    waker: Arc<Mutex<Option<std::task::Waker>>>,
}

impl CorrelationRaceMock {
    fn new() -> Self {
        Self {
            incoming: Arc::new(Mutex::new(VecDeque::from([
                TransportFrame::Text(AUTH.into()),
                TransportFrame::Text(PI_V3_ROOM_OPERATION_IDS.into()),
            ]))),
            sent: Arc::new(Mutex::new(Vec::new())),
            attempted_room_send: Arc::new(Mutex::new(None)),
            allow_room_send: Arc::new(AtomicBool::new(true)),
            waker: Arc::new(Mutex::new(None)),
        }
    }

    fn block_room_sends(&self) {
        self.allow_room_send.store(false, Ordering::Release);
    }

    fn release_room_sends(&self) {
        self.allow_room_send.store(true, Ordering::Release);
        if let Some(waker) = self.waker.lock().unwrap().take() {
            waker.wake();
        }
    }

    fn push(&self, message: ServerMessage) {
        self.incoming
            .lock()
            .unwrap()
            .push_back(text_server_frame(message));
        if let Some(waker) = self.waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}

impl Transport for CorrelationRaceMock {
    fn abort(&mut self) {
        let _ = self.waker.lock().unwrap().take();
    }

    fn poll_send(
        &mut self,
        cx: &mut std::task::Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        let room_message = match frame.as_ref() {
            Some(TransportFrame::Text(json)) => serde_json::from_str::<ClientMessage>(json)
                .ok()
                .filter(|message| matches!(message, ClientMessage::RoomOperation { .. }))
                .map(|_| json.clone()),
            _ => None,
        };
        if let Some(json) = room_message {
            *self.attempted_room_send.lock().unwrap() = Some(json);
            if !self.allow_room_send.load(Ordering::Acquire) {
                *self.waker.lock().unwrap() = Some(cx.waker().clone());
                return std::task::Poll::Pending;
            }
        }
        if let Some(TransportFrame::Text(json)) = frame.take() {
            self.sent.lock().unwrap().push(json);
        }
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_recv(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<TransportFrame, SignalFishError>>> {
        if let Some(frame) = self.incoming.lock().unwrap().pop_front() {
            return std::task::Poll::Ready(Some(Ok(frame)));
        }
        *self.waker.lock().unwrap() = Some(cx.waker().clone());
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
    fn outside() -> Self {
        Self {
            incoming: Arc::new(Mutex::new(VecDeque::from([
                TransportFrame::Text(AUTH.into()),
                TransportFrame::Text(PI_V3.into()),
            ]))),
            sent: Arc::new(Mutex::new(Vec::new())),
            required_room_commands: Arc::new(Mutex::new(RoomCommandRequirements::default())),
        }
    }

    fn v3() -> Self {
        Self {
            incoming: Arc::new(Mutex::new(VecDeque::from([
                TransportFrame::Text(AUTH.into()),
                TransportFrame::Text(PI_V3.into()),
                TransportFrame::Text(
                    r#"{"type":"RoomJoined","data":{"room_id":"00000000-0000-0000-0000-000000000008","room_code":"ROOM","player_id":"00000000-0000-0000-0000-000000000009","game_name":"game","max_players":4,"supports_authority":true,"current_players":[{"id":"00000000-0000-0000-0000-000000000009","name":"local","is_authority":true,"is_ready":true,"connected_at":"2026-01-01T00:00:00Z","epoch":1,"seq":0},{"id":"00000000-0000-0000-0000-000000000007","name":"peer","is_authority":false,"is_ready":true,"connected_at":"2026-01-01T00:00:00Z","epoch":1,"seq":0},{"id":"00000000-0000-0000-0000-000000000006","name":"off-plan","is_authority":false,"is_ready":true,"connected_at":"2026-01-01T00:00:00Z","epoch":1,"seq":0}],"is_authority":true,"lobby_state":"finalized","ready_players":[],"relay_type":"websocket","current_spectators":[]}}"#.into(),
                ),
                TransportFrame::Text(
                    r#"{"type":"SessionPlan","data":{"generation":"00000000-0000-0000-0000-00000000000c","topology":"mesh","transport":"webrtc","peers":[{"player_id":"00000000-0000-0000-0000-000000000007","player_name":"peer","is_authority":false,"initiate":true}],"fallback":"relay"}}"#.into(),
                ),
            ]))),
            sent: Arc::new(Mutex::new(Vec::new())),
            required_room_commands: Arc::new(Mutex::new(RoomCommandRequirements::default())),
        }
    }

    fn spectator() -> Self {
        Self {
            incoming: Arc::new(Mutex::new(VecDeque::from([
                TransportFrame::Text(AUTH.into()),
                TransportFrame::Text(PI_V3.into()),
                TransportFrame::Text(
                    r#"{"type":"SpectatorJoined","data":{"room_id":"00000000-0000-0000-0000-000000000008","room_code":"ROOM","spectator_id":"00000000-0000-0000-0000-000000000005","game_name":"game","current_players":[{"id":"00000000-0000-0000-0000-000000000009","name":"local","is_authority":true,"is_ready":true,"connected_at":"2026-01-01T00:00:00Z","epoch":1,"seq":0},{"id":"00000000-0000-0000-0000-000000000007","name":"peer","is_authority":false,"is_ready":true,"connected_at":"2026-01-01T00:00:00Z","epoch":1,"seq":0}],"current_spectators":[],"lobby_state":"lobby"}}"#.into(),
                ),
            ]))),
            sent: Arc::new(Mutex::new(Vec::new())),
            required_room_commands: Arc::new(Mutex::new(RoomCommandRequirements::default())),
        }
    }

    fn membership_trace(phase: MembershipPhase) -> Self {
        let base = match phase {
            MembershipPhase::Outside => Self::outside(),
            MembershipPhase::Player
            | MembershipPhase::PlayerLeft
            | MembershipPhase::PlayerRejoined => Self::v3(),
            MembershipPhase::Spectator
            | MembershipPhase::SpectatorLeft
            | MembershipPhase::SpectatorRejoined => Self::spectator(),
        };
        let mut frames = base.incoming.lock().unwrap().clone();
        match phase {
            MembershipPhase::PlayerLeft => {
                frames.push_back(text_server_frame(ServerMessage::RoomLeft));
            }
            MembershipPhase::PlayerRejoined => {
                let rejoined = match &frames[2] {
                    TransportFrame::Text(frame) => TransportFrame::Text(frame.replace(
                        "00000000-0000-0000-0000-000000000009",
                        "00000000-0000-0000-0000-00000000000f",
                    )),
                    TransportFrame::Binary(_) => unreachable!("room baseline must be text"),
                };
                let plan = frames[3].clone();
                frames.push_back(text_server_frame(ServerMessage::RoomLeft));
                frames.push_back(rejoined);
                frames.push_back(plan);
            }
            MembershipPhase::SpectatorLeft => {
                frames.push_back(text_server_frame(ServerMessage::SpectatorLeft {
                    room_id: Some(uuid::Uuid::from_u128(8)),
                    room_code: Some("ROOM".into()),
                    reason: None,
                    current_spectators: vec![],
                }));
            }
            MembershipPhase::SpectatorRejoined => {
                let rejoined = match &frames[2] {
                    TransportFrame::Text(frame) => TransportFrame::Text(frame.replace(
                        "00000000-0000-0000-0000-000000000005",
                        "00000000-0000-0000-0000-00000000000f",
                    )),
                    TransportFrame::Binary(_) => unreachable!("spectator baseline must be text"),
                };
                frames.push_back(text_server_frame(ServerMessage::SpectatorLeft {
                    room_id: Some(uuid::Uuid::from_u128(8)),
                    room_code: Some("ROOM".into()),
                    reason: None,
                    current_spectators: vec![],
                }));
                frames.push_back(rejoined);
            }
            MembershipPhase::Outside | MembershipPhase::Player | MembershipPhase::Spectator => {}
        }
        frames.push_back(text_server_frame(ServerMessage::Pong));
        Self {
            incoming: Arc::new(Mutex::new(frames)),
            sent: Arc::new(Mutex::new(Vec::new())),
            required_room_commands: Arc::new(Mutex::new(RoomCommandRequirements::default())),
        }
    }
}

impl Transport for FrameMock {
    fn abort(&mut self) {}

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
        let response_kind = self
            .incoming
            .lock()
            .unwrap()
            .front()
            .and_then(room_response_kind);
        if let Some(kind) = response_kind {
            let index = kind.index();
            let sent_count = self
                .sent
                .lock()
                .unwrap()
                .iter()
                .filter(|frame| {
                    let TransportFrame::Text(json) = frame else {
                        return false;
                    };
                    text_matches_room_command(json, kind)
                })
                .count();
            if sent_count < self.required_room_commands.lock().unwrap().counts[index] {
                return std::task::Poll::Pending;
            }
        }
        let delivered = match self.incoming.lock().unwrap().pop_front() {
            Some(frame) => std::task::Poll::Ready(Some(Ok(frame))),
            None => std::task::Poll::Pending,
        };
        if let std::task::Poll::Ready(Some(Ok(frame))) = &delivered {
            advance_frame_room_command_requirements(
                frame,
                &mut self.required_room_commands.lock().unwrap(),
            );
        }
        delivered
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
    SignalForGeneration,
    Offer,
    Answer,
    IceCandidate,
    RawSignal,
    RawSignalForGeneration,
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
            Self::SignalForGeneration => client.send_signal_for_generation(
                peer,
                Some(uuid::Uuid::from_u128(12)),
                PeerSignal::Offer("bound-sdp".into()),
            ),
            Self::Offer => client.send_offer(peer, "offer".into()),
            Self::Answer => client.send_answer(peer, "answer".into()),
            Self::IceCandidate => client.send_ice_candidate(peer, "candidate".into()),
            Self::RawSignal => client.send_raw_signal(peer, serde_json::json!({"Custom": 1})),
            Self::RawSignalForGeneration => client.send_raw_signal_for_generation(
                peer,
                Some(uuid::Uuid::from_u128(12)),
                serde_json::json!({"BoundCustom": 1}),
            ),
            Self::TransportStatus => client.report_transport_status(TransportKind::WebRtc, true),
        }
    }
}

const ALL_COMMON_COMMANDS: [CommonCommandCase; 22] = [
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
    CommonCommandCase::SignalForGeneration,
    CommonCommandCase::Offer,
    CommonCommandCase::Answer,
    CommonCommandCase::IceCandidate,
    CommonCommandCase::RawSignal,
    CommonCommandCase::RawSignalForGeneration,
    CommonCommandCase::TransportStatus,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MembershipResult {
    AdmittedOrLaterGuard,
    NotInRoom,
    AlreadyInRoom,
    NeedsPlayer,
    NeedsSpectator,
}

fn membership_result(result: Result<(), SignalFishError>) -> MembershipResult {
    match result {
        Err(SignalFishError::NotInRoom) => MembershipResult::NotInRoom,
        Err(SignalFishError::AlreadyInRoom) => MembershipResult::AlreadyInRoom,
        Err(SignalFishError::WrongRoomRole {
            required: RoomRole::Player,
            actual: RoomRole::Spectator,
        }) => MembershipResult::NeedsPlayer,
        Err(SignalFishError::WrongRoomRole {
            required: RoomRole::Spectator,
            actual: RoomRole::Player,
        }) => MembershipResult::NeedsSpectator,
        _ => MembershipResult::AdmittedOrLaterGuard,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MembershipPhase {
    Outside,
    Player,
    PlayerLeft,
    PlayerRejoined,
    Spectator,
    SpectatorLeft,
    SpectatorRejoined,
}

fn expected_membership_result(phase: MembershipPhase, case: CommonCommandCase) -> MembershipResult {
    let no_membership_only = matches!(
        case,
        CommonCommandCase::JoinRoom
            | CommonCommandCase::Reconnect
            | CommonCommandCase::JoinSpectator
    );
    let spectator_only = matches!(case, CommonCommandCase::LeaveSpectator);
    let any_role = matches!(case, CommonCommandCase::Ping);
    match phase {
        MembershipPhase::Outside | MembershipPhase::PlayerLeft | MembershipPhase::SpectatorLeft
            if no_membership_only || any_role =>
        {
            MembershipResult::AdmittedOrLaterGuard
        }
        MembershipPhase::Outside | MembershipPhase::PlayerLeft | MembershipPhase::SpectatorLeft => {
            MembershipResult::NotInRoom
        }
        MembershipPhase::Player | MembershipPhase::PlayerRejoined if no_membership_only => {
            MembershipResult::AlreadyInRoom
        }
        MembershipPhase::Player | MembershipPhase::PlayerRejoined if spectator_only => {
            MembershipResult::NeedsSpectator
        }
        MembershipPhase::Player | MembershipPhase::PlayerRejoined => {
            MembershipResult::AdmittedOrLaterGuard
        }
        MembershipPhase::Spectator | MembershipPhase::SpectatorRejoined if no_membership_only => {
            MembershipResult::AlreadyInRoom
        }
        MembershipPhase::Spectator | MembershipPhase::SpectatorRejoined
            if spectator_only || any_role =>
        {
            MembershipResult::AdmittedOrLaterGuard
        }
        MembershipPhase::Spectator | MembershipPhase::SpectatorRejoined => {
            MembershipResult::NeedsPlayer
        }
    }
}

fn frame_mock_for_phase(phase: MembershipPhase) -> FrameMock {
    FrameMock::membership_trace(phase)
}

fn expected_membership_state(phase: MembershipPhase) -> (Option<RoomRole>, Option<PlayerId>) {
    match phase {
        MembershipPhase::Outside | MembershipPhase::PlayerLeft | MembershipPhase::SpectatorLeft => {
            (None, None)
        }
        MembershipPhase::Player => (Some(RoomRole::Player), Some(uuid::Uuid::from_u128(9))),
        MembershipPhase::PlayerRejoined => {
            (Some(RoomRole::Player), Some(uuid::Uuid::from_u128(15)))
        }
        MembershipPhase::Spectator => (Some(RoomRole::Spectator), Some(uuid::Uuid::from_u128(5))),
        MembershipPhase::SpectatorRejoined => {
            (Some(RoomRole::Spectator), Some(uuid::Uuid::from_u128(15)))
        }
    }
}

async fn async_membership_result(
    phase: MembershipPhase,
    case: CommonCommandCase,
) -> MembershipResult {
    let (mut client, mut events) = SignalFishClient::start(
        frame_mock_for_phase(phase),
        SignalFishConfig::new("app").enable_v3(),
    );
    let initial = match phase {
        MembershipPhase::Outside => None,
        MembershipPhase::Player | MembershipPhase::PlayerLeft | MembershipPhase::PlayerRejoined => {
            Some(InitialRoomOperation::JoinPlayer)
        }
        MembershipPhase::Spectator
        | MembershipPhase::SpectatorLeft
        | MembershipPhase::SpectatorRejoined => Some(InitialRoomOperation::JoinSpectator),
    };
    admit_initial_room_operation(&mut client, initial);
    let mut leave_issued = false;
    let mut rejoin_issued = false;
    loop {
        match events.recv().await {
            Some(SignalFishEvent::RoomJoined { .. })
                if matches!(
                    phase,
                    MembershipPhase::PlayerLeft | MembershipPhase::PlayerRejoined
                ) && !leave_issued =>
            {
                client
                    .leave_room()
                    .expect("scripted player leave is admitted");
                leave_issued = true;
            }
            Some(SignalFishEvent::RoomLeft)
                if phase == MembershipPhase::PlayerRejoined && !rejoin_issued =>
            {
                client
                    .join_room(JoinRoomParams::new("game", "local"))
                    .expect("scripted player rejoin is admitted");
                rejoin_issued = true;
            }
            Some(SignalFishEvent::SpectatorJoined { .. })
                if matches!(
                    phase,
                    MembershipPhase::SpectatorLeft | MembershipPhase::SpectatorRejoined
                ) && !leave_issued =>
            {
                client
                    .leave_spectator()
                    .expect("scripted spectator leave is admitted");
                leave_issued = true;
            }
            Some(SignalFishEvent::SpectatorLeft { .. })
                if phase == MembershipPhase::SpectatorRejoined && !rejoin_issued =>
            {
                client
                    .join_as_spectator("game".into(), "ROOM".into(), "local".into())
                    .expect("scripted spectator rejoin is admitted");
                rejoin_issued = true;
            }
            Some(SignalFishEvent::Pong) => break,
            Some(_) => {}
            None => panic!("membership trace closed before Pong"),
        }
    }
    let (expected_role, expected_participant) = expected_membership_state(phase);
    assert_eq!(client.room_role(), expected_role, "async {phase:?}");
    assert_eq!(
        client.snapshot().player_id,
        expected_participant,
        "async {phase:?}"
    );
    let result = membership_result(case.invoke(&mut client));
    client.shutdown().await;
    result
}

fn polling_membership_result(phase: MembershipPhase, case: CommonCommandCase) -> MembershipResult {
    let mut client = SignalFishPollingClient::new(
        frame_mock_for_phase(phase),
        SignalFishConfig::new("app").enable_v3(),
    );
    let initial = match phase {
        MembershipPhase::Outside => None,
        MembershipPhase::Player | MembershipPhase::PlayerLeft | MembershipPhase::PlayerRejoined => {
            Some(InitialRoomOperation::JoinPlayer)
        }
        MembershipPhase::Spectator
        | MembershipPhase::SpectatorLeft
        | MembershipPhase::SpectatorRejoined => Some(InitialRoomOperation::JoinSpectator),
    };
    admit_initial_room_operation(&mut client, initial);
    let _ = client.poll();
    if matches!(
        phase,
        MembershipPhase::PlayerLeft | MembershipPhase::PlayerRejoined
    ) {
        client
            .leave_room()
            .expect("scripted player leave is admitted");
        let _ = client.poll();
    } else if matches!(
        phase,
        MembershipPhase::SpectatorLeft | MembershipPhase::SpectatorRejoined
    ) {
        client
            .leave_spectator()
            .expect("scripted spectator leave is admitted");
        let _ = client.poll();
    }
    if phase == MembershipPhase::PlayerRejoined {
        client
            .join_room(JoinRoomParams::new("game", "local"))
            .expect("scripted player rejoin is admitted");
        let _ = client.poll();
    } else if phase == MembershipPhase::SpectatorRejoined {
        client
            .join_as_spectator("game".into(), "ROOM".into(), "local".into())
            .expect("scripted spectator rejoin is admitted");
        let _ = client.poll();
    }
    let (expected_role, expected_participant) = expected_membership_state(phase);
    assert_eq!(client.room_role(), expected_role, "polling {phase:?}");
    assert_eq!(
        client.snapshot().player_id,
        expected_participant,
        "polling {phase:?}"
    );
    let result = membership_result(case.invoke(&mut client));
    client.close();
    result
}

const PEER_UUID: &str = "00000000-0000-0000-0000-000000000007";
const AUTH: &str = r#"{"type":"Authenticated","data":{"app_name":"test","rate_limits":{"per_minute":60,"per_hour":1000,"per_day":10000}}}"#;
const PI_V3: &str = r#"{"type":"ProtocolInfo","data":{"capabilities":[],"game_data_formats":["json","message_pack"],"protocol_version":3,"min_protocol_version":2,"max_protocol_version":3,"transports":["websocket"]}}"#;
const PI_V3_ROOM_OPERATION_IDS: &str = r#"{"type":"ProtocolInfo","data":{"capabilities":["room_operation_ids"],"game_data_formats":["json","message_pack"],"protocol_version":3,"min_protocol_version":2,"max_protocol_version":3,"transports":["websocket"]}}"#;
// A v2 negotiation omits the version fields, so it deserializes to
// `protocol_version: None` — a terminal relay floor.
const PI_V2: &str = r#"{"type":"ProtocolInfo","data":{"capabilities":[],"game_data_formats":["json","message_pack"]}}"#;

// ── Shared mock transport (works for both async + polling drivers) ────

#[derive(Clone)]
struct SharedMock {
    incoming: Arc<Mutex<VecDeque<Option<Result<String, SignalFishError>>>>>,
    sent: Arc<Mutex<Vec<String>>>,
    required_room_commands: Arc<Mutex<RoomCommandRequirements>>,
    gate_room_responses: bool,
    gate_reconnect_responses: bool,
}

#[derive(Clone)]
enum PostFailureRecv {
    Farewell,
    Pong,
    PongBytes(usize),
    ProtocolViolation,
    Pending,
    Eof,
    Error,
}

struct SendFailureFarewellState {
    authenticate_sent: bool,
    authenticated_delivered: bool,
    send_failed: bool,
    pre_failure: VecDeque<PostFailureRecv>,
    post_failure: VecDeque<PostFailureRecv>,
    post_failure_recv_calls: usize,
    ping_attempts: usize,
    close_calls: usize,
    abort_calls: usize,
    close_info: Option<signal_fish_client::TransportCloseInfo>,
    waker: Option<std::task::Waker>,
}

/// Refuses the first post-authentication Ping without taking its frame, then
/// makes a complete server farewell and EOF immediately ready. The same mock
/// drives both clients at the duplex send/receive failure boundary.
#[derive(Clone)]
struct SendFailureFarewellTransport {
    state: Arc<Mutex<SendFailureFarewellState>>,
}

impl SendFailureFarewellTransport {
    fn new(
        post_failure: impl IntoIterator<Item = PostFailureRecv>,
        close_info: Option<signal_fish_client::TransportCloseInfo>,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(SendFailureFarewellState {
                authenticate_sent: false,
                authenticated_delivered: false,
                send_failed: false,
                pre_failure: VecDeque::new(),
                post_failure: post_failure.into_iter().collect(),
                post_failure_recv_calls: 0,
                ping_attempts: 0,
                close_calls: 0,
                abort_calls: 0,
                close_info,
                waker: None,
            })),
        }
    }

    fn with_pre_failure(self, steps: impl IntoIterator<Item = PostFailureRecv>) -> Self {
        self.state.lock().unwrap().pre_failure = steps.into_iter().collect();
        self
    }

    fn ping_attempts(&self) -> usize {
        self.state.lock().unwrap().ping_attempts
    }

    fn send_failed(&self) -> bool {
        self.state.lock().unwrap().send_failed
    }

    fn close_calls(&self) -> usize {
        self.state.lock().unwrap().close_calls
    }

    fn abort_calls(&self) -> usize {
        self.state.lock().unwrap().abort_calls
    }

    fn post_failure_recv_calls(&self) -> usize {
        self.state.lock().unwrap().post_failure_recv_calls
    }

    fn poll_recv_step(
        step: PostFailureRecv,
    ) -> std::task::Poll<Option<Result<TransportFrame, SignalFishError>>> {
        let result = match step {
            PostFailureRecv::Farewell => Ok(TransportFrame::Text(
                r#"{"type":"Error","data":{"message":"Disconnected as a slow consumer","error_code":"SLOW_CONSUMER"}}"#.into(),
            )),
            PostFailureRecv::Pong => Ok(TransportFrame::Text(r#"{"type":"Pong"}"#.into())),
            PostFailureRecv::PongBytes(bytes) => {
                let mut pong = r#"{"type":"Pong"}"#.to_string();
                pong.extend(std::iter::repeat_n(' ', bytes.saturating_sub(pong.len())));
                Ok(TransportFrame::Text(pong))
            }
            PostFailureRecv::ProtocolViolation => Ok(TransportFrame::Text(AUTH.into())),
            PostFailureRecv::Pending => return std::task::Poll::Pending,
            PostFailureRecv::Eof => return std::task::Poll::Ready(None),
            PostFailureRecv::Error => Err(SignalFishError::TransportReceive(
                "scripted read failure".into(),
            )),
        };
        std::task::Poll::Ready(Some(result))
    }
}

impl Default for SendFailureFarewellTransport {
    fn default() -> Self {
        Self::new([PostFailureRecv::Farewell, PostFailureRecv::Eof], None)
    }
}

impl Transport for SendFailureFarewellTransport {
    fn abort(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.abort_calls = state.abort_calls.saturating_add(1);
        let _ = state.waker.take();
    }

    fn poll_send(
        &mut self,
        _cx: &mut std::task::Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        let message = frame.as_ref().and_then(|frame| match frame {
            TransportFrame::Text(json) => serde_json::from_str::<ClientMessage>(json).ok(),
            TransportFrame::Binary(_) => None,
        });
        let mut state = self.state.lock().unwrap();
        match message {
            Some(ClientMessage::Authenticate { .. }) => {
                let _ = frame.take();
                state.authenticate_sent = true;
                if let Some(waker) = state.waker.take() {
                    waker.wake();
                }
                std::task::Poll::Ready(Ok(()))
            }
            Some(ClientMessage::Ping) => {
                state.ping_attempts = state.ping_attempts.saturating_add(1);
                state.send_failed = true;
                if let Some(waker) = state.waker.take() {
                    waker.wake();
                }
                std::task::Poll::Ready(Err(SignalFishError::TransportSend(
                    "scripted write failure".into(),
                )))
            }
            _ => panic!("farewell transport received an unexpected outbound frame"),
        }
    }

    fn poll_recv(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<TransportFrame, SignalFishError>>> {
        let mut state = self.state.lock().unwrap();
        if state.authenticate_sent && !state.authenticated_delivered {
            state.authenticated_delivered = true;
            return std::task::Poll::Ready(Some(Ok(TransportFrame::Text(AUTH.into()))));
        }
        if !state.send_failed {
            if let Some(step) = state.pre_failure.pop_front() {
                let polled = Self::poll_recv_step(step);
                if polled.is_ready() {
                    return polled;
                }
            }
        }
        if state.send_failed {
            state.post_failure_recv_calls = state.post_failure_recv_calls.saturating_add(1);
            if let Some(step) = state.post_failure.pop_front() {
                let polled = Self::poll_recv_step(step);
                if polled.is_ready() {
                    return polled;
                }
            }
        }
        state.waker = Some(cx.waker().clone());
        std::task::Poll::Pending
    }

    fn poll_close(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        let mut state = self.state.lock().unwrap();
        state.close_calls = state.close_calls.saturating_add(1);
        std::task::Poll::Ready(Ok(()))
    }

    fn close_info(&self) -> Option<signal_fish_client::TransportCloseInfo> {
        self.state.lock().unwrap().close_info.clone()
    }
}

async fn wait_until(condition: impl Fn() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("scripted transport state should advance promptly");
}

async fn count_async_terminal_pongs(
    steps: impl IntoIterator<Item = PostFailureRecv>,
) -> (usize, usize) {
    let transport = SendFailureFarewellTransport::new(steps, None);
    let observer = transport.clone();
    let (mut client, mut events) = SignalFishClient::start(transport, SignalFishConfig::new("app"));
    assert!(matches!(
        events.recv().await,
        Some(SignalFishEvent::Connected)
    ));
    assert!(matches!(
        events.recv().await,
        Some(SignalFishEvent::Authenticated { .. })
    ));
    client
        .ping()
        .expect("scripted async Ping should be admitted");
    let pongs = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        let mut pongs = 0usize;
        loop {
            match events
                .recv()
                .await
                .expect("terminal event channel must remain open through Disconnected")
            {
                SignalFishEvent::Pong => pongs = pongs.saturating_add(1),
                SignalFishEvent::Disconnected { .. } => break pongs,
                other => panic!("unexpected async terminal event: {other:?}"),
            }
        }
    })
    .await
    .expect("bounded terminal drain must reach Disconnected");
    client.shutdown().await;
    (pongs, observer.post_failure_recv_calls())
}

fn count_polling_terminal_pongs(
    steps: impl IntoIterator<Item = PostFailureRecv>,
    receive_frames: usize,
    receive_bytes: usize,
) -> (usize, usize, u64) {
    let transport = SendFailureFarewellTransport::new(steps, None);
    let observer = transport.clone();
    let options = PollingClientOptions {
        work_budget: PollingWorkBudget {
            receive_frames,
            receive_bytes,
            ..PollingWorkBudget::default()
        },
        ..PollingClientOptions::default()
    };
    let mut client =
        SignalFishPollingClient::new_with_options(transport, SignalFishConfig::new("app"), options);
    let _ = client.poll();
    client.ping().expect("polling Ping should be admitted");
    let events = client.poll();
    let pongs = events
        .iter()
        .filter(|event| matches!(event, SignalFishEvent::Pong))
        .count();
    assert!(matches!(
        events.last(),
        Some(SignalFishEvent::Disconnected { .. })
    ));
    let recv_calls = observer.post_failure_recv_calls();
    let exhaustions = client.polling_stats().receive_budget_exhaustions;
    let _ = client.poll();
    assert_eq!(observer.post_failure_recv_calls(), recv_calls);
    (pongs, recv_calls, exhaustions)
}

impl SharedMock {
    fn new(msgs: Vec<&str>) -> Self {
        Self {
            incoming: Arc::new(Mutex::new(
                msgs.into_iter().map(|m| Some(Ok(m.to_string()))).collect(),
            )),
            sent: Arc::new(Mutex::new(Vec::new())),
            required_room_commands: Arc::new(Mutex::new(RoomCommandRequirements::default())),
            gate_room_responses: false,
            gate_reconnect_responses: false,
        }
    }
    fn from_msgs(msgs: Vec<Option<Result<String, SignalFishError>>>) -> Self {
        Self {
            incoming: Arc::new(Mutex::new(msgs.into())),
            sent: Arc::new(Mutex::new(Vec::new())),
            required_room_commands: Arc::new(Mutex::new(RoomCommandRequirements::default())),
            gate_room_responses: false,
            gate_reconnect_responses: false,
        }
    }

    fn from_msgs_gated(
        msgs: Vec<Option<Result<String, SignalFishError>>>,
        gate_reconnect_responses: bool,
    ) -> Self {
        let mut mock = Self::from_msgs(msgs);
        mock.gate_room_responses = true;
        mock.gate_reconnect_responses = gate_reconnect_responses;
        mock
    }
}

impl Transport for SharedMock {
    fn abort(&mut self) {}

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
        let response_kind = self.incoming.lock().unwrap().front().and_then(|item| {
            let Some(Ok(json)) = item else {
                return None;
            };
            room_response_kind_json(json)
        });
        if self.gate_room_responses {
            if let Some(kind) = response_kind.filter(|kind| {
                !matches!(kind, RoomResponseKind::ReconnectPlayer) || self.gate_reconnect_responses
            }) {
                let index = kind.index();
                let sent_count = self
                    .sent
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|json| text_matches_room_command(json, kind))
                    .count();
                if sent_count < self.required_room_commands.lock().unwrap().counts[index] {
                    return std::task::Poll::Pending;
                }
            }
        }
        let item = self.incoming.lock().unwrap().pop_front();
        match item {
            Some(inner) => {
                if let Some(Ok(json)) = &inner {
                    advance_room_command_requirements(
                        json,
                        &mut self.required_room_commands.lock().unwrap(),
                    );
                }
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

#[derive(Clone)]
struct ReadinessControls {
    ready: Arc<AtomicBool>,
    terminal: Arc<AtomicBool>,
    waker: Arc<Mutex<Option<std::task::Waker>>>,
}

impl ReadinessControls {
    fn set_ready(&self) {
        self.ready.store(true, Ordering::Release);
        self.wake();
    }

    fn terminate(&self) {
        self.terminal.store(true, Ordering::Release);
        self.wake();
    }

    fn wake(&self) {
        if let Some(waker) = self.waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}

struct ReadinessMock {
    controls: ReadinessControls,
}

impl ReadinessMock {
    fn new() -> (Self, ReadinessControls) {
        let controls = ReadinessControls {
            ready: Arc::new(AtomicBool::new(false)),
            terminal: Arc::new(AtomicBool::new(false)),
            waker: Arc::new(Mutex::new(None)),
        };
        (
            Self {
                controls: controls.clone(),
            },
            controls,
        )
    }
}

impl Transport for ReadinessMock {
    fn abort(&mut self) {
        self.controls.terminal.store(true, Ordering::Release);
        let _ = self.controls.waker.lock().unwrap().take();
    }

    fn poll_send(
        &mut self,
        cx: &mut std::task::Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        if !self.is_ready() {
            *self.controls.waker.lock().unwrap() = Some(cx.waker().clone());
            if !self.is_ready() {
                return std::task::Poll::Pending;
            }
        }
        let _ = frame.take();
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_recv(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<TransportFrame, SignalFishError>>> {
        if self.controls.terminal.swap(false, Ordering::AcqRel) {
            return std::task::Poll::Ready(None);
        }
        *self.controls.waker.lock().unwrap() = Some(cx.waker().clone());
        if self.controls.terminal.swap(false, Ordering::AcqRel) {
            std::task::Poll::Ready(None)
        } else {
            std::task::Poll::Pending
        }
    }

    fn poll_close(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn is_ready(&self) -> bool {
        self.controls.ready.load(Ordering::Acquire)
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
        game_data_formats: vec![GameDataEncoding::Json, GameDataEncoding::MessagePack],
        player_name_rules: None,
        protocol_version: Some(3),
        min_protocol_version: Some(2),
        max_protocol_version: Some(3),
        transports: Some(vec![
            signal_fish_client::protocol::MessageTransport::Websocket,
        ]),
        max_outbound_message_size: Some(8 * 1024 * 1024),
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
    let local = uuid::Uuid::from_u128(200);
    let payload = ReconnectedPayload {
        room_id: uuid::Uuid::from_u128(100),
        room_code: "R".into(),
        player_id: local,
        game_name: "g".into(),
        max_players: 4,
        supports_authority: false,
        current_players: vec![PlayerInfo {
            id: local,
            name: "local".into(),
            is_authority: true,
            is_ready: false,
            connected_at: "2026-01-01T00:00:00Z".into(),
            connection_info: None,
            epoch: Some(1),
            seq: Some(0),
        }],
        is_authority: true,
        lobby_state: LobbyState::Waiting,
        ready_players: vec![],
        relay_type: "tcp".into(),
        current_spectators: vec![],
        ice_servers: vec![],
        missed_events: missed,
        replay: Some(ReplayStatus::Complete),
        sender_watermarks: vec![SenderWatermark {
            player_id: local,
            epoch: 1,
            seq: 0,
        }],
        reconnection_token: Some("rotated-token".into()),
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
    sent: Arc<Mutex<Vec<TransportFrame>>>,
    required_room_commands: Arc<Mutex<RoomCommandRequirements>>,
    gate_reconnect_responses: bool,
}

impl TraceMock {
    fn new(frames: Vec<TransportFrame>, gate_reconnect_responses: bool) -> Self {
        Self {
            incoming: Arc::new(Mutex::new(
                frames
                    .into_iter()
                    .map(|frame| Some(Ok(frame)))
                    .chain(std::iter::once(None))
                    .collect(),
            )),
            sent: Arc::new(Mutex::new(Vec::new())),
            required_room_commands: Arc::new(Mutex::new(RoomCommandRequirements::default())),
            gate_reconnect_responses,
        }
    }
}

impl Transport for TraceMock {
    fn abort(&mut self) {}

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
        let response_kind = self.incoming.lock().unwrap().front().and_then(|item| {
            let Some(Ok(TransportFrame::Text(json))) = item else {
                return None;
            };
            room_response_kind_json(json)
        });
        if let Some(kind) = response_kind.filter(|kind| {
            !matches!(kind, RoomResponseKind::ReconnectPlayer) || self.gate_reconnect_responses
        }) {
            let index = kind.index();
            let sent_count = self
                .sent
                .lock()
                .unwrap()
                .iter()
                .filter(|frame| {
                    let TransportFrame::Text(json) = frame else {
                        return false;
                    };
                    text_matches_room_command(json, kind)
                })
                .count();
            if sent_count < self.required_room_commands.lock().unwrap().counts[index] {
                return std::task::Poll::Pending;
            }
        }
        let delivered = match self.incoming.lock().unwrap().pop_front() {
            Some(item) => std::task::Poll::Ready(item),
            None => std::task::Poll::Pending,
        };
        if let std::task::Poll::Ready(Some(Ok(frame))) = &delivered {
            advance_frame_room_command_requirements(
                frame,
                &mut self.required_room_commands.lock().unwrap(),
            );
        }
        delivered
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
            generation,
            topology,
            transport,
            host,
            direct_endpoint,
            peers,
            ice_servers,
            fallback,
        } => event_fields!(
            "SessionPlan",
            generation,
            topology,
            transport,
            host,
            direct_endpoint,
            peers,
            ice_servers,
            fallback
        ),
        SignalFishEvent::NewPeer {
            peer_id,
            you_initiate,
        } => event_fields!("NewPeer", peer_id, you_initiate),
        SignalFishEvent::SignalReceived {
            from,
            generation,
            signal,
        } => {
            event_fields!("SignalReceived", from, generation, signal)
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
        SignalFishEvent::RoomOperationFailed { reason, error_code } => {
            event_fields!("RoomOperationFailed", reason, error_code)
        }
    }
}

#[derive(Default)]
struct RoomTraceContinuation {
    player_leave_issued: bool,
    player_rejoin_issued: bool,
    spectator_leave_issued: bool,
    spectator_rejoin_issued: bool,
}

impl RoomTraceContinuation {
    fn after_event(
        &mut self,
        client: &mut dyn SignalFishClientApi,
        event: &SignalFishEvent,
        response_counts: &[usize; 5],
    ) {
        match event {
            SignalFishEvent::RoomJoined { .. }
                if response_counts[RoomResponseKind::LeavePlayer.index()] > 0
                    && !self.player_leave_issued =>
            {
                client
                    .leave_room()
                    .expect("scripted RoomLeft must follow an admitted leave");
                self.player_leave_issued = true;
            }
            SignalFishEvent::RoomLeft
                if response_counts[RoomResponseKind::JoinPlayer.index()] > 1
                    && !self.player_rejoin_issued =>
            {
                client
                    .join_room(JoinRoomParams::new("game", "local"))
                    .expect("scripted RoomJoined must follow an admitted rejoin");
                self.player_rejoin_issued = true;
            }
            SignalFishEvent::SpectatorJoined { .. }
                if response_counts[RoomResponseKind::LeaveSpectator.index()] > 0
                    && !self.spectator_leave_issued =>
            {
                client
                    .leave_spectator()
                    .expect("scripted SpectatorLeft must follow an admitted leave");
                self.spectator_leave_issued = true;
            }
            SignalFishEvent::SpectatorLeft { .. }
                if response_counts[RoomResponseKind::JoinSpectator.index()] > 1
                    && !self.spectator_rejoin_issued =>
            {
                client
                    .join_as_spectator("game".into(), "ROOM".into(), "local".into())
                    .expect("scripted SpectatorJoined must follow an admitted rejoin");
                self.spectator_rejoin_issued = true;
            }
            _ => {}
        }
    }
}

fn drive_polling_room_continuations<T: Transport>(
    client: &mut SignalFishPollingClient<T>,
    response_counts: &[usize; 5],
    events: &mut Vec<SignalFishEvent>,
) {
    if response_counts[RoomResponseKind::LeavePlayer.index()] > 0 {
        client
            .leave_room()
            .expect("scripted RoomLeft must follow an admitted leave");
        events.extend(client.poll());
        if response_counts[RoomResponseKind::JoinPlayer.index()] > 1 {
            client
                .join_room(JoinRoomParams::new("game", "local"))
                .expect("scripted RoomJoined must follow an admitted rejoin");
            events.extend(client.poll());
        }
    }
    if response_counts[RoomResponseKind::LeaveSpectator.index()] > 0 {
        client
            .leave_spectator()
            .expect("scripted SpectatorLeft must follow an admitted leave");
        events.extend(client.poll());
        if response_counts[RoomResponseKind::JoinSpectator.index()] > 1 {
            client
                .join_as_spectator("game".into(), "ROOM".into(), "local".into())
                .expect("scripted SpectatorJoined must follow an admitted rejoin");
            events.extend(client.poll());
        }
    }
}

async fn assert_frame_trace_parity(
    frames: Vec<TransportFrame>,
    config: SignalFishConfig,
) -> Vec<String> {
    assert_frame_trace_parity_with_stats(frames, config, false, None).await
}

async fn assert_frame_trace_parity_with_reconnect(
    frames: Vec<TransportFrame>,
    config: SignalFishConfig,
    admit_reconnect: bool,
) -> Vec<String> {
    assert_frame_trace_parity_with_stats(frames, config, admit_reconnect, None).await
}

async fn assert_frame_trace_parity_with_stats(
    frames: Vec<TransportFrame>,
    config: SignalFishConfig,
    admit_reconnect: bool,
    expected_stats: Option<ClientStats>,
) -> Vec<String> {
    let make_mock = || TraceMock::new(frames.clone(), admit_reconnect);
    let initial_room_operation = initial_room_operation(&frames);
    let response_counts = room_response_counts(&frames);

    let async_mock = make_mock();
    let (mut async_client, mut async_rx) = SignalFishClient::start(async_mock, config.clone());
    admit_initial_room_operation(&mut async_client, initial_room_operation);
    if admit_reconnect {
        async_client
            .reconnect(
                uuid::Uuid::from_u128(200),
                uuid::Uuid::from_u128(100),
                "submitted-token".into(),
            )
            .expect("async reconnect must queue");
    }
    let mut async_events = Vec::new();
    let mut room_continuation = RoomTraceContinuation::default();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while let Some(event) = async_rx.recv().await {
            room_continuation.after_event(&mut async_client, &event, &response_counts);
            async_events.push(event);
        }
    })
    .await
    .expect("async server trace should terminate on scripted close");

    let polling_mock = make_mock();
    let mut polling_client = SignalFishPollingClient::new(polling_mock, config);
    admit_initial_room_operation(&mut polling_client, initial_room_operation);
    if admit_reconnect {
        polling_client
            .reconnect(
                uuid::Uuid::from_u128(200),
                uuid::Uuid::from_u128(100),
                "submitted-token".into(),
            )
            .expect("polling reconnect must queue");
    }
    let mut polling_events = polling_client.poll();
    drive_polling_room_continuations(&mut polling_client, &response_counts, &mut polling_events);

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
    let async_stats = async_client.stats();
    assert_eq!(async_stats, polling_client.stats());
    if let Some(expected) = expected_stats {
        assert_eq!(async_stats, expected);
    }
    async_events
}

async fn assert_server_trace_parity(lines: &str, config: SignalFishConfig) {
    let frames = lines
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| TransportFrame::Text(line.to_owned()))
        .collect();
    let _ = assert_frame_trace_parity(frames, config).await;
}

#[tokio::test]
async fn readiness_phase_and_terminal_order_have_complete_driver_parity() {
    let (async_transport, async_controls) = ReadinessMock::new();
    let (mut async_client, mut async_events) =
        SignalFishClient::start(async_transport, SignalFishConfig::new("app"));
    let (polling_transport, polling_controls) = ReadinessMock::new();
    let mut polling_client =
        SignalFishPollingClient::new(polling_transport, SignalFishConfig::new("app"));

    assert_eq!(async_client.snapshot(), polling_client.snapshot());
    assert_eq!(
        (
            async_client.is_connected(),
            async_client.is_transport_ready(),
            async_client.is_authenticated(),
            async_client.room_role(),
        ),
        (true, false, false, None)
    );
    tokio::task::yield_now().await;
    assert!(matches!(
        async_events.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    assert!(polling_client.poll().is_empty());

    async_controls.set_ready();
    polling_controls.set_ready();
    let async_connected =
        tokio::time::timeout(std::time::Duration::from_secs(1), async_events.recv())
            .await
            .expect("async readiness transition should wake")
            .expect("Connected should be delivered");
    let polling_connected = polling_client.poll();
    assert!(matches!(async_connected, SignalFishEvent::Connected));
    assert!(matches!(
        polling_connected.as_slice(),
        [SignalFishEvent::Connected]
    ));
    assert_eq!(async_client.snapshot(), polling_client.snapshot());

    async_controls.terminate();
    polling_controls.terminate();
    let async_terminal =
        tokio::time::timeout(std::time::Duration::from_secs(1), async_events.recv())
            .await
            .expect("async terminal transition should wake")
            .expect("Disconnected should be delivered");
    let polling_terminal = polling_client.poll();
    assert!(matches!(
        async_terminal,
        SignalFishEvent::Disconnected { .. }
    ));
    assert!(matches!(
        polling_terminal.as_slice(),
        [SignalFishEvent::Disconnected { .. }]
    ));
    assert_eq!(async_client.snapshot(), polling_client.snapshot());
    assert_eq!(
        (
            async_client.is_connected(),
            async_client.is_transport_ready(),
            async_client.is_authenticated(),
            async_client.room_role(),
        ),
        (false, false, false, None)
    );

    async_client.shutdown().await;
}

async fn assert_open_text_trace_parity(
    messages: Vec<String>,
    config: SignalFishConfig,
) -> (Vec<String>, signal_fish_client::ClientSnapshot) {
    assert_open_text_trace_parity_with_reconnect(messages, config, false).await
}

async fn assert_open_reconnect_trace_parity(
    messages: Vec<String>,
    config: SignalFishConfig,
) -> (Vec<String>, signal_fish_client::ClientSnapshot) {
    assert_open_text_trace_parity_with_reconnect(messages, config, true).await
}

async fn assert_open_text_trace_parity_with_reconnect(
    messages: Vec<String>,
    config: SignalFishConfig,
    admit_reconnect: bool,
) -> (Vec<String>, signal_fish_client::ClientSnapshot) {
    let expected_events = messages.len() + 1;
    let frames = messages
        .iter()
        .cloned()
        .map(TransportFrame::Text)
        .collect::<Vec<_>>();
    let initial_room_operation = initial_room_operation(&frames);
    let response_counts = room_response_counts(&frames);
    let make_mock = || {
        SharedMock::from_msgs_gated(
            messages
                .iter()
                .cloned()
                .map(|message| Some(Ok(message)))
                .collect(),
            admit_reconnect,
        )
    };

    let async_mock = make_mock();
    let (mut async_client, mut async_rx) = SignalFishClient::start(async_mock, config.clone());
    admit_initial_room_operation(&mut async_client, initial_room_operation);
    if admit_reconnect {
        async_client
            .reconnect(
                uuid::Uuid::from_u128(200),
                uuid::Uuid::from_u128(100),
                "submitted-token".into(),
            )
            .expect("async reconnect must queue");
    }
    let mut async_events = Vec::with_capacity(expected_events);
    let mut room_continuation = RoomTraceContinuation::default();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while async_events.len() < expected_events {
            let event = async_rx
                .recv()
                .await
                .expect("open async trace should emit every scripted outcome");
            room_continuation.after_event(&mut async_client, &event, &response_counts);
            async_events.push(event);
        }
    })
    .await
    .expect("open async trace should process every scripted frame");
    let async_snapshot = async_client.snapshot();

    let polling_mock = make_mock();
    let mut polling_client = SignalFishPollingClient::new(polling_mock, config);
    admit_initial_room_operation(&mut polling_client, initial_room_operation);
    if admit_reconnect {
        polling_client
            .reconnect(
                uuid::Uuid::from_u128(200),
                uuid::Uuid::from_u128(100),
                "submitted-token".into(),
            )
            .expect("polling reconnect must queue");
    }
    let mut polling_events = polling_client.poll();
    drive_polling_room_continuations(&mut polling_client, &response_counts, &mut polling_events);
    let polling_snapshot = polling_client.snapshot();

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
    assert_eq!(async_snapshot, polling_snapshot);
    assert_eq!(async_client.stats(), polling_client.stats());
    assert_eq!(
        async_client.session_topology(),
        polling_client.session_topology()
    );
    assert_eq!(
        async_client.session_transport(),
        polling_client.session_transport()
    );
    assert_eq!(async_client.is_p2p_active(), polling_client.is_p2p_active());
    assert_eq!(
        async_client.session_topology(),
        async_snapshot.session_topology
    );
    assert_eq!(
        async_client.session_transport(),
        async_snapshot.session_transport
    );
    assert_eq!(
        selected_plan_through_common_api(&async_client),
        selected_plan_through_common_api(&polling_client)
    );
    assert_eq!(
        selected_plan_through_common_api(&async_client),
        (
            async_snapshot.session_topology,
            async_snapshot.session_transport,
            async_client.is_p2p_active(),
        )
    );
    async_client.shutdown().await;
    (async_events, async_snapshot)
}

fn protocol_info_with_formats(formats: Vec<GameDataEncoding>) -> String {
    let mut payload = pi_v3_payload();
    payload.game_data_formats = formats;
    serde_json::to_string(&ServerMessage::ProtocolInfo(payload))
        .expect("ProtocolInfo fixture should serialize")
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

#[tokio::test]
async fn requested_and_effective_game_data_formats_have_complete_parity() {
    let cases = [
        (
            None,
            vec![GameDataEncoding::Json, GameDataEncoding::MessagePack],
            GameDataEncoding::Json,
        ),
        (
            Some(GameDataEncoding::MessagePack),
            vec![GameDataEncoding::Json, GameDataEncoding::MessagePack],
            GameDataEncoding::MessagePack,
        ),
        (
            Some(GameDataEncoding::MessagePack),
            vec![GameDataEncoding::Json],
            GameDataEncoding::Json,
        ),
        (
            Some(GameDataEncoding::Rkyv),
            vec![GameDataEncoding::Json, GameDataEncoding::MessagePack],
            GameDataEncoding::Json,
        ),
    ];

    for (requested, advertised, effective) in cases {
        let mut config = SignalFishConfig::new("app").enable_v3();
        config.game_data_format = requested;
        let (_events, snapshot) = assert_open_text_trace_parity(
            vec![AUTH.into(), protocol_info_with_formats(advertised)],
            config,
        )
        .await;
        assert_eq!(snapshot.requested_game_data_format, requested);
        assert_eq!(snapshot.effective_game_data_format, Some(effective));
    }
}

#[tokio::test]
async fn malformed_duplicate_and_around_negotiation_frames_have_complete_parity() {
    let mut config = SignalFishConfig::new("app").enable_v3();
    config.game_data_format = Some(GameDataEncoding::MessagePack);
    let reconnect = reconnected_with_missed(vec![]);
    let (events, snapshot) = assert_open_reconnect_trace_parity(
        vec![
            AUTH.into(),
            reconnect.clone(),
            protocol_info_with_formats(vec![]),
            protocol_info_with_formats(vec![GameDataEncoding::Json, GameDataEncoding::MessagePack]),
            protocol_info_with_formats(vec![GameDataEncoding::Json]),
            reconnect,
        ],
        config,
    )
    .await;

    assert_eq!(
        events
            .iter()
            .map(|event| event.split('|').next().expect("event kind"))
            .collect::<Vec<_>>(),
        [
            "Authenticated",
            "ProtocolViolation",
            "ProtocolViolation",
            "ProtocolInfo",
            "ProtocolViolation",
            "Reconnected",
        ]
    );
    assert_eq!(
        snapshot.requested_game_data_format,
        Some(GameDataEncoding::MessagePack)
    );
    assert_eq!(
        snapshot.effective_game_data_format,
        Some(GameDataEncoding::MessagePack),
        "invalid or duplicate negotiation must not partially replace effective state"
    );
    assert!(snapshot.room_id.is_some());
}

#[tokio::test]
async fn json_fallback_is_enforced_before_outbound_transport_admission_in_both_drivers() {
    let fallback_error = serde_json::to_string(&ServerMessage::Error {
        message: "rkyv is unsupported; falling back to JSON".into(),
        error_code: Some(ErrorCode::UnsupportedGameDataFormat),
    })
    .expect("fallback Error fixture should serialize");
    let protocol_info =
        protocol_info_with_formats(vec![GameDataEncoding::Json, GameDataEncoding::MessagePack]);
    let room = match binary_accountability_prefix(uuid::Uuid::from_u128(365)).remove(2) {
        TransportFrame::Text(room) => room,
        TransportFrame::Binary(_) => unreachable!("room baseline must be text"),
    };
    let messages = vec![fallback_error, AUTH.into(), protocol_info, room];
    let mut config = SignalFishConfig::new("app").enable_v3();
    config.game_data_format = Some(GameDataEncoding::Rkyv);

    let async_mock = SharedMock::from_msgs(
        messages
            .iter()
            .cloned()
            .map(|message| Some(Ok(message)))
            .collect(),
    );
    let (mut async_client, mut async_events) =
        SignalFishClient::start(async_mock.clone(), config.clone());
    admit_initial_room_operation(&mut async_client, Some(InitialRoomOperation::JoinPlayer));
    for _ in 0..=messages.len() {
        tokio::time::timeout(std::time::Duration::from_secs(1), async_events.recv())
            .await
            .expect("async fallback handshake event should arrive")
            .expect("async fallback event stream should remain open");
    }
    async_mock.sent.lock().unwrap().clear();
    assert!(matches!(
        async_client.send_binary_game_data(vec![1, 2, 3]),
        Err(SignalFishError::BinaryFormatNotNegotiated)
    ));
    assert!(async_mock.sent.lock().unwrap().is_empty());
    async_client
        .send_game_data(serde_json::json!({"fallback": true}))
        .expect("JSON send should remain valid after fallback");
    wait_for_sent_len(&async_mock, 1).await;
    assert!(matches!(
        serde_json::from_str::<ClientMessage>(&async_mock.sent.lock().unwrap()[0])
            .expect("async fallback send should be a ClientMessage"),
        ClientMessage::GameData { .. }
    ));

    let polling_mock = SharedMock::from_msgs(
        messages
            .into_iter()
            .map(|message| Some(Ok(message)))
            .collect(),
    );
    let mut polling_client = SignalFishPollingClient::new(polling_mock.clone(), config);
    admit_initial_room_operation(&mut polling_client, Some(InitialRoomOperation::JoinPlayer));
    let _ = polling_client.poll();
    polling_mock.sent.lock().unwrap().clear();
    assert!(matches!(
        polling_client.send_binary_game_data(vec![1, 2, 3]),
        Err(SignalFishError::BinaryFormatNotNegotiated)
    ));
    let _ = polling_client.poll();
    assert!(polling_mock.sent.lock().unwrap().is_empty());
    polling_client
        .send_game_data(serde_json::json!({"fallback": true}))
        .expect("polling JSON send should remain valid after fallback");
    let _ = polling_client.poll();
    assert_eq!(polling_mock.sent.lock().unwrap().len(), 1);

    assert_eq!(async_client.snapshot(), polling_client.snapshot());
    assert_eq!(
        async_client.effective_game_data_format(),
        Some(GameDataEncoding::Json)
    );
    async_client.shutdown().await;
}

#[tokio::test]
async fn fallback_json_is_delivered_and_binary_is_rejected_with_driver_parity() {
    let sender = uuid::Uuid::from_u128(365);
    let mut frames = binary_accountability_prefix(sender);
    frames[1] = TransportFrame::Text(protocol_info_with_formats(vec![
        GameDataEncoding::Json,
        GameDataEncoding::MessagePack,
    ]));
    frames.push(text_server_frame(ServerMessage::GameData {
        from_player: sender,
        data: serde_json::json!({"fallback": true}),
        seq: Some(1),
        epoch: Some(1),
        class: Some(DeliveryClass::Reliable),
        key: None,
    }));
    frames.push(TransportFrame::Binary(
        rmp_serde::to_vec_named(&V3BinaryGameDataFrame {
            from_player: sender,
            encoding: GameDataEncoding::MessagePack,
            payload: vec![1, 2, 3],
            seq: 2,
            epoch: 1,
        })
        .expect("binary fallback fixture should serialize"),
    ));
    let mut config = SignalFishConfig::new("app").enable_v3();
    config.game_data_format = Some(GameDataEncoding::Rkyv);

    let events = assert_frame_trace_parity(frames, config).await;
    let kinds = events
        .iter()
        .map(|event| event.split('|').next().expect("event kind"))
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            "Authenticated",
            "ProtocolInfo",
            "RoomJoined",
            "GameData",
            "ProtocolViolation",
            "Disconnected",
        ]
    );
}

#[tokio::test]
async fn message_pack_receiver_accepts_json_origin_text_relay_with_driver_parity() {
    let sender = uuid::Uuid::from_u128(366);
    let mut frames = binary_accountability_prefix(sender);
    frames[1] = TransportFrame::Text(protocol_info_with_formats(vec![
        GameDataEncoding::Json,
        GameDataEncoding::MessagePack,
    ]));
    frames.push(text_server_frame(ServerMessage::GameData {
        from_player: sender,
        data: serde_json::json!({"json_origin": true}),
        seq: Some(1),
        epoch: Some(1),
        class: Some(DeliveryClass::Reliable),
        key: None,
    }));
    let mut config = SignalFishConfig::new("app").enable_v3();
    config.game_data_format = Some(GameDataEncoding::MessagePack);

    let events = assert_frame_trace_parity(frames, config).await;
    assert!(events.iter().any(|event| event.starts_with("GameData|")));
    assert!(!events
        .iter()
        .any(|event| event.starts_with("ProtocolViolation|")));
}

#[tokio::test]
async fn observe_advances_text_binary_representation_violation_with_driver_parity() {
    let sender = uuid::Uuid::from_u128(367);
    let mut frames = binary_accountability_prefix(sender);
    frames[1] = TransportFrame::Text(protocol_info_with_formats(vec![
        GameDataEncoding::Json,
        GameDataEncoding::MessagePack,
    ]));
    frames.push(text_server_frame(ServerMessage::GameDataBinary {
        from_player: sender,
        encoding: GameDataEncoding::MessagePack,
        payload: vec![1],
        seq: Some(1),
        epoch: Some(1),
    }));
    frames.push(TransportFrame::Binary(
        rmp_serde::to_vec_named(&V3BinaryGameDataFrame {
            from_player: sender,
            encoding: GameDataEncoding::MessagePack,
            payload: vec![2],
            seq: 2,
            epoch: 1,
        })
        .expect("binary fixture should serialize"),
    ));
    let mut config = SignalFishConfig::new("app")
        .enable_v3()
        .with_protocol_violation_policy(ProtocolViolationPolicy::Observe);
    config.game_data_format = Some(GameDataEncoding::MessagePack);

    let events = assert_frame_trace_parity(frames, config).await;
    assert_eq!(
        events
            .iter()
            .filter(|event| event.starts_with("ProtocolViolation|"))
            .count(),
        1,
        "the following seq=2 frame must not see a synthetic gap"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.starts_with("GameDataBinary|"))
            .count(),
        2
    );
}

fn binary_accountability_prefix(player_id: PlayerId) -> Vec<TransportFrame> {
    let room_joined = ServerMessage::RoomJoined(Box::new(RoomJoinedPayload {
        room_id: uuid::Uuid::from_u128(200),
        room_code: "BINARY".into(),
        player_id: uuid::Uuid::from_u128(100),
        game_name: "binary-parity".into(),
        max_players: 4,
        supports_authority: false,
        current_players: vec![
            PlayerInfo {
                id: uuid::Uuid::from_u128(100),
                name: "local".into(),
                is_authority: true,
                is_ready: false,
                connected_at: "2026-01-01T00:00:00Z".into(),
                connection_info: None,
                epoch: Some(1),
                seq: Some(0),
            },
            PlayerInfo {
                id: player_id,
                name: "sender".into(),
                is_authority: false,
                is_ready: false,
                connected_at: "2026-01-01T00:00:00Z".into(),
                connection_info: None,
                epoch: Some(1),
                seq: Some(0),
            },
            PlayerInfo {
                id: uuid::Uuid::from_u128(352),
                name: "off-plan".into(),
                is_authority: false,
                is_ready: false,
                connected_at: "2026-01-01T00:00:00Z".into(),
                connection_info: None,
                epoch: Some(1),
                seq: Some(0),
            },
        ],
        is_authority: true,
        lobby_state: LobbyState::Lobby,
        ready_players: vec![],
        relay_type: "websocket".into(),
        current_spectators: vec![],
        ice_servers: vec![],
        reconnection_token: Some("binary-parity-token".into()),
    }));
    vec![
        TransportFrame::Text(AUTH.into()),
        TransportFrame::Text(PI_V3.into()),
        TransportFrame::Text(serde_json::to_string(&room_joined).unwrap()),
    ]
}

fn finalized_v2_room_frame() -> TransportFrame {
    text_server_frame(ServerMessage::RoomJoined(Box::new(RoomJoinedPayload {
        room_id: uuid::Uuid::from_u128(210),
        room_code: "V2ROOM".into(),
        player_id: uuid::Uuid::from_u128(211),
        game_name: "v2-parity".into(),
        max_players: 4,
        supports_authority: false,
        current_players: vec![PlayerInfo {
            id: uuid::Uuid::from_u128(211),
            name: "local".into(),
            is_authority: false,
            is_ready: true,
            connected_at: "2026-01-01T00:00:00Z".into(),
            connection_info: None,
            epoch: None,
            seq: None,
        }],
        is_authority: false,
        lobby_state: LobbyState::Finalized,
        ready_players: vec![],
        relay_type: "websocket".into(),
        current_spectators: vec![],
        ice_servers: vec![],
        reconnection_token: None,
    })))
}

fn spectator_accountability_prefix(player_id: PlayerId) -> Vec<TransportFrame> {
    let spectator_joined = ServerMessage::SpectatorJoined(Box::new(SpectatorJoinedPayload {
        room_id: uuid::Uuid::from_u128(201),
        room_code: "SPECTATOR".into(),
        spectator_id: uuid::Uuid::from_u128(101),
        game_name: "spectator-parity".into(),
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
        current_spectators: vec![],
        lobby_state: LobbyState::Lobby,
        reason: None,
    }));
    vec![
        TransportFrame::Text(AUTH.into()),
        TransportFrame::Text(PI_V3.into()),
        TransportFrame::Text(
            serde_json::to_string(&spectator_joined)
                .expect("serialize spectator accountability prefix"),
        ),
    ]
}

fn text_server_frame(message: ServerMessage) -> TransportFrame {
    TransportFrame::Text(
        serde_json::to_string(&message).expect("serialize accountability parity fixture"),
    )
}

#[tokio::test]
async fn unsolicited_join_and_reconnect_responses_have_driver_policy_parity() {
    let player_joined = match &binary_accountability_prefix(uuid::Uuid::from_u128(365))[2] {
        TransportFrame::Text(message) => message.clone(),
        TransportFrame::Binary(_) => unreachable!("room baseline must be text"),
    };
    let spectator_joined = match &spectator_accountability_prefix(uuid::Uuid::from_u128(365))[2] {
        TransportFrame::Text(message) => message.clone(),
        TransportFrame::Binary(_) => unreachable!("spectator baseline must be text"),
    };
    let responses = [
        player_joined,
        serde_json::to_string(&ServerMessage::RoomJoinFailed {
            reason: "unsolicited".into(),
            error_code: Some(ErrorCode::RoomFull),
        })
        .expect("RoomJoinFailed fixture"),
        reconnected_with_missed(vec![]),
        serde_json::to_string(&ServerMessage::ReconnectionFailed {
            reason: "unsolicited".into(),
            error_code: ErrorCode::ReconnectionFailed,
        })
        .expect("ReconnectionFailed fixture"),
        spectator_joined,
        serde_json::to_string(&ServerMessage::SpectatorJoinFailed {
            reason: "unsolicited".into(),
            error_code: Some(ErrorCode::SpectatorNotAllowed),
        })
        .expect("SpectatorJoinFailed fixture"),
    ];

    for policy in [
        ProtocolViolationPolicy::Observe,
        ProtocolViolationPolicy::Quarantine,
        ProtocolViolationPolicy::Disconnect,
    ] {
        for response in &responses {
            let messages = vec![AUTH, PI_V3, response];
            let config = SignalFishConfig::new("app")
                .enable_v3()
                .with_protocol_violation_policy(policy);

            let async_mock = SharedMock::new(messages.clone());
            let (mut async_client, mut async_events) =
                SignalFishClient::start(async_mock, config.clone());
            let mut observed = Vec::new();
            loop {
                let event =
                    tokio::time::timeout(std::time::Duration::from_secs(1), async_events.recv())
                        .await
                        .expect("async unsolicited response must be processed")
                        .expect("async event stream must remain available through the violation");
                let terminal = matches!(event, SignalFishEvent::Disconnected { .. });
                let violation = matches!(event, SignalFishEvent::ProtocolViolation { .. });
                if !matches!(event, SignalFishEvent::Connected) {
                    observed.push(canonical_event(&event));
                }
                if violation && policy != ProtocolViolationPolicy::Disconnect || terminal {
                    break;
                }
            }

            let polling_mock = SharedMock::new(messages);
            let mut polling_client = SignalFishPollingClient::new(polling_mock, config);
            let polling = polling_client
                .poll()
                .iter()
                .filter(|event| !matches!(event, SignalFishEvent::Connected))
                .map(canonical_event)
                .collect::<Vec<_>>();

            assert_eq!(observed, polling, "policy {policy:?}, response {response}");
            assert_eq!(async_client.room_role(), None);
            assert_eq!(polling_client.room_role(), None);
            assert_eq!(async_client.snapshot().room_id, None);
            assert_eq!(polling_client.snapshot().room_id, None);
            async_client.shutdown().await;
            polling_client.close();
        }
    }
}

async fn async_unsolicited_exit(
    policy: ProtocolViolationPolicy,
    operation: InitialRoomOperation,
    baseline: String,
    response: String,
) -> (Vec<String>, Option<RoomRole>) {
    let mock = SharedMock::new(vec![AUTH, PI_V3]);
    let controls = mock.clone();
    let config = SignalFishConfig::new("app")
        .enable_v3()
        .with_protocol_violation_policy(policy);
    let (mut client, mut events) = SignalFishClient::start(mock, config);
    loop {
        if matches!(events.recv().await, Some(SignalFishEvent::ProtocolInfo(_))) {
            break;
        }
    }

    controls
        .incoming
        .lock()
        .unwrap()
        .push_back(Some(Ok(baseline)));
    admit_initial_room_operation(&mut client, Some(operation));
    loop {
        let event = events
            .recv()
            .await
            .expect("async setup must establish membership");
        if matches!(
            (operation, event),
            (
                InitialRoomOperation::JoinPlayer,
                SignalFishEvent::RoomJoined { .. }
            ) | (
                InitialRoomOperation::JoinSpectator,
                SignalFishEvent::SpectatorJoined { .. }
            )
        ) {
            break;
        }
    }

    controls
        .incoming
        .lock()
        .unwrap()
        .push_back(Some(Ok(response)));
    client.ping().expect("ping wakes the scripted transport");
    let mut observed = Vec::new();
    loop {
        let event = events
            .recv()
            .await
            .expect("async unsolicited exit must reach a terminal policy event");
        let violation = matches!(event, SignalFishEvent::ProtocolViolation { .. });
        let disconnected = matches!(event, SignalFishEvent::Disconnected { .. });
        observed.push(canonical_event(&event));
        if violation && policy != ProtocolViolationPolicy::Disconnect || disconnected {
            break;
        }
    }
    let role = client.room_role();
    client.shutdown().await;
    (observed, role)
}

fn polling_unsolicited_exit(
    policy: ProtocolViolationPolicy,
    operation: InitialRoomOperation,
    baseline: String,
    response: String,
) -> (Vec<String>, Option<RoomRole>) {
    let mock = SharedMock::new(vec![AUTH, PI_V3]);
    let controls = mock.clone();
    let config = SignalFishConfig::new("app")
        .enable_v3()
        .with_protocol_violation_policy(policy);
    let mut client = SignalFishPollingClient::new(mock, config);
    let _ = client.poll();

    controls
        .incoming
        .lock()
        .unwrap()
        .push_back(Some(Ok(baseline)));
    admit_initial_room_operation(&mut client, Some(operation));
    let setup = client.poll();
    assert!(setup.iter().any(|event| matches!(
        (operation, event),
        (
            InitialRoomOperation::JoinPlayer,
            SignalFishEvent::RoomJoined { .. }
        ) | (
            InitialRoomOperation::JoinSpectator,
            SignalFishEvent::SpectatorJoined { .. }
        )
    )));

    controls
        .incoming
        .lock()
        .unwrap()
        .push_back(Some(Ok(response)));
    client.ping().expect("ping wakes the scripted transport");
    let observed = client.poll().iter().map(canonical_event).collect();
    let role = client.room_role();
    client.close();
    (observed, role)
}

#[tokio::test]
async fn unsolicited_room_exits_reach_correlation_in_both_drivers() {
    let player_baseline = match &binary_accountability_prefix(uuid::Uuid::from_u128(365))[2] {
        TransportFrame::Text(message) => message.clone(),
        TransportFrame::Binary(_) => unreachable!("room baseline must be text"),
    };
    let spectator_baseline = match &spectator_accountability_prefix(uuid::Uuid::from_u128(365))[2] {
        TransportFrame::Text(message) => message.clone(),
        TransportFrame::Binary(_) => unreachable!("spectator baseline must be text"),
    };
    let cases = [
        (
            InitialRoomOperation::JoinPlayer,
            player_baseline,
            serde_json::to_string(&ServerMessage::RoomLeft).expect("RoomLeft fixture"),
            RoomRole::Player,
        ),
        (
            InitialRoomOperation::JoinSpectator,
            spectator_baseline,
            serde_json::to_string(&ServerMessage::SpectatorLeft {
                room_id: Some(uuid::Uuid::from_u128(201)),
                room_code: Some("SPECTATOR".into()),
                reason: None,
                current_spectators: vec![],
            })
            .expect("SpectatorLeft fixture"),
            RoomRole::Spectator,
        ),
    ];

    for policy in [
        ProtocolViolationPolicy::Observe,
        ProtocolViolationPolicy::Quarantine,
        ProtocolViolationPolicy::Disconnect,
    ] {
        for (operation, baseline, response, retained_role) in &cases {
            let (async_events, async_role) =
                async_unsolicited_exit(policy, *operation, baseline.clone(), response.clone())
                    .await;
            let (polling_events, polling_role) =
                polling_unsolicited_exit(policy, *operation, baseline.clone(), response.clone());
            assert_eq!(async_events, polling_events, "{policy:?}, {response}");
            if policy != ProtocolViolationPolicy::Disconnect {
                assert_eq!(async_role, Some(*retained_role));
                assert_eq!(polling_role, Some(*retained_role));
            }
        }
    }
}

#[tokio::test]
async fn delayed_duplicate_exit_does_not_consume_rejoin_fence_in_either_driver() {
    let mut frames = binary_accountability_prefix(uuid::Uuid::from_u128(365));
    let rejoined = frames[2].clone();
    frames.extend([
        text_server_frame(ServerMessage::RoomLeft),
        text_server_frame(ServerMessage::RoomLeft),
        rejoined,
    ]);

    for policy in [
        ProtocolViolationPolicy::Observe,
        ProtocolViolationPolicy::Quarantine,
    ] {
        let events = assert_frame_trace_parity(
            frames.clone(),
            SignalFishConfig::new("app")
                .enable_v3()
                .with_protocol_violation_policy(policy),
        )
        .await;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.starts_with("RoomJoined|"))
                .count(),
            2,
            "{policy:?} must preserve the live rejoin fence after the delayed exit: {events:?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.as_str() == "RoomLeft")
                .count(),
            1,
            "the duplicate exit must be rejected under {policy:?}: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|event| event.starts_with("ProtocolViolation|Lifecycle|")),
            "the duplicate exit must reach lifecycle correlation under {policy:?}: {events:?}"
        );
    }
}

#[tokio::test]
async fn authoritative_room_closed_spectator_exit_has_driver_parity() {
    let mut frames = spectator_accountability_prefix(uuid::Uuid::from_u128(365));
    frames.push(text_server_frame(ServerMessage::SpectatorLeft {
        room_id: Some(uuid::Uuid::from_u128(201)),
        room_code: Some("SPECTATOR".into()),
        reason: Some(signal_fish_client::protocol::SpectatorStateChangeReason::RoomClosed),
        current_spectators: vec![],
    }));
    for policy in [
        ProtocolViolationPolicy::Observe,
        ProtocolViolationPolicy::Quarantine,
        ProtocolViolationPolicy::Disconnect,
    ] {
        let events = assert_frame_trace_parity(
            frames.clone(),
            SignalFishConfig::new("app")
                .enable_v3()
                .with_protocol_violation_policy(policy),
        )
        .await;
        assert!(events
            .iter()
            .any(|event| event.starts_with("SpectatorLeft")));
        assert!(events
            .iter()
            .all(|event| !event.starts_with("ProtocolViolation")));
    }
}

#[tokio::test]
async fn lifecycle_plan_and_signal_matrix_has_complete_driver_parity() {
    use signal_fish_client::protocol::{DirectEndpoint, SessionPeer, SessionPlanPayload, Topology};

    for policy in [
        ProtocolViolationPolicy::Quarantine,
        ProtocolViolationPolicy::Disconnect,
        ProtocolViolationPolicy::Observe,
    ] {
        let events = assert_frame_trace_parity(
            vec![text_server_frame(ServerMessage::Pong)],
            SignalFishConfig::new("app").with_protocol_violation_policy(policy),
        )
        .await;
        assert!(events.iter().any(|event| event.starts_with("Pong")));
        assert!(
            events
                .iter()
                .all(|event| !event.starts_with("ProtocolViolation")),
            "pre-auth Pong under {policy:?}: {events:?}"
        );
    }

    let peer = uuid::Uuid::from_u128(350);
    let generation = uuid::Uuid::from_u128(351);
    let plan = ServerMessage::SessionPlan(Box::new(SessionPlanPayload {
        generation: Some(generation),
        topology: Topology::Mesh,
        transport: TransportKind::WebRtc,
        host: None,
        direct_endpoint: None,
        peers: vec![SessionPeer {
            player_id: peer,
            player_name: "peer".into(),
            is_authority: false,
            initiate: false,
        }],
        ice_servers: vec![],
        fallback: TransportKind::Relay,
    }));

    let mut room_prefix = binary_accountability_prefix(peer);
    room_prefix.push(text_server_frame(ServerMessage::LobbyStateChanged {
        lobby_state: LobbyState::Finalized,
        ready_players: vec![],
        all_ready: true,
    }));

    let valid_pairs = [
        (Topology::Relay, TransportKind::Relay),
        (Topology::Host, TransportKind::Direct),
        (Topology::Host, TransportKind::WebRtc),
        (Topology::Mesh, TransportKind::WebRtc),
    ];
    for topology in [Topology::Relay, Topology::Host, Topology::Mesh] {
        for transport in [
            TransportKind::Relay,
            TransportKind::Direct,
            TransportKind::WebRtc,
        ] {
            let pair_plan = ServerMessage::SessionPlan(Box::new(SessionPlanPayload {
                generation: Some(generation),
                topology,
                transport,
                host: (topology == Topology::Host).then_some(peer),
                direct_endpoint: (transport == TransportKind::Direct).then_some(DirectEndpoint {
                    host: "192.0.2.10".into(),
                    port: 7_777,
                }),
                peers: if topology == Topology::Relay {
                    vec![]
                } else {
                    vec![SessionPeer {
                        player_id: peer,
                        player_name: "peer".into(),
                        is_authority: false,
                        initiate: false,
                    }]
                },
                ice_servers: vec![],
                fallback: TransportKind::Relay,
            }));
            let events = assert_frame_trace_parity(
                room_prefix
                    .iter()
                    .cloned()
                    .chain(std::iter::once(text_server_frame(pair_plan)))
                    .collect(),
                SignalFishConfig::new("app")
                    .enable_v3()
                    .with_protocol_violation_policy(ProtocolViolationPolicy::Observe),
            )
            .await;
            let accepted = valid_pairs.contains(&(topology, transport));
            assert_eq!(
                events.iter().any(|event| event.starts_with("SessionPlan")),
                accepted,
                "{topology:?}+{transport:?}: {events:?}"
            );
            assert_eq!(
                events
                    .iter()
                    .any(|event| event.starts_with("ProtocolViolation|Lifecycle|")),
                !accepted,
                "{topology:?}+{transport:?}: {events:?}"
            );
        }
    }

    let mut noncanonical_plan = match plan.clone() {
        ServerMessage::SessionPlan(plan) => *plan,
        _ => unreachable!("plan fixture is a SessionPlan"),
    };
    noncanonical_plan.topology = Topology::Relay;
    noncanonical_plan.transport = TransportKind::Relay;

    let mut replacement_plan = match plan.clone() {
        ServerMessage::SessionPlan(plan) => *plan,
        _ => unreachable!("plan fixture is a SessionPlan"),
    };
    replacement_plan.generation = Some(uuid::Uuid::from_u128(353));
    replacement_plan.peers.clear();

    let invalid_cases = [
        (
            "pre-auth",
            vec![text_server_frame(ServerMessage::PlayerJoined {
                player: PlayerInfo {
                    id: peer,
                    name: "peer".into(),
                    is_authority: false,
                    is_ready: false,
                    connected_at: "2026-01-01T00:00:00Z".into(),
                    connection_info: None,
                    epoch: Some(1),
                    seq: Some(0),
                },
            })],
            "PlayerJoined",
        ),
        (
            "authenticated-pre-room",
            vec![
                TransportFrame::Text(AUTH.into()),
                TransportFrame::Text(PI_V3.into()),
                text_server_frame(ServerMessage::PlayerLeft {
                    player_id: peer,
                    epoch: Some(1),
                    final_seq: Some(0),
                }),
            ],
            "PlayerLeft",
        ),
        (
            "delivery-report-pre-room",
            vec![
                TransportFrame::Text(AUTH.into()),
                TransportFrame::Text(PI_V3.into()),
                mixed_coalesced_unsupported_report(peer),
            ],
            "DeliveryReport",
        ),
        (
            "post-room",
            room_prefix
                .iter()
                .cloned()
                .chain(std::iter::once(text_server_frame(ServerMessage::RoomLeft)))
                .chain(std::iter::once(text_server_frame(plan.clone())))
                .collect(),
            "SessionPlan",
        ),
        (
            "v3-message-under-v2",
            vec![
                TransportFrame::Text(AUTH.into()),
                TransportFrame::Text(PI_V2.into()),
                finalized_v2_room_frame(),
                text_server_frame(plan.clone()),
            ],
            "SessionPlan",
        ),
        (
            "delivery-report-post-room",
            room_prefix
                .iter()
                .cloned()
                .chain(std::iter::once(text_server_frame(ServerMessage::RoomLeft)))
                .chain(std::iter::once(mixed_coalesced_unsupported_report(peer)))
                .collect(),
            "DeliveryReport",
        ),
        (
            "noncanonical-plan",
            room_prefix
                .iter()
                .cloned()
                .chain(std::iter::once(text_server_frame(
                    ServerMessage::SessionPlan(Box::new(noncanonical_plan)),
                )))
                .collect(),
            "SessionPlan",
        ),
        (
            "self-signal",
            room_prefix
                .iter()
                .cloned()
                .chain([
                    text_server_frame(plan.clone()),
                    text_server_frame(ServerMessage::Signal {
                        from: uuid::Uuid::from_u128(100),
                        generation: Some(generation),
                        signal: serde_json::json!({"Offer": "self"}),
                    }),
                ])
                .collect(),
            "SignalReceived",
        ),
        (
            "same-room-off-plan-signal",
            room_prefix
                .iter()
                .cloned()
                .chain([
                    text_server_frame(plan.clone()),
                    text_server_frame(ServerMessage::Signal {
                        from: uuid::Uuid::from_u128(352),
                        generation: Some(generation),
                        signal: serde_json::json!({"Offer": "off-plan"}),
                    }),
                ])
                .collect(),
            "SignalReceived",
        ),
        (
            "removed-by-replan-signal",
            room_prefix
                .iter()
                .cloned()
                .chain([
                    text_server_frame(plan.clone()),
                    text_server_frame(ServerMessage::SessionPlan(Box::new(replacement_plan))),
                    text_server_frame(ServerMessage::Signal {
                        from: peer,
                        generation: Some(uuid::Uuid::from_u128(353)),
                        signal: serde_json::json!({"Offer": "removed"}),
                    }),
                ])
                .collect(),
            "SignalReceived",
        ),
    ];

    for policy in [
        ProtocolViolationPolicy::Quarantine,
        ProtocolViolationPolicy::Disconnect,
        ProtocolViolationPolicy::Observe,
    ] {
        for (name, frames, suppressed_event) in &invalid_cases {
            let events = assert_frame_trace_parity(
                frames.clone(),
                SignalFishConfig::new("app")
                    .enable_v3()
                    .with_protocol_violation_policy(policy),
            )
            .await;
            assert!(
                events
                    .iter()
                    .any(|event| event.starts_with("ProtocolViolation|Lifecycle|")),
                "{name} under {policy:?}: {events:?}"
            );
            assert!(
                events
                    .iter()
                    .all(|event| !event.starts_with(suppressed_event)),
                "{name} delivered {suppressed_event} under {policy:?}: {events:?}"
            );
        }
    }

    let mut generationless_webrtc = match plan.clone() {
        ServerMessage::SessionPlan(plan) => *plan,
        _ => unreachable!("plan fixture is a SessionPlan"),
    };
    generationless_webrtc.generation = None;
    let mut generationless_relay = generationless_webrtc.clone();
    generationless_relay.topology = Topology::Relay;
    generationless_relay.transport = TransportKind::Relay;
    generationless_relay.peers.clear();
    generationless_relay.ice_servers.clear();
    for policy in [
        ProtocolViolationPolicy::Quarantine,
        ProtocolViolationPolicy::Disconnect,
        ProtocolViolationPolicy::Observe,
    ] {
        let events = assert_frame_trace_parity(
            room_prefix
                .iter()
                .cloned()
                .chain([
                    text_server_frame(ServerMessage::SessionPlan(Box::new(
                        generationless_webrtc.clone(),
                    ))),
                    text_server_frame(ServerMessage::SessionPlan(Box::new(
                        generationless_relay.clone(),
                    ))),
                    text_server_frame(ServerMessage::Signal {
                        from: peer,
                        generation: None,
                        signal: serde_json::json!({"Offer": "late"}),
                    }),
                ])
                .collect(),
            SignalFishConfig::new("app")
                .enable_v3()
                .with_protocol_violation_policy(policy),
        )
        .await;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.starts_with("SessionPlan"))
                .count(),
            2,
            "generation-less replan under {policy:?}: {events:?}"
        );
        assert!(
            events.iter().all(|event| {
                !event.starts_with("SignalReceived") && !event.starts_with("ProtocolViolation")
            }),
            "generation-less late signal under {policy:?}: {events:?}"
        );
    }

    let mut valid = room_prefix;
    valid.extend([
        text_server_frame(plan),
        text_server_frame(ServerMessage::Signal {
            from: peer,
            generation: Some(generation),
            signal: serde_json::json!({"Offer": "sdp"}),
        }),
        text_server_frame(ServerMessage::RoomLeft),
        text_server_frame(ServerMessage::RelayStats {
            interval_ms: 1_000,
            sent_to_you: 0,
            dropped_for_you: 0,
            backpressure_events: 0,
        }),
        text_server_frame(ServerMessage::GoingAway {
            deadline_ms: 5_000,
            retry_after_secs: Some(1),
        }),
    ]);
    let events = assert_frame_trace_parity(valid, SignalFishConfig::new("app").enable_v3()).await;
    for expected in ["SessionPlan", "SignalReceived", "RelayStats", "GoingAway"] {
        assert!(
            events.iter().any(|event| event.starts_with(expected)),
            "missing {expected}: {events:?}"
        );
    }
}

#[tokio::test]
async fn superseded_session_plan_replay_has_complete_driver_policy_parity() {
    use signal_fish_client::protocol::{SessionPeer, SessionPlanPayload, Topology};

    let local = uuid::Uuid::from_u128(100);
    let peer_a = uuid::Uuid::from_u128(352);
    let peer_b = uuid::Uuid::from_u128(350);
    let generation_a = uuid::Uuid::from_u128(351);
    let generation_b = uuid::Uuid::from_u128(353);
    let plan_a = SessionPlanPayload {
        generation: Some(generation_a),
        topology: Topology::Mesh,
        transport: TransportKind::WebRtc,
        host: None,
        direct_endpoint: None,
        peers: vec![SessionPeer {
            player_id: peer_a,
            player_name: "old-peer".into(),
            is_authority: false,
            initiate: false,
        }],
        ice_servers: vec![],
        fallback: TransportKind::Relay,
    };
    let plan_b = SessionPlanPayload {
        generation: Some(generation_b),
        topology: Topology::Host,
        transport: TransportKind::WebRtc,
        host: Some(local),
        direct_endpoint: None,
        peers: vec![SessionPeer {
            player_id: peer_b,
            player_name: "current-peer".into(),
            is_authority: false,
            initiate: true,
        }],
        ice_servers: vec![],
        fallback: TransportKind::Relay,
    };

    for policy in [
        ProtocolViolationPolicy::Observe,
        ProtocolViolationPolicy::Quarantine,
        ProtocolViolationPolicy::Disconnect,
    ] {
        let mut frames = binary_accountability_prefix(peer_b);
        frames.push(text_server_frame(ServerMessage::LobbyStateChanged {
            lobby_state: LobbyState::Finalized,
            ready_players: vec![local, peer_a, peer_b],
            all_ready: true,
        }));
        frames.extend([
            text_server_frame(ServerMessage::SessionPlan(Box::new(plan_a.clone()))),
            text_server_frame(ServerMessage::SessionPlan(Box::new(plan_b.clone()))),
            text_server_frame(ServerMessage::SessionPlan(Box::new(plan_a.clone()))),
            text_server_frame(ServerMessage::Signal {
                from: peer_a,
                generation: Some(generation_a),
                signal: serde_json::json!({"Offer": "stale"}),
            }),
            text_server_frame(ServerMessage::Signal {
                from: peer_b,
                generation: Some(generation_b),
                signal: serde_json::json!({"Offer": "current"}),
            }),
        ]);

        let events = assert_frame_trace_parity(
            frames,
            SignalFishConfig::new("app")
                .enable_v3()
                .with_protocol_violation_policy(policy),
        )
        .await;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.starts_with("SessionPlan|"))
                .count(),
            2,
            "replayed A must not replace B under {policy:?}: {events:?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.starts_with("ProtocolViolation|Lifecycle|"))
                .count(),
            1,
            "replayed A must emit exactly one lifecycle violation under {policy:?}: {events:?}"
        );
        let signals = events
            .iter()
            .filter(|event| event.starts_with("SignalReceived|"))
            .collect::<Vec<_>>();
        if policy == ProtocolViolationPolicy::Disconnect {
            assert!(
                signals.is_empty(),
                "disconnect must terminate before signals"
            );
            assert!(events
                .iter()
                .any(|event| event.starts_with("Disconnected|")));
        } else {
            assert_eq!(signals.len(), 1, "{policy:?}: {events:?}");
            assert!(
                signals[0].contains(&generation_b.to_string()) && signals[0].contains("current"),
                "only B's current signal may survive under {policy:?}: {events:?}"
            );
        }
    }
}

fn mixed_coalesced_unsupported_report(sender: PlayerId) -> TransportFrame {
    text_server_frame(ServerMessage::DeliveryReport(Box::new(
        DeliveryReportPayload {
            per_class: DeliveryCountersByClass {
                reliable: ReliableDeliveryCounters {
                    unsupported_format: 2,
                    ..ReliableDeliveryCounters::default()
                },
                latest: LatestDeliveryCounters {
                    superseded: 1,
                    ..LatestDeliveryCounters::default()
                },
                ..DeliveryCountersByClass::default()
            },
            gaps: vec![
                DeliveryGap {
                    from_player: sender,
                    epoch: 1,
                    from_seq: 1,
                    to_seq: 1,
                    reason: DeliveryGapReason::LatestSuperseded,
                },
                DeliveryGap {
                    from_player: sender,
                    epoch: 1,
                    from_seq: 2,
                    to_seq: 3,
                    reason: DeliveryGapReason::UnsupportedFormat,
                },
            ],
        },
    )))
}

fn unsupported_format_advisory() -> TransportFrame {
    text_server_frame(ServerMessage::Error {
        message: "unsupported payload format".into(),
        error_code: Some(ErrorCode::UnsupportedGameDataFormat),
    })
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
    let _ = assert_frame_trace_parity(
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
        let _ = assert_frame_trace_parity(frames, config).await;
    }
}

#[tokio::test]
async fn suppressed_receipts_and_lifetime_stats_have_exact_driver_parity() {
    let sender = uuid::Uuid::from_u128(98);
    let game_data = |seq, epoch| {
        text_server_frame(ServerMessage::GameData {
            from_player: sender,
            data: serde_json::json!({"seq": seq, "epoch": epoch}),
            seq: Some(seq),
            epoch: Some(epoch),
            class: Some(DeliveryClass::Reliable),
            key: None,
        })
    };
    let mut frames = binary_accountability_prefix(sender);
    frames.extend([
        game_data(1, 1),
        text_server_frame(ServerMessage::PlayerLeft {
            player_id: sender,
            epoch: Some(1),
            final_seq: Some(2),
        }),
        text_server_frame(ServerMessage::PlayerReconnected {
            player_id: sender,
            epoch: Some(2),
        }),
        game_data(2, 1),
        game_data(3, 2),
        game_data(1, 2),
    ]);

    let events = assert_frame_trace_parity_with_stats(
        frames,
        SignalFishConfig::new("app")
            .enable_v3()
            .with_protocol_violation_policy(ProtocolViolationPolicy::Quarantine),
        false,
        Some(ClientStats {
            game_data_received: 4,
            ..ClientStats::default()
        }),
    )
    .await;

    assert_eq!(
        events
            .iter()
            .filter(|event| event.starts_with("GameData|"))
            .count(),
        1,
        "one applied receipt plus three stale/quarantined receipts"
    );
}

#[tokio::test]
async fn mixed_coalesced_unsupported_advisory_trace_has_complete_policy_parity() {
    let sender = uuid::Uuid::from_u128(302);

    let mut frames = binary_accountability_prefix(sender);
    frames.extend([
        mixed_coalesced_unsupported_report(sender),
        text_server_frame(ServerMessage::GameData {
            from_player: sender,
            data: serde_json::json!({"seq": 4}),
            seq: Some(4),
            epoch: Some(1),
            class: Some(DeliveryClass::Reliable),
            key: None,
        }),
        unsupported_format_advisory(),
    ]);

    for policy in [
        ProtocolViolationPolicy::Quarantine,
        ProtocolViolationPolicy::Disconnect,
        ProtocolViolationPolicy::Observe,
    ] {
        let events = assert_frame_trace_parity(
            frames.clone(),
            SignalFishConfig::new("app")
                .enable_v3()
                .with_protocol_violation_policy(policy),
        )
        .await;
        let event_kinds = events
            .iter()
            .map(|event| {
                event
                    .split('|')
                    .next()
                    .expect("event projection has a kind")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            event_kinds,
            [
                "Authenticated",
                "ProtocolInfo",
                "RoomJoined",
                "DeliveryReport",
                "GameData",
                "Error",
                "Disconnected",
            ],
            "{policy:?} must accept and deliver the complete delayed-advisory trace"
        );
    }
}

#[tokio::test]
async fn room_exit_clears_unsupported_advisory_authorization_in_both_drivers() {
    let sender = uuid::Uuid::from_u128(303);
    let cases = [
        (
            "RoomJoined",
            "RoomLeft",
            binary_accountability_prefix(sender),
            text_server_frame(ServerMessage::RoomLeft),
        ),
        (
            "SpectatorJoined",
            "SpectatorLeft",
            spectator_accountability_prefix(sender),
            text_server_frame(ServerMessage::SpectatorLeft {
                room_id: None,
                room_code: None,
                reason: None,
                current_spectators: vec![],
            }),
        ),
    ];

    for (joined_kind, left_kind, prefix, leave_frame) in cases {
        let mut frames = prefix;
        frames.extend([
            mixed_coalesced_unsupported_report(sender),
            leave_frame,
            unsupported_format_advisory(),
        ]);
        for policy in [
            ProtocolViolationPolicy::Quarantine,
            ProtocolViolationPolicy::Disconnect,
            ProtocolViolationPolicy::Observe,
        ] {
            let events = assert_frame_trace_parity(
                frames.clone(),
                SignalFishConfig::new("app")
                    .enable_v3()
                    .with_protocol_violation_policy(policy),
            )
            .await;
            assert!(
                events.iter().any(|event| {
                    event.starts_with("ProtocolViolation|Causality|")
                        && event.contains("lacked a prior causal DeliveryReport")
                }),
                "{left_kind} under {policy:?} must revoke old-room advisory authorization: {events:?}"
            );

            let mut expected_kinds = vec![
                "Authenticated",
                "ProtocolInfo",
                joined_kind,
                "DeliveryReport",
                left_kind,
                "ProtocolViolation",
            ];
            if policy == ProtocolViolationPolicy::Observe {
                expected_kinds.push("Error");
            }
            expected_kinds.push("Disconnected");
            let event_kinds = events
                .iter()
                .map(|event| {
                    event
                        .split('|')
                        .next()
                        .expect("event projection has a kind")
                })
                .collect::<Vec<_>>();
            assert_eq!(event_kinds, expected_kinds, "{left_kind} under {policy:?}");
        }
    }
}

#[tokio::test]
async fn every_common_command_produces_identical_physical_frames() {
    let cases = [
        CommonCommandCase::LeaveRoom,
        CommonCommandCase::ReliableData,
        CommonCommandCase::LatestData,
        CommonCommandCase::VolatileData,
        CommonCommandCase::BinaryData,
        CommonCommandCase::SetReady,
        CommonCommandCase::StartGame,
        CommonCommandCase::RequestAuthority,
        CommonCommandCase::ProvideConnectionInfo,
        CommonCommandCase::Ping,
        CommonCommandCase::Signal,
        CommonCommandCase::SignalForGeneration,
        CommonCommandCase::Offer,
        CommonCommandCase::Answer,
        CommonCommandCase::IceCandidate,
        CommonCommandCase::RawSignal,
        CommonCommandCase::RawSignalForGeneration,
        CommonCommandCase::TransportStatus,
    ];

    for case in cases {
        let mut config = SignalFishConfig::new("app").enable_v3();
        config.game_data_format = Some(GameDataEncoding::MessagePack);

        let async_mock = FrameMock::v3();
        let async_sent = Arc::clone(&async_mock.sent);
        let (mut async_client, mut async_events) =
            SignalFishClient::start(async_mock, config.clone());
        admit_initial_room_operation(&mut async_client, Some(InitialRoomOperation::JoinPlayer));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !matches!(
                async_events.recv().await,
                Some(SignalFishEvent::SessionPlan { .. })
            ) {}
        })
        .await
        .unwrap_or_else(|_| panic!("{case:?}: async SessionPlan timed out"));
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
        admit_initial_room_operation(&mut polling_client, Some(InitialRoomOperation::JoinPlayer));
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
async fn unauthorized_outbound_signals_fail_without_wire_output_in_both_drivers() {
    let unauthorized = [
        uuid::Uuid::from_u128(9),
        uuid::Uuid::from_u128(6),
        uuid::Uuid::from_u128(99),
    ];
    let generation = Some(uuid::Uuid::from_u128(12));
    let config = SignalFishConfig::new("app").enable_mesh();

    let async_mock = FrameMock::v3();
    let async_sent = Arc::clone(&async_mock.sent);
    let (mut async_client, mut async_events) = SignalFishClient::start(async_mock, config.clone());
    admit_initial_room_operation(&mut async_client, Some(InitialRoomOperation::JoinPlayer));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !matches!(
            async_events.recv().await,
            Some(SignalFishEvent::SessionPlan { .. })
        ) {}
    })
    .await
    .expect("async SessionPlan");
    async_sent.lock().unwrap().clear();
    for target in unauthorized {
        for result in [
            async_client.send_signal(target, PeerSignal::Offer("sdp".into())),
            async_client.send_signal_for_generation(
                target,
                generation,
                PeerSignal::Answer("sdp".into()),
            ),
            async_client.send_raw_signal(target, serde_json::json!({"Custom": true})),
            async_client.send_raw_signal_for_generation(
                target,
                generation,
                serde_json::json!({"Bound": true}),
            ),
        ] {
            assert!(matches!(
                result,
                Err(SignalFishError::SessionPlanUnavailable)
            ));
        }
    }
    assert!(async_sent.lock().unwrap().is_empty());

    let polling_mock = FrameMock::v3();
    let polling_sent = Arc::clone(&polling_mock.sent);
    let mut polling_client = SignalFishPollingClient::new(polling_mock, config);
    admit_initial_room_operation(&mut polling_client, Some(InitialRoomOperation::JoinPlayer));
    let _ = polling_client.poll();
    polling_sent.lock().unwrap().clear();
    for target in unauthorized {
        for result in [
            polling_client.send_signal(target, PeerSignal::Offer("sdp".into())),
            polling_client.send_signal_for_generation(
                target,
                generation,
                PeerSignal::Answer("sdp".into()),
            ),
            polling_client.send_raw_signal(target, serde_json::json!({"Custom": true})),
            polling_client.send_raw_signal_for_generation(
                target,
                generation,
                serde_json::json!({"Bound": true}),
            ),
        ] {
            assert!(matches!(
                result,
                Err(SignalFishError::SessionPlanUnavailable)
            ));
        }
    }
    let _ = polling_client.poll();
    assert!(polling_sent.lock().unwrap().is_empty());
    async_client.shutdown().await;
}

#[tokio::test]
async fn pending_transport_queue_capacity_and_errors_match() {
    let config = SignalFishConfig::new("app").with_command_channel_capacity(1);

    let async_mock = NeverSendMock::new();
    let async_gate = async_mock.clone();
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
    let polling_gate = polling_mock.clone();
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

    for case in [
        CommonCommandCase::JoinRoom,
        CommonCommandCase::Reconnect,
        CommonCommandCase::JoinSpectator,
    ] {
        assert!(matches!(
            case.invoke(&mut async_client),
            Err(SignalFishError::SendBufferFull { capacity: 1 })
        ));
        assert!(matches!(
            case.invoke(&mut polling_client),
            Err(SignalFishError::SendBufferFull { capacity: 1 })
        ));
    }

    // A locally invalid room operation wins over queue congestion and cannot
    // consume additional capacity in either driver.
    assert!(matches!(
        async_client.send_game_data(serde_json::json!({"invalid": true})),
        Err(SignalFishError::NotInRoom)
    ));
    assert!(matches!(
        polling_client.send_game_data(serde_json::json!({"invalid": true})),
        Err(SignalFishError::NotInRoom)
    ));
    assert_eq!(async_client.send_capacity(), 0);
    assert_eq!(polling_client.send_capacity(), 0);

    async_gate.release();
    polling_gate.release();
    let _ = polling_client.poll();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while async_client.send_capacity() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("async queue should recover after transport release");
    assert_eq!(polling_client.send_capacity(), 1);
    async_client
        .join_room(JoinRoomParams::new("game", "async"))
        .expect("full-queue refusal must not create an async pending fence");
    polling_client
        .join_room(JoinRoomParams::new("game", "polling"))
        .expect("full-queue refusal must not create a polling pending fence");
    assert!(matches!(
        async_client.send_game_data(serde_json::json!({"after": "join"})),
        Err(SignalFishError::RoomOperationPending)
    ));
    assert!(matches!(
        polling_client.send_game_data(serde_json::json!({"after": "join"})),
        Err(SignalFishError::RoomOperationPending)
    ));
    async_client.shutdown().await;
}

fn room_operation_id(json: &str) -> Option<uuid::Uuid> {
    match serde_json::from_str::<ClientMessage>(json).ok()? {
        ClientMessage::RoomOperation { operation_id, .. } => Some(operation_id),
        _ => None,
    }
}

fn correlated_join_failure(operation_id: uuid::Uuid, reason: &str) -> ServerMessage {
    ServerMessage::RoomOperationResult {
        operation_id,
        result: Box::new(RoomOperationResult::RoomJoinFailed {
            reason: reason.into(),
            error_code: Some(ErrorCode::RoomFull),
        }),
    }
}

#[tokio::test]
async fn stale_same_kind_result_while_current_send_is_blocked_has_driver_parity() {
    let config = SignalFishConfig::new("app")
        .enable_v3()
        .with_protocol_violation_policy(ProtocolViolationPolicy::Observe);

    let async_mock = CorrelationRaceMock::new();
    let async_control = async_mock.clone();
    let (mut async_client, mut async_events) = SignalFishClient::start(async_mock, config.clone());
    for expected in ["Connected", "Authenticated", "ProtocolInfo"] {
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), async_events.recv())
            .await
            .expect("async negotiation event timeout")
            .expect("async negotiation event");
        assert_eq!(format!("{event:?}"), expected);
    }

    async_client
        .join_room(JoinRoomParams::new("game", "async-a"))
        .expect("async A admitted");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while async_control
            .sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(|json| room_operation_id(json))
            .count()
            < 1
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("async A send timeout");
    let async_a = async_control
        .sent
        .lock()
        .unwrap()
        .iter()
        .find_map(|json| room_operation_id(json))
        .expect("async A id");
    async_control.push(correlated_join_failure(async_a, "A complete"));
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(1), async_events.recv())
            .await
            .expect("async A response timeout"),
        Some(SignalFishEvent::RoomJoinFailed { .. })
    ));

    async_control.block_room_sends();
    async_client
        .join_room(JoinRoomParams::new("game", "async-b"))
        .expect("async B admitted");
    let async_b = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if let Some(id) = async_control
                .attempted_room_send
                .lock()
                .unwrap()
                .as_deref()
                .and_then(room_operation_id)
                .filter(|id| *id != async_a)
            {
                break id;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("async B retained-send timeout");
    assert_eq!(
        async_control
            .sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(|json| room_operation_id(json))
            .count(),
        1,
        "B must remain client-owned while the transport is blocked"
    );
    async_control.push(correlated_join_failure(async_a, "A stale"));
    let async_stale = tokio::time::timeout(std::time::Duration::from_secs(1), async_events.recv())
        .await
        .expect("async stale response timeout")
        .expect("async stale response event");
    assert!(matches!(
        async_stale,
        SignalFishEvent::ProtocolViolation { .. }
    ));
    assert!(matches!(
        async_client.join_room(JoinRoomParams::new("game", "blocked")),
        Err(SignalFishError::RoomOperationPending)
    ));
    async_control.release_room_sends();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while async_control
            .sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(|json| room_operation_id(json))
            .count()
            < 2
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("async B send timeout");
    async_control.push(correlated_join_failure(async_b, "B complete"));
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(1), async_events.recv())
            .await
            .expect("async B response timeout"),
        Some(SignalFishEvent::RoomJoinFailed { .. })
    ));

    let polling_mock = CorrelationRaceMock::new();
    let polling_control = polling_mock.clone();
    let mut polling_client = SignalFishPollingClient::new(polling_mock, config);
    assert_eq!(
        polling_client
            .poll()
            .iter()
            .map(|event| format!("{event:?}"))
            .collect::<Vec<_>>(),
        ["Connected", "Authenticated", "ProtocolInfo"]
    );
    polling_client
        .join_room(JoinRoomParams::new("game", "polling-a"))
        .expect("polling A admitted");
    let _ = polling_client.poll();
    let polling_a = polling_control
        .sent
        .lock()
        .unwrap()
        .iter()
        .find_map(|json| room_operation_id(json))
        .expect("polling A id");
    polling_control.push(correlated_join_failure(polling_a, "A complete"));
    assert!(matches!(
        polling_client.poll().as_slice(),
        [SignalFishEvent::RoomJoinFailed { .. }]
    ));

    polling_control.block_room_sends();
    polling_client
        .join_room(JoinRoomParams::new("game", "polling-b"))
        .expect("polling B admitted");
    let _ = polling_client.poll();
    let polling_b = polling_control
        .attempted_room_send
        .lock()
        .unwrap()
        .as_deref()
        .and_then(room_operation_id)
        .filter(|id| *id != polling_a)
        .expect("polling B retained id");
    assert_eq!(
        polling_control
            .sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(|json| room_operation_id(json))
            .count(),
        1
    );
    polling_control.push(correlated_join_failure(polling_a, "A stale"));
    assert!(matches!(
        polling_client.poll().as_slice(),
        [SignalFishEvent::ProtocolViolation { .. }]
    ));
    assert!(matches!(
        polling_client.join_room(JoinRoomParams::new("game", "blocked")),
        Err(SignalFishError::RoomOperationPending)
    ));
    polling_control.release_room_sends();
    let _ = polling_client.poll();
    assert_eq!(
        polling_control
            .sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(|json| room_operation_id(json))
            .count(),
        2
    );
    polling_control.push(correlated_join_failure(polling_b, "B complete"));
    assert!(matches!(
        polling_client.poll().as_slice(),
        [SignalFishEvent::RoomJoinFailed { .. }]
    ));

    assert_ne!(async_a, async_b);
    assert_ne!(polling_a, polling_b);
    assert_eq!(async_client.snapshot(), polling_client.snapshot());
    async_client.shutdown().await;
}

#[tokio::test]
async fn every_common_command_has_membership_guard_parity_across_drivers() {
    for phase in [
        MembershipPhase::Outside,
        MembershipPhase::Player,
        MembershipPhase::PlayerLeft,
        MembershipPhase::PlayerRejoined,
        MembershipPhase::Spectator,
        MembershipPhase::SpectatorLeft,
        MembershipPhase::SpectatorRejoined,
    ] {
        for case in ALL_COMMON_COMMANDS {
            let expected = expected_membership_result(phase, case);
            let async_result = async_membership_result(phase, case).await;
            let polling_result = polling_membership_result(phase, case);
            assert_eq!(async_result, expected, "async {phase:?} {case:?}");
            assert_eq!(polling_result, expected, "polling {phase:?} {case:?}");
        }
    }
}

#[tokio::test]
async fn admitted_leave_fences_following_player_commands_in_both_drivers() {
    let config = SignalFishConfig::new("app").enable_v3();

    let async_mock = FrameMock::v3();
    let async_sent = Arc::clone(&async_mock.sent);
    let (mut async_client, mut async_events) = SignalFishClient::start(async_mock, config.clone());
    admit_initial_room_operation(&mut async_client, Some(InitialRoomOperation::JoinPlayer));
    while !matches!(
        async_events.recv().await,
        Some(SignalFishEvent::SessionPlan { .. })
    ) {}
    async_sent.lock().unwrap().clear();
    async_client.leave_room().expect("async leave is admitted");
    assert!(matches!(
        async_client.send_game_data(serde_json::json!({"after": "leave"})),
        Err(SignalFishError::RoomOperationPending)
    ));

    let polling_mock = FrameMock::v3();
    let polling_sent = Arc::clone(&polling_mock.sent);
    let mut polling_client = SignalFishPollingClient::new(polling_mock, config);
    admit_initial_room_operation(&mut polling_client, Some(InitialRoomOperation::JoinPlayer));
    let _ = polling_client.poll();
    polling_sent.lock().unwrap().clear();
    polling_client
        .leave_room()
        .expect("polling leave is admitted");
    assert!(matches!(
        polling_client.send_game_data(serde_json::json!({"after": "leave"})),
        Err(SignalFishError::RoomOperationPending)
    ));

    let _ = polling_client.poll();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while async_sent.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("async LeaveRoom frame");
    assert_eq!(async_sent.lock().unwrap().len(), 1);
    assert_eq!(polling_sent.lock().unwrap().len(), 1);
    assert!(matches!(
        &async_sent.lock().unwrap()[0],
        TransportFrame::Text(frame) if frame.contains("LeaveRoom")
    ));
    assert!(matches!(
        &polling_sent.lock().unwrap()[0],
        TransportFrame::Text(frame) if frame.contains("LeaveRoom")
    ));
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
        CommonCommandCase::SignalForGeneration,
        CommonCommandCase::Offer,
        CommonCommandCase::Answer,
        CommonCommandCase::IceCandidate,
        CommonCommandCase::RawSignal,
        CommonCommandCase::RawSignalForGeneration,
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
async fn parity_membership_precedes_v3_pre_negotiation_mode() {
    let peer: PlayerId = PEER_UUID.parse().unwrap();

    let async_mock = SharedMock::new(vec![]);
    let (mut client, _events) = SignalFishClient::start(async_mock, SignalFishConfig::new("app"));
    let async_err = client.send_offer(peer, "sdp").unwrap_err();

    let poll_mock = SharedMock::new(vec![]);
    let mut poll_client =
        SignalFishPollingClient::new(poll_mock, SignalFishConfig::new("app").enable_mesh());
    let poll_err = poll_client.send_offer(peer, "sdp").unwrap_err();

    assert!(matches!(async_err, SignalFishError::NotInRoom));
    assert!(matches!(poll_err, SignalFishError::NotInRoom));
}

#[tokio::test]
async fn parity_ensure_v3_relay_only_mode_after_v2_negotiation() {
    // Both clients must report the terminal "relay-only" mode once a v2
    // `ProtocolInfo` has been observed — distinct from the "pre-negotiation"
    // state before any `ProtocolInfo` arrives (see the parity test above).
    let peer: PlayerId = PEER_UUID.parse().unwrap();

    let room = match finalized_v2_room_frame() {
        TransportFrame::Text(room) => room,
        TransportFrame::Binary(_) => unreachable!("room baseline must be text"),
    };
    let messages = [AUTH.to_string(), PI_V2.to_string(), room];
    let async_mock = SharedMock::from_msgs(
        messages
            .iter()
            .cloned()
            .map(|message| Some(Ok(message)))
            .collect(),
    );
    let (mut client, mut events) =
        SignalFishClient::start(async_mock, SignalFishConfig::new("app"));
    admit_initial_room_operation(&mut client, Some(InitialRoomOperation::JoinPlayer));
    // Drain until the v2 room baseline has been processed into client state.
    loop {
        match events.recv().await {
            Some(SignalFishEvent::RoomJoined { .. }) | None => break,
            _ => {}
        }
    }
    let async_err = client.send_offer(peer, "sdp").unwrap_err();

    let poll_mock = SharedMock::from_msgs(
        messages
            .into_iter()
            .map(|message| Some(Ok(message)))
            .collect(),
    );
    let mut poll_client = SignalFishPollingClient::new(poll_mock, SignalFishConfig::new("app"));
    admit_initial_room_operation(&mut poll_client, Some(InitialRoomOperation::JoinPlayer));
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
async fn parity_reconnect_preserves_outer_v3_negotiation() {
    let recon = reconnected_with_missed(vec![]);

    let async_mock = SharedMock::new(vec![AUTH, PI_V3, &recon]);
    let (mut client, mut events) =
        SignalFishClient::start(async_mock, SignalFishConfig::new("app").enable_mesh());
    client
        .reconnect(
            uuid::Uuid::from_u128(200),
            uuid::Uuid::from_u128(100),
            "submitted-token".into(),
        )
        .expect("async reconnect must queue");
    for _ in 0..4 {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), events.recv()).await;
    }
    assert_eq!(
        client.negotiated_protocol_version(),
        Some(3),
        "async: reconnect must preserve the connection negotiation"
    );

    let poll_mock = SharedMock::new(vec![AUTH, PI_V3, &recon]);
    let mut poll_client = SignalFishPollingClient::new(poll_mock, SignalFishConfig::new("app"));
    poll_client
        .reconnect(
            uuid::Uuid::from_u128(200),
            uuid::Uuid::from_u128(100),
            "submitted-token".into(),
        )
        .expect("polling reconnect must queue");
    poll_client.poll();
    assert_eq!(
        poll_client.negotiated_protocol_version(),
        Some(3),
        "polling: reconnect must preserve the connection negotiation"
    );
}

// ── PARITY 5: reconnect with v2 missed_events must NOT downgrade ──────

#[tokio::test]
async fn parity_reconnect_rejects_nested_protocol_info_without_downgrade() {
    let recon_v2 = reconnected_with_missed(vec![ServerMessage::ProtocolInfo(pi_v2_payload())]);
    let events = assert_frame_trace_parity_with_reconnect(
        vec![
            TransportFrame::Text(AUTH.into()),
            TransportFrame::Text(PI_V3.into()),
            TransportFrame::Text(recon_v2),
        ],
        SignalFishConfig::new("app")
            .enable_v3()
            .with_protocol_violation_policy(ProtocolViolationPolicy::Observe),
        true,
    )
    .await;
    assert!(events.iter().any(|event| {
        event.starts_with("ProtocolViolation|Lifecycle|")
            && event.contains("non-replayable ProtocolInfo")
    }));
    assert!(!events.iter().any(|event| event.starts_with("Reconnected")));
}

#[tokio::test]
async fn malformed_reconnect_baselines_have_complete_driver_parity() {
    let valid = reconnected_with_missed(vec![]);
    let ServerMessage::Reconnected(mut invalid_payload) =
        serde_json::from_str::<ServerMessage>(&valid).unwrap()
    else {
        unreachable!("reconnect fixture must decode as Reconnected")
    };
    invalid_payload.reconnection_token = None;
    let invalid = serde_json::to_string(&ServerMessage::Reconnected(invalid_payload)).unwrap();

    for policy in [
        ProtocolViolationPolicy::Observe,
        ProtocolViolationPolicy::Quarantine,
    ] {
        let (events, snapshot) = assert_open_reconnect_trace_parity(
            vec![AUTH.into(), PI_V3.into(), invalid.clone(), valid.clone()],
            SignalFishConfig::new("app")
                .enable_v3()
                .with_protocol_violation_policy(policy),
        )
        .await;
        assert!(events[2].starts_with("ProtocolViolation|Lifecycle|"));
        assert!(events[3].starts_with("Reconnected|"));
        assert_eq!(snapshot.room_id, Some(uuid::Uuid::from_u128(100)));
        assert_eq!(
            snapshot.reconnection_token.as_deref(),
            Some("rotated-token")
        );
        assert!(!snapshot.quarantined);
    }

    let events = assert_frame_trace_parity_with_reconnect(
        vec![
            TransportFrame::Text(AUTH.into()),
            TransportFrame::Text(PI_V3.into()),
            TransportFrame::Text(invalid),
        ],
        SignalFishConfig::new("app")
            .enable_v3()
            .with_protocol_violation_policy(ProtocolViolationPolicy::Disconnect),
        true,
    )
    .await;
    assert!(events[2].starts_with("ProtocolViolation|Lifecycle|"));
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

#[tokio::test]
async fn parity_mesh_capability_requires_webrtc_and_p2p_topology() {
    let cases = [
        (vec![TransportKind::WebRtc], vec![Topology::Mesh], true),
        (vec![TransportKind::WebRtc], vec![Topology::Host], true),
        (vec![TransportKind::WebRtc], vec![Topology::Relay], false),
        (vec![TransportKind::Relay], vec![Topology::Mesh], false),
    ];

    for (transports, topologies, expected) in cases {
        let config = SignalFishConfig::new("app")
            .with_protocol_version(3)
            .with_transports(transports)
            .with_topologies(topologies);

        let async_mock = SharedMock::new(vec![AUTH, PI_V3]);
        let (mut async_client, mut events) = SignalFishClient::start(async_mock, config.clone());
        for _ in 0..3 {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
                .await
                .expect("async capability trace should make progress");
        }

        let poll_mock = SharedMock::new(vec![AUTH, PI_V3]);
        let mut polling_client = SignalFishPollingClient::new(poll_mock, config);
        let _ = polling_client.poll();

        assert_eq!(async_client.supports_mesh(), expected);
        assert_eq!(polling_client.supports_mesh(), expected);
        async_client.shutdown().await;
    }
}

#[tokio::test]
async fn parity_selected_plan_accessors_follow_ordered_canonical_transitions() {
    use signal_fish_client::protocol::{DirectEndpoint, SessionPeer, SessionPlanPayload};

    fn plan(
        topology: Topology,
        transport: TransportKind,
        generation: u128,
        peer: PlayerId,
    ) -> ServerMessage {
        ServerMessage::SessionPlan(Box::new(SessionPlanPayload {
            generation: Some(uuid::Uuid::from_u128(generation)),
            topology,
            transport,
            host: (topology == Topology::Host).then_some(peer),
            direct_endpoint: (transport == TransportKind::Direct).then_some(DirectEndpoint {
                host: "192.0.2.10".into(),
                port: 7_777,
            }),
            peers: if topology == Topology::Relay {
                vec![]
            } else {
                vec![SessionPeer {
                    player_id: peer,
                    player_name: "peer".into(),
                    is_authority: false,
                    initiate: false,
                }]
            },
            ice_servers: vec![],
            fallback: TransportKind::Relay,
        }))
    }

    let peer = uuid::Uuid::from_u128(350);
    let transitions = [
        (Topology::Relay, TransportKind::Relay, false, 351),
        (Topology::Mesh, TransportKind::WebRtc, true, 352),
        (Topology::Host, TransportKind::Direct, true, 353),
        (Topology::Host, TransportKind::WebRtc, true, 354),
    ];

    let mut messages = binary_accountability_prefix(peer)
        .into_iter()
        .map(|frame| match frame {
            TransportFrame::Text(text) => text,
            TransportFrame::Binary(_) => unreachable!("prefix is text-only"),
        })
        .collect::<Vec<_>>();
    messages.push(
        serde_json::to_string(&ServerMessage::LobbyStateChanged {
            lobby_state: LobbyState::Finalized,
            ready_players: vec![],
            all_ready: true,
        })
        .expect("finalized lobby event should serialize"),
    );

    for (index, (topology, transport, p2p_active, generation)) in
        transitions.into_iter().enumerate()
    {
        messages.push(
            serde_json::to_string(&plan(topology, transport, generation, peer))
                .expect("session plan should serialize"),
        );

        let (events, snapshot) = assert_open_text_trace_parity(
            messages.clone(),
            SignalFishConfig::new("app").enable_mesh(),
        )
        .await;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.starts_with("SessionPlan|"))
                .count(),
            index + 1,
            "every ordered plan transition must surface exactly once"
        );
        assert_eq!(snapshot.session_topology, Some(topology));
        assert_eq!(snapshot.session_transport, Some(transport));
        assert_eq!(
            matches!(
                snapshot.session_topology,
                Some(Topology::Host | Topology::Mesh)
            ),
            p2p_active
        );
    }
}

#[tokio::test]
async fn parity_selected_plan_resets_at_room_and_connection_boundaries() {
    use signal_fish_client::protocol::{SessionPeer, SessionPlanPayload};

    let peer = uuid::Uuid::from_u128(350);
    let plan = ServerMessage::SessionPlan(Box::new(SessionPlanPayload {
        generation: Some(uuid::Uuid::from_u128(351)),
        topology: Topology::Mesh,
        transport: TransportKind::WebRtc,
        host: None,
        direct_endpoint: None,
        peers: vec![SessionPeer {
            player_id: peer,
            player_name: "peer".into(),
            is_authority: false,
            initiate: false,
        }],
        ice_servers: vec![],
        fallback: TransportKind::Relay,
    }));
    let mut populated = binary_accountability_prefix(peer)
        .into_iter()
        .map(|frame| match frame {
            TransportFrame::Text(text) => text,
            TransportFrame::Binary(_) => unreachable!("prefix is text-only"),
        })
        .collect::<Vec<_>>();
    populated.extend([
        serde_json::to_string(&ServerMessage::LobbyStateChanged {
            lobby_state: LobbyState::Finalized,
            ready_players: vec![],
            all_ready: true,
        })
        .expect("finalized lobby event should serialize"),
        serde_json::to_string(&plan).expect("session plan should serialize"),
    ]);

    let (events, snapshot) = assert_open_text_trace_parity(
        populated.clone(),
        SignalFishConfig::new("app").enable_mesh(),
    )
    .await;
    assert!(events.iter().any(|event| event.starts_with("SessionPlan|")));
    assert_eq!(snapshot.session_topology, Some(Topology::Mesh));
    assert_eq!(snapshot.session_transport, Some(TransportKind::WebRtc));

    let mut room_exit = populated.clone();
    room_exit
        .push(serde_json::to_string(&ServerMessage::RoomLeft).expect("RoomLeft should serialize"));
    let (events, snapshot) =
        assert_open_text_trace_parity(room_exit, SignalFishConfig::new("app").enable_mesh()).await;
    let lifecycle = events
        .iter()
        .filter_map(|event| {
            if event.starts_with("SessionPlan|") {
                Some("SessionPlan")
            } else if event == "RoomLeft" {
                Some("RoomLeft")
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(lifecycle, ["SessionPlan", "RoomLeft"]);
    assert!(snapshot.session_topology.is_none());
    assert!(snapshot.session_transport.is_none());

    let polling_mock = SharedMock::from_msgs(
        populated
            .iter()
            .cloned()
            .map(|message| Some(Ok(message)))
            .collect(),
    );
    let mut polling_client =
        SignalFishPollingClient::new(polling_mock, SignalFishConfig::new("app").enable_mesh());
    admit_initial_room_operation(&mut polling_client, Some(InitialRoomOperation::JoinPlayer));
    let _ = polling_client.poll();
    assert_eq!(polling_client.session_topology(), Some(Topology::Mesh));
    assert_eq!(
        polling_client.session_transport(),
        Some(TransportKind::WebRtc)
    );
    polling_client.close();
    assert!(polling_client.session_topology().is_none());
    assert!(polling_client.session_transport().is_none());

    let async_mock = SharedMock::from_msgs(
        populated
            .into_iter()
            .map(|message| Some(Ok(message)))
            .chain(std::iter::once(Some(Err(
                SignalFishError::TransportReceive("reset".into()),
            ))))
            .collect(),
    );
    let (mut async_client, mut events) =
        SignalFishClient::start(async_mock, SignalFishConfig::new("app").enable_mesh());
    admit_initial_room_operation(&mut async_client, Some(InitialRoomOperation::JoinPlayer));
    let mut saw_plan = false;
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
            .await
            .expect("async disconnect trace should make progress")
        {
            Some(SignalFishEvent::SessionPlan { .. }) => saw_plan = true,
            Some(SignalFishEvent::Disconnected { .. }) => break,
            Some(_) => {}
            None => panic!("async event stream ended before Disconnected"),
        }
    }
    assert!(saw_plan, "the disconnect must follow a populated plan");
    assert!(async_client.session_topology().is_none());
    assert!(async_client.session_transport().is_none());
}

#[tokio::test]
async fn parity_reconnected_baseline_is_planless_until_fresh_session_plan() {
    use signal_fish_client::protocol::SessionPlanPayload;

    let (events, snapshot) = assert_open_reconnect_trace_parity(
        vec![AUTH.into(), PI_V3.into(), reconnected_with_missed(vec![])],
        SignalFishConfig::new("app").enable_mesh(),
    )
    .await;
    assert!(events.iter().any(|event| event.starts_with("Reconnected|")));
    assert!(snapshot.session_topology.is_none());
    assert!(snapshot.session_transport.is_none());
    assert!(snapshot.session_generation.is_none());

    let ServerMessage::Reconnected(mut finalized) =
        serde_json::from_str::<ServerMessage>(&reconnected_with_missed(vec![]))
            .expect("Reconnected fixture should decode")
    else {
        unreachable!("reconnect fixture is Reconnected")
    };
    finalized.lobby_state = LobbyState::Finalized;
    let fresh_plan = ServerMessage::SessionPlan(Box::new(SessionPlanPayload {
        generation: Some(uuid::Uuid::from_u128(355)),
        topology: Topology::Relay,
        transport: TransportKind::Relay,
        host: None,
        direct_endpoint: None,
        peers: vec![],
        ice_servers: vec![],
        fallback: TransportKind::Relay,
    }));
    let (events, snapshot) = assert_open_reconnect_trace_parity(
        vec![
            AUTH.into(),
            PI_V3.into(),
            serde_json::to_string(&ServerMessage::Reconnected(finalized))
                .expect("finalized Reconnected fixture should serialize"),
            serde_json::to_string(&fresh_plan).expect("fresh SessionPlan should serialize"),
        ],
        SignalFishConfig::new("app").enable_mesh(),
    )
    .await;
    let lifecycle = events
        .iter()
        .filter_map(|event| {
            if event.starts_with("Reconnected|") {
                Some("Reconnected")
            } else if event.starts_with("SessionPlan|") {
                Some("SessionPlan")
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(lifecycle, ["Reconnected", "SessionPlan"]);
    assert_eq!(snapshot.session_topology, Some(Topology::Relay));
    assert_eq!(snapshot.session_transport, Some(TransportKind::Relay));
    assert_eq!(
        snapshot.session_generation,
        Some(uuid::Uuid::from_u128(355))
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

#[tokio::test]
async fn parity_ready_farewell_survives_simultaneous_send_failure() {
    let async_transport = SendFailureFarewellTransport::default();
    let async_observer = async_transport.clone();
    let async_config = SignalFishConfig::new("app")
        .with_event_channel_capacity(1)
        .with_command_channel_capacity(4);
    let (mut async_client, mut async_events) =
        SignalFishClient::start(async_transport, async_config);

    assert!(matches!(
        async_events.recv().await,
        Some(SignalFishEvent::Connected)
    ));
    wait_until(|| async_client.is_authenticated() && async_events.capacity() == 0).await;
    async_client
        .ping()
        .expect("first Ping should enter the async command queue");
    async_client
        .ping()
        .expect("second Ping should queue behind the failing command");
    wait_until(|| async_observer.send_failed()).await;
    assert!(matches!(
        async_client.ping(),
        Err(SignalFishError::NotConnected)
    ));
    assert!(matches!(
        async_events.recv().await,
        Some(SignalFishEvent::Authenticated { .. })
    ));

    let async_farewell =
        tokio::time::timeout(std::time::Duration::from_secs(1), async_events.recv())
            .await
            .expect("ready farewell should make progress through event delivery")
            .expect("farewell event channel should remain open");
    assert!(matches!(
        async_farewell,
        SignalFishEvent::Error {
            error_code: Some(ErrorCode::SlowConsumer),
            ..
        }
    ));
    let async_terminal =
        tokio::time::timeout(std::time::Duration::from_secs(1), async_events.recv())
            .await
            .expect("Disconnected should follow the farewell")
            .expect("terminal event channel should remain open");
    let SignalFishEvent::Disconnected {
        reason: async_reason,
        last_server_error: async_error,
    } = async_terminal
    else {
        panic!("expected async Disconnected after farewell");
    };
    assert_eq!(
        async_reason.as_deref(),
        Some("transport send error: scripted write failure")
    );
    let async_error = async_error.expect("async disconnect should attribute the farewell");
    assert_eq!(async_error.error_code, Some(ErrorCode::SlowConsumer));
    assert_eq!(async_observer.ping_attempts(), 1);
    assert_eq!(async_observer.close_calls(), 1);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), async_events.recv())
            .await
            .expect("terminal event sender should be dropped")
            .is_none(),
        "no event may follow Disconnected"
    );

    let polling_transport = SendFailureFarewellTransport::default();
    let polling_observer = polling_transport.clone();
    let mut polling_client =
        SignalFishPollingClient::new(polling_transport, SignalFishConfig::new("app"));
    let setup_events = polling_client.poll();
    assert!(matches!(
        setup_events.as_slice(),
        [
            SignalFishEvent::Connected,
            SignalFishEvent::Authenticated { .. }
        ]
    ));
    polling_client
        .ping()
        .expect("first Ping should enter the polling command queue");
    polling_client
        .ping()
        .expect("second Ping should queue behind the failing command");

    let polling_events = polling_client.poll();
    assert!(matches!(
        polling_events.as_slice(),
        [
            SignalFishEvent::Error {
                error_code: Some(ErrorCode::SlowConsumer),
                ..
            },
            SignalFishEvent::Disconnected { .. }
        ]
    ));
    let SignalFishEvent::Disconnected {
        reason: polling_reason,
        last_server_error: polling_error,
    } = &polling_events[1]
    else {
        unreachable!("slice shape asserted above")
    };
    assert_eq!(polling_reason, &async_reason);
    assert_eq!(
        polling_error.as_ref(),
        Some(&async_error),
        "both drivers must attribute the same terminal server farewell"
    );
    assert_eq!(polling_observer.ping_attempts(), 1);
    assert!(matches!(
        polling_client.ping(),
        Err(SignalFishError::NotConnected)
    ));

    async_client.shutdown().await;
}

#[tokio::test]
async fn shutdown_preempts_capacity_one_send_failure_drain() {
    let transport = SendFailureFarewellTransport::default();
    let observer = transport.clone();
    let config = SignalFishConfig::new("app")
        .with_event_channel_capacity(1)
        .with_command_channel_capacity(4)
        .with_shutdown_timeout(std::time::Duration::from_secs(5));
    let (mut client, mut events) = SignalFishClient::start(transport, config);

    assert!(matches!(
        events.recv().await,
        Some(SignalFishEvent::Connected)
    ));
    wait_until(|| client.is_authenticated() && events.capacity() == 0).await;
    client
        .ping()
        .expect("Ping should enter the async command queue");
    client
        .ping()
        .expect("queued work behind the failing Ping should be discarded");
    wait_until(|| observer.send_failed()).await;

    tokio::time::timeout(std::time::Duration::from_millis(250), client.shutdown())
        .await
        .expect("shutdown should preempt blocked farewell delivery");
    assert!(!client.is_connected());
    assert!(!client.is_authenticated());
    assert_eq!(observer.ping_attempts(), 1);
    assert_eq!(observer.close_calls(), 1);

    assert!(matches!(
        events.recv().await,
        Some(SignalFishEvent::Authenticated { .. })
    ));
    assert!(events.recv().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn terminal_deadline_bounds_blocked_async_farewell_delivery() {
    let transport = SendFailureFarewellTransport::default();
    let observer = transport.clone();
    let config = SignalFishConfig::new("app")
        .with_event_channel_capacity(1)
        .with_shutdown_timeout(std::time::Duration::from_millis(20));
    let (mut client, mut events) = SignalFishClient::start(transport, config);

    assert!(matches!(
        events.recv().await,
        Some(SignalFishEvent::Connected)
    ));
    wait_until(|| client.is_authenticated() && events.capacity() == 0).await;
    client
        .ping()
        .expect("Ping should enter the async command queue");
    wait_until(|| observer.send_failed()).await;
    tokio::time::advance(std::time::Duration::from_millis(20)).await;
    wait_until(|| !client.is_connected()).await;

    assert_eq!(observer.ping_attempts(), 1);
    assert_eq!(observer.post_failure_recv_calls(), 1);
    assert_eq!(observer.close_calls(), 0);
    assert_eq!(observer.abort_calls(), 1);
    assert!(matches!(
        events.recv().await,
        Some(SignalFishEvent::Authenticated { .. })
    ));
    assert!(events.recv().await.is_none());
    client.shutdown().await;
}

#[test]
fn zero_terminal_deadline_skips_polling_farewell_drain() {
    let transport = SendFailureFarewellTransport::default();
    let observer = transport.clone();
    let config = SignalFishConfig::new("app").with_shutdown_timeout(std::time::Duration::ZERO);
    let mut client = SignalFishPollingClient::new(transport, config);
    let _ = client.poll();
    client
        .ping()
        .expect("Ping should enter the polling command queue");

    let events = client.poll();
    assert!(matches!(
        events.as_slice(),
        [SignalFishEvent::Disconnected {
            last_server_error: None,
            ..
        }]
    ));
    assert_eq!(observer.post_failure_recv_calls(), 0);
    assert_eq!(observer.ping_attempts(), 1);
    assert_eq!(observer.abort_calls(), 1);
}

#[tokio::test]
async fn terminal_send_failure_cause_precedence_and_ready_stop_are_driver_aligned() {
    struct Case {
        name: &'static str,
        steps: Vec<PostFailureRecv>,
        close_info: Option<signal_fish_client::TransportCloseInfo>,
        expected_reason: &'static str,
        expected_error_event: bool,
        expected_recv_calls: usize,
    }

    let peer_close = signal_fish_client::TransportCloseInfo {
        code: Some(4000),
        reason: Some("peer ended".into()),
        clean: Some(true),
        initiated_by_peer: true,
    };
    let local_close = signal_fish_client::TransportCloseInfo {
        initiated_by_peer: false,
        ..peer_close.clone()
    };
    let cases = [
        Case {
            name: "Pending preserves the send failure and is never repolled",
            steps: vec![PostFailureRecv::Pending, PostFailureRecv::Pong],
            close_info: None,
            expected_reason: "transport send error: scripted write failure",
            expected_error_event: false,
            expected_recv_calls: 1,
        },
        Case {
            name: "bare EOF preserves the send failure",
            steps: vec![PostFailureRecv::Eof],
            close_info: None,
            expected_reason: "transport send error: scripted write failure",
            expected_error_event: false,
            expected_recv_calls: 1,
        },
        Case {
            name: "peer close metadata overrides the send failure at EOF",
            steps: vec![PostFailureRecv::Eof],
            close_info: Some(peer_close.clone()),
            expected_reason: "closed by server: code=Some(4000), reason=Some(\"peer ended\")",
            expected_error_event: false,
            expected_recv_calls: 1,
        },
        Case {
            name: "receive error preserves the send failure",
            steps: vec![PostFailureRecv::Error],
            close_info: None,
            expected_reason: "transport send error: scripted write failure",
            expected_error_event: false,
            expected_recv_calls: 1,
        },
        Case {
            name: "peer metadata and farewell attribution remain independent",
            steps: vec![PostFailureRecv::Farewell, PostFailureRecv::Eof],
            close_info: Some(peer_close),
            expected_reason: "closed by server: code=Some(4000), reason=Some(\"peer ended\")",
            expected_error_event: true,
            expected_recv_calls: 2,
        },
        Case {
            name: "non-peer metadata does not override the send failure",
            steps: vec![PostFailureRecv::Farewell, PostFailureRecv::Eof],
            close_info: Some(local_close),
            expected_reason: "transport send error: scripted write failure",
            expected_error_event: true,
            expected_recv_calls: 2,
        },
    ];

    for case in cases {
        let async_transport =
            SendFailureFarewellTransport::new(case.steps.clone(), case.close_info.clone());
        let async_observer = async_transport.clone();
        let (mut async_client, mut async_events) =
            SignalFishClient::start(async_transport, SignalFishConfig::new("app"));
        assert!(matches!(
            async_events.recv().await,
            Some(SignalFishEvent::Connected)
        ));
        assert!(matches!(
            async_events.recv().await,
            Some(SignalFishEvent::Authenticated { .. })
        ));
        async_client
            .ping()
            .expect("scripted async Ping should be admitted");

        let mut async_terminal_events = Vec::new();
        for _ in 0..3 {
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(1), async_events.recv())
                    .await
                    .unwrap_or_else(|_| panic!("{}: async terminal event timed out", case.name))
                    .unwrap_or_else(|| panic!("{}: async event channel closed early", case.name));
            let disconnected = matches!(event, SignalFishEvent::Disconnected { .. });
            async_terminal_events.push(event);
            if disconnected {
                break;
            }
        }

        let polling_transport = SendFailureFarewellTransport::new(case.steps, case.close_info);
        let polling_observer = polling_transport.clone();
        let mut polling_client =
            SignalFishPollingClient::new(polling_transport, SignalFishConfig::new("app"));
        let setup = polling_client.poll();
        assert_eq!(setup.len(), 2, "{}: polling setup events", case.name);
        polling_client
            .ping()
            .expect("scripted polling Ping should be admitted");
        let polling_terminal_events = polling_client.poll();

        for (driver, events) in [
            ("async", async_terminal_events.as_slice()),
            ("polling", polling_terminal_events.as_slice()),
        ] {
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, SignalFishEvent::Error { .. }))
                    .count(),
                usize::from(case.expected_error_event),
                "{}: {driver} Error event count",
                case.name
            );
            let terminal = events
                .last()
                .unwrap_or_else(|| panic!("{}: {driver} terminal events", case.name));
            let SignalFishEvent::Disconnected {
                reason,
                last_server_error,
            } = terminal
            else {
                panic!("{}: {driver} must end with Disconnected", case.name);
            };
            assert_eq!(
                reason.as_deref(),
                Some(case.expected_reason),
                "{}: {driver} terminal cause",
                case.name
            );
            assert_eq!(
                last_server_error.is_some(),
                case.expected_error_event,
                "{}: {driver} farewell attribution",
                case.name
            );
        }
        assert_eq!(
            async_observer.post_failure_recv_calls(),
            case.expected_recv_calls,
            "{}: async receive bound",
            case.name
        );
        assert_eq!(
            polling_observer.post_failure_recv_calls(),
            case.expected_recv_calls,
            "{}: polling receive bound",
            case.name
        );
        let _ = polling_client.poll();
        assert_eq!(
            polling_observer.post_failure_recv_calls(),
            case.expected_recv_calls,
            "{}: polling close must not resume terminal receive",
            case.name
        );
        async_client.shutdown().await;
    }
}

#[tokio::test]
async fn protocol_directed_disconnect_stops_send_failure_drain_in_both_drivers() {
    let steps = [PostFailureRecv::ProtocolViolation, PostFailureRecv::Pong];
    let config = || {
        SignalFishConfig::new("app")
            .with_protocol_violation_policy(ProtocolViolationPolicy::Disconnect)
    };

    let async_transport = SendFailureFarewellTransport::new(steps.clone(), None);
    let async_observer = async_transport.clone();
    let (mut async_client, mut async_events) = SignalFishClient::start(async_transport, config());
    assert!(matches!(
        async_events.recv().await,
        Some(SignalFishEvent::Connected)
    ));
    assert!(matches!(
        async_events.recv().await,
        Some(SignalFishEvent::Authenticated { .. })
    ));
    async_client.ping().expect("async Ping should be admitted");
    assert!(matches!(
        async_events.recv().await,
        Some(SignalFishEvent::ProtocolViolation { .. })
    ));
    assert!(matches!(
        async_events.recv().await,
        Some(SignalFishEvent::Disconnected {
            last_server_error: None,
            ..
        })
    ));
    assert_eq!(async_observer.post_failure_recv_calls(), 1);

    let polling_transport = SendFailureFarewellTransport::new(steps, None);
    let polling_observer = polling_transport.clone();
    let mut polling_client = SignalFishPollingClient::new(polling_transport, config());
    let _ = polling_client.poll();
    polling_client
        .ping()
        .expect("polling Ping should be admitted");
    assert!(matches!(
        polling_client.poll().as_slice(),
        [
            SignalFishEvent::ProtocolViolation { .. },
            SignalFishEvent::Disconnected {
                last_server_error: None,
                ..
            }
        ]
    ));
    assert_eq!(polling_observer.post_failure_recv_calls(), 1);

    async_client.shutdown().await;
}

#[test]
fn polling_send_failure_drain_processes_a_prefetched_farewell_first() {
    let transport = SendFailureFarewellTransport::new([PostFailureRecv::Pong], None)
        .with_pre_failure([PostFailureRecv::Farewell]);
    let observer = transport.clone();
    let options = PollingClientOptions {
        work_budget: PollingWorkBudget {
            receive_frames: 1,
            ..PollingWorkBudget::default()
        },
        ..PollingClientOptions::default()
    };
    let mut client =
        SignalFishPollingClient::new_with_options(transport, SignalFishConfig::new("app"), options);
    assert!(matches!(
        client.poll().as_slice(),
        [
            SignalFishEvent::Connected,
            SignalFishEvent::Authenticated { .. }
        ]
    ));
    assert_eq!(client.polling_stats().receive_budget_exhaustions, 1);
    client.ping().expect("polling Ping should be admitted");

    assert!(matches!(
        client.poll().as_slice(),
        [
            SignalFishEvent::Error {
                error_code: Some(ErrorCode::SlowConsumer),
                ..
            },
            SignalFishEvent::Disconnected {
                last_server_error: Some(_),
                ..
            }
        ]
    ));
    assert_eq!(observer.post_failure_recv_calls(), 0);
    assert_eq!(client.polling_stats().receive_budget_exhaustions, 2);
}

#[tokio::test]
async fn terminal_send_failure_drain_honors_shared_and_polling_receive_budgets() {
    const SHARED_LIMIT: usize = 64;

    for (name, steps, expected_frames) in [
        (
            "shared frame limit",
            std::iter::repeat_n(PostFailureRecv::Pong, SHARED_LIMIT + 1).collect::<Vec<_>>(),
            SHARED_LIMIT,
        ),
        (
            "shared byte limit crossing",
            vec![
                PostFailureRecv::PongBytes(32 * 1024),
                PostFailureRecv::PongBytes(32 * 1024),
                PostFailureRecv::Pong,
            ],
            2,
        ),
        (
            "shared oversized first frame",
            vec![
                PostFailureRecv::PongBytes(64 * 1024 + 1),
                PostFailureRecv::Pong,
            ],
            1,
        ),
    ] {
        let (pongs, recv_calls) = count_async_terminal_pongs(steps).await;
        assert_eq!(pongs, expected_frames, "{name}");
        assert_eq!(recv_calls, expected_frames, "{name}");
    }

    for (name, receive_frames, receive_bytes, steps, expected_frames) in [
        (
            "smaller caller frame budget",
            2,
            usize::MAX,
            std::iter::repeat_n(PostFailureRecv::Pong, SHARED_LIMIT + 1).collect::<Vec<_>>(),
            2,
        ),
        (
            "shared polling safety cap",
            100,
            usize::MAX,
            std::iter::repeat_n(PostFailureRecv::Pong, SHARED_LIMIT + 1).collect::<Vec<_>>(),
            SHARED_LIMIT,
        ),
        (
            "polling byte limit crossing",
            usize::MAX,
            32,
            vec![
                PostFailureRecv::PongBytes(20),
                PostFailureRecv::PongBytes(20),
                PostFailureRecv::Pong,
            ],
            2,
        ),
        (
            "polling oversized first frame",
            usize::MAX,
            32,
            vec![PostFailureRecv::PongBytes(33), PostFailureRecv::Pong],
            1,
        ),
    ] {
        let (pongs, recv_calls, exhaustions) =
            count_polling_terminal_pongs(steps, receive_frames, receive_bytes);
        assert_eq!(pongs, expected_frames, "{name}");
        assert_eq!(recv_calls, expected_frames, "{name}");
        assert_eq!(exhaustions, 1, "{name}");
    }
}
