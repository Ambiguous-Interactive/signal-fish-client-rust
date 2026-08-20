//! Shared, transport-independent Signal Fish client state machine.
//!
//! The async and polling clients deliberately keep their driving mechanics
//! separate. Everything that interprets protocol frames or mutates observable
//! client state lives here so both drivers cannot drift semantically.

use std::collections::{HashSet, VecDeque};

use crate::accountability::{self, DeliveryAccountability, GameDataDisposition};
use crate::client::{
    bounded_binary_preview, decode_binary_server_message, ClientSnapshot, ClientStats,
    GameDataDelivery, JoinRoomParams, ProtocolViolationPolicy, SignalFishConfig,
};
use crate::event::{ProtocolViolationKind, ServerErrorInfo, SignalFishEvent};
use crate::protocol::{
    ClientMessage, ConnectionInfo, DeliveryClass, GameDataEncoding, PlayerId, RoomId,
    ServerMessage, SessionGeneration, SessionPlanPayload, Topology, TransportKind,
};
use crate::signal::PeerSignal;
use crate::transport::TransportFrame;

/// Result of processing one physical server frame.
pub(crate) struct FrameOutcome {
    pub(crate) events: Vec<SignalFishEvent>,
    pub(crate) disconnect: bool,
}

pub(crate) enum CoreCommand {
    Message(ClientMessage),
    Binary(Vec<u8>),
}

impl std::fmt::Debug for CoreCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Message(_) => "Message",
            Self::Binary(_) => "Binary",
        })
    }
}

pub(crate) enum ClientOperation {
    JoinRoom(JoinRoomParams),
    LeaveRoom,
    GameData(serde_json::Value, GameDataDelivery),
    Binary(Vec<u8>),
    SetReady,
    StartGame,
    RequestAuthority(bool),
    ProvideConnectionInfo(ConnectionInfo),
    Reconnect(PlayerId, RoomId, String),
    JoinAsSpectator(String, String, String),
    LeaveSpectator,
    Ping,
    Signal(PlayerId, SignalGeneration, PeerSignal),
    RawSignal(PlayerId, SignalGeneration, serde_json::Value),
    TransportStatus(TransportKind, bool),
}

#[derive(Clone, Copy)]
pub(crate) enum SignalGeneration {
    Current,
    Exact(Option<SessionGeneration>),
    #[cfg(feature = "tokio-runtime")]
    Bound {
        generation: Option<SessionGeneration>,
        plan_revision: u64,
    },
}

#[cfg(feature = "tokio-runtime")]
pub(crate) struct ReliableOperationBinding {
    room_revision: Option<u64>,
}

impl FrameOutcome {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            disconnect: false,
        }
    }
}

/// Shared protocol state and behavior used by both public client drivers.
pub(crate) struct ClientCore {
    snapshot: ClientSnapshot,
    protocol_info_seen: bool,
    mesh_enabled: bool,
    stats: ClientStats,
    last_server_error: Option<ServerErrorInfo>,
    violation_policy: ProtocolViolationPolicy,
    accountability: DeliveryAccountability,
    membership: Membership,
    room_finalized: bool,
    room_players: HashSet<PlayerId>,
    session_plan_seen: bool,
    session_peers: HashSet<PlayerId>,
    session_transport: Option<TransportKind>,
    retired_generationless_signal_peers: HashSet<PlayerId>,
    pending_reconnects: VecDeque<PendingReconnect>,
    #[cfg(feature = "tokio-runtime")]
    session_plan_revision: u64,
    #[cfg(feature = "tokio-runtime")]
    room_revision: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Membership {
    #[default]
    None,
    Player,
    Spectator,
}

struct RoomBaseline {
    player_id: PlayerId,
    room_id: RoomId,
    room_code: String,
    reconnection_token: Option<String>,
    membership: Membership,
    finalized: bool,
    players: HashSet<PlayerId>,
}

struct PendingReconnect {
    player_id: PlayerId,
    room_id: RoomId,
    token: String,
}

impl ClientCore {
    pub(crate) fn authenticate(config: &SignalFishConfig) -> CoreCommand {
        CoreCommand::Message(ClientMessage::Authenticate {
            app_id: config.app_id.clone(),
            sdk_version: config.sdk_version.clone(),
            platform: config.platform.clone(),
            game_data_format: config.game_data_format,
            protocol_version: config.protocol_version,
            supported_transports: config.supported_transports.clone(),
            supported_topologies: config.supported_topologies.clone(),
        })
    }

    pub(crate) fn new(
        requested_game_data_format: Option<GameDataEncoding>,
        violation_policy: ProtocolViolationPolicy,
        mesh_enabled: bool,
    ) -> Self {
        Self {
            snapshot: ClientSnapshot {
                connected: true,
                requested_game_data_format,
                ..ClientSnapshot::default()
            },
            protocol_info_seen: false,
            mesh_enabled,
            stats: ClientStats::default(),
            last_server_error: None,
            violation_policy,
            accountability: DeliveryAccountability::new(false),
            membership: Membership::None,
            room_finalized: false,
            room_players: HashSet::new(),
            session_plan_seen: false,
            session_peers: HashSet::new(),
            session_transport: None,
            retired_generationless_signal_peers: HashSet::new(),
            pending_reconnects: VecDeque::new(),
            #[cfg(feature = "tokio-runtime")]
            session_plan_revision: 0,
            #[cfg(feature = "tokio-runtime")]
            room_revision: 0,
        }
    }

    pub(crate) fn snapshot(&self) -> ClientSnapshot {
        self.snapshot.clone()
    }

    pub(crate) fn stats(&self) -> ClientStats {
        self.stats
    }

    pub(crate) fn is_connected(&self) -> bool {
        self.snapshot.connected
    }

    pub(crate) fn is_authenticated(&self) -> bool {
        self.snapshot.authenticated
    }

    pub(crate) fn negotiated_protocol_version(&self) -> Option<u16> {
        self.snapshot.negotiated_protocol_version
    }

    pub(crate) fn requested_game_data_format(&self) -> Option<GameDataEncoding> {
        self.snapshot.requested_game_data_format
    }

    pub(crate) fn effective_game_data_format(&self) -> Option<GameDataEncoding> {
        self.snapshot.effective_game_data_format
    }

    #[cfg(all(feature = "mesh", feature = "tokio-runtime"))]
    pub(crate) fn session_plan_revision(&self) -> u64 {
        self.session_plan_revision
    }

    pub(crate) fn supports_mesh(&self) -> bool {
        self.mesh_enabled
            && self
                .negotiated_protocol_version()
                .is_some_and(|version| version >= 3)
    }

    #[cfg(feature = "polling-client")]
    pub(crate) fn current_player_id(&self) -> Option<PlayerId> {
        self.snapshot.player_id
    }

    #[cfg(feature = "polling-client")]
    pub(crate) fn current_room_id(&self) -> Option<RoomId> {
        self.snapshot.room_id
    }

    #[cfg(feature = "polling-client")]
    pub(crate) fn current_room_code(&self) -> Option<&str> {
        self.snapshot.room_code.as_deref()
    }

    pub(crate) fn validate(&self, operation: &ClientOperation) -> crate::error::Result<()> {
        if !self.is_connected() {
            return Err(crate::SignalFishError::NotConnected);
        }
        match operation {
            ClientOperation::GameData(_, GameDataDelivery::Latest { .. })
            | ClientOperation::GameData(_, GameDataDelivery::Volatile)
            | ClientOperation::Binary(_)
            | ClientOperation::Signal(..)
            | ClientOperation::RawSignal(..)
            | ClientOperation::TransportStatus(..) => self.ensure_v3()?,
            _ => {}
        }
        if matches!(&operation, ClientOperation::Binary(_))
            && self.effective_game_data_format() == Some(GameDataEncoding::Json)
        {
            return Err(crate::SignalFishError::BinaryFormatNotNegotiated);
        }
        match operation {
            ClientOperation::Signal(_, requested_generation, _)
            | ClientOperation::RawSignal(_, requested_generation, _) => {
                if !self.session_plan_seen {
                    return Err(crate::SignalFishError::SessionPlanUnavailable);
                }
                let (stale, attempted) = match requested_generation {
                    SignalGeneration::Current => (false, None),
                    SignalGeneration::Exact(generation) => {
                        (*generation != self.snapshot.session_generation, *generation)
                    }
                    #[cfg(feature = "tokio-runtime")]
                    SignalGeneration::Bound {
                        generation,
                        plan_revision,
                    } => (
                        *generation != self.snapshot.session_generation
                            || (generation.is_none()
                                && *plan_revision != self.session_plan_revision),
                        *generation,
                    ),
                };
                if stale {
                    return Err(crate::SignalFishError::StaleSessionGeneration {
                        attempted,
                        current: self.snapshot.session_generation,
                    });
                }
                let peer_id = match operation {
                    ClientOperation::Signal(peer_id, ..)
                    | ClientOperation::RawSignal(peer_id, ..) => peer_id,
                    _ => return Ok(()),
                };
                if !self.session_peers.contains(peer_id) {
                    return Err(crate::SignalFishError::SessionPlanUnavailable);
                }
                if self.session_transport != Some(TransportKind::WebRtc)
                    || !self.room_players.contains(peer_id)
                    || Some(*peer_id) == self.snapshot.player_id
                {
                    return Err(crate::SignalFishError::SessionPlanUnavailable);
                }
            }
            _ => {}
        }
        Ok(())
    }

    #[cfg(feature = "tokio-runtime")]
    pub(crate) fn bind_reliable_operation(
        &self,
        operation: &mut ClientOperation,
    ) -> ReliableOperationBinding {
        let room_scoped = matches!(
            operation,
            ClientOperation::GameData(..)
                | ClientOperation::Binary(_)
                | ClientOperation::Signal(..)
                | ClientOperation::RawSignal(..)
        );
        match operation {
            ClientOperation::Signal(_, generation, _)
            | ClientOperation::RawSignal(_, generation, _) => {
                if matches!(generation, SignalGeneration::Current) {
                    *generation = SignalGeneration::Bound {
                        generation: self.snapshot.session_generation,
                        plan_revision: self.session_plan_revision,
                    };
                }
            }
            _ => {}
        }
        ReliableOperationBinding {
            room_revision: room_scoped.then_some(self.room_revision),
        }
    }

    #[cfg(feature = "tokio-runtime")]
    pub(crate) fn prepare_reliable(
        &self,
        operation: ClientOperation,
        binding: ReliableOperationBinding,
    ) -> crate::error::Result<CoreCommand> {
        if binding
            .room_revision
            .is_some_and(|revision| revision != self.room_revision)
        {
            return Err(crate::SignalFishError::NotInRoom);
        }
        self.prepare(operation)
    }

    pub(crate) fn prepare(&self, operation: ClientOperation) -> crate::error::Result<CoreCommand> {
        self.validate(&operation)?;
        let message = match operation {
            ClientOperation::JoinRoom(params) => ClientMessage::JoinRoom {
                game_name: params.game_name,
                room_code: params.room_code,
                player_name: params.player_name,
                max_players: params.max_players,
                supports_authority: params.supports_authority,
                relay_transport: params.relay_transport,
            },
            ClientOperation::LeaveRoom => ClientMessage::LeaveRoom,
            ClientOperation::GameData(data, delivery) => {
                let (class, key) = match delivery {
                    GameDataDelivery::Reliable => (None, None),
                    GameDataDelivery::Latest { key } => (Some(DeliveryClass::Latest), Some(key)),
                    GameDataDelivery::Volatile => (Some(DeliveryClass::Volatile), None),
                };
                ClientMessage::GameData { data, class, key }
            }
            ClientOperation::Binary(payload) => {
                return Ok(CoreCommand::Binary(payload));
            }
            ClientOperation::SetReady => ClientMessage::PlayerReady,
            ClientOperation::StartGame => ClientMessage::StartGame,
            ClientOperation::RequestAuthority(become_authority) => {
                ClientMessage::AuthorityRequest { become_authority }
            }
            ClientOperation::ProvideConnectionInfo(connection_info) => {
                ClientMessage::ProvideConnectionInfo { connection_info }
            }
            ClientOperation::Reconnect(player_id, room_id, auth_token) => {
                ClientMessage::Reconnect {
                    player_id,
                    room_id,
                    auth_token,
                }
            }
            ClientOperation::JoinAsSpectator(game_name, room_code, spectator_name) => {
                ClientMessage::JoinAsSpectator {
                    game_name,
                    room_code,
                    spectator_name,
                }
            }
            ClientOperation::LeaveSpectator => ClientMessage::LeaveSpectator,
            ClientOperation::Ping => ClientMessage::Ping,
            ClientOperation::Signal(to, requested_generation, signal) => {
                let generation = match requested_generation {
                    SignalGeneration::Current => self.snapshot.session_generation,
                    SignalGeneration::Exact(generation) => generation,
                    #[cfg(feature = "tokio-runtime")]
                    SignalGeneration::Bound { generation, .. } => generation,
                };
                ClientMessage::Signal {
                    to,
                    generation,
                    signal: signal.into(),
                }
            }
            ClientOperation::RawSignal(to, requested_generation, signal) => {
                let generation = match requested_generation {
                    SignalGeneration::Current => self.snapshot.session_generation,
                    SignalGeneration::Exact(generation) => generation,
                    #[cfg(feature = "tokio-runtime")]
                    SignalGeneration::Bound { generation, .. } => generation,
                };
                ClientMessage::Signal {
                    to,
                    generation,
                    signal,
                }
            }
            ClientOperation::TransportStatus(transport, connected) => {
                ClientMessage::TransportStatus {
                    transport,
                    connected,
                }
            }
        };
        Ok(CoreCommand::Message(message))
    }

    fn ensure_v3(&self) -> crate::error::Result<()> {
        if self
            .negotiated_protocol_version()
            .is_some_and(|version| version >= 3)
        {
            return Ok(());
        }
        let mode = if self.protocol_info_seen {
            "relay-only"
        } else {
            "pre-negotiation"
        };
        Err(crate::SignalFishError::ProtocolUnsupported { mode })
    }

    pub(crate) fn record_game_data_sent(&mut self) {
        self.stats.game_data_sent = self.stats.game_data_sent.saturating_add(1);
    }

    pub(crate) fn record_reconnect_admitted(
        &mut self,
        player_id: PlayerId,
        room_id: RoomId,
        token: String,
    ) {
        self.pending_reconnects.push_back(PendingReconnect {
            player_id,
            room_id,
            token,
        });
    }

    pub(crate) fn clear_session(&mut self) {
        self.snapshot.authenticated = false;
        self.snapshot.negotiated_protocol_version = None;
        self.snapshot.effective_game_data_format = None;
        self.snapshot.player_id = None;
        self.snapshot.room_id = None;
        self.snapshot.room_code = None;
        self.snapshot.reconnection_token = None;
        self.snapshot.session_generation = None;
        self.snapshot.quarantined = false;
        self.protocol_info_seen = false;
        self.membership = Membership::None;
        self.room_finalized = false;
        self.room_players.clear();
        self.session_plan_seen = false;
        self.session_peers.clear();
        self.session_transport = None;
        self.retired_generationless_signal_peers.clear();
        self.pending_reconnects.clear();
        #[cfg(feature = "tokio-runtime")]
        self.advance_session_plan_revision();
    }

    pub(crate) fn disconnect(&mut self, reason: Option<String>) -> SignalFishEvent {
        self.accountability.observe_terminal();
        self.snapshot.connected = false;
        self.clear_session();
        SignalFishEvent::Disconnected {
            reason,
            last_server_error: self.last_server_error.take(),
        }
    }

    pub(crate) fn process_frame(&mut self, frame: TransportFrame) -> FrameOutcome {
        match frame {
            TransportFrame::Text(text) => self.process_text(text),
            TransportFrame::Binary(bytes) => self.process_binary(bytes),
        }
    }

    fn process_text(&mut self, text: String) -> FrameOutcome {
        let mut outcome = FrameOutcome::new();
        let server_msg = match serde_json::from_str::<ServerMessage>(&text) {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(
                    "failed to deserialize server message ({} bytes): {error}",
                    text.len()
                );
                let disconnect = self.observe_undecodable(&mut outcome.events);
                self.stats.messages_undecodable = self.stats.messages_undecodable.saturating_add(1);
                outcome
                    .events
                    .push(SignalFishEvent::decode_failed(&text, &error));
                outcome.disconnect = disconnect;
                return outcome;
            }
        };

        if let Err(diagnostic) = self.validate_inbound_message(&server_msg) {
            self.reject_inbound(&mut outcome, diagnostic);
            return outcome;
        }

        let duplicate_protocol_info =
            matches!(server_msg, ServerMessage::ProtocolInfo(_)) && self.protocol_info_seen;
        if let ServerMessage::ProtocolInfo(payload) = &server_msg {
            if !duplicate_protocol_info {
                self.accountability = DeliveryAccountability::new(
                    payload.protocol_version.is_some_and(|version| version >= 3),
                );
            }
        }

        let authoritative_baseline = matches!(
            server_msg,
            ServerMessage::RoomJoined(_)
                | ServerMessage::SpectatorJoined(_)
                | ServerMessage::Reconnected(_)
        );
        let effective_game_data_format = self.effective_game_data_format().unwrap_or_default();
        let validation = if duplicate_protocol_info {
            self.accountability
                .observe_server_message(false)
                .map(|()| GameDataDisposition::Apply)
        } else {
            accountability::validate_server_frame(
                &mut self.accountability,
                &server_msg,
                effective_game_data_format,
                false,
            )
        };

        let (disposition, validation_failed) = match validation {
            Ok(disposition) => {
                if authoritative_baseline {
                    self.snapshot.quarantined = false;
                }
                (disposition, false)
            }
            Err(diagnostic) => {
                self.push_violation(&mut outcome.events, diagnostic);
                if self.violation_policy == ProtocolViolationPolicy::Disconnect {
                    outcome.disconnect = true;
                    return outcome;
                }
                let disposition = if self.violation_policy == ProtocolViolationPolicy::Observe
                    && matches!(server_msg, ServerMessage::GameDataBinary { .. })
                {
                    match accountability::validate_server_message(
                        &mut self.accountability,
                        &server_msg,
                    ) {
                        Ok(disposition) => disposition,
                        Err(diagnostic) => {
                            self.push_violation(&mut outcome.events, diagnostic);
                            GameDataDisposition::Apply
                        }
                    }
                } else if self.violation_policy == ProtocolViolationPolicy::Observe {
                    GameDataDisposition::Apply
                } else {
                    GameDataDisposition::Stale
                };
                (disposition, true)
            }
        };

        if validation_failed
            && (authoritative_baseline
                || self.violation_policy == ProtocolViolationPolicy::Quarantine)
        {
            return outcome;
        }
        if duplicate_protocol_info {
            return outcome;
        }
        if let ServerMessage::Signal {
            from, generation, ..
        } = &server_msg
        {
            if !self.session_plan_seen || self.should_suppress_inbound_signal(*from, *generation) {
                tracing::debug!(
                    ?generation,
                    current_generation = ?self.snapshot.session_generation,
                    "discarding signal for a stale, unknown, or legacy-unfenceable session generation"
                );
                return outcome;
            }
        }
        let is_game_data = matches!(
            server_msg,
            ServerMessage::GameData { .. } | ServerMessage::GameDataBinary { .. }
        );
        if is_game_data
            && (disposition == GameDataDisposition::Stale
                || (self.snapshot.quarantined
                    && self.violation_policy == ProtocolViolationPolicy::Quarantine))
        {
            return outcome;
        }

        self.update_state(&server_msg);
        outcome.events.push(SignalFishEvent::from(server_msg));
        outcome
    }

    fn process_binary(&mut self, bytes: Vec<u8>) -> FrameOutcome {
        let mut outcome = FrameOutcome::new();
        if !self.snapshot.authenticated || self.membership == Membership::None {
            self.reject_inbound(
                &mut outcome,
                format!(
                    "lifecycle violation: binary game data is invalid while authenticated={} and membership={:?}",
                    self.snapshot.authenticated, self.membership
                ),
            );
            return outcome;
        }
        let Some(effective_game_data_format) = self.effective_game_data_format() else {
            self.reject_inbound(
                &mut outcome,
                "lifecycle violation: binary game data arrived before game-data format negotiation completed"
                    .into(),
            );
            return outcome;
        };
        let mut observe_representation_violation = false;
        if let Err(diagnostic) = accountability::validate_physical_binary_allowed(
            &mut self.accountability,
            effective_game_data_format,
        ) {
            self.push_violation(&mut outcome.events, diagnostic);
            match self.violation_policy {
                ProtocolViolationPolicy::Quarantine => return outcome,
                ProtocolViolationPolicy::Disconnect => {
                    outcome.disconnect = true;
                    return outcome;
                }
                ProtocolViolationPolicy::Observe => observe_representation_violation = true,
            }
        }
        let protocol_v3 = self
            .snapshot
            .negotiated_protocol_version
            .is_some_and(|version| version >= 3);
        let server_msg = match decode_binary_server_message(&bytes, protocol_v3) {
            Ok(message) => message,
            Err(error) => {
                let disconnect = self.observe_undecodable(&mut outcome.events);
                self.stats.messages_undecodable = self.stats.messages_undecodable.saturating_add(1);
                outcome.events.push(SignalFishEvent::DecodeFailed {
                    message_type: Some("BinaryGameData".into()),
                    error,
                    raw_prefix: bounded_binary_preview(&bytes),
                });
                outcome.disconnect = disconnect;
                return outcome;
            }
        };

        if let Err(diagnostic) = self.validate_inbound_message(&server_msg) {
            self.reject_inbound(&mut outcome, diagnostic);
            return outcome;
        }

        let validation = if observe_representation_violation {
            accountability::validate_server_message(&mut self.accountability, &server_msg)
        } else {
            accountability::validate_server_frame(
                &mut self.accountability,
                &server_msg,
                effective_game_data_format,
                true,
            )
        };
        let disposition = match validation {
            Ok(disposition) => disposition,
            Err(diagnostic) => {
                self.push_violation(&mut outcome.events, diagnostic);
                if self.violation_policy == ProtocolViolationPolicy::Disconnect {
                    outcome.disconnect = true;
                    return outcome;
                }
                if self.violation_policy == ProtocolViolationPolicy::Observe {
                    GameDataDisposition::Apply
                } else {
                    GameDataDisposition::Stale
                }
            }
        };

        if disposition == GameDataDisposition::Stale
            || (self.snapshot.quarantined
                && self.violation_policy == ProtocolViolationPolicy::Quarantine)
        {
            return outcome;
        }

        self.update_state(&server_msg);
        outcome.events.push(SignalFishEvent::from(server_msg));
        outcome
    }

    fn observe_undecodable(&mut self, events: &mut Vec<SignalFishEvent>) -> bool {
        if let Err(diagnostic) = self.accountability.observe_server_message(false) {
            self.push_violation(events, diagnostic);
            return self.violation_policy == ProtocolViolationPolicy::Disconnect;
        }
        false
    }

    fn push_violation(&mut self, events: &mut Vec<SignalFishEvent>, diagnostic: String) {
        events.push(SignalFishEvent::ProtocolViolation {
            kind: ProtocolViolationKind::from_diagnostic(&diagnostic),
            diagnostic,
        });
        if self.violation_policy == ProtocolViolationPolicy::Quarantine {
            self.snapshot.quarantined = true;
        }
    }

    fn reject_inbound(&mut self, outcome: &mut FrameOutcome, diagnostic: String) {
        self.push_violation(&mut outcome.events, diagnostic);
        outcome.disconnect = self.violation_policy == ProtocolViolationPolicy::Disconnect;
    }

    fn validate_inbound_message(&self, message: &ServerMessage) -> Result<(), String> {
        let authenticated = self.snapshot.authenticated;
        let membership = self.membership;
        let message_name = server_message_name(message);

        let requires_negotiation = !matches!(
            message,
            ServerMessage::Authenticated { .. }
                | ServerMessage::AuthenticationError { .. }
                | ServerMessage::ProtocolInfo(_)
                | ServerMessage::Pong
                | ServerMessage::Error { .. }
        );
        if requires_negotiation && !self.protocol_info_seen {
            return Err(format!(
                "lifecycle violation: {message_name} arrived before ProtocolInfo completed negotiation"
            ));
        }

        let phase_valid = match message {
            ServerMessage::Authenticated { .. } | ServerMessage::AuthenticationError { .. } => {
                !authenticated && membership == Membership::None
            }
            ServerMessage::ProtocolInfo(_) => {
                authenticated && membership == Membership::None && !self.protocol_info_seen
            }
            ServerMessage::RoomJoined(_)
            | ServerMessage::Reconnected(_)
            | ServerMessage::SpectatorJoined(_) => authenticated && membership == Membership::None,
            ServerMessage::RoomJoinFailed { .. }
            | ServerMessage::ReconnectionFailed { .. }
            | ServerMessage::SpectatorJoinFailed { .. } => authenticated,
            ServerMessage::RoomLeft => authenticated && membership == Membership::Player,
            ServerMessage::SpectatorLeft { .. } => {
                authenticated && membership == Membership::Spectator
            }
            ServerMessage::PlayerJoined { .. }
            | ServerMessage::PlayerLeft { .. }
            | ServerMessage::GameData { .. }
            | ServerMessage::GameDataBinary { .. }
            | ServerMessage::AuthorityChanged { .. }
            | ServerMessage::LobbyStateChanged { .. }
            | ServerMessage::GameStarting { .. }
            | ServerMessage::PlayerReconnected { .. }
            | ServerMessage::NewSpectatorJoined { .. }
            | ServerMessage::SpectatorDisconnected { .. }
            | ServerMessage::DeliveryReport(_) => authenticated && membership != Membership::None,
            ServerMessage::Signal { .. }
            | ServerMessage::NewPeer { .. }
            | ServerMessage::SessionPlan(_)
            | ServerMessage::PeerTransportStatus { .. } => {
                authenticated && membership == Membership::Player
            }
            ServerMessage::AuthorityResponse { .. }
            | ServerMessage::GoingAway { .. }
            | ServerMessage::RelayStats { .. } => authenticated,
            ServerMessage::Pong | ServerMessage::Error { .. } => true,
        };

        if !phase_valid {
            return Err(format!(
                "lifecycle violation: {message_name} is invalid while authenticated={authenticated} and membership={membership:?}"
            ));
        }

        let v3_only = matches!(
            message,
            ServerMessage::Signal { .. }
                | ServerMessage::NewPeer { .. }
                | ServerMessage::SessionPlan(_)
                | ServerMessage::PeerTransportStatus { .. }
                | ServerMessage::RelayStats { .. }
                | ServerMessage::GoingAway { .. }
                | ServerMessage::DeliveryReport(_)
        );
        if v3_only
            && self
                .snapshot
                .negotiated_protocol_version
                .is_none_or(|version| version < 3)
        {
            return Err(format!(
                "lifecycle violation: {message_name} requires negotiated protocol v3"
            ));
        }

        match message {
            ServerMessage::ProtocolInfo(payload) => validate_protocol_info_formats(payload),
            ServerMessage::RoomJoined(payload) => {
                validate_local_player_snapshot(payload.player_id, &payload.current_players)
            }
            ServerMessage::SessionPlan(plan) => {
                if !self.room_finalized {
                    return Err(
                        "lifecycle violation: SessionPlan arrived before room finalization".into(),
                    );
                }
                self.validate_session_plan(plan, self.snapshot.player_id, &self.room_players)
            }
            ServerMessage::Signal {
                from, generation, ..
            } => {
                if !self.session_plan_seen {
                    return Err(
                        "lifecycle violation: Signal arrived before an authoritative SessionPlan"
                            .into(),
                    );
                }
                if self.should_suppress_inbound_signal(*from, *generation) {
                    return Ok(());
                }
                if self.session_transport != Some(TransportKind::WebRtc) {
                    return Err(
                        "lifecycle violation: Signal requires an authoritative WebRTC SessionPlan"
                            .into(),
                    );
                }
                if !self.session_peers.contains(from)
                    || !self.room_players.contains(from)
                    || Some(*from) == self.snapshot.player_id
                {
                    return Err(format!(
                        "lifecycle violation: Signal sender {from} is not in the authoritative session peer set"
                    ));
                }
                Ok(())
            }
            ServerMessage::NewPeer { .. }
                if !(self.session_plan_seen
                    && self.session_transport == Some(TransportKind::WebRtc)) =>
            {
                Err(
                    "lifecycle violation: NewPeer requires an authoritative WebRTC SessionPlan"
                        .into(),
                )
            }
            ServerMessage::NewPeer { peer_id, .. } if Some(*peer_id) == self.snapshot.player_id => {
                Err(format!(
                    "lifecycle violation: NewPeer names the local player {peer_id}"
                ))
            }
            ServerMessage::NewPeer { peer_id, .. } if !self.room_players.contains(peer_id) => Err(
                format!("lifecycle violation: NewPeer {peer_id} is not a current room player"),
            ),
            ServerMessage::PeerTransportStatus { peer_id, .. }
                if Some(*peer_id) == self.snapshot.player_id
                    || !self.room_players.contains(peer_id) =>
            {
                Err(format!(
                    "lifecycle violation: PeerTransportStatus {peer_id} is not another current room player"
                ))
            }
            ServerMessage::Reconnected(payload) => {
                validate_local_player_snapshot(payload.player_id, &payload.current_players)?;
                self.validate_reconnected_payload(payload)
            }
            ServerMessage::ReconnectionFailed { .. } if self.pending_reconnects.is_empty() => Err(
                "lifecycle violation: ReconnectionFailed arrived without an admitted Reconnect"
                    .into(),
            ),
            _ => Ok(()),
        }
    }

    fn validate_session_plan(
        &self,
        plan: &SessionPlanPayload,
        local_player_id: Option<PlayerId>,
        room_players: &HashSet<PlayerId>,
    ) -> Result<(), String> {
        if plan.fallback != TransportKind::Relay {
            return Err("lifecycle violation: SessionPlan fallback must be relay".into());
        }

        let canonical_shape = match (plan.topology, plan.transport) {
            (Topology::Relay, TransportKind::Relay) => {
                plan.host.is_none()
                    && plan.direct_endpoint.is_none()
                    && plan.peers.is_empty()
                    && plan.ice_servers.is_empty()
            }
            (Topology::Host, TransportKind::Direct) => {
                plan.host.is_some()
                    && plan.direct_endpoint.as_ref().is_some_and(|endpoint| {
                        direct_host_is_usable(&endpoint.host) && endpoint.port != 0
                    })
                    && plan.ice_servers.is_empty()
            }
            (Topology::Host, TransportKind::WebRtc) => {
                plan.host.is_some() && plan.direct_endpoint.is_none()
            }
            (Topology::Mesh, TransportKind::WebRtc) => {
                plan.host.is_none() && plan.direct_endpoint.is_none()
            }
            _ => false,
        };
        if !canonical_shape {
            return Err(format!(
                "lifecycle violation: SessionPlan has a noncanonical {:?}+{:?} cross-field shape",
                plan.topology, plan.transport
            ));
        }

        let mut peer_ids = HashSet::with_capacity(plan.peers.len());
        if plan.host.is_some_and(|host| !room_players.contains(&host)) {
            return Err(
                "lifecycle violation: SessionPlan host is not a current room player".into(),
            );
        }
        for peer in &plan.peers {
            if Some(peer.player_id) == local_player_id {
                return Err(format!(
                    "lifecycle violation: SessionPlan peers contains the local player {}",
                    peer.player_id
                ));
            }
            if !peer_ids.insert(peer.player_id) {
                return Err(format!(
                    "lifecycle violation: SessionPlan contains duplicate peer {}",
                    peer.player_id
                ));
            }
            if !room_players.contains(&peer.player_id) {
                return Err(format!(
                    "lifecycle violation: SessionPlan peer {} is not a current room player",
                    peer.player_id
                ));
            }
        }

        if plan.topology == Topology::Host
            && plan.host != local_player_id
            && !(plan.peers.is_empty()
                || (plan.peers.len() == 1
                    && plan.peers.first().map(|peer| peer.player_id) == plan.host))
        {
            return Err(
                "lifecycle violation: non-host SessionPlan peers must be empty or contain only the elected host"
                    .into(),
            );
        }

        Ok(())
    }

    fn should_suppress_inbound_signal(
        &self,
        from: PlayerId,
        generation: Option<SessionGeneration>,
    ) -> bool {
        generation != self.snapshot.session_generation
            // Server 0.4 omitted generations. Retained peer identity is the
            // only fence for a signal racing a generation-less re-plan.
            || (generation.is_none()
                && self.snapshot.session_generation.is_none()
                && self.retired_generationless_signal_peers.contains(&from)
                && !(self.session_transport == Some(TransportKind::WebRtc)
                    && self.session_peers.contains(&from)))
    }

    fn validate_reconnect_replay(
        &self,
        payload: &crate::protocol::ReconnectedPayload,
    ) -> Result<(), String> {
        let protocol_v3 = self
            .snapshot
            .negotiated_protocol_version
            .is_some_and(|version| version >= 3);
        for message in &payload.missed_events {
            if !matches!(
                message,
                ServerMessage::PlayerJoined { .. }
                    | ServerMessage::PlayerLeft { .. }
                    | ServerMessage::PlayerReconnected { .. }
                    | ServerMessage::NewSpectatorJoined { .. }
                    | ServerMessage::SpectatorDisconnected { .. }
                    | ServerMessage::LobbyStateChanged { .. }
                    | ServerMessage::AuthorityChanged { .. }
            ) {
                return Err(format!(
                    "lifecycle violation: Reconnected missed_events contains non-replayable {}",
                    server_message_name(message)
                ));
            }
            let valid_stamp = match message {
                ServerMessage::PlayerJoined { player } => {
                    player.id != payload.player_id
                        && if protocol_v3 {
                            player.epoch.is_some_and(|epoch| epoch > 0) && player.seq.is_some()
                        } else {
                            player.epoch.is_none() && player.seq.is_none()
                        }
                }
                ServerMessage::PlayerLeft {
                    player_id,
                    epoch,
                    final_seq,
                } => {
                    *player_id != payload.player_id
                        && if protocol_v3 {
                            epoch.is_some_and(|epoch| epoch > 0) && final_seq.is_some()
                        } else {
                            epoch.is_none() && final_seq.is_none()
                        }
                }
                ServerMessage::PlayerReconnected { player_id, epoch } => {
                    *player_id != payload.player_id
                        && if protocol_v3 {
                            epoch.is_some_and(|epoch| epoch > 0)
                        } else {
                            epoch.is_none()
                        }
                }
                _ => true,
            };
            if !valid_stamp {
                return Err(format!(
                    "lifecycle violation: replayed {} has invalid self/version metadata",
                    server_message_name(message)
                ));
            }
        }
        Ok(())
    }

    fn validate_reconnected_payload(
        &self,
        payload: &crate::protocol::ReconnectedPayload,
    ) -> Result<(), String> {
        let Some(pending) = self.pending_reconnects.front() else {
            return Err(
                "lifecycle violation: Reconnected arrived without an admitted Reconnect".into(),
            );
        };
        if payload.player_id != pending.player_id || payload.room_id != pending.room_id {
            return Err(
                "lifecycle violation: Reconnected identity did not match the admitted Reconnect"
                    .into(),
            );
        }
        let protocol_v3 = self
            .snapshot
            .negotiated_protocol_version
            .is_some_and(|version| version >= 3);
        if protocol_v3 {
            if payload.replay.is_none() {
                return Err(
                    "lifecycle violation: v3 Reconnected omitted replay completeness".into(),
                );
            }
            let Some(token) = payload
                .reconnection_token
                .as_deref()
                .filter(|token| !token.is_empty())
            else {
                return Err(
                    "lifecycle violation: v3 Reconnected omitted a nonempty rotated reconnection token"
                        .into(),
                );
            };
            if pending.token == token {
                return Err(
                    "lifecycle violation: v3 Reconnected did not rotate the submitted reconnection token"
                        .into(),
                );
            }
        } else if payload.replay.is_some()
            || !payload.sender_watermarks.is_empty()
            || payload.reconnection_token.is_some()
        {
            return Err(
                "lifecycle violation: v2 Reconnected exposed v3 replay, watermark, or token metadata"
                    .into(),
            );
        }
        self.validate_reconnect_replay(payload)
    }

    fn update_state(&mut self, message: &ServerMessage) {
        match message {
            ServerMessage::Authenticated { .. } => self.snapshot.authenticated = true,
            ServerMessage::Error {
                message,
                error_code,
            } => {
                self.last_server_error = Some(ServerErrorInfo {
                    message: message.clone(),
                    error_code: error_code.clone(),
                });
            }
            ServerMessage::AuthenticationError { error, error_code } => {
                self.last_server_error = Some(ServerErrorInfo {
                    message: error.clone(),
                    error_code: Some(error_code.clone()),
                });
            }
            ServerMessage::ProtocolInfo(payload) => {
                self.snapshot.negotiated_protocol_version =
                    payload.protocol_version.filter(|version| *version >= 3);
                self.snapshot.effective_game_data_format =
                    Some(resolve_effective_game_data_format(
                        self.requested_game_data_format(),
                        &payload.game_data_formats,
                    ));
                self.protocol_info_seen = true;
            }
            ServerMessage::RoomJoined(payload) => {
                self.set_room(RoomBaseline {
                    player_id: payload.player_id,
                    room_id: payload.room_id,
                    room_code: payload.room_code.clone(),
                    reconnection_token: payload.reconnection_token.clone(),
                    membership: Membership::Player,
                    finalized: payload.lobby_state == crate::protocol::LobbyState::Finalized,
                    players: payload
                        .current_players
                        .iter()
                        .map(|player| player.id)
                        .collect(),
                });
            }
            ServerMessage::RoomLeft => self.clear_room(),
            ServerMessage::Reconnected(payload) => {
                self.pending_reconnects.pop_front();
                self.set_room(RoomBaseline {
                    player_id: payload.player_id,
                    room_id: payload.room_id,
                    room_code: payload.room_code.clone(),
                    reconnection_token: payload.reconnection_token.clone(),
                    membership: Membership::Player,
                    finalized: payload.lobby_state == crate::protocol::LobbyState::Finalized,
                    players: payload
                        .current_players
                        .iter()
                        .map(|player| player.id)
                        .collect(),
                });
            }
            ServerMessage::SpectatorJoined(payload) => {
                self.set_room(RoomBaseline {
                    player_id: payload.spectator_id,
                    room_id: payload.room_id,
                    room_code: payload.room_code.clone(),
                    reconnection_token: None,
                    membership: Membership::Spectator,
                    finalized: payload.lobby_state == crate::protocol::LobbyState::Finalized,
                    players: payload
                        .current_players
                        .iter()
                        .map(|player| player.id)
                        .collect(),
                });
            }
            ServerMessage::SpectatorLeft { .. } => self.clear_room(),
            ServerMessage::ReconnectionFailed { .. } => {
                self.pending_reconnects.pop_front();
            }
            ServerMessage::SessionPlan(payload) => {
                self.replace_session_plan(
                    payload.generation,
                    payload.peers.iter().map(|peer| peer.player_id),
                    payload.transport,
                );
            }
            ServerMessage::NewPeer { peer_id, .. } => {
                self.session_peers.insert(*peer_id);
                if self.snapshot.session_generation.is_none()
                    && self.session_transport == Some(TransportKind::WebRtc)
                {
                    self.retired_generationless_signal_peers.remove(peer_id);
                }
            }
            ServerMessage::PlayerLeft { player_id, .. } => {
                if self.snapshot.session_generation.is_none()
                    && self.session_transport == Some(TransportKind::WebRtc)
                    && self.session_peers.contains(player_id)
                {
                    self.retired_generationless_signal_peers.insert(*player_id);
                }
                self.session_peers.remove(player_id);
                self.room_players.remove(player_id);
            }
            ServerMessage::PlayerJoined { player } => {
                self.room_players.insert(player.id);
            }
            ServerMessage::LobbyStateChanged { lobby_state, .. } => {
                self.room_finalized = *lobby_state == crate::protocol::LobbyState::Finalized;
            }
            ServerMessage::GameStarting { .. } => {
                self.room_finalized = true;
            }
            ServerMessage::GameData { .. } | ServerMessage::GameDataBinary { .. } => {
                self.stats.game_data_received = self.stats.game_data_received.saturating_add(1);
            }
            _ => {}
        }
    }

    fn set_room(&mut self, baseline: RoomBaseline) {
        self.snapshot.player_id = Some(baseline.player_id);
        self.snapshot.room_id = Some(baseline.room_id);
        self.snapshot.room_code = Some(baseline.room_code);
        self.snapshot.reconnection_token = baseline.reconnection_token;
        self.snapshot.session_generation = None;
        self.snapshot.quarantined = false;
        self.membership = baseline.membership;
        self.room_finalized = baseline.finalized;
        self.room_players = baseline.players;
        self.session_plan_seen = false;
        self.session_peers.clear();
        self.session_transport = None;
        self.retired_generationless_signal_peers.clear();
        #[cfg(feature = "tokio-runtime")]
        self.advance_room_revision();
    }

    fn clear_room(&mut self) {
        self.accountability.reset_room();
        self.snapshot.room_id = None;
        self.snapshot.room_code = None;
        self.snapshot.reconnection_token = None;
        self.snapshot.session_generation = None;
        self.snapshot.quarantined = false;
        self.membership = Membership::None;
        self.room_finalized = false;
        self.room_players.clear();
        self.session_plan_seen = false;
        self.session_peers.clear();
        self.session_transport = None;
        self.retired_generationless_signal_peers.clear();
        #[cfg(feature = "tokio-runtime")]
        self.advance_room_revision();
    }

    fn replace_session_plan(
        &mut self,
        generation: Option<SessionGeneration>,
        peers: impl IntoIterator<Item = PlayerId>,
        transport: TransportKind,
    ) {
        if self.session_plan_seen
            && self.snapshot.session_generation.is_none()
            && self.session_transport == Some(TransportKind::WebRtc)
        {
            self.retired_generationless_signal_peers
                .extend(self.session_peers.iter().copied());
        }
        let peers: HashSet<_> = peers.into_iter().collect();
        if generation.is_none() && transport == TransportKind::WebRtc {
            self.retired_generationless_signal_peers
                .retain(|peer| !peers.contains(peer));
        }
        self.snapshot.session_generation = generation;
        self.session_plan_seen = true;
        self.session_peers = peers;
        self.session_transport = Some(transport);
        #[cfg(feature = "tokio-runtime")]
        self.advance_session_plan_revision();
    }

    #[cfg(feature = "tokio-runtime")]
    fn advance_session_plan_revision(&mut self) {
        self.session_plan_revision = self.session_plan_revision.wrapping_add(1);
    }

    #[cfg(feature = "tokio-runtime")]
    fn advance_room_revision(&mut self) {
        self.room_revision = self.room_revision.wrapping_add(1);
        self.advance_session_plan_revision();
    }
}

fn validate_protocol_info_formats(
    payload: &crate::protocol::ProtocolInfoPayload,
) -> Result<(), String> {
    match payload.game_data_formats.as_slice() {
        [GameDataEncoding::Json]
        | [GameDataEncoding::Json, GameDataEncoding::MessagePack] => Ok(()),
        formats => Err(format!(
            "lifecycle violation: ProtocolInfo game_data_formats {formats:?} does not match the canonical Server 0.7 negotiation order [Json, MessagePack?]"
        )),
    }?;
    match (
        payload.protocol_version,
        payload.min_protocol_version,
        payload.max_protocol_version,
    ) {
        (None, None, None) if payload.transports.is_none() => Ok(()),
        (Some(version), Some(min), Some(max))
            if version >= 3
                && min >= 2
                && min <= version
                && version <= max
                && payload.transports.as_deref() == Some(&[crate::protocol::MessageTransport::Websocket]) =>
        {
            Ok(())
        }
        version_tuple => Err(format!(
            "lifecycle violation: ProtocolInfo version range {version_tuple:?} and transports presence do not form a coherent v2/v3 negotiation"
        )),
    }
}

fn resolve_effective_game_data_format(
    requested: Option<GameDataEncoding>,
    advertised: &[GameDataEncoding],
) -> GameDataEncoding {
    requested
        .filter(|format| advertised.contains(format))
        .unwrap_or(GameDataEncoding::Json)
}

fn validate_local_player_snapshot(
    local_player_id: PlayerId,
    players: &[crate::protocol::PlayerInfo],
) -> Result<(), String> {
    if players
        .iter()
        .filter(|player| player.id == local_player_id)
        .count()
        != 1
    {
        return Err(format!(
            "lifecycle violation: authoritative player snapshot must contain local player {local_player_id} exactly once"
        ));
    }
    Ok(())
}

fn server_message_name(message: &ServerMessage) -> &'static str {
    match message {
        ServerMessage::Authenticated { .. } => "Authenticated",
        ServerMessage::ProtocolInfo(_) => "ProtocolInfo",
        ServerMessage::AuthenticationError { .. } => "AuthenticationError",
        ServerMessage::RoomJoined(_) => "RoomJoined",
        ServerMessage::RoomJoinFailed { .. } => "RoomJoinFailed",
        ServerMessage::RoomLeft => "RoomLeft",
        ServerMessage::PlayerJoined { .. } => "PlayerJoined",
        ServerMessage::PlayerLeft { .. } => "PlayerLeft",
        ServerMessage::GameData { .. } => "GameData",
        ServerMessage::GameDataBinary { .. } => "GameDataBinary",
        ServerMessage::AuthorityChanged { .. } => "AuthorityChanged",
        ServerMessage::AuthorityResponse { .. } => "AuthorityResponse",
        ServerMessage::LobbyStateChanged { .. } => "LobbyStateChanged",
        ServerMessage::GameStarting { .. } => "GameStarting",
        ServerMessage::Pong => "Pong",
        ServerMessage::Reconnected(_) => "Reconnected",
        ServerMessage::ReconnectionFailed { .. } => "ReconnectionFailed",
        ServerMessage::PlayerReconnected { .. } => "PlayerReconnected",
        ServerMessage::SpectatorJoined(_) => "SpectatorJoined",
        ServerMessage::SpectatorJoinFailed { .. } => "SpectatorJoinFailed",
        ServerMessage::SpectatorLeft { .. } => "SpectatorLeft",
        ServerMessage::NewSpectatorJoined { .. } => "NewSpectatorJoined",
        ServerMessage::SpectatorDisconnected { .. } => "SpectatorDisconnected",
        ServerMessage::Error { .. } => "Error",
        ServerMessage::Signal { .. } => "Signal",
        ServerMessage::NewPeer { .. } => "NewPeer",
        ServerMessage::SessionPlan(_) => "SessionPlan",
        ServerMessage::PeerTransportStatus { .. } => "PeerTransportStatus",
        ServerMessage::RelayStats { .. } => "RelayStats",
        ServerMessage::GoingAway { .. } => "GoingAway",
        ServerMessage::DeliveryReport(_) => "DeliveryReport",
    }
}

fn direct_host_is_usable(host: &str) -> bool {
    use std::net::IpAddr;

    if host.is_empty() || host.len() > 253 || host.trim() != host {
        return false;
    }

    let hostname = host.strip_suffix('.').unwrap_or(host);
    if let Ok(address) = hostname.parse::<IpAddr>() {
        return hostname == host && !address.is_unspecified();
    }

    !hostname.is_empty()
        && hostname.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]
mod tests {
    use super::*;
    use crate::protocol::{
        DirectEndpoint, IceServer, LobbyState, MessageTransport, PlayerInfo, ProtocolInfoPayload,
        RateLimitInfo, ReconnectedPayload, ReplayStatus, RoomJoinedPayload, SenderWatermark,
        SessionPeer,
    };

    const LOCAL: u128 = 1;
    const PEER: u128 = 2;
    const GENERATION: u128 = 3;

    fn process(core: &mut ClientCore, message: ServerMessage) -> FrameOutcome {
        core.process_frame(TransportFrame::Text(
            serde_json::to_string(&message).expect("server message fixture should serialize"),
        ))
    }

    fn authenticated() -> ServerMessage {
        ServerMessage::Authenticated {
            app_name: "test".into(),
            organization: None,
            rate_limits: RateLimitInfo {
                per_minute: 60,
                per_hour: 1_000,
                per_day: 10_000,
            },
        }
    }

    fn protocol_info(version: Option<u16>) -> ServerMessage {
        ServerMessage::ProtocolInfo(ProtocolInfoPayload {
            platform: None,
            sdk_version: None,
            minimum_version: None,
            recommended_version: None,
            capabilities: vec![],
            notes: None,
            game_data_formats: vec![GameDataEncoding::Json],
            player_name_rules: None,
            protocol_version: version,
            min_protocol_version: version.map(|_| 2),
            max_protocol_version: version,
            transports: version.map(|_| vec![MessageTransport::Websocket]),
        })
    }

    fn player(id: u128) -> PlayerInfo {
        PlayerInfo {
            id: PlayerId::from_u128(id),
            name: format!("player-{id}"),
            is_authority: id == LOCAL,
            is_ready: false,
            connected_at: "2026-01-01T00:00:00Z".into(),
            connection_info: None,
            epoch: Some(1),
            seq: Some(0),
        }
    }

    fn room_joined() -> ServerMessage {
        ServerMessage::RoomJoined(Box::new(RoomJoinedPayload {
            room_id: RoomId::from_u128(10),
            room_code: "ROOM".into(),
            player_id: PlayerId::from_u128(LOCAL),
            game_name: "game".into(),
            max_players: 4,
            supports_authority: true,
            current_players: vec![player(LOCAL), player(PEER), player(4)],
            is_authority: true,
            lobby_state: LobbyState::Finalized,
            ready_players: vec![],
            relay_type: "websocket".into(),
            current_spectators: vec![],
            ice_servers: vec![],
            reconnection_token: Some("token".into()),
        }))
    }

    fn room_joined_v2() -> ServerMessage {
        let ServerMessage::RoomJoined(mut payload) = room_joined() else {
            unreachable!("room_joined helper always returns RoomJoined")
        };
        for player in &mut payload.current_players {
            player.epoch = None;
            player.seq = None;
        }
        ServerMessage::RoomJoined(payload)
    }

    fn reconnected(missed_events: Vec<ServerMessage>) -> ServerMessage {
        ServerMessage::Reconnected(Box::new(ReconnectedPayload {
            room_id: RoomId::from_u128(10),
            room_code: "ROOM".into(),
            player_id: PlayerId::from_u128(LOCAL),
            game_name: "game".into(),
            max_players: 4,
            supports_authority: true,
            current_players: vec![player(LOCAL), player(PEER), player(4)],
            is_authority: true,
            lobby_state: LobbyState::Finalized,
            ready_players: vec![],
            relay_type: "websocket".into(),
            current_spectators: vec![],
            ice_servers: vec![],
            missed_events,
            replay: Some(ReplayStatus::Complete),
            sender_watermarks: [LOCAL, PEER, 4]
                .into_iter()
                .map(|id| SenderWatermark {
                    player_id: PlayerId::from_u128(id),
                    epoch: 1,
                    seq: 0,
                })
                .collect(),
            reconnection_token: Some("rotated-token".into()),
        }))
    }

    fn reconnected_v2() -> ServerMessage {
        let ServerMessage::Reconnected(mut payload) = reconnected(vec![]) else {
            unreachable!("reconnected helper always returns Reconnected")
        };
        for player in &mut payload.current_players {
            player.epoch = None;
            player.seq = None;
        }
        payload.replay = None;
        payload.sender_watermarks.clear();
        payload.reconnection_token = None;
        ServerMessage::Reconnected(payload)
    }

    fn v3_room(policy: ProtocolViolationPolicy) -> ClientCore {
        let mut core = ClientCore::new(Some(GameDataEncoding::Json), policy, true);
        assert_eq!(process(&mut core, authenticated()).events.len(), 1);
        assert_eq!(process(&mut core, protocol_info(Some(3))).events.len(), 1);
        assert_eq!(process(&mut core, room_joined()).events.len(), 1);
        core
    }

    fn peer(id: u128) -> SessionPeer {
        SessionPeer {
            player_id: PlayerId::from_u128(id),
            player_name: format!("peer-{id}"),
            is_authority: false,
            initiate: true,
        }
    }

    fn plan(topology: Topology, transport: TransportKind) -> SessionPlanPayload {
        let host = (topology == Topology::Host).then(|| PlayerId::from_u128(PEER));
        let direct_endpoint = (transport == TransportKind::Direct).then(|| DirectEndpoint {
            host: "192.0.2.10".into(),
            port: 7_777,
        });
        let peers = if topology == Topology::Relay {
            vec![]
        } else {
            vec![peer(PEER)]
        };
        SessionPlanPayload {
            generation: Some(SessionGeneration::from_u128(GENERATION)),
            topology,
            transport,
            host,
            direct_endpoint,
            peers,
            ice_servers: if transport == TransportKind::WebRtc {
                vec![IceServer {
                    urls: vec!["stun:example.test".into()],
                    username: None,
                    credential: None,
                }]
            } else {
                vec![]
            },
            fallback: TransportKind::Relay,
        }
    }

    fn assert_lifecycle_violation(outcome: &FrameOutcome) {
        assert_eq!(outcome.events.len(), 1, "{:#?}", outcome.events);
        assert!(matches!(
            &outcome.events[0],
            SignalFishEvent::ProtocolViolation {
                kind: ProtocolViolationKind::Lifecycle,
                ..
            }
        ));
    }

    #[test]
    fn lifecycle_classifier_rejects_pre_auth_pre_room_post_room_and_v2_v3_mismatches() {
        let cases = [
            ("pre-auth room message", vec![], room_joined()),
            (
                "authenticated pre-room room message",
                vec![authenticated(), protocol_info(Some(3))],
                ServerMessage::PlayerLeft {
                    player_id: PlayerId::from_u128(PEER),
                    epoch: Some(1),
                    final_seq: Some(0),
                },
            ),
            (
                "post-room room message",
                vec![
                    authenticated(),
                    protocol_info(Some(3)),
                    room_joined(),
                    ServerMessage::RoomLeft,
                ],
                ServerMessage::SessionPlan(Box::new(plan(Topology::Mesh, TransportKind::WebRtc))),
            ),
            (
                "v3 message under v2",
                vec![authenticated(), protocol_info(None), room_joined_v2()],
                ServerMessage::SessionPlan(Box::new(plan(Topology::Mesh, TransportKind::WebRtc))),
            ),
        ];

        for (name, prefix, invalid) in cases {
            let mut core = ClientCore::new(
                Some(GameDataEncoding::Json),
                ProtocolViolationPolicy::Observe,
                true,
            );
            for message in prefix {
                let _ = process(&mut core, message);
            }
            let before = core.snapshot();
            let outcome = process(&mut core, invalid);
            assert_lifecycle_violation(&outcome);
            assert!(!outcome.disconnect, "{name}");
            assert_eq!(core.snapshot(), before, "{name}");
        }
    }

    #[test]
    fn invalid_lifecycle_policy_controls_quarantine_and_disconnect_but_never_applies() {
        for (policy, quarantined, disconnect) in [
            (ProtocolViolationPolicy::Quarantine, true, false),
            (ProtocolViolationPolicy::Disconnect, false, true),
            (ProtocolViolationPolicy::Observe, false, false),
        ] {
            let mut core = v3_room(policy);
            let generation_before = core.snapshot().session_generation;
            let outcome = process(&mut core, authenticated());
            assert_lifecycle_violation(&outcome);
            assert_eq!(outcome.disconnect, disconnect, "{policy:?}");
            assert_eq!(core.snapshot().quarantined, quarantined, "{policy:?}");
            assert_eq!(core.snapshot().session_generation, generation_before);
            assert!(core.snapshot().authenticated);
        }
    }

    #[test]
    fn session_plan_topology_transport_cross_product_accepts_only_four_pairs() {
        let valid = [
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
                let mut core = v3_room(ProtocolViolationPolicy::Observe);
                let outcome = process(
                    &mut core,
                    ServerMessage::SessionPlan(Box::new(plan(topology, transport))),
                );
                if valid.contains(&(topology, transport)) {
                    assert!(matches!(
                        outcome.events.as_slice(),
                        [SignalFishEvent::SessionPlan { .. }]
                    ));
                } else {
                    assert_lifecycle_violation(&outcome);
                    assert!(core.snapshot().session_generation.is_none());
                }
            }
        }
    }

    #[test]
    fn session_plan_cross_fields_and_peer_identity_are_transactional() {
        let mut invalid_plans = Vec::new();

        let mut fallback = plan(Topology::Mesh, TransportKind::WebRtc);
        fallback.fallback = TransportKind::Direct;
        invalid_plans.push(fallback);

        let mut missing_host = plan(Topology::Host, TransportKind::WebRtc);
        missing_host.host = None;
        invalid_plans.push(missing_host);

        let mut relay_with_peers = plan(Topology::Relay, TransportKind::Relay);
        relay_with_peers.peers.push(peer(PEER));
        invalid_plans.push(relay_with_peers);

        let mut direct_with_ice = plan(Topology::Host, TransportKind::Direct);
        direct_with_ice.ice_servers.push(IceServer {
            urls: vec!["stun:example.test".into()],
            username: None,
            credential: None,
        });
        invalid_plans.push(direct_with_ice);

        let mut self_peer = plan(Topology::Mesh, TransportKind::WebRtc);
        self_peer.peers.push(peer(LOCAL));
        invalid_plans.push(self_peer);

        let mut duplicate_peer = plan(Topology::Mesh, TransportKind::WebRtc);
        duplicate_peer.peers.push(peer(PEER));
        invalid_plans.push(duplicate_peer);

        let mut non_room_peer = plan(Topology::Mesh, TransportKind::WebRtc);
        non_room_peer.peers.push(peer(99));
        invalid_plans.push(non_room_peer);

        let mut non_host_with_extra_peer = plan(Topology::Host, TransportKind::WebRtc);
        non_host_with_extra_peer.peers.push(peer(4));
        invalid_plans.push(non_host_with_extra_peer);

        let mut invalid_endpoint = plan(Topology::Host, TransportKind::Direct);
        invalid_endpoint.direct_endpoint = Some(DirectEndpoint {
            host: "0.0.0.0".into(),
            port: 7_777,
        });
        invalid_plans.push(invalid_endpoint);

        for host in [
            "",
            " example.test",
            "example.test ",
            "::",
            "bad..example",
            "-bad.example",
            "bad-.example",
            "bad_name.example",
        ] {
            let mut invalid_endpoint = plan(Topology::Host, TransportKind::Direct);
            invalid_endpoint.direct_endpoint = Some(DirectEndpoint {
                host: host.into(),
                port: 7_777,
            });
            invalid_plans.push(invalid_endpoint);
        }

        for host in ["a".repeat(64), "a".repeat(254)] {
            let mut invalid_endpoint = plan(Topology::Host, TransportKind::Direct);
            invalid_endpoint.direct_endpoint = Some(DirectEndpoint { host, port: 7_777 });
            invalid_plans.push(invalid_endpoint);
        }

        let mut zero_port = plan(Topology::Host, TransportKind::Direct);
        zero_port.direct_endpoint = Some(DirectEndpoint {
            host: "example.test".into(),
            port: 0,
        });
        invalid_plans.push(zero_port);

        let mut generationless_invalid = plan(Topology::Mesh, TransportKind::WebRtc);
        generationless_invalid.generation = None;
        generationless_invalid.fallback = TransportKind::Direct;
        invalid_plans.push(generationless_invalid);

        for invalid in invalid_plans {
            let mut core = v3_room(ProtocolViolationPolicy::Observe);
            let _ = process(
                &mut core,
                ServerMessage::SessionPlan(Box::new(plan(Topology::Mesh, TransportKind::WebRtc))),
            );
            let before = core.snapshot();
            let peers_before = core.session_peers.clone();
            let transport_before = core.session_transport;
            let outcome = process(&mut core, ServerMessage::SessionPlan(Box::new(invalid)));
            assert_lifecycle_violation(&outcome);
            assert_eq!(core.snapshot(), before);
            assert_eq!(core.session_peers, peers_before);
            assert_eq!(core.session_transport, transport_before);
        }
    }

    #[test]
    fn player_room_baseline_requires_the_local_player_exactly_once() {
        for policy in [
            ProtocolViolationPolicy::Quarantine,
            ProtocolViolationPolicy::Disconnect,
            ProtocolViolationPolicy::Observe,
        ] {
            for local_count in [0, 2] {
                let mut core = ClientCore::new(Some(GameDataEncoding::Json), policy, true);
                let _ = process(&mut core, authenticated());
                let _ = process(&mut core, protocol_info(Some(3)));
                core.record_reconnect_admitted(
                    PlayerId::from_u128(LOCAL),
                    RoomId::from_u128(10),
                    "submitted-token".into(),
                );
                let before = core.snapshot();
                let stats_before = core.stats();
                let ServerMessage::RoomJoined(mut payload) = room_joined() else {
                    unreachable!("room_joined helper always returns RoomJoined")
                };
                payload
                    .current_players
                    .retain(|entry| entry.id != payload.player_id);
                for _ in 0..local_count {
                    payload.current_players.push(player(LOCAL));
                }

                let outcome = process(&mut core, ServerMessage::RoomJoined(payload));
                assert_lifecycle_violation(&outcome);
                assert_eq!(
                    outcome.disconnect,
                    policy == ProtocolViolationPolicy::Disconnect
                );
                assert_eq!(core.snapshot().room_id, before.room_id);
                assert_eq!(core.snapshot().player_id, before.player_id);
                assert_eq!(
                    core.snapshot().session_generation,
                    before.session_generation
                );
                assert_eq!(core.stats(), stats_before);
                assert!(core.session_peers.is_empty());
            }
        }
    }

    #[test]
    fn generationless_server_04_plan_remains_valid_when_its_shape_is_canonical() {
        let mut core = v3_room(ProtocolViolationPolicy::Observe);
        let mut legacy = plan(Topology::Mesh, TransportKind::WebRtc);
        legacy.generation = None;
        let outcome = process(&mut core, ServerMessage::SessionPlan(Box::new(legacy)));
        assert!(matches!(
            outcome.events.as_slice(),
            [SignalFishEvent::SessionPlan {
                generation: None,
                ..
            }]
        ));
    }

    #[test]
    fn signal_peer_membership_is_shared_by_inbound_and_outbound_paths() {
        let mut core = v3_room(ProtocolViolationPolicy::Observe);
        let valid_plan = plan(Topology::Mesh, TransportKind::WebRtc);
        let _ = process(&mut core, ServerMessage::SessionPlan(Box::new(valid_plan)));

        let accepted = process(
            &mut core,
            ServerMessage::Signal {
                from: PlayerId::from_u128(PEER),
                generation: Some(SessionGeneration::from_u128(GENERATION)),
                signal: serde_json::json!({"Offer": "sdp"}),
            },
        );
        assert!(matches!(
            accepted.events.as_slice(),
            [SignalFishEvent::SignalReceived { .. }]
        ));

        let unknown = PlayerId::from_u128(4);
        let rejected = process(
            &mut core,
            ServerMessage::Signal {
                from: unknown,
                generation: Some(SessionGeneration::from_u128(GENERATION)),
                signal: serde_json::json!({"Offer": "sdp"}),
            },
        );
        assert_lifecycle_violation(&rejected);

        let error = core
            .prepare(ClientOperation::Signal(
                unknown,
                SignalGeneration::Current,
                PeerSignal::Offer("sdp".into()),
            ))
            .expect_err("off-plan outbound signal must fail locally");
        assert!(matches!(
            error,
            crate::SignalFishError::SessionPlanUnavailable
        ));
    }

    #[test]
    fn replacement_plan_and_player_left_remove_signal_authority() {
        let mut core = v3_room(ProtocolViolationPolicy::Observe);
        let _ = process(
            &mut core,
            ServerMessage::SessionPlan(Box::new(plan(Topology::Mesh, TransportKind::WebRtc))),
        );
        let _ = process(
            &mut core,
            ServerMessage::PlayerLeft {
                player_id: PlayerId::from_u128(PEER),
                epoch: Some(1),
                final_seq: Some(0),
            },
        );
        assert!(matches!(
            core.prepare(ClientOperation::Signal(
                PlayerId::from_u128(PEER),
                SignalGeneration::Current,
                PeerSignal::Offer("sdp".into()),
            )),
            Err(crate::SignalFishError::SessionPlanUnavailable)
        ));

        let new_peer = process(
            &mut core,
            ServerMessage::NewPeer {
                peer_id: PlayerId::from_u128(4),
                you_initiate: false,
            },
        );
        assert!(matches!(
            new_peer.events.as_slice(),
            [SignalFishEvent::NewPeer { .. }]
        ));
        assert!(core
            .prepare(ClientOperation::Signal(
                PlayerId::from_u128(4),
                SignalGeneration::Current,
                PeerSignal::Offer("sdp".into()),
            ))
            .is_ok());
        let mut replacement = plan(Topology::Relay, TransportKind::Relay);
        replacement.generation = Some(SessionGeneration::from_u128(5));
        let _ = process(&mut core, ServerMessage::SessionPlan(Box::new(replacement)));
        assert!(core.session_peers.is_empty());
    }

    #[test]
    fn reconnect_replay_rejects_non_replayable_session_messages_atomically() {
        let invalid = [
            ServerMessage::SessionPlan(Box::new(plan(Topology::Mesh, TransportKind::WebRtc))),
            ServerMessage::Signal {
                from: PlayerId::from_u128(PEER),
                generation: Some(SessionGeneration::from_u128(GENERATION)),
                signal: serde_json::json!({"Offer": "sdp"}),
            },
            ServerMessage::NewPeer {
                peer_id: PlayerId::from_u128(4),
                you_initiate: true,
            },
            ServerMessage::GameData {
                from_player: PlayerId::from_u128(PEER),
                data: serde_json::json!({"invalid": true}),
                seq: Some(1),
                epoch: Some(1),
                class: Some(DeliveryClass::Reliable),
                key: None,
            },
        ];

        for policy in [
            ProtocolViolationPolicy::Quarantine,
            ProtocolViolationPolicy::Disconnect,
            ProtocolViolationPolicy::Observe,
        ] {
            for nested in &invalid {
                let mut core = ClientCore::new(Some(GameDataEncoding::Json), policy, true);
                let _ = process(&mut core, authenticated());
                let _ = process(&mut core, protocol_info(Some(3)));
                let before = core.snapshot();
                let outcome = process(&mut core, reconnected(vec![nested.clone()]));
                assert_lifecycle_violation(&outcome);
                assert_eq!(core.snapshot().room_id, before.room_id);
                assert_eq!(
                    core.snapshot().session_generation,
                    before.session_generation
                );
                assert_eq!(core.stats(), ClientStats::default());
            }
        }
    }

    #[test]
    fn reconnected_payload_version_matrix_is_transactional() {
        let mut v3_invalid = Vec::new();

        let ServerMessage::Reconnected(mut missing_replay) = reconnected(vec![]) else {
            unreachable!()
        };
        missing_replay.replay = None;
        v3_invalid.push(("missing replay", ServerMessage::Reconnected(missing_replay)));

        let ServerMessage::Reconnected(mut missing_token) = reconnected(vec![]) else {
            unreachable!()
        };
        missing_token.reconnection_token = None;
        v3_invalid.push(("missing token", ServerMessage::Reconnected(missing_token)));

        let ServerMessage::Reconnected(mut empty_token) = reconnected(vec![]) else {
            unreachable!()
        };
        empty_token.reconnection_token = Some(String::new());
        v3_invalid.push(("empty token", ServerMessage::Reconnected(empty_token)));

        let ServerMessage::Reconnected(mut missing_watermark) = reconnected(vec![]) else {
            unreachable!()
        };
        missing_watermark.sender_watermarks.pop();
        v3_invalid.push((
            "missing watermark",
            ServerMessage::Reconnected(missing_watermark),
        ));

        let ServerMessage::Reconnected(mut duplicate_watermark) = reconnected(vec![]) else {
            unreachable!()
        };
        duplicate_watermark
            .sender_watermarks
            .push(duplicate_watermark.sender_watermarks[0]);
        v3_invalid.push((
            "duplicate watermark",
            ServerMessage::Reconnected(duplicate_watermark),
        ));

        let ServerMessage::Reconnected(mut mismatched_watermark) = reconnected(vec![]) else {
            unreachable!()
        };
        mismatched_watermark.sender_watermarks[0].seq = 1;
        v3_invalid.push((
            "mismatched watermark",
            ServerMessage::Reconnected(mismatched_watermark),
        ));

        let ServerMessage::Reconnected(mut missing_self) = reconnected(vec![]) else {
            unreachable!()
        };
        missing_self
            .current_players
            .retain(|player| player.id != missing_self.player_id);
        missing_self
            .sender_watermarks
            .retain(|watermark| watermark.player_id != missing_self.player_id);
        v3_invalid.push(("missing self", ServerMessage::Reconnected(missing_self)));

        let ServerMessage::Reconnected(mut duplicate_self) = reconnected(vec![]) else {
            unreachable!()
        };
        duplicate_self
            .current_players
            .push(duplicate_self.current_players[0].clone());
        v3_invalid.push(("duplicate self", ServerMessage::Reconnected(duplicate_self)));

        let ServerMessage::Reconnected(mut unrotated_token) = reconnected(vec![]) else {
            unreachable!()
        };
        unrotated_token.reconnection_token = Some("submitted-token".into());
        v3_invalid.push((
            "unrotated token",
            ServerMessage::Reconnected(unrotated_token),
        ));

        for policy in [
            ProtocolViolationPolicy::Quarantine,
            ProtocolViolationPolicy::Disconnect,
            ProtocolViolationPolicy::Observe,
        ] {
            for (name, invalid) in &v3_invalid {
                let mut core = ClientCore::new(Some(GameDataEncoding::Json), policy, true);
                let _ = process(&mut core, authenticated());
                let _ = process(&mut core, protocol_info(Some(3)));
                core.record_reconnect_admitted(
                    PlayerId::from_u128(LOCAL),
                    RoomId::from_u128(10),
                    "submitted-token".into(),
                );
                let before = core.snapshot();
                let outcome = process(&mut core, invalid.clone());
                assert!(
                    matches!(
                        outcome.events.as_slice(),
                        [SignalFishEvent::ProtocolViolation { .. }]
                    ),
                    "{name}: {:#?}",
                    outcome.events
                );
                assert_eq!(
                    outcome.disconnect,
                    policy == ProtocolViolationPolicy::Disconnect,
                    "{name}"
                );
                let mut expected = before;
                expected.quarantined = policy == ProtocolViolationPolicy::Quarantine;
                assert_eq!(core.snapshot(), expected, "{name}");
                assert_eq!(core.membership, Membership::None, "{name}");
                assert!(!core.session_plan_seen, "{name}");
                assert!(core.session_peers.is_empty(), "{name}");
            }
        }

        let mut v2_invalid = Vec::new();
        let ServerMessage::Reconnected(mut replay) = reconnected_v2() else {
            unreachable!()
        };
        replay.replay = Some(ReplayStatus::Complete);
        v2_invalid.push(ServerMessage::Reconnected(replay));
        let ServerMessage::Reconnected(mut token) = reconnected_v2() else {
            unreachable!()
        };
        token.reconnection_token = Some("v3-only".into());
        v2_invalid.push(ServerMessage::Reconnected(token));
        let ServerMessage::Reconnected(mut watermark) = reconnected_v2() else {
            unreachable!()
        };
        watermark.sender_watermarks.push(SenderWatermark {
            player_id: PlayerId::from_u128(LOCAL),
            epoch: 1,
            seq: 0,
        });
        v2_invalid.push(ServerMessage::Reconnected(watermark));

        for invalid in v2_invalid {
            let mut core = ClientCore::new(
                Some(GameDataEncoding::Json),
                ProtocolViolationPolicy::Observe,
                true,
            );
            let _ = process(&mut core, authenticated());
            let _ = process(&mut core, protocol_info(None));
            core.record_reconnect_admitted(
                PlayerId::from_u128(LOCAL),
                RoomId::from_u128(10),
                "submitted-token".into(),
            );
            let before = core.snapshot();
            let outcome = process(&mut core, invalid);
            assert!(matches!(
                outcome.events.as_slice(),
                [SignalFishEvent::ProtocolViolation { .. }]
            ));
            assert_eq!(core.snapshot(), before);
        }

        for (version, valid) in [(None, reconnected_v2()), (Some(3), reconnected(vec![]))] {
            let mut core = ClientCore::new(
                Some(GameDataEncoding::Json),
                ProtocolViolationPolicy::Observe,
                true,
            );
            let _ = process(&mut core, authenticated());
            let _ = process(&mut core, protocol_info(version));
            core.record_reconnect_admitted(
                PlayerId::from_u128(LOCAL),
                RoomId::from_u128(10),
                "submitted-token".into(),
            );
            let outcome = process(&mut core, valid);
            assert!(matches!(
                outcome.events.as_slice(),
                [SignalFishEvent::Reconnected { .. }]
            ));
            assert_eq!(core.membership, Membership::Player);
        }
    }

    #[test]
    fn reconnect_responses_require_the_matching_admitted_request() {
        let new_core = || {
            let mut core = ClientCore::new(
                Some(GameDataEncoding::Json),
                ProtocolViolationPolicy::Observe,
                true,
            );
            let _ = process(&mut core, authenticated());
            let _ = process(&mut core, protocol_info(Some(3)));
            core
        };

        let mut unsolicited = new_core();
        let before = unsolicited.snapshot();
        assert_lifecycle_violation(&process(&mut unsolicited, reconnected(vec![])));
        assert_eq!(unsolicited.snapshot(), before);

        let mut wrong_identity = new_core();
        wrong_identity.record_reconnect_admitted(
            PlayerId::from_u128(99),
            RoomId::from_u128(10),
            "submitted-token".into(),
        );
        let before = wrong_identity.snapshot();
        assert_lifecycle_violation(&process(&mut wrong_identity, reconnected(vec![])));
        assert_eq!(wrong_identity.snapshot(), before);

        let mut failed = new_core();
        failed.record_reconnect_admitted(
            PlayerId::from_u128(LOCAL),
            RoomId::from_u128(10),
            "submitted-token".into(),
        );
        let failure = process(
            &mut failed,
            ServerMessage::ReconnectionFailed {
                reason: "expired".into(),
                error_code: crate::ErrorCode::ReconnectionExpired,
            },
        );
        assert!(matches!(
            failure.events.as_slice(),
            [SignalFishEvent::ReconnectionFailed { .. }]
        ));
        let ServerMessage::Reconnected(mut unrotated) = reconnected(vec![]) else {
            unreachable!()
        };
        unrotated.reconnection_token = Some("submitted-token".into());
        assert_lifecycle_violation(&process(&mut failed, ServerMessage::Reconnected(unrotated)));
    }

    #[test]
    fn protocol_info_version_tuple_is_coherent_and_transactional() {
        let mut invalid = Vec::new();

        let ServerMessage::ProtocolInfo(mut missing_min) = protocol_info(Some(3)) else {
            unreachable!()
        };
        missing_min.min_protocol_version = None;
        invalid.push(missing_min);

        let ServerMessage::ProtocolInfo(mut missing_transports) = protocol_info(Some(3)) else {
            unreachable!()
        };
        missing_transports.transports = None;
        invalid.push(missing_transports);

        let ServerMessage::ProtocolInfo(mut empty_transports) = protocol_info(Some(3)) else {
            unreachable!()
        };
        empty_transports.transports = Some(vec![]);
        invalid.push(empty_transports);

        let ServerMessage::ProtocolInfo(mut duplicate_transports) = protocol_info(Some(3)) else {
            unreachable!()
        };
        duplicate_transports.transports = Some(vec![
            MessageTransport::Websocket,
            MessageTransport::Websocket,
        ]);
        invalid.push(duplicate_transports);

        let ServerMessage::ProtocolInfo(mut inverted) = protocol_info(Some(3)) else {
            unreachable!()
        };
        inverted.min_protocol_version = Some(4);
        invalid.push(inverted);

        let ServerMessage::ProtocolInfo(mut outside) = protocol_info(Some(3)) else {
            unreachable!()
        };
        outside.max_protocol_version = Some(2);
        invalid.push(outside);

        let ServerMessage::ProtocolInfo(mut v2_with_range) = protocol_info(None) else {
            unreachable!()
        };
        v2_with_range.min_protocol_version = Some(2);
        invalid.push(v2_with_range);

        let ServerMessage::ProtocolInfo(mut v2_with_transports) = protocol_info(None) else {
            unreachable!()
        };
        v2_with_transports.transports = Some(vec![MessageTransport::Websocket]);
        invalid.push(v2_with_transports);

        for payload in invalid {
            let mut core = ClientCore::new(
                Some(GameDataEncoding::MessagePack),
                ProtocolViolationPolicy::Observe,
                true,
            );
            let _ = process(&mut core, authenticated());
            let before = core.snapshot();
            assert_lifecycle_violation(&process(&mut core, ServerMessage::ProtocolInfo(payload)));
            assert_eq!(core.snapshot(), before);
            assert!(!core.protocol_info_seen);
        }
    }

    #[test]
    fn invalid_reconnect_accountability_never_resets_the_existing_frontier() {
        for policy in [
            ProtocolViolationPolicy::Quarantine,
            ProtocolViolationPolicy::Disconnect,
            ProtocolViolationPolicy::Observe,
        ] {
            let mut core = v3_room(policy);
            let first = process(
                &mut core,
                ServerMessage::GameData {
                    from_player: PlayerId::from_u128(PEER),
                    data: serde_json::json!({"seq": 1}),
                    seq: Some(1),
                    epoch: Some(1),
                    class: Some(DeliveryClass::Reliable),
                    key: None,
                },
            );
            assert!(matches!(
                first.events.as_slice(),
                [SignalFishEvent::GameData { .. }]
            ));
            let before = core.snapshot();
            let peers_before = core.session_peers.clone();
            core.membership = Membership::None;
            core.record_reconnect_admitted(
                PlayerId::from_u128(LOCAL),
                RoomId::from_u128(10),
                "submitted-token".into(),
            );

            let ServerMessage::Reconnected(mut invalid) = reconnected(vec![]) else {
                unreachable!()
            };
            invalid.sender_watermarks.pop();
            let outcome = process(&mut core, ServerMessage::Reconnected(invalid));
            assert!(matches!(
                outcome.events.as_slice(),
                [SignalFishEvent::ProtocolViolation { .. }]
            ));
            let mut expected = before;
            expected.quarantined = policy == ProtocolViolationPolicy::Quarantine;
            assert_eq!(core.snapshot(), expected);
            assert_eq!(core.session_peers, peers_before);

            core.membership = Membership::Player;
            core.snapshot.quarantined = false;
            let next = process(
                &mut core,
                ServerMessage::GameData {
                    from_player: PlayerId::from_u128(PEER),
                    data: serde_json::json!({"seq": 2}),
                    seq: Some(2),
                    epoch: Some(1),
                    class: Some(DeliveryClass::Reliable),
                    key: None,
                },
            );
            assert!(
                matches!(next.events.as_slice(), [SignalFishEvent::GameData { .. }]),
                "{policy:?}: {:#?}",
                next.events
            );
        }
    }

    #[test]
    fn stale_signal_after_relay_replan_is_silently_suppressed() {
        for policy in [
            ProtocolViolationPolicy::Quarantine,
            ProtocolViolationPolicy::Disconnect,
            ProtocolViolationPolicy::Observe,
        ] {
            let mut core = v3_room(policy);
            let _ = process(
                &mut core,
                ServerMessage::SessionPlan(Box::new(plan(Topology::Mesh, TransportKind::WebRtc))),
            );
            let mut relay = plan(Topology::Relay, TransportKind::Relay);
            relay.generation = Some(SessionGeneration::from_u128(5));
            let _ = process(&mut core, ServerMessage::SessionPlan(Box::new(relay)));

            let outcome = process(
                &mut core,
                ServerMessage::Signal {
                    from: PlayerId::from_u128(PEER),
                    generation: Some(SessionGeneration::from_u128(GENERATION)),
                    signal: serde_json::json!({"Offer": "late"}),
                },
            );
            assert!(outcome.events.is_empty(), "{policy:?}");
            assert!(!outcome.disconnect, "{policy:?}");
            assert!(!core.snapshot().quarantined, "{policy:?}");
        }
    }

    #[test]
    fn generationless_signal_after_relay_replan_is_silently_suppressed() {
        for policy in [
            ProtocolViolationPolicy::Quarantine,
            ProtocolViolationPolicy::Disconnect,
            ProtocolViolationPolicy::Observe,
        ] {
            let mut core = v3_room(policy);
            let mut webrtc = plan(Topology::Mesh, TransportKind::WebRtc);
            webrtc.generation = None;
            let _ = process(&mut core, ServerMessage::SessionPlan(Box::new(webrtc)));
            let mut relay = plan(Topology::Relay, TransportKind::Relay);
            relay.generation = None;
            let _ = process(&mut core, ServerMessage::SessionPlan(Box::new(relay)));

            let outcome = process(
                &mut core,
                ServerMessage::Signal {
                    from: PlayerId::from_u128(PEER),
                    generation: None,
                    signal: serde_json::json!({"Offer": "late"}),
                },
            );
            assert!(outcome.events.is_empty(), "{policy:?}");
            assert!(!outcome.disconnect, "{policy:?}");
            assert!(!core.snapshot().quarantined, "{policy:?}");
        }
    }

    #[test]
    fn generationless_departure_order_preserves_the_stale_signal_fence() {
        for policy in [
            ProtocolViolationPolicy::Quarantine,
            ProtocolViolationPolicy::Disconnect,
            ProtocolViolationPolicy::Observe,
        ] {
            for player_left_before_relay in [true, false] {
                let mut core = v3_room(policy);
                let mut webrtc = plan(Topology::Mesh, TransportKind::WebRtc);
                webrtc.generation = None;
                let _ = process(&mut core, ServerMessage::SessionPlan(Box::new(webrtc)));
                let player_left = ServerMessage::PlayerLeft {
                    player_id: PlayerId::from_u128(PEER),
                    epoch: Some(1),
                    final_seq: Some(0),
                };
                if player_left_before_relay {
                    let _ = process(&mut core, player_left.clone());
                }
                let mut relay = plan(Topology::Relay, TransportKind::Relay);
                relay.generation = None;
                let _ = process(&mut core, ServerMessage::SessionPlan(Box::new(relay)));
                if !player_left_before_relay {
                    let _ = process(&mut core, player_left);
                }

                let outcome = process(
                    &mut core,
                    ServerMessage::Signal {
                        from: PlayerId::from_u128(PEER),
                        generation: None,
                        signal: serde_json::json!({"Offer": "queued-before-departure"}),
                    },
                );
                assert!(outcome.events.is_empty(), "{policy:?}");
                assert!(!outcome.disconnect, "{policy:?}");
                assert!(!core.snapshot().quarantined, "{policy:?}");
            }
        }
    }

    #[test]
    fn generationless_relay_signal_without_retired_authority_is_a_violation() {
        for policy in [
            ProtocolViolationPolicy::Quarantine,
            ProtocolViolationPolicy::Disconnect,
            ProtocolViolationPolicy::Observe,
        ] {
            let mut initial_relay = v3_room(policy);
            let mut relay = plan(Topology::Relay, TransportKind::Relay);
            relay.generation = None;
            let _ = process(
                &mut initial_relay,
                ServerMessage::SessionPlan(Box::new(relay.clone())),
            );
            let outcome = process(
                &mut initial_relay,
                ServerMessage::Signal {
                    from: PlayerId::from_u128(PEER),
                    generation: None,
                    signal: serde_json::json!({"Offer": "never-authorized"}),
                },
            );
            assert_lifecycle_violation(&outcome);

            let mut retired_other_peer = v3_room(policy);
            let mut webrtc = plan(Topology::Mesh, TransportKind::WebRtc);
            webrtc.generation = None;
            let _ = process(
                &mut retired_other_peer,
                ServerMessage::SessionPlan(Box::new(webrtc)),
            );
            let _ = process(
                &mut retired_other_peer,
                ServerMessage::SessionPlan(Box::new(relay)),
            );
            let outcome = process(
                &mut retired_other_peer,
                ServerMessage::Signal {
                    from: PlayerId::from_u128(4),
                    generation: None,
                    signal: serde_json::json!({"Offer": "never-in-plan"}),
                },
            );
            assert_lifecycle_violation(&outcome);
        }
    }

    #[test]
    fn generationless_webrtc_updates_reauthorize_a_retired_peer() {
        let mut core = v3_room(ProtocolViolationPolicy::Observe);
        let mut webrtc = plan(Topology::Mesh, TransportKind::WebRtc);
        webrtc.generation = None;
        let _ = process(
            &mut core,
            ServerMessage::SessionPlan(Box::new(webrtc.clone())),
        );
        let mut relay = plan(Topology::Relay, TransportKind::Relay);
        relay.generation = None;
        let _ = process(&mut core, ServerMessage::SessionPlan(Box::new(relay)));
        let _ = process(
            &mut core,
            ServerMessage::SessionPlan(Box::new(webrtc.clone())),
        );
        let outcome = process(
            &mut core,
            ServerMessage::Signal {
                from: PlayerId::from_u128(PEER),
                generation: None,
                signal: serde_json::json!({"Offer": "replanned"}),
            },
        );
        assert!(matches!(
            outcome.events.as_slice(),
            [SignalFishEvent::SignalReceived { .. }]
        ));

        webrtc.peers.clear();
        let _ = process(&mut core, ServerMessage::SessionPlan(Box::new(webrtc)));
        assert!(core
            .retired_generationless_signal_peers
            .contains(&PlayerId::from_u128(PEER)));
        let _ = process(
            &mut core,
            ServerMessage::NewPeer {
                peer_id: PlayerId::from_u128(PEER),
                you_initiate: true,
            },
        );
        let outcome = process(
            &mut core,
            ServerMessage::Signal {
                from: PlayerId::from_u128(PEER),
                generation: None,
                signal: serde_json::json!({"Offer": "new-peer"}),
            },
        );
        assert!(matches!(
            outcome.events.as_slice(),
            [SignalFishEvent::SignalReceived { .. }]
        ));
    }

    #[test]
    fn room_reset_clears_generationless_retired_signal_peers() {
        let mut core = v3_room(ProtocolViolationPolicy::Observe);
        let mut webrtc = plan(Topology::Mesh, TransportKind::WebRtc);
        webrtc.generation = None;
        let _ = process(&mut core, ServerMessage::SessionPlan(Box::new(webrtc)));
        let mut relay = plan(Topology::Relay, TransportKind::Relay);
        relay.generation = None;
        let _ = process(&mut core, ServerMessage::SessionPlan(Box::new(relay)));
        assert!(!core.retired_generationless_signal_peers.is_empty());

        let _ = process(&mut core, ServerMessage::RoomLeft);
        assert!(core.retired_generationless_signal_peers.is_empty());
    }

    #[test]
    fn pong_is_connection_scoped_before_authentication_and_negotiation() {
        for policy in [
            ProtocolViolationPolicy::Quarantine,
            ProtocolViolationPolicy::Disconnect,
            ProtocolViolationPolicy::Observe,
        ] {
            for prefix in [
                vec![],
                vec![authenticated()],
                vec![authenticated(), protocol_info(Some(3))],
                vec![authenticated(), protocol_info(Some(3)), room_joined()],
                vec![
                    authenticated(),
                    protocol_info(Some(3)),
                    room_joined(),
                    ServerMessage::RoomLeft,
                ],
            ] {
                let mut core = ClientCore::new(Some(GameDataEncoding::Json), policy, true);
                for message in prefix {
                    let _ = process(&mut core, message);
                }
                let before = core.snapshot();
                let outcome = process(&mut core, ServerMessage::Pong);
                assert!(matches!(outcome.events.as_slice(), [SignalFishEvent::Pong]));
                assert!(!outcome.disconnect, "{policy:?}");
                assert_eq!(core.snapshot(), before, "{policy:?}");
            }
        }
    }

    #[test]
    fn peer_transport_status_requires_another_room_player_not_a_plan_peer() {
        let mut core = v3_room(ProtocolViolationPolicy::Observe);
        let _ = process(
            &mut core,
            ServerMessage::SessionPlan(Box::new(plan(Topology::Mesh, TransportKind::WebRtc))),
        );

        let valid_off_plan = process(
            &mut core,
            ServerMessage::PeerTransportStatus {
                peer_id: PlayerId::from_u128(4),
                transport: TransportKind::Relay,
                connected: true,
            },
        );
        assert!(matches!(
            valid_off_plan.events.as_slice(),
            [SignalFishEvent::PeerTransportStatus { .. }]
        ));

        for peer_id in [PlayerId::from_u128(LOCAL), PlayerId::from_u128(99)] {
            let outcome = process(
                &mut core,
                ServerMessage::PeerTransportStatus {
                    peer_id,
                    transport: TransportKind::WebRtc,
                    connected: false,
                },
            );
            assert_lifecycle_violation(&outcome);
        }
    }

    #[test]
    fn authority_denial_is_connection_scoped_after_negotiation() {
        let mut core = ClientCore::new(
            Some(GameDataEncoding::Json),
            ProtocolViolationPolicy::Observe,
            true,
        );
        let _ = process(&mut core, authenticated());
        let _ = process(&mut core, protocol_info(Some(3)));
        let outcome = process(
            &mut core,
            ServerMessage::AuthorityResponse {
                granted: false,
                reason: Some("not in room".into()),
                error_code: None,
            },
        );
        assert!(matches!(
            outcome.events.as_slice(),
            [SignalFishEvent::AuthorityResponse { granted: false, .. }]
        ));
    }
}
