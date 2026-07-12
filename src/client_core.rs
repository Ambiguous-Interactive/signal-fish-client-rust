//! Shared, transport-independent Signal Fish client state machine.
//!
//! The async and polling clients deliberately keep their driving mechanics
//! separate. Everything that interprets protocol frames or mutates observable
//! client state lives here so both drivers cannot drift semantically.

use crate::accountability::{self, DeliveryAccountability, GameDataDisposition};
use crate::client::{
    bounded_binary_preview, decode_binary_server_message, ClientSnapshot, ClientStats,
    GameDataDelivery, JoinRoomParams, ProtocolViolationPolicy, SignalFishConfig,
};
use crate::event::{ProtocolViolationKind, ServerErrorInfo, SignalFishEvent};
use crate::protocol::{
    ClientMessage, ConnectionInfo, DeliveryClass, GameDataEncoding, PlayerId, RoomId,
    ServerMessage, TransportKind,
};
use crate::signal::PeerSignal;
use crate::transport::TransportFrame;

/// Result of processing one physical server frame.
pub(crate) struct FrameOutcome {
    pub(crate) events: Vec<SignalFishEvent>,
    pub(crate) disconnect: bool,
}

#[derive(Debug)]
pub(crate) enum CoreCommand {
    Message(ClientMessage),
    Binary(Vec<u8>),
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
    Signal(PlayerId, PeerSignal),
    RawSignal(PlayerId, serde_json::Value),
    TransportStatus(TransportKind, bool),
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
    game_data_encoding: GameDataEncoding,
    stats: ClientStats,
    last_server_error: Option<ServerErrorInfo>,
    violation_policy: ProtocolViolationPolicy,
    accountability: DeliveryAccountability,
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
        game_data_encoding: GameDataEncoding,
        violation_policy: ProtocolViolationPolicy,
        mesh_enabled: bool,
    ) -> Self {
        Self {
            snapshot: ClientSnapshot {
                connected: true,
                ..ClientSnapshot::default()
            },
            protocol_info_seen: false,
            mesh_enabled,
            game_data_encoding,
            stats: ClientStats::default(),
            last_server_error: None,
            violation_policy,
            accountability: DeliveryAccountability::new(false),
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

    pub(crate) fn prepare(&self, operation: ClientOperation) -> crate::error::Result<CoreCommand> {
        if !self.is_connected() {
            return Err(crate::SignalFishError::NotConnected);
        }
        match &operation {
            ClientOperation::GameData(_, GameDataDelivery::Latest { .. })
            | ClientOperation::GameData(_, GameDataDelivery::Volatile)
            | ClientOperation::Binary(_)
            | ClientOperation::Signal(..)
            | ClientOperation::RawSignal(..)
            | ClientOperation::TransportStatus(..) => self.ensure_v3()?,
            _ => {}
        }
        if matches!(&operation, ClientOperation::Binary(_))
            && self.game_data_encoding == GameDataEncoding::Json
        {
            return Err(crate::SignalFishError::BinaryFormatNotNegotiated);
        }
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
            ClientOperation::Signal(to, signal) => ClientMessage::Signal {
                to,
                signal: signal.into(),
            },
            ClientOperation::RawSignal(to, signal) => ClientMessage::Signal { to, signal },
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

    pub(crate) fn clear_session(&mut self) {
        self.snapshot.authenticated = false;
        self.snapshot.negotiated_protocol_version = None;
        self.snapshot.player_id = None;
        self.snapshot.room_id = None;
        self.snapshot.room_code = None;
        self.snapshot.reconnection_token = None;
        self.snapshot.quarantined = false;
        self.protocol_info_seen = false;
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
        let validation = if duplicate_protocol_info {
            self.accountability
                .observe_server_message(false)
                .map(|()| GameDataDisposition::Apply)
        } else {
            accountability::validate_server_frame(
                &mut self.accountability,
                &server_msg,
                self.game_data_encoding,
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
                let disposition = if self.violation_policy == ProtocolViolationPolicy::Observe {
                    GameDataDisposition::Apply
                } else {
                    GameDataDisposition::Stale
                };
                (disposition, true)
            }
        };

        if validation_failed && self.violation_policy == ProtocolViolationPolicy::Quarantine {
            return outcome;
        }
        if duplicate_protocol_info {
            return outcome;
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
        let mut observe_representation_violation = false;
        if let Err(diagnostic) = accountability::validate_physical_binary_allowed(
            &mut self.accountability,
            self.game_data_encoding,
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

        let validation = if observe_representation_violation {
            accountability::validate_server_message(&mut self.accountability, &server_msg)
        } else {
            accountability::validate_server_frame(
                &mut self.accountability,
                &server_msg,
                self.game_data_encoding,
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

    fn update_state(&mut self, message: &ServerMessage) {
        match message {
            ServerMessage::Authenticated { .. } => self.snapshot.authenticated = true,
            ServerMessage::Error {
                message,
                error_code,
            } => {
                if error_code.as_ref() == Some(&crate::ErrorCode::UnsupportedGameDataFormat) {
                    self.game_data_encoding = GameDataEncoding::Json;
                }
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
                self.protocol_info_seen = true;
            }
            ServerMessage::RoomJoined(payload) => {
                self.set_room(
                    payload.player_id,
                    payload.room_id,
                    payload.room_code.clone(),
                    payload.reconnection_token.clone(),
                );
            }
            ServerMessage::RoomLeft => self.clear_room(),
            ServerMessage::Reconnected(payload) => {
                self.set_room(
                    payload.player_id,
                    payload.room_id,
                    payload.room_code.clone(),
                    payload.reconnection_token.clone(),
                );
                if let Some(version) =
                    crate::protocol::replayed_negotiated_version(&payload.missed_events)
                {
                    self.snapshot.negotiated_protocol_version = Some(version);
                    self.protocol_info_seen = true;
                }
            }
            ServerMessage::SpectatorJoined(payload) => {
                self.set_room(
                    payload.spectator_id,
                    payload.room_id,
                    payload.room_code.clone(),
                    None,
                );
            }
            ServerMessage::SpectatorLeft { .. } => self.clear_room(),
            ServerMessage::GameData { .. } | ServerMessage::GameDataBinary { .. } => {
                self.stats.game_data_received = self.stats.game_data_received.saturating_add(1);
            }
            _ => {}
        }
    }

    fn set_room(
        &mut self,
        player_id: PlayerId,
        room_id: RoomId,
        room_code: String,
        reconnection_token: Option<String>,
    ) {
        self.snapshot.player_id = Some(player_id);
        self.snapshot.room_id = Some(room_id);
        self.snapshot.room_code = Some(room_code);
        self.snapshot.reconnection_token = reconnection_token;
        self.snapshot.quarantined = false;
    }

    fn clear_room(&mut self) {
        self.snapshot.room_id = None;
        self.snapshot.room_code = None;
        self.snapshot.reconnection_token = None;
        self.snapshot.quarantined = false;
    }
}
