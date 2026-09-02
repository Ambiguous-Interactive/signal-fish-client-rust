//! Shared, transport-independent Signal Fish client state machine.
//!
//! The async and polling clients deliberately keep their driving mechanics
//! separate. Everything that interprets protocol frames or mutates observable
//! client state lives here so both drivers cannot drift semantically.

use std::collections::{HashSet, VecDeque};
use std::io;

use crate::accountability::{self, DeliveryAccountability, GameDataDisposition};
use crate::client::{
    bounded_binary_preview, decode_binary_server_message, ClientSnapshot, ClientStats,
    GameDataDelivery, JoinRoomParams, ProtocolViolationPolicy, RoomRole, SignalFishConfig,
};
use crate::event::{ProtocolViolationKind, ServerErrorInfo, SignalFishEvent};
use crate::protocol::{
    ClientMessage, ConnectionInfo, DeliveryClass, GameDataEncoding, PlayerId, RoomId,
    RoomOperationId, RoomOperationRequest, RoomOperationResult, ServerMessage, SessionGeneration,
    SessionPlanPayload, Topology, TransportKind, ROOM_OPERATION_IDS_CAPABILITY,
};
use crate::signal::PeerSignal;
#[cfg(feature = "tokio-runtime")]
use crate::transport::TransportDiagnostics;
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

const GAME_DATA_JSON_ENVELOPE_CAPACITY: usize = 128;
const GAME_DATA_JSON_PREALLOCATION_THRESHOLD: usize = 4_096;

/// Maximum container nesting accepted for outbound JSON game data.
///
/// Serialization of a [`serde_json::Value`] recurses once per container
/// level, so an unbounded payload could overflow the stack on a driver
/// thread — aborting the whole process — after the send call had already
/// reported success. Admission therefore refuses deeper payloads with
/// [`SignalFishError::PayloadTooDeep`](crate::SignalFishError::PayloadTooDeep).
/// The bound matches serde_json's default deserialization recursion limit,
/// so every payload the SDK could itself receive is also sendable.
pub(crate) const MAX_GAME_DATA_DEPTH: usize = 128;

/// Bounded-budget container-depth check for outbound JSON game data.
///
/// The recursion depth is capped by `budget`, so validation itself can never
/// overflow the stack no matter how deeply the caller nested the value.
fn game_data_depth_within(value: &serde_json::Value, budget: usize) -> bool {
    match value {
        serde_json::Value::Array(items) if budget > 0 => {
            let child_budget = budget.saturating_sub(1);
            items
                .iter()
                .all(|item| game_data_depth_within(item, child_budget))
        }
        serde_json::Value::Object(map) if budget > 0 => {
            let child_budget = budget.saturating_sub(1);
            map.values()
                .all(|item| game_data_depth_within(item, child_budget))
        }
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => false,
        _ => true,
    }
}

/// Serialize one client message while avoiding geometric buffer growth for
/// application-owned JSON payloads.
///
/// `serde_json::to_string` begins with a small fixed buffer. That is a good
/// default for control messages, but it repeatedly reallocates when a
/// direct `GameData` string is already known to be large. Its existing byte
/// length gives an O(1) capacity hint, with fixed space for the adjacent-tagged
/// envelope, delivery class, and key. Heavily escaped strings may still grow
/// the buffer; ordinary game payloads avoid both an extra scan and geometric
/// reallocations. Structured values retain serde_json's default path until a
/// representative workload proves that a recursive hint is worthwhile.
pub(crate) fn serialize_client_message(message: &ClientMessage) -> serde_json::Result<String> {
    let ClientMessage::GameData { data, .. } = message else {
        return serde_json::to_string(message);
    };

    let serde_json::Value::String(data) = data else {
        return serde_json::to_string(message);
    };
    if data.len() < GAME_DATA_JSON_PREALLOCATION_THRESHOLD {
        return serde_json::to_string(message);
    }
    let value_capacity = data.len().saturating_add(2);
    let capacity = value_capacity.saturating_add(GAME_DATA_JSON_ENVELOPE_CAPACITY);
    let mut encoded = Vec::with_capacity(capacity);
    serde_json::to_writer(&mut encoded, message)?;
    String::from_utf8(encoded)
        .map_err(|error| serde_json::Error::io(io::Error::new(io::ErrorKind::InvalidData, error)))
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

/// How many recently superseded plan generations stay fenced against
/// replay. The realistic duplicate/replay window is adjacent in time on the
/// single ordered transport stream, so a small fence preserves the guard
/// while bounding per-room memory under generation churn.
const RETIRED_SESSION_GENERATION_FENCE: usize = 8;

/// Absolute roster admission fallback for baselines that do not advertise a
/// capacity (spectator joins). `max_players` is a `u8` on the wire, so no
/// legitimate room can hold more distinct players than this; the fallback
/// bounds `room_players` growth even when the exact ceiling is unknown.
const ABSOLUTE_ROSTER_CAPACITY: usize = 256;

/// Shared protocol state and behavior used by both public client drivers.
pub(crate) struct ClientCore {
    snapshot: ClientSnapshot,
    protocol_info_seen: bool,
    requested_room_operation_ids: bool,
    room_operation_ids: bool,
    mesh_capable: bool,
    stats: ClientStats,
    // Latest backend-reported scheduling/buffering diagnostics. The async
    // driver refreshes the sample at loop-cycle start and after every
    // pending-send or receive poll, so deferred watermark/capacity hits are
    // visible while backpressure is in flight rather than only at the next
    // wakeup. The polling driver reads its owned transport directly instead;
    // this copy keeps the async handle's accessor lock-free relative to the
    // transport task.
    #[cfg(feature = "tokio-runtime")]
    transport_diagnostics: TransportDiagnostics,
    last_server_error: Option<ServerErrorInfo>,
    violation_policy: ProtocolViolationPolicy,
    accountability: DeliveryAccountability,
    authority_player: Option<PlayerId>,
    room_finalized: bool,
    room_players: HashSet<PlayerId>,
    // Player-slot capacity advertised by the latest authoritative baseline
    // (`RoomJoined`/`Reconnected`). Spectator baselines omit the field on the
    // wire, leaving the absolute [`ABSOLUTE_ROSTER_CAPACITY`] fallback as the
    // admission ceiling. Incremental `PlayerJoined` inserts beyond the ceiling
    // are lifecycle violations instead of unbounded roster growth (issue #166).
    room_max_players: Option<u8>,
    session_plan_seen: bool,
    session_peers: HashSet<PlayerId>,
    // Recently superseded plan generations, fenced against replayed plans
    // re-asserting an already-superseded authoritative view. Bounded to the
    // most recent [`RETIRED_SESSION_GENERATION_FENCE`] entries: the realistic
    // duplicate/replay window is adjacent in time (one ordered transport
    // stream), while an unbounded set let generation-churn grow memory for
    // the whole room stay. Replays older than the fence degrade to a fresh
    // authoritative plan, which a hostile connected server can already send
    // verbatim; cleared whenever an authoritative baseline rebuilds the
    // room/session.
    retired_session_generations: VecDeque<SessionGeneration>,
    // Peers the authoritative plan dropped (via `PlayerLeft` or a plan
    // replacement) whose final in-flight signals may still arrive stamped
    // with the still-current generation. Their late signals are benign
    // races to suppress, not integrity violations. Cleared whenever an
    // authoritative baseline rebuilds the room/session; within one room the
    // bound is O(distinct departed peers between generation bumps), because
    // Server 0.8 re-plans on membership churn.
    retired_signal_peers: HashSet<PlayerId>,
    pending_room_operation: Option<PendingRoomOperationState>,
    // The reply to an admitted voluntary spectator leave may legally arrive
    // after an authoritative exit (`Disconnected`/`Removed`/`RoomClosed`)
    // already tore down the room: the server must still answer the request it
    // accepted, but its effect is superseded. At most one matching terminal
    // reply is absorbed as this benign race instead of a lifecycle violation;
    // nonmatching frames continue through normal validation without consuming
    // the allowance. A matching reply consumes it once, and a fresh baseline
    // clears it; later duplicates therefore violate normally.
    absorbed_spectator_leave: Option<OvertakenSpectatorLeave>,
    pending_reconnects: VecDeque<PendingReconnect>,
    #[cfg(feature = "tokio-runtime")]
    admission_frozen: bool,
    #[cfg(feature = "tokio-runtime")]
    session_plan_revision: u64,
    #[cfg(feature = "tokio-runtime")]
    room_revision: u64,
}

struct RoomBaseline {
    player_id: PlayerId,
    room_id: RoomId,
    room_code: String,
    reconnection_token: Option<String>,
    room_role: RoomRole,
    authority_player: Option<PlayerId>,
    finalized: bool,
    players: HashSet<PlayerId>,
    // Spectator baselines omit the wire field, so this is `None` there.
    max_players: Option<u8>,
}

pub(crate) struct PendingReconnect {
    player_id: PlayerId,
    room_id: RoomId,
    token: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PendingRoomOperation {
    JoinPlayer,
    LeavePlayer,
    ReconnectPlayer,
    JoinSpectator,
    LeaveSpectator,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PendingRoomOperationState {
    kind: PendingRoomOperation,
    operation_id: Option<RoomOperationId>,
}

#[derive(Debug, PartialEq, Eq)]
struct OvertakenSpectatorLeave {
    pending: PendingRoomOperationState,
    room_id: Option<RoomId>,
    room_code: Option<String>,
}

pub(crate) struct ClientOperationAdmission {
    pending: PendingRoomOperationState,
    reconnect: Option<PendingReconnect>,
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
            requested_capabilities: config
                .requests_room_operation_ids()
                .then(|| vec![ROOM_OPERATION_IDS_CAPABILITY.to_string()]),
        })
    }

    #[cfg(test)]
    pub(crate) fn new(
        requested_game_data_format: Option<GameDataEncoding>,
        violation_policy: ProtocolViolationPolicy,
        mesh_capable: bool,
    ) -> Self {
        Self::new_with_room_operation_ids(
            requested_game_data_format,
            violation_policy,
            mesh_capable,
            false,
        )
    }

    pub(crate) fn new_with_room_operation_ids(
        requested_game_data_format: Option<GameDataEncoding>,
        violation_policy: ProtocolViolationPolicy,
        mesh_capable: bool,
        requested_room_operation_ids: bool,
    ) -> Self {
        Self {
            snapshot: ClientSnapshot {
                connected: true,
                requested_game_data_format,
                ..ClientSnapshot::default()
            },
            protocol_info_seen: false,
            requested_room_operation_ids,
            room_operation_ids: false,
            mesh_capable,
            stats: ClientStats::default(),
            #[cfg(feature = "tokio-runtime")]
            transport_diagnostics: TransportDiagnostics::default(),
            last_server_error: None,
            violation_policy,
            accountability: DeliveryAccountability::new(false),
            authority_player: None,
            room_finalized: false,
            room_players: HashSet::new(),
            room_max_players: None,
            session_plan_seen: false,
            session_peers: HashSet::new(),
            retired_session_generations: VecDeque::new(),
            retired_signal_peers: HashSet::new(),
            pending_room_operation: None,
            absorbed_spectator_leave: None,
            pending_reconnects: VecDeque::new(),
            #[cfg(feature = "tokio-runtime")]
            admission_frozen: false,
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

    pub(crate) fn is_transport_ready(&self) -> bool {
        self.snapshot.transport_ready
    }

    pub(crate) fn mark_transport_ready(&mut self) -> bool {
        if !self.snapshot.connected || self.snapshot.transport_ready {
            return false;
        }
        self.snapshot.transport_ready = true;
        true
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
        self.mesh_capable
            && self
                .negotiated_protocol_version()
                .is_some_and(|version| version >= 3)
    }

    pub(crate) fn session_topology(&self) -> Option<Topology> {
        self.snapshot.session_topology
    }

    pub(crate) fn session_transport(&self) -> Option<TransportKind> {
        self.snapshot.session_transport
    }

    pub(crate) fn is_p2p_active(&self) -> bool {
        matches!(
            self.session_topology(),
            Some(Topology::Host | Topology::Mesh)
        )
    }

    pub(crate) fn room_role(&self) -> Option<RoomRole> {
        self.snapshot.room_role
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
        #[cfg(feature = "tokio-runtime")]
        if self.admission_frozen {
            return Err(crate::SignalFishError::NotConnected);
        }
        if !self.is_connected() {
            return Err(crate::SignalFishError::NotConnected);
        }
        if matches!(
            operation,
            ClientOperation::JoinRoom(_)
                | ClientOperation::LeaveRoom
                | ClientOperation::Reconnect(..)
                | ClientOperation::JoinAsSpectator(..)
                | ClientOperation::LeaveSpectator
        ) && !self.snapshot.authenticated
        {
            // The inbound lifecycle gates require `authenticated` for every
            // room response, so admitting an outbound room operation before
            // authentication would arm its fence against responses the SDK
            // itself classifies as violations — poisoning every later room
            // operation with `RoomOperationPending` until teardown.
            return Err(crate::SignalFishError::NotAuthenticated);
        }
        if self.pending_room_operation.is_some() && !matches!(operation, ClientOperation::Ping) {
            return Err(crate::SignalFishError::RoomOperationPending);
        }
        if matches!(
            operation,
            ClientOperation::JoinRoom(_)
                | ClientOperation::Reconnect(..)
                | ClientOperation::JoinAsSpectator(..)
        ) && self.room_role().is_some()
        {
            return Err(crate::SignalFishError::AlreadyInRoom);
        }
        let required_role = match operation {
            ClientOperation::LeaveRoom
            | ClientOperation::GameData(..)
            | ClientOperation::Binary(_)
            | ClientOperation::SetReady
            | ClientOperation::StartGame
            | ClientOperation::RequestAuthority(_)
            | ClientOperation::ProvideConnectionInfo(_)
            | ClientOperation::Signal(..)
            | ClientOperation::RawSignal(..)
            | ClientOperation::TransportStatus(..) => Some(RoomRole::Player),
            ClientOperation::LeaveSpectator => Some(RoomRole::Spectator),
            ClientOperation::JoinRoom(_)
            | ClientOperation::Reconnect(..)
            | ClientOperation::JoinAsSpectator(..)
            | ClientOperation::Ping => None,
        };
        if let Some(required) = required_role {
            match self.room_role() {
                None => return Err(crate::SignalFishError::NotInRoom),
                Some(actual) if actual != required => {
                    return Err(crate::SignalFishError::WrongRoomRole { required, actual });
                }
                Some(_) => {}
            }
        }
        let local_player = self.snapshot.player_id;
        let authority_required = matches!(operation, ClientOperation::RequestAuthority(false))
            || (matches!(operation, ClientOperation::StartGame) && self.authority_player.is_some());
        if authority_required && self.authority_player != local_player {
            return Err(crate::SignalFishError::AuthorityRequired);
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
                if self.snapshot.session_transport != Some(TransportKind::WebRtc)
                    || !self.room_players.contains(peer_id)
                    || Some(*peer_id) == self.snapshot.player_id
                {
                    return Err(crate::SignalFishError::SessionPlanUnavailable);
                }
            }
            _ => {}
        }
        // Payload-shape admission comes last, so connection, membership,
        // role, authority, protocol, and plan refusals keep precedence over
        // per-payload refusals. Every caller-supplied `serde_json::Value`
        // that reaches the recursive outbound serializer is bounded here.
        let payload_shape = match operation {
            ClientOperation::GameData(data, _) => Some(data),
            ClientOperation::RawSignal(_, _, signal) => Some(signal),
            ClientOperation::ProvideConnectionInfo(connection_info) => match connection_info {
                ConnectionInfo::Custom { data } => Some(data),
                ConnectionInfo::Direct { .. }
                | ConnectionInfo::UnityRelay { .. }
                | ConnectionInfo::Relay { .. }
                | ConnectionInfo::WebRTC { .. } => None,
            },
            _ => None,
        };
        if let Some(payload) = payload_shape {
            if !game_data_depth_within(payload, MAX_GAME_DATA_DEPTH) {
                return Err(crate::SignalFishError::PayloadTooDeep {
                    max_depth: MAX_GAME_DATA_DEPTH,
                });
            }
        }
        Ok(())
    }

    #[cfg(feature = "tokio-runtime")]
    pub(crate) fn freeze_admission(&mut self) {
        self.admission_frozen = true;
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

    #[cfg(any(feature = "tokio-runtime", test))]
    pub(crate) fn prepare(&self, operation: ClientOperation) -> crate::error::Result<CoreCommand> {
        if Self::admission_for(&operation).is_some() {
            // Internal reliable-send paths never carry membership transitions.
            // Refuse a future accidental call rather than manufacturing a
            // correlated frame whose admission could be discarded.
            return Err(crate::SignalFishError::RoomOperationPending);
        }
        self.prepare_with_admission(operation)
            .map(|(command, admission)| {
                debug_assert!(admission.is_none());
                command
            })
    }

    pub(crate) fn prepare_with_admission(
        &self,
        operation: ClientOperation,
    ) -> crate::error::Result<(CoreCommand, Option<ClientOperationAdmission>)> {
        self.validate(&operation)?;
        let mut admission = Self::admission_for(&operation);
        if self.room_operation_ids {
            if let Some(admission) = &mut admission {
                admission.pending.operation_id = Some(RoomOperationId::new_v4());
            }
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
                return Ok((CoreCommand::Binary(payload), admission));
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
        let message = if let Some(operation_id) = admission
            .as_ref()
            .and_then(|admission| admission.pending.operation_id)
        {
            correlate_room_operation(message, operation_id)
        } else {
            message
        };
        Ok((CoreCommand::Message(message), admission))
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

    #[cfg(feature = "tokio-runtime")]
    pub(crate) fn record_transport_diagnostics(&mut self, diagnostics: TransportDiagnostics) {
        self.transport_diagnostics = diagnostics;
    }

    #[cfg(feature = "tokio-runtime")]
    pub(crate) fn transport_diagnostics(&self) -> TransportDiagnostics {
        self.transport_diagnostics
    }

    pub(crate) fn admission_for(operation: &ClientOperation) -> Option<ClientOperationAdmission> {
        let (kind, reconnect) = match operation {
            ClientOperation::JoinRoom(_) => (PendingRoomOperation::JoinPlayer, None),
            ClientOperation::LeaveRoom => (PendingRoomOperation::LeavePlayer, None),
            ClientOperation::Reconnect(player_id, room_id, token) => (
                PendingRoomOperation::ReconnectPlayer,
                Some(PendingReconnect {
                    player_id: *player_id,
                    room_id: *room_id,
                    token: token.clone(),
                }),
            ),
            ClientOperation::JoinAsSpectator(..) => (PendingRoomOperation::JoinSpectator, None),
            ClientOperation::LeaveSpectator => (PendingRoomOperation::LeaveSpectator, None),
            _ => return None,
        };
        Some(ClientOperationAdmission {
            pending: PendingRoomOperationState {
                kind,
                operation_id: None,
            },
            reconnect,
        })
    }

    pub(crate) fn record_admission(&mut self, admission: Option<ClientOperationAdmission>) {
        let Some(admission) = admission else {
            return;
        };
        self.pending_room_operation = Some(admission.pending);
        if let Some(reconnect) = admission.reconnect {
            self.pending_reconnects.push_back(reconnect);
        }
    }

    #[cfg(test)]
    pub(crate) fn record_reconnect_admitted(
        &mut self,
        player_id: PlayerId,
        room_id: RoomId,
        token: String,
    ) {
        self.record_admission(Self::admission_for(&ClientOperation::Reconnect(
            player_id, room_id, token,
        )));
    }

    pub(crate) fn clear_session(&mut self) {
        self.snapshot.authenticated = false;
        self.snapshot.negotiated_protocol_version = None;
        self.snapshot.effective_game_data_format = None;
        self.snapshot.server_max_outbound_message_size = None;
        self.snapshot.player_id = None;
        self.snapshot.room_id = None;
        self.snapshot.room_code = None;
        self.snapshot.reconnection_token = None;
        self.snapshot.session_generation = None;
        self.snapshot.session_topology = None;
        self.snapshot.session_transport = None;
        self.snapshot.quarantined = false;
        self.protocol_info_seen = false;
        self.room_operation_ids = false;
        self.snapshot.room_role = None;
        self.authority_player = None;
        self.room_finalized = false;
        self.room_players.clear();
        self.room_max_players = None;
        self.session_plan_seen = false;
        self.session_peers.clear();
        self.retired_session_generations.clear();
        self.retired_signal_peers.clear();
        self.pending_room_operation = None;
        self.absorbed_spectator_leave = None;
        self.pending_reconnects.clear();
        #[cfg(feature = "tokio-runtime")]
        self.advance_session_plan_revision();
    }

    pub(crate) fn disconnect(&mut self, reason: Option<String>) -> SignalFishEvent {
        self.accountability.observe_terminal();
        self.snapshot.connected = false;
        self.snapshot.transport_ready = false;
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
        if self.absorb_overtaken_terminal_reply(&server_msg) {
            return outcome;
        }
        let server_msg = match self.normalize_room_operation_result(server_msg) {
            Ok(Ok(message)) => message,
            Ok(Err((reason, error_code))) => {
                self.clear_pending_operation_after_correlated_failure();
                outcome
                    .events
                    .push(SignalFishEvent::RoomOperationFailed { reason, error_code });
                return outcome;
            }
            Err(diagnostic) => {
                self.reject_inbound(&mut outcome, diagnostic);
                return outcome;
            }
        };

        if matches!(
            &server_msg,
            ServerMessage::GameData { .. } | ServerMessage::GameDataBinary { .. }
        ) {
            self.stats.game_data_received = self.stats.game_data_received.saturating_add(1);
        }

        if let Err(diagnostic) = self.validate_inbound_message(&server_msg) {
            self.reject_inbound(&mut outcome, diagnostic);
            return outcome;
        }

        // `validate_inbound_message` rejects any second `ProtocolInfo` before
        // this point, so the accountability swap below always runs on the one
        // permitted negotiation frame.
        if let ServerMessage::ProtocolInfo(payload) = &server_msg {
            self.accountability = DeliveryAccountability::new(
                payload.protocol_version.is_some_and(|version| version >= 3),
            );
        }

        let authoritative_baseline = matches!(
            server_msg,
            ServerMessage::RoomJoined(_)
                | ServerMessage::SpectatorJoined(_)
                | ServerMessage::Reconnected(_)
        );
        let effective_game_data_format = self.effective_game_data_format().unwrap_or_default();
        let validation = accountability::validate_server_frame(
            &mut self.accountability,
            &server_msg,
            effective_game_data_format,
            false,
        );

        let (disposition, validation_failed) = match validation {
            Ok(disposition) => {
                if authoritative_baseline {
                    self.snapshot.quarantined = false;
                }
                (disposition, false)
            }
            Err(diagnostic) => {
                self.push_violation(&mut outcome.events, diagnostic);
                // An admitted directed operation's typed answer still settles
                // that operation when delivery accountability rejects its
                // payload: the fence exists to arbitrate the in-flight wire
                // race, not to convert one drifting frame into a permanent
                // local admission lockout under any violation policy.
                self.retire_answered_room_operation(&server_msg);
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

    /// Consume the one terminal reply an authoritative spectator exit
    /// overtook, if this frame is exactly that reply.
    ///
    /// The voluntary-leave request was acknowledged by queue admission and
    /// reached the server, so a conforming server still answers it even after
    /// its own authoritative exit (`Disconnected`/`Removed`/`RoomClosed`)
    /// removed the spectator first. Absorbing that superseded reply keeps the
    /// benign wire race from latching quarantine — or tearing down under
    /// [`ProtocolViolationPolicy::Disconnect`] — while every other shape,
    /// duplicate reply, or uncorrelated id still violates below.
    fn absorb_overtaken_terminal_reply(&mut self, message: &ServerMessage) -> bool {
        let Some(absorbed) = self.absorbed_spectator_leave.as_ref() else {
            return false;
        };
        let consumed = match message {
            ServerMessage::RoomOperationResult {
                operation_id,
                result,
            } => {
                self.pending_room_operation
                    .as_ref()
                    .is_none_or(|pending| pending.operation_id != Some(*operation_id))
                    && absorbed.pending.operation_id == Some(*operation_id)
                    && room_operation_result_matches(absorbed.pending.kind, result)
                    && match result.as_ref() {
                        RoomOperationResult::SpectatorLeft {
                            room_id, room_code, ..
                        } => room_identity_matches(
                            absorbed.room_id,
                            absorbed.room_code.as_ref(),
                            *room_id,
                            room_code.as_ref(),
                        ),
                        _ => true,
                    }
            }
            ServerMessage::SpectatorLeft {
                room_id,
                room_code,
                reason,
                ..
            } => {
                !self.pending_room_operation.as_ref().is_some_and(|pending| {
                    pending.kind == PendingRoomOperation::LeaveSpectator
                        && pending.operation_id.is_none()
                }) && absorbed.pending.operation_id.is_none()
                    && spectator_exit_answers_voluntary_leave(reason.as_ref())
                    && room_identity_matches(
                        absorbed.room_id,
                        absorbed.room_code.as_ref(),
                        *room_id,
                        room_code.as_ref(),
                    )
            }
            _ => false,
        };
        if consumed {
            self.absorbed_spectator_leave = None;
            tracing::debug!("absorbed terminal reply overtaken by an authoritative spectator exit");
        }
        consumed
    }

    fn normalize_room_operation_result(
        &self,
        message: ServerMessage,
    ) -> Result<Result<ServerMessage, (String, Option<crate::ErrorCode>)>, String> {
        let ServerMessage::RoomOperationResult {
            operation_id,
            result,
        } = message
        else {
            if self
                .pending_room_operation
                .as_ref()
                .and_then(|pending| pending.operation_id)
                .is_some()
                && is_uncorrelated_directed_room_response(&message)
            {
                return Err(format!(
                    "lifecycle violation: {} omitted the negotiated room operation id",
                    server_message_name(&message)
                ));
            }
            return Ok(Ok(message));
        };

        if !self.room_operation_ids {
            return Err(
                "lifecycle violation: RoomOperationResult arrived without negotiating room_operation_ids"
                    .into(),
            );
        }
        let Some(pending) = self.pending_room_operation.as_ref() else {
            return Err(format!(
                "lifecycle violation: RoomOperationResult {operation_id} arrived without a pending room operation"
            ));
        };
        if pending.operation_id != Some(operation_id) {
            return Err(format!(
                "lifecycle violation: RoomOperationResult {operation_id} did not match the pending room operation id"
            ));
        }
        if !room_operation_result_matches(pending.kind, &result) {
            return Err(format!(
                "lifecycle violation: {} result conflicts with pending room operation {:?}",
                room_operation_result_name(&result),
                pending.kind
            ));
        }
        Ok(result.into_server_message())
    }

    fn clear_pending_operation_after_correlated_failure(&mut self) {
        if self
            .pending_room_operation
            .as_ref()
            .is_some_and(|pending| pending.kind == PendingRoomOperation::ReconnectPlayer)
        {
            self.pending_reconnects.pop_front();
        }
        self.pending_room_operation = None;
    }

    /// Retire an admitted directed room operation's fence once a frame
    /// classifies as that operation's typed terminal answer.
    ///
    /// Runs only when delivery accountability rejected the frame (the healthy
    /// path retires inside `update_state`), so suppression never strands the
    /// fence. Only the three authoritative baselines (`RoomJoined`,
    /// `SpectatorJoined`, `Reconnected`) can be accountability-rejected, so in
    /// practice this seam retires only join/spectator-join/reconnect fences;
    /// the remaining classifier arms are shared-classifier symmetry. Frames
    /// that merely name a different or forged operation keep it armed exactly
    /// as before: they violate and stay fenced by contract. Correlated
    /// `OperationFailed` results never reach this helper — their envelope
    /// unwraps to the dedicated failure path above before any validation runs.
    fn retire_answered_room_operation(&mut self, message: &ServerMessage) {
        let Some(pending) = self.pending_room_operation.as_ref() else {
            return;
        };
        if !terminal_message_matches(pending.kind, message) {
            return;
        }
        if pending.kind == PendingRoomOperation::ReconnectPlayer {
            self.pending_reconnects.pop_front();
        }
        self.pending_room_operation = None;
    }

    /// Release the operation fence when a command that armed it at queue
    /// admission fails to serialize at dequeue time.
    ///
    /// Unreachable while every `ClientMessage` field serializes infallibly,
    /// but one fallible field away from a permanent `RoomOperationPending` in
    /// both drivers, so the fence is released instead of asserted away.
    /// Matching is by operation kind only: at most one fence exists at a
    /// time, so a same-kind message — wrapped or not — can only be the
    /// command that armed it, never a competing operation.
    pub(crate) fn dequeue_serialization_failed(&mut self, message: &ClientMessage) {
        let kind = match message {
            ClientMessage::JoinRoom { .. } => PendingRoomOperation::JoinPlayer,
            ClientMessage::LeaveRoom => PendingRoomOperation::LeavePlayer,
            ClientMessage::Reconnect { .. } => PendingRoomOperation::ReconnectPlayer,
            ClientMessage::JoinAsSpectator { .. } => PendingRoomOperation::JoinSpectator,
            ClientMessage::LeaveSpectator => PendingRoomOperation::LeaveSpectator,
            ClientMessage::RoomOperation { operation, .. } => match operation.as_ref() {
                RoomOperationRequest::JoinRoom { .. } => PendingRoomOperation::JoinPlayer,
                RoomOperationRequest::LeaveRoom => PendingRoomOperation::LeavePlayer,
                RoomOperationRequest::Reconnect { .. } => PendingRoomOperation::ReconnectPlayer,
                RoomOperationRequest::JoinAsSpectator { .. } => PendingRoomOperation::JoinSpectator,
                RoomOperationRequest::LeaveSpectator => PendingRoomOperation::LeaveSpectator,
            },
            _ => return,
        };
        if self
            .pending_room_operation
            .as_ref()
            .is_some_and(|pending| pending.kind == kind)
        {
            if kind == PendingRoomOperation::ReconnectPlayer {
                self.pending_reconnects.pop_front();
            }
            self.pending_room_operation = None;
            tracing::warn!(
                "dequeued room operation failed to serialize; released its admission fence"
            );
        }
    }

    fn process_binary(&mut self, bytes: Vec<u8>) -> FrameOutcome {
        let mut outcome = FrameOutcome::new();
        if !self.snapshot.authenticated || self.room_role().is_none() {
            self.reject_inbound(
                &mut outcome,
                format!(
                    "lifecycle violation: binary game data is invalid while authenticated={} and membership={:?}",
                    self.snapshot.authenticated,
                    self.room_role()
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

        self.stats.game_data_received = self.stats.game_data_received.saturating_add(1);

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
        let membership = self.room_role();
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
                !authenticated && membership.is_none()
            }
            ServerMessage::ProtocolInfo(_) => {
                authenticated && membership.is_none() && !self.protocol_info_seen
            }
            ServerMessage::RoomJoined(_)
            | ServerMessage::Reconnected(_)
            | ServerMessage::SpectatorJoined(_) => authenticated && membership.is_none(),
            ServerMessage::RoomJoinFailed { .. }
            | ServerMessage::ReconnectionFailed { .. }
            | ServerMessage::SpectatorJoinFailed { .. } => authenticated,
            ServerMessage::RoomLeft => authenticated && membership == Some(RoomRole::Player),
            ServerMessage::SpectatorLeft { .. } => {
                authenticated && membership == Some(RoomRole::Spectator)
            }
            ServerMessage::RoomOperationResult { .. } => false,
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
            | ServerMessage::DeliveryReport(_) => authenticated && membership.is_some(),
            ServerMessage::Signal { .. }
            | ServerMessage::NewPeer { .. }
            | ServerMessage::SessionPlan(_)
            | ServerMessage::PeerTransportStatus { .. } => {
                authenticated && membership == Some(RoomRole::Player)
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
            ServerMessage::ProtocolInfo(payload) => {
                validate_protocol_info_formats(payload)?;
                if payload
                    .capabilities
                    .iter()
                    .any(|capability| capability == ROOM_OPERATION_IDS_CAPABILITY)
                    && !self.requested_room_operation_ids
                {
                    return Err(
                        "lifecycle violation: ProtocolInfo enabled unrequested room_operation_ids"
                            .into(),
                    );
                }
                Ok(())
            }
            ServerMessage::RoomJoined(payload) => {
                self.validate_pending_room_response(
                    PendingRoomOperation::JoinPlayer,
                    "RoomJoined",
                )?;
                if self
                    .snapshot
                    .negotiated_protocol_version
                    .is_none_or(|version| version < 3)
                    && (payload.reconnection_token.is_some() || !payload.ice_servers.is_empty())
                {
                    return Err(
                        "lifecycle violation: v2 RoomJoined exposed v3 token or ICE-server metadata"
                            .into(),
                    );
                }
                validate_local_player_snapshot(
                    payload.player_id,
                    payload.is_authority,
                    &payload.current_players,
                )
            }
            ServerMessage::RoomJoinFailed { .. } => self.validate_pending_room_response(
                PendingRoomOperation::JoinPlayer,
                "RoomJoinFailed",
            ),
            ServerMessage::RoomLeft => self
                .validate_pending_room_response(PendingRoomOperation::LeavePlayer, "RoomLeft"),
            ServerMessage::SpectatorJoined(payload) => {
                self.validate_pending_room_response(
                    PendingRoomOperation::JoinSpectator,
                    "SpectatorJoined",
                )?;
                validate_authority_snapshot(&payload.current_players)
            }
            ServerMessage::SpectatorJoinFailed { .. } => self.validate_pending_room_response(
                PendingRoomOperation::JoinSpectator,
                "SpectatorJoinFailed",
            ),
            ServerMessage::SpectatorLeft {
                room_id,
                room_code,
                reason,
                ..
            } => {
                let authoritative_exit = spectator_exit_is_authoritative(reason.as_ref());
                match reason {
                    Some(
                        crate::protocol::SpectatorStateChangeReason::Disconnected
                        | crate::protocol::SpectatorStateChangeReason::Removed
                        | crate::protocol::SpectatorStateChangeReason::RoomClosed,
                    ) => {
                        // The wire authority leaves `room_id` optional; an
                        // authoritative exit from the current room need not
                        // repeat its identity, but a named room must match.
                        if room_id.is_some_and(|room_id| Some(room_id) != self.snapshot.room_id) {
                            return Err(
                                "lifecycle violation: authoritative SpectatorLeft must identify the current room"
                                    .into(),
                            );
                        }
                    }
                    Some(crate::protocol::SpectatorStateChangeReason::VoluntaryLeave) | None => {
                        self.validate_pending_room_response(
                            PendingRoomOperation::LeaveSpectator,
                            "SpectatorLeft",
                        )?;
                    }
                    Some(crate::protocol::SpectatorStateChangeReason::Joined) => {
                        return Err(
                            "lifecycle violation: SpectatorLeft carries a joined reason".into()
                        );
                    }
                }
                if (!authoritative_exit
                    && room_id.is_some_and(|room_id| Some(room_id) != self.snapshot.room_id))
                    || room_code
                        .as_ref()
                        .is_some_and(|room_code| Some(room_code) != self.snapshot.room_code.as_ref())
                {
                    return Err(
                        "lifecycle violation: SpectatorLeft identifies a different room"
                            .into(),
                    );
                }
                Ok(())
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
                if self.snapshot.session_transport != Some(TransportKind::WebRtc) {
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
                    && self.snapshot.session_transport == Some(TransportKind::WebRtc)) =>
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
            ServerMessage::PlayerJoined { player }
                if player.is_authority
                    && self
                        .authority_player
                        .is_some_and(|authority| authority != player.id) =>
            {
                Err(
                    "lifecycle violation: PlayerJoined introduces a second authority player"
                        .into(),
                )
            }
            ServerMessage::PlayerJoined { player }
                if !self.room_players.contains(&player.id)
                    && self.room_players.len() >= self.roster_capacity() =>
            {
                Err(format!(
                    "lifecycle violation: PlayerJoined {} exceeds the advertised room capacity of {} players",
                    player.id,
                    self.roster_capacity()
                ))
            }
            ServerMessage::AuthorityChanged {
                authority_player,
                you_are_authority,
            } => {
                if authority_player
                    .is_some_and(|player_id| !self.room_players.contains(&player_id))
                {
                    return Err(format!(
                        "lifecycle violation: AuthorityChanged names a player outside the current room roster: {authority_player:?}"
                    ));
                }
                let local_is_authority = *authority_player == self.snapshot.player_id;
                if *you_are_authority != local_is_authority {
                    return Err(
                        "lifecycle violation: AuthorityChanged local authority flag disagrees with the authority player"
                            .into(),
                    );
                }
                Ok(())
            }
            ServerMessage::Reconnected(payload) => {
                self.validate_pending_room_response(
                    PendingRoomOperation::ReconnectPlayer,
                    "Reconnected",
                )?;
                validate_local_player_snapshot(
                    payload.player_id,
                    payload.is_authority,
                    &payload.current_players,
                )?;
                self.validate_reconnected_payload(payload)
            }
            ServerMessage::ReconnectionFailed { .. } => {
                self.validate_pending_room_response(
                    PendingRoomOperation::ReconnectPlayer,
                    "ReconnectionFailed",
                )?;
                if self.pending_reconnects.is_empty() {
                    return Err(
                        "lifecycle violation: ReconnectionFailed arrived without an admitted Reconnect"
                            .into(),
                    );
                }
                Ok(())
            }
            ServerMessage::RoomOperationResult { .. } => Err(
                "lifecycle violation: RoomOperationResult was not normalized before validation"
                    .into(),
            ),
            _ => Ok(()),
        }
    }

    /// Effective player-roster admission ceiling: the advertised
    /// `max_players`, or the wire-absolute fallback when the latest baseline
    /// could not advertise one (spectator joins).
    fn roster_capacity(&self) -> usize {
        self.room_max_players
            .map_or(ABSOLUTE_ROSTER_CAPACITY, usize::from)
    }

    fn validate_pending_room_response(
        &self,
        expected: PendingRoomOperation,
        response: &str,
    ) -> Result<(), String> {
        match self.pending_room_operation.as_ref() {
            Some(pending) if pending.kind == expected => Ok(()),
            Some(pending) => Err(format!(
                "lifecycle violation: {response} conflicts with pending room operation {:?}",
                pending.kind
            )),
            None => Err(format!(
                "lifecycle violation: {response} arrived without an admitted {expected:?} operation"
            )),
        }
    }

    fn validate_session_plan(
        &self,
        plan: &SessionPlanPayload,
        local_player_id: Option<PlayerId>,
        room_players: &HashSet<PlayerId>,
    ) -> Result<(), String> {
        if plan.generation != self.snapshot.session_generation
            && plan
                .generation
                .is_some_and(|generation| self.retired_session_generations.contains(&generation))
        {
            return Err(format!(
                "lifecycle violation: SessionPlan generation {:?} was already superseded",
                plan.generation
            ));
        }

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
        if generation != self.snapshot.session_generation {
            return true;
        }
        match self.snapshot.session_generation {
            // Server 0.4 omitted generations. Retained peer identity is the
            // only fence for a signal racing a generation-less re-plan, so
            // retirement persists for the whole room session.
            None => {
                self.retired_signal_peers.contains(&from)
                    && !(self.snapshot.session_transport == Some(TransportKind::WebRtc)
                        && self.session_peers.contains(&from))
            }
            // Generation-carrying sessions bound retirement to the live
            // generation: a departed or re-planned-out peer's final signals
            // were valid when the server relayed them and merely raced this
            // client's view of the departure, whatever transport now carries
            // the stream — gating on the current transport would re-open the
            // relay-fallback race this fence closes. The window extends
            // through a roster rejoin until the peer is re-paired by a plan
            // or compatibility `NewPeer`.
            Some(_) => {
                self.retired_signal_peers.contains(&from) && !self.session_peers.contains(&from)
            }
        }
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
                self.snapshot.server_max_outbound_message_size = payload.max_outbound_message_size;
                self.room_operation_ids = self.requested_room_operation_ids
                    && payload.protocol_version.is_some_and(|version| version >= 3)
                    && payload
                        .capabilities
                        .iter()
                        .any(|capability| capability == ROOM_OPERATION_IDS_CAPABILITY);
                self.protocol_info_seen = true;
            }
            ServerMessage::RoomJoined(payload) => {
                self.set_room(RoomBaseline {
                    player_id: payload.player_id,
                    room_id: payload.room_id,
                    room_code: payload.room_code.clone(),
                    reconnection_token: payload.reconnection_token.clone(),
                    room_role: RoomRole::Player,
                    authority_player: payload
                        .current_players
                        .iter()
                        .find(|player| player.is_authority)
                        .map(|player| player.id),
                    finalized: payload.lobby_state == crate::protocol::LobbyState::Finalized,
                    players: payload
                        .current_players
                        .iter()
                        .map(|player| player.id)
                        .collect(),
                    max_players: Some(payload.max_players),
                });
            }
            ServerMessage::RoomJoinFailed { .. } => {
                if self
                    .pending_room_operation
                    .as_ref()
                    .is_some_and(|pending| pending.kind == PendingRoomOperation::JoinPlayer)
                {
                    self.pending_room_operation = None;
                }
            }
            ServerMessage::RoomLeft => self.clear_room(),
            ServerMessage::Reconnected(payload) => {
                self.pending_reconnects.pop_front();
                self.set_room(RoomBaseline {
                    player_id: payload.player_id,
                    room_id: payload.room_id,
                    room_code: payload.room_code.clone(),
                    reconnection_token: payload.reconnection_token.clone(),
                    room_role: RoomRole::Player,
                    authority_player: payload
                        .current_players
                        .iter()
                        .find(|player| player.is_authority)
                        .map(|player| player.id),
                    finalized: payload.lobby_state == crate::protocol::LobbyState::Finalized,
                    players: payload
                        .current_players
                        .iter()
                        .map(|player| player.id)
                        .collect(),
                    max_players: Some(payload.max_players),
                });
            }
            ServerMessage::SpectatorJoined(payload) => {
                self.set_room(RoomBaseline {
                    player_id: payload.spectator_id,
                    room_id: payload.room_id,
                    room_code: payload.room_code.clone(),
                    reconnection_token: None,
                    room_role: RoomRole::Spectator,
                    authority_player: payload
                        .current_players
                        .iter()
                        .find(|player| player.is_authority)
                        .map(|player| player.id),
                    finalized: payload.lobby_state == crate::protocol::LobbyState::Finalized,
                    players: payload
                        .current_players
                        .iter()
                        .map(|player| player.id)
                        .collect(),
                    max_players: None,
                });
            }
            ServerMessage::SpectatorJoinFailed { .. } => {
                if self
                    .pending_room_operation
                    .as_ref()
                    .is_some_and(|pending| pending.kind == PendingRoomOperation::JoinSpectator)
                {
                    self.pending_room_operation = None;
                }
            }
            ServerMessage::SpectatorLeft { reason, .. } => {
                let authoritative_exit = spectator_exit_is_authoritative(reason.as_ref());
                let mut overtaken_leave = None;
                if authoritative_exit
                    && self
                        .pending_room_operation
                        .as_ref()
                        .is_some_and(|pending| pending.kind == PendingRoomOperation::LeaveSpectator)
                {
                    overtaken_leave =
                        self.pending_room_operation
                            .take()
                            .map(|pending| OvertakenSpectatorLeave {
                                pending,
                                room_id: self.snapshot.room_id,
                                room_code: self.snapshot.room_code.clone(),
                            });
                }
                self.clear_room();
                self.absorbed_spectator_leave = overtaken_leave;
            }
            ServerMessage::ReconnectionFailed { .. } => {
                self.pending_reconnects.pop_front();
                if self
                    .pending_room_operation
                    .as_ref()
                    .is_some_and(|pending| pending.kind == PendingRoomOperation::ReconnectPlayer)
                {
                    self.pending_room_operation = None;
                }
            }
            ServerMessage::SessionPlan(payload) => {
                self.replace_session_plan(
                    payload.generation,
                    payload.peers.iter().map(|peer| peer.player_id),
                    payload.topology,
                    payload.transport,
                );
            }
            ServerMessage::NewPeer { peer_id, .. } => {
                self.session_peers.insert(*peer_id);
                self.retired_signal_peers.remove(peer_id);
            }
            ServerMessage::PlayerLeft { player_id, .. } => {
                if self.authority_player == Some(*player_id) {
                    self.authority_player = None;
                }
                if self.snapshot.session_transport == Some(TransportKind::WebRtc)
                    && self.session_peers.contains(player_id)
                {
                    self.retired_signal_peers.insert(*player_id);
                }
                self.session_peers.remove(player_id);
                self.room_players.remove(player_id);
            }
            ServerMessage::PlayerJoined { player } => {
                self.room_players.insert(player.id);
                if player.is_authority {
                    self.authority_player = Some(player.id);
                }
            }
            ServerMessage::AuthorityChanged {
                authority_player, ..
            } => {
                self.authority_player = *authority_player;
            }
            ServerMessage::LobbyStateChanged { lobby_state, .. } => {
                self.room_finalized = *lobby_state == crate::protocol::LobbyState::Finalized;
            }
            ServerMessage::GameStarting { .. } => {
                self.room_finalized = true;
            }
            _ => {}
        }
    }

    fn set_room(&mut self, baseline: RoomBaseline) {
        self.snapshot.player_id = Some(baseline.player_id);
        self.snapshot.room_id = Some(baseline.room_id);
        self.snapshot.room_code = Some(baseline.room_code);
        self.snapshot.room_role = Some(baseline.room_role);
        self.snapshot.reconnection_token = baseline.reconnection_token;
        self.snapshot.session_generation = None;
        self.snapshot.session_topology = None;
        self.snapshot.session_transport = None;
        self.snapshot.quarantined = false;
        self.authority_player = baseline.authority_player;
        self.room_finalized = baseline.finalized;
        self.room_players = baseline.players;
        self.room_max_players = baseline.max_players;
        self.session_plan_seen = false;
        self.session_peers.clear();
        self.retired_session_generations.clear();
        self.retired_signal_peers.clear();
        self.pending_room_operation = None;
        self.absorbed_spectator_leave = None;
        self.pending_reconnects.clear();
        #[cfg(feature = "tokio-runtime")]
        self.advance_room_revision();
    }

    fn clear_room(&mut self) {
        self.accountability.reset_room();
        self.snapshot.player_id = None;
        self.snapshot.room_id = None;
        self.snapshot.room_code = None;
        self.snapshot.room_role = None;
        self.snapshot.reconnection_token = None;
        self.snapshot.session_generation = None;
        self.snapshot.session_topology = None;
        self.snapshot.session_transport = None;
        self.snapshot.quarantined = false;
        self.authority_player = None;
        self.room_finalized = false;
        self.room_players.clear();
        self.room_max_players = None;
        self.session_plan_seen = false;
        self.session_peers.clear();
        self.retired_session_generations.clear();
        self.retired_signal_peers.clear();
        self.pending_room_operation = None;
        // Clearing these two here is unobservable: the fence is cleared in
        // tandem, every reader is fence-gated, and the SpectatorLeft caller
        // overwrites the allowance immediately after. Owning the full
        // room-scoped set locally keeps these transitions correct even if a
        // future exit kind reaches them.
        self.absorbed_spectator_leave = None;
        self.pending_reconnects.clear();
        #[cfg(feature = "tokio-runtime")]
        self.advance_room_revision();
    }

    fn replace_session_plan(
        &mut self,
        generation: Option<SessionGeneration>,
        peers: impl IntoIterator<Item = PlayerId>,
        topology: Topology,
        transport: TransportKind,
    ) {
        if self.snapshot.session_generation != generation {
            if let Some(current) = self.snapshot.session_generation {
                self.retired_session_generations.push_back(current);
                while self.retired_session_generations.len() > RETIRED_SESSION_GENERATION_FENCE {
                    self.retired_session_generations.pop_front();
                }
            }
            // Peer retirements were scoped to the superseded generation;
            // its signals now die in the generation check alone.
            self.retired_signal_peers.clear();
        }
        let peers: HashSet<_> = peers.into_iter().collect();
        if self.session_plan_seen
            && self.snapshot.session_generation == generation
            && self.snapshot.session_transport == Some(TransportKind::WebRtc)
        {
            // Peers dropped by a same-generation replacement may still have
            // signals in flight stamped with that still-live generation;
            // retire them so those final frames stay benign races. A
            // generation change must not carry retirement across: dropped
            // peers never held authority under the new generation, so their
            // current-generation signals are genuine violations while their
            // superseded-generation frames already die in the generation
            // check.
            self.retired_signal_peers.extend(
                self.session_peers
                    .iter()
                    .filter(|peer| !peers.contains(*peer))
                    .copied(),
            );
        }
        // Every peer named by the new plan is live again.
        self.retired_signal_peers
            .retain(|peer| !peers.contains(peer));
        self.snapshot.session_generation = generation;
        self.snapshot.session_topology = Some(topology);
        self.snapshot.session_transport = Some(transport);
        self.session_plan_seen = true;
        self.session_peers = peers;
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

fn correlate_room_operation(
    message: ClientMessage,
    operation_id: RoomOperationId,
) -> ClientMessage {
    let operation = match message {
        ClientMessage::JoinRoom {
            game_name,
            room_code,
            player_name,
            max_players,
            supports_authority,
            relay_transport,
        } => RoomOperationRequest::JoinRoom {
            game_name,
            room_code,
            player_name,
            max_players,
            supports_authority,
            relay_transport,
        },
        ClientMessage::LeaveRoom => RoomOperationRequest::LeaveRoom,
        ClientMessage::Reconnect {
            player_id,
            room_id,
            auth_token,
        } => RoomOperationRequest::Reconnect {
            player_id,
            room_id,
            auth_token,
        },
        ClientMessage::JoinAsSpectator {
            game_name,
            room_code,
            spectator_name,
        } => RoomOperationRequest::JoinAsSpectator {
            game_name,
            room_code,
            spectator_name,
        },
        ClientMessage::LeaveSpectator => RoomOperationRequest::LeaveSpectator,
        other => return other,
    };
    ClientMessage::RoomOperation {
        operation_id,
        operation: Box::new(operation),
    }
}

fn is_uncorrelated_directed_room_response(message: &ServerMessage) -> bool {
    match message {
        ServerMessage::RoomJoined(_)
        | ServerMessage::RoomJoinFailed { .. }
        | ServerMessage::RoomLeft
        | ServerMessage::Reconnected(_)
        | ServerMessage::ReconnectionFailed { .. }
        | ServerMessage::SpectatorJoined(_)
        | ServerMessage::SpectatorJoinFailed { .. } => true,
        ServerMessage::SpectatorLeft { reason, .. } => {
            !spectator_exit_is_authoritative(reason.as_ref())
        }
        _ => false,
    }
}

/// Whether a [`SpectatorLeft`](crate::protocol::ServerMessage::SpectatorLeft)
/// reason reports an authoritative exit rather than an answer to an admitted
/// voluntary leave. Single source of truth for every classifier and state arm
/// that partitions the two, so the boundary cannot drift apart between them.
fn spectator_exit_is_authoritative(
    reason: Option<&crate::protocol::SpectatorStateChangeReason>,
) -> bool {
    matches!(
        reason,
        Some(
            crate::protocol::SpectatorStateChangeReason::Disconnected
                | crate::protocol::SpectatorStateChangeReason::Removed
                | crate::protocol::SpectatorStateChangeReason::RoomClosed
        )
    )
}

/// Whether a `SpectatorLeft` reason answers an admitted voluntary leave:
/// exactly the no-reason or voluntary-leave faces.
///
/// The reason space partitions in three, and the third face is not the
/// complement of either side above: a malformed
/// [`Joined`](crate::protocol::SpectatorStateChangeReason::Joined) reason
/// belongs to no valid partition, so every classifier must exclude it
/// explicitly instead of folding it into the voluntary face via negation.
fn spectator_exit_answers_voluntary_leave(
    reason: Option<&crate::protocol::SpectatorStateChangeReason>,
) -> bool {
    matches!(
        reason,
        None | Some(crate::protocol::SpectatorStateChangeReason::VoluntaryLeave)
    )
}

/// Whether an unwrapped server message is the typed terminal answer for a
/// pending directed room operation (the uncorrelated face of
/// [`ClientCore::retire_answered_room_operation`]).
fn terminal_message_matches(kind: PendingRoomOperation, message: &ServerMessage) -> bool {
    match kind {
        PendingRoomOperation::JoinPlayer => matches!(
            message,
            ServerMessage::RoomJoined(_) | ServerMessage::RoomJoinFailed { .. }
        ),
        PendingRoomOperation::LeavePlayer => matches!(message, ServerMessage::RoomLeft),
        PendingRoomOperation::ReconnectPlayer => matches!(
            message,
            ServerMessage::Reconnected(_) | ServerMessage::ReconnectionFailed { .. }
        ),
        PendingRoomOperation::JoinSpectator => matches!(
            message,
            ServerMessage::SpectatorJoined(_) | ServerMessage::SpectatorJoinFailed { .. }
        ),
        PendingRoomOperation::LeaveSpectator => match message {
            ServerMessage::SpectatorLeft { reason, .. } => {
                spectator_exit_answers_voluntary_leave(reason.as_ref())
            }
            _ => false,
        },
    }
}

fn room_operation_result_matches(
    pending: PendingRoomOperation,
    result: &RoomOperationResult,
) -> bool {
    match (pending, result) {
        (_, RoomOperationResult::OperationFailed { .. }) => true,
        (
            PendingRoomOperation::JoinPlayer,
            RoomOperationResult::RoomJoined(_) | RoomOperationResult::RoomJoinFailed { .. },
        )
        | (PendingRoomOperation::LeavePlayer, RoomOperationResult::RoomLeft)
        | (
            PendingRoomOperation::ReconnectPlayer,
            RoomOperationResult::Reconnected(_) | RoomOperationResult::ReconnectionFailed { .. },
        )
        | (
            PendingRoomOperation::JoinSpectator,
            RoomOperationResult::SpectatorJoined(_)
            | RoomOperationResult::SpectatorJoinFailed { .. },
        ) => true,
        (
            PendingRoomOperation::LeaveSpectator,
            RoomOperationResult::SpectatorLeft { reason, .. },
        ) => spectator_exit_answers_voluntary_leave(reason.as_ref()),
        _ => false,
    }
}

fn room_identity_matches(
    expected_id: Option<RoomId>,
    expected_code: Option<&String>,
    actual_id: Option<RoomId>,
    actual_code: Option<&String>,
) -> bool {
    actual_id.is_none_or(|room_id| Some(room_id) == expected_id)
        && actual_code.is_none_or(|room_code| Some(room_code) == expected_code)
}

fn room_operation_result_name(result: &RoomOperationResult) -> &'static str {
    match result {
        RoomOperationResult::RoomJoined(_) => "RoomJoined",
        RoomOperationResult::RoomJoinFailed { .. } => "RoomJoinFailed",
        RoomOperationResult::RoomLeft => "RoomLeft",
        RoomOperationResult::Reconnected(_) => "Reconnected",
        RoomOperationResult::ReconnectionFailed { .. } => "ReconnectionFailed",
        RoomOperationResult::SpectatorJoined(_) => "SpectatorJoined",
        RoomOperationResult::SpectatorJoinFailed { .. } => "SpectatorJoinFailed",
        RoomOperationResult::SpectatorLeft { .. } => "SpectatorLeft",
        RoomOperationResult::OperationFailed { .. } => "OperationFailed",
    }
}

fn validate_protocol_info_formats(
    payload: &crate::protocol::ProtocolInfoPayload,
) -> Result<(), String> {
    /// Vendored AsyncAPI authority: `max_outbound_message_size` is bounded
    /// `minimum: 1, maximum: 67108864`, and absent on negotiated v2. The
    /// version triple itself stays deliberately forward-compatible (`>= 3`
    /// pins `v4_negotiation_still_enables_mesh`): future protocol versions
    /// are treated as v3, so only the size field is bounded here.
    const MAX_OUTBOUND_MESSAGE_SIZE_BOUND: usize = 67_108_864;
    match payload.game_data_formats.as_slice() {
        [GameDataEncoding::Json]
        | [GameDataEncoding::Json, GameDataEncoding::MessagePack] => Ok(()),
        formats => Err(format!(
            "lifecycle violation: ProtocolInfo game_data_formats {formats:?} does not match the canonical Server 0.8 negotiation order [Json, MessagePack?]"
        )),
    }?;
    match (
        payload.protocol_version,
        payload.min_protocol_version,
        payload.max_protocol_version,
    ) {
        (None, None, None) if payload.transports.is_none() => {
            if let Some(size) = payload.max_outbound_message_size {
                return Err(format!(
                    "lifecycle violation: v2 ProtocolInfo exposed the v3-only \
                     max_outbound_message_size ({size})"
                ));
            }
            Ok(())
        }
        (Some(version), Some(min), Some(max))
            if version >= 3
                && min >= 2
                && min <= version
                && version <= max
                && payload.transports.as_deref() == Some(&[crate::protocol::MessageTransport::Websocket]) =>
        {
            match payload.max_outbound_message_size {
                None => Ok(()),
                Some(size) if (1..=MAX_OUTBOUND_MESSAGE_SIZE_BOUND).contains(&size) => Ok(()),
                Some(size) => Err(format!(
                    "lifecycle violation: ProtocolInfo max_outbound_message_size {size} \
                     violates the vendored authority bounds 1..=67108864"
                )),
            }
        }
        version_tuple => Err(format!(
            "lifecycle violation: ProtocolInfo version range {version_tuple:?} and transports presence do not form a coherent v2/v3 negotiation"
        )),
    }?;
    if payload
        .capabilities
        .iter()
        .any(|capability| capability == ROOM_OPERATION_IDS_CAPABILITY)
        && payload.protocol_version.is_none_or(|version| version < 3)
    {
        return Err(
            "lifecycle violation: ProtocolInfo advertised room_operation_ids below protocol v3"
                .into(),
        );
    }
    Ok(())
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
    local_is_authority: bool,
    players: &[crate::protocol::PlayerInfo],
) -> Result<(), String> {
    let mut local_players = players.iter().filter(|player| player.id == local_player_id);
    let Some(local_player) = local_players.next() else {
        return Err(format!(
            "lifecycle violation: authoritative player snapshot must contain local player {local_player_id} exactly once"
        ));
    };
    if local_players.next().is_some() {
        return Err(format!(
            "lifecycle violation: authoritative player snapshot must contain local player {local_player_id} exactly once"
        ));
    }
    if local_player.is_authority != local_is_authority {
        return Err(format!(
            "lifecycle violation: local authority flag for {local_player_id} disagrees with the authoritative player roster"
        ));
    }
    validate_authority_snapshot(players)
}

fn validate_authority_snapshot(players: &[crate::protocol::PlayerInfo]) -> Result<(), String> {
    let authority_count = players.iter().filter(|player| player.is_authority).count();
    if authority_count > 1 {
        return Err(
            "lifecycle violation: authoritative player snapshot contains multiple authority players"
                .into(),
        );
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
        ServerMessage::RoomOperationResult { .. } => "RoomOperationResult",
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
    clippy::unwrap_used,
    clippy::unreachable
)]
mod tests {
    use super::*;
    use crate::protocol::{
        DirectEndpoint, IceServer, LobbyState, MessageTransport, PlayerInfo, ProtocolInfoPayload,
        RateLimitInfo, ReconnectedPayload, ReplayStatus, RoomJoinedPayload, SenderWatermark,
        SessionPeer, SpectatorJoinedPayload, V2BinaryGameDataFrame,
    };

    /// A chain of `depth` nested arrays around one scalar.
    fn nested_chain(depth: u32) -> serde_json::Value {
        let mut value = serde_json::json!(depth);
        for _ in 0..depth {
            value = serde_json::Value::Array(vec![value]);
        }
        value
    }

    #[test]
    fn game_data_depth_walk_matches_the_container_bound_exactly() {
        assert!(game_data_depth_within(
            &serde_json::json!(42),
            MAX_GAME_DATA_DEPTH
        ));
        assert!(game_data_depth_within(
            &serde_json::json!({}),
            MAX_GAME_DATA_DEPTH
        ));
        assert!(game_data_depth_within(
            &serde_json::json!({"a": [1, {"b": [true, null]}]}),
            MAX_GAME_DATA_DEPTH
        ));
        // Exactly at the bound is accepted; one deeper is refused, on every
        // container shape.
        let at_bound = nested_chain(MAX_GAME_DATA_DEPTH as u32);
        assert!(game_data_depth_within(&at_bound, MAX_GAME_DATA_DEPTH));
        let over = serde_json::Value::Array(vec![at_bound]);
        assert!(!game_data_depth_within(&over, MAX_GAME_DATA_DEPTH));

        let at_bound = serde_json::json!({"k": nested_chain(127)});
        assert!(game_data_depth_within(&at_bound, MAX_GAME_DATA_DEPTH));
        let over = serde_json::json!({"k": nested_chain(128)});
        assert!(!game_data_depth_within(&over, MAX_GAME_DATA_DEPTH));

        // Deep branches do not mask shallow violations elsewhere.
        let mixed = serde_json::json!([nested_chain(200), {"flat": true, "deep": nested_chain(2)}]);
        assert!(!game_data_depth_within(&mixed, MAX_GAME_DATA_DEPTH));

        // Validation must stay stack-safe for pathologically nested caller
        // payloads: this chain is far deeper than any thread could recurse
        // over, and the bounded-budget walk refuses it without overflowing.
        let pathological = nested_chain(200_000);
        assert!(!game_data_depth_within(&pathological, MAX_GAME_DATA_DEPTH));
        // Dropping a 200k-deep chain recurses in `serde_json` itself and
        // would abort the test thread; leaking this proof-only fixture is
        // the bounded outcome (the same deliberate-leak precedent as the
        // Emscripten close-before-delete policy).
        #[allow(clippy::mem_forget)]
        std::mem::forget(pathological);
    }

    #[test]
    fn validate_refuses_deep_caller_payloads_at_admission() {
        let core = v3_room(ProtocolViolationPolicy::Quarantine);
        let deep = serde_json::Value::Array(vec![nested_chain(MAX_GAME_DATA_DEPTH as u32)]);
        assert!(matches!(
            core.validate(&ClientOperation::GameData(
                deep.clone(),
                GameDataDelivery::Reliable
            )),
            Err(crate::SignalFishError::PayloadTooDeep { max_depth: 128 })
        ));
        assert!(matches!(
            core.validate(&ClientOperation::ProvideConnectionInfo(
                ConnectionInfo::Custom { data: deep },
            )),
            Err(crate::SignalFishError::PayloadTooDeep { max_depth: 128 })
        ));
        // At the bound the same operations are admitted.
        core.validate(&ClientOperation::GameData(
            nested_chain(MAX_GAME_DATA_DEPTH as u32),
            GameDataDelivery::Reliable,
        ))
        .expect("at-bound game data must validate");
        core.validate(&ClientOperation::ProvideConnectionInfo(
            ConnectionInfo::Custom {
                data: nested_chain(MAX_GAME_DATA_DEPTH as u32),
            },
        ))
        .expect("at-bound custom connection info must validate");
    }

    #[test]
    fn optimized_game_data_serialization_preserves_canonical_wire() {
        let values = [
            serde_json::Value::Null,
            serde_json::json!(false),
            serde_json::json!(u64::MAX),
            serde_json::json!(i64::MIN),
            serde_json::json!(f64::MAX),
            serde_json::json!("quoted \" text \\ and controls\n\t\u{0000}"),
            serde_json::json!({
                "unicode-鱼": ["signal", {"nested": "j".repeat(4_096)}],
                "numbers": [0, -1, 1.5e200],
            }),
            serde_json::Value::String("\"\\\n\t\u{0000}".repeat(1_024)),
            serde_json::Value::String("鱼🐟".repeat(1_024)),
        ];
        let deliveries = [
            (None, None),
            (Some(DeliveryClass::Latest), Some(u32::MAX)),
            (Some(DeliveryClass::Volatile), None),
        ];

        for value in values {
            for (class, key) in deliveries {
                let message = ClientMessage::GameData {
                    data: value.clone(),
                    class,
                    key,
                };
                let canonical = serde_json::to_string(&message)
                    .expect("canonical GameData fixture should serialize");
                let optimized = serialize_client_message(&message)
                    .expect("optimized GameData fixture should serialize");
                assert_eq!(optimized, canonical);
            }
        }

        let ordinary_large_payload = serde_json::Value::String("j".repeat(4_096));
        let message = ClientMessage::GameData {
            data: ordinary_large_payload.clone(),
            class: None,
            key: None,
        };
        assert!(
            ordinary_large_payload
                .as_str()
                .expect("fixture is a JSON string")
                .len()
                .saturating_add(2)
                .saturating_add(GAME_DATA_JSON_ENVELOPE_CAPACITY)
                >= serde_json::to_string(&message)
                    .expect("large GameData fixture should serialize")
                    .len()
        );

        let control = ClientMessage::Ping;
        assert_eq!(
            serialize_client_message(&control).expect("optimized control message should serialize"),
            serde_json::to_string(&control).expect("canonical control message should serialize")
        );
    }

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
        // Server-sendable shapes only: a v2 connection's ProtocolInfo omits
        // all five v3 fields including `protocol_version` itself (vendored
        // AsyncAPI), so versions below 3 produce exactly the `None` shape.
        let v3_fields = version.filter(|version| *version >= 3);
        ServerMessage::ProtocolInfo(ProtocolInfoPayload {
            platform: None,
            sdk_version: None,
            minimum_version: None,
            recommended_version: None,
            capabilities: vec![],
            notes: None,
            game_data_formats: vec![GameDataEncoding::Json],
            player_name_rules: None,
            protocol_version: v3_fields,
            min_protocol_version: v3_fields.map(|_| 2),
            max_protocol_version: v3_fields,
            transports: v3_fields.map(|_| vec![MessageTransport::Websocket]),
            max_outbound_message_size: v3_fields.map(|_| 8 * 1024 * 1024),
        })
    }

    fn protocol_info_with_room_operation_ids(version: u16) -> ServerMessage {
        let ServerMessage::ProtocolInfo(mut payload) = protocol_info(Some(version)) else {
            unreachable!("protocol_info helper always returns ProtocolInfo")
        };
        payload
            .capabilities
            .push(ROOM_OPERATION_IDS_CAPABILITY.to_string());
        ServerMessage::ProtocolInfo(payload)
    }

    fn correlated_outside(policy: ProtocolViolationPolicy) -> ClientCore {
        let mut core = ClientCore::new_with_room_operation_ids(
            Some(GameDataEncoding::Json),
            policy,
            true,
            true,
        );
        assert_eq!(process(&mut core, authenticated()).events.len(), 1);
        assert_eq!(
            process(&mut core, protocol_info_with_room_operation_ids(3))
                .events
                .len(),
            1
        );
        core
    }

    fn prepare_and_admit(
        core: &mut ClientCore,
        operation: ClientOperation,
    ) -> (CoreCommand, RoomOperationId) {
        let (command, admission) = core
            .prepare_with_admission(operation)
            .expect("room operation should prepare");
        let CoreCommand::Message(ClientMessage::RoomOperation { operation_id, .. }) = &command
        else {
            panic!("negotiated room operation must use the correlated envelope")
        };
        let operation_id = *operation_id;
        core.record_admission(admission);
        assert_eq!(
            core.pending_room_operation
                .as_ref()
                .and_then(|pending| pending.operation_id),
            Some(operation_id),
            "the emitted UUID and admission fence must be identical"
        );
        (command, operation_id)
    }

    fn correlated_result(
        operation_id: RoomOperationId,
        result: RoomOperationResult,
    ) -> ServerMessage {
        ServerMessage::RoomOperationResult {
            operation_id,
            result: Box::new(result),
        }
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
        // The v2 dialect carries no v3-only room metadata.
        payload.reconnection_token = None;
        payload.ice_servers = Vec::new();
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
        core.record_admission(ClientCore::admission_for(&ClientOperation::JoinRoom(
            JoinRoomParams::new("game", "local"),
        )));
        assert_eq!(process(&mut core, room_joined()).events.len(), 1);
        core
    }

    fn v3_spectator(policy: ProtocolViolationPolicy) -> ClientCore {
        let mut core = ClientCore::new(Some(GameDataEncoding::Json), policy, true);
        let _ = process(&mut core, authenticated());
        let _ = process(&mut core, protocol_info(Some(3)));
        core.record_admission(ClientCore::admission_for(
            &ClientOperation::JoinAsSpectator("game".into(), "ROOM".into(), "viewer".into()),
        ));
        let _ = process(&mut core, spectator_joined());
        core
    }

    #[test]
    fn room_operation_capability_requires_request_and_echo_with_legacy_fallback() {
        let join = || ClientOperation::JoinRoom(JoinRoomParams::new("game", "local"));

        let mut requested_without_echo = ClientCore::new_with_room_operation_ids(
            Some(GameDataEncoding::Json),
            ProtocolViolationPolicy::Observe,
            true,
            true,
        );
        let _ = process(&mut requested_without_echo, authenticated());
        let _ = process(&mut requested_without_echo, protocol_info(Some(3)));
        let (command, admission) = requested_without_echo
            .prepare_with_admission(join())
            .expect("missing echo falls back to the legacy command");
        assert!(matches!(
            command,
            CoreCommand::Message(ClientMessage::JoinRoom { .. })
        ));
        assert_eq!(
            admission
                .as_ref()
                .and_then(|admission| admission.pending.operation_id),
            None
        );

        let mut unsolicited = ClientCore::new(
            Some(GameDataEncoding::Json),
            ProtocolViolationPolicy::Observe,
            true,
        );
        let _ = process(&mut unsolicited, authenticated());
        let outcome = process(&mut unsolicited, protocol_info_with_room_operation_ids(3));
        assert_lifecycle_violation_containing(&outcome, "unrequested room_operation_ids");
        assert!(!unsolicited.room_operation_ids);

        let mut below_v3 = ClientCore::new_with_room_operation_ids(
            Some(GameDataEncoding::Json),
            ProtocolViolationPolicy::Observe,
            true,
            true,
        );
        let _ = process(&mut below_v3, authenticated());
        let ServerMessage::ProtocolInfo(mut payload) = protocol_info(None) else {
            unreachable!("protocol_info helper always returns ProtocolInfo")
        };
        payload
            .capabilities
            .push(ROOM_OPERATION_IDS_CAPABILITY.to_string());
        let outcome = process(&mut below_v3, ServerMessage::ProtocolInfo(payload));
        assert_lifecycle_violation_containing(&outcome, "below protocol v3");
        assert!(!below_v3.room_operation_ids);

        let mut unknown_only = ClientCore::new_with_room_operation_ids(
            Some(GameDataEncoding::Json),
            ProtocolViolationPolicy::Observe,
            true,
            true,
        );
        let _ = process(&mut unknown_only, authenticated());
        let ServerMessage::ProtocolInfo(mut payload) = protocol_info(Some(3)) else {
            unreachable!("protocol_info helper always returns ProtocolInfo")
        };
        payload.capabilities.push("future_capability".into());
        let outcome = process(&mut unknown_only, ServerMessage::ProtocolInfo(payload));
        assert!(matches!(
            outcome.events.as_slice(),
            [SignalFishEvent::ProtocolInfo(_)]
        ));
        assert!(!unknown_only.room_operation_ids);
    }

    #[test]
    fn advertised_outbound_limit_follows_negotiation_and_teardown_lifecycle() {
        let mut v3 = ClientCore::new(
            Some(GameDataEncoding::Json),
            ProtocolViolationPolicy::Observe,
            true,
        );
        assert_eq!(v3.snapshot().server_max_outbound_message_size, None);
        let _ = process(&mut v3, authenticated());
        let _ = process(&mut v3, protocol_info(Some(3)));
        assert_eq!(
            v3.snapshot().server_max_outbound_message_size,
            Some(8 * 1024 * 1024)
        );
        let _ = v3.disconnect(None);
        assert_eq!(v3.snapshot().server_max_outbound_message_size, None);

        let mut v2 = ClientCore::new(
            Some(GameDataEncoding::Json),
            ProtocolViolationPolicy::Observe,
            true,
        );
        let _ = process(&mut v2, authenticated());
        let _ = process(&mut v2, protocol_info(None));
        assert_eq!(v2.snapshot().server_max_outbound_message_size, None);

        let mut omitted = ClientCore::new(
            Some(GameDataEncoding::Json),
            ProtocolViolationPolicy::Observe,
            true,
        );
        let _ = process(&mut omitted, authenticated());
        let ServerMessage::ProtocolInfo(mut payload) = protocol_info(Some(3)) else {
            unreachable!("protocol_info helper always returns ProtocolInfo")
        };
        payload.max_outbound_message_size = None;
        let _ = process(&mut omitted, ServerMessage::ProtocolInfo(payload));
        assert_eq!(omitted.snapshot().server_max_outbound_message_size, None);
    }

    #[test]
    fn legacy_admission_before_capability_echo_retains_its_wire_mode() {
        let mut core = ClientCore::new_with_room_operation_ids(
            Some(GameDataEncoding::Json),
            ProtocolViolationPolicy::Observe,
            true,
            true,
        );
        let _ = process(&mut core, authenticated());
        let (command, admission) = core
            .prepare_with_admission(ClientOperation::JoinRoom(JoinRoomParams::new(
                "game", "local",
            )))
            .expect("pre-ProtocolInfo join retains legacy interoperability");
        assert!(matches!(
            command,
            CoreCommand::Message(ClientMessage::JoinRoom { .. })
        ));
        core.record_admission(admission);
        assert_eq!(
            core.pending_room_operation
                .as_ref()
                .and_then(|pending| pending.operation_id),
            None
        );

        let _ = process(&mut core, protocol_info_with_room_operation_ids(3));
        assert!(core.room_operation_ids);
        let outcome = process(&mut core, room_joined());
        assert!(matches!(
            outcome.events.as_slice(),
            [SignalFishEvent::RoomJoined { .. }]
        ));

        let (_, operation_id) = prepare_and_admit(&mut core, ClientOperation::LeaveRoom);
        assert_ne!(operation_id, RoomOperationId::nil());
    }

    #[test]
    fn correlated_admission_uses_fresh_ids_and_rejects_mismatches_without_unfencing() {
        let mut core = correlated_outside(ProtocolViolationPolicy::Observe);
        let join = || ClientOperation::JoinRoom(JoinRoomParams::new("game", "local"));
        let (_, first_id) = prepare_and_admit(&mut core, join());

        let wrong_id = RoomOperationId::from_u128(0xdddd);
        let outcome = process(
            &mut core,
            correlated_result(
                wrong_id,
                RoomOperationResult::RoomJoinFailed {
                    reason: "stale".into(),
                    error_code: None,
                },
            ),
        );
        assert_lifecycle_violation_containing(&outcome, "did not match");
        assert_eq!(
            core.pending_room_operation
                .as_ref()
                .and_then(|pending| pending.operation_id),
            Some(first_id)
        );

        let outcome = process(
            &mut core,
            correlated_result(first_id, RoomOperationResult::RoomLeft),
        );
        assert_lifecycle_violation_containing(&outcome, "conflicts");
        assert_eq!(
            core.pending_room_operation
                .as_ref()
                .and_then(|pending| pending.operation_id),
            Some(first_id)
        );

        let outcome = process(
            &mut core,
            correlated_result(
                first_id,
                RoomOperationResult::RoomJoinFailed {
                    reason: "full".into(),
                    error_code: Some(crate::ErrorCode::RoomFull),
                },
            ),
        );
        assert!(matches!(
            outcome.events.as_slice(),
            [SignalFishEvent::RoomJoinFailed { .. }]
        ));
        assert!(core.pending_room_operation.is_none());

        let (_, second_id) = prepare_and_admit(&mut core, join());
        assert_ne!(first_id, second_id, "each admission needs a fresh UUID");
        let outcome = process(
            &mut core,
            correlated_result(
                first_id,
                RoomOperationResult::RoomJoinFailed {
                    reason: "delayed duplicate".into(),
                    error_code: None,
                },
            ),
        );
        assert_lifecycle_violation_containing(&outcome, "did not match");
        assert_eq!(
            core.pending_room_operation
                .as_ref()
                .and_then(|pending| pending.operation_id),
            Some(second_id)
        );
    }

    #[test]
    fn correlated_operation_failed_is_attributed_without_becoming_server_error() {
        let mut core = correlated_outside(ProtocolViolationPolicy::Observe);
        let (_, operation_id) = prepare_and_admit(
            &mut core,
            ClientOperation::Reconnect(
                PlayerId::from_u128(LOCAL),
                RoomId::from_u128(10),
                "submitted-token".into(),
            ),
        );
        assert_eq!(core.pending_reconnects.len(), 1);

        let outcome = process(
            &mut core,
            correlated_result(
                operation_id,
                RoomOperationResult::OperationFailed {
                    reason: "cannot reconnect".into(),
                    error_code: Some(crate::ErrorCode::ReconnectionFailed),
                },
            ),
        );
        assert!(matches!(
            outcome.events.as_slice(),
            [SignalFishEvent::RoomOperationFailed { reason, .. }] if reason == "cannot reconnect"
        ));
        assert!(core.pending_room_operation.is_none());
        assert!(core.pending_reconnects.is_empty());
    }

    #[test]
    fn terminal_message_classifier_excludes_authoritative_spectator_exits() {
        for reason in [
            crate::protocol::SpectatorStateChangeReason::Disconnected,
            crate::protocol::SpectatorStateChangeReason::Removed,
            crate::protocol::SpectatorStateChangeReason::RoomClosed,
        ] {
            assert!(
                !terminal_message_matches(
                    PendingRoomOperation::LeaveSpectator,
                    &ServerMessage::SpectatorLeft {
                        room_id: None,
                        room_code: None,
                        reason: Some(reason),
                        current_spectators: vec![],
                    },
                ),
                "authoritative exits settle through teardown, never a leave fence"
            );
        }
        for reason in [
            None,
            Some(crate::protocol::SpectatorStateChangeReason::VoluntaryLeave),
        ] {
            assert!(terminal_message_matches(
                PendingRoomOperation::LeaveSpectator,
                &ServerMessage::SpectatorLeft {
                    room_id: None,
                    room_code: None,
                    reason,
                    current_spectators: vec![],
                },
            ));
        }
        assert!(
            !terminal_message_matches(
                PendingRoomOperation::LeaveSpectator,
                &ServerMessage::SpectatorLeft {
                    room_id: None,
                    room_code: None,
                    reason: Some(crate::protocol::SpectatorStateChangeReason::Joined),
                    current_spectators: vec![],
                },
            ),
            "a malformed joined reason belongs to no valid partition"
        );
    }

    #[test]
    fn accountability_invalid_baselines_retire_their_operation_fence() {
        // The roster corruption must trip delivery-accountability baselining,
        // not any earlier phase validator, so a non-authority player id is
        // simply repeated.
        let roster_duplicate_room_joined_payload = |mut payload: Box<RoomJoinedPayload>| {
            payload.current_players.push(player(PEER));
            payload
        };
        let room_joined_payload = |message: ServerMessage| match message {
            ServerMessage::RoomJoined(payload) => payload,
            _ => unreachable!("room_joined helper always returns RoomJoined"),
        };

        for policy in [
            ProtocolViolationPolicy::Observe,
            ProtocolViolationPolicy::Quarantine,
        ] {
            let mut core = correlated_outside(policy);
            let (_, join_id) = prepare_and_admit(
                &mut core,
                ClientOperation::JoinRoom(JoinRoomParams::new("game", "local")),
            );
            let outcome = process(
                &mut core,
                correlated_result(
                    join_id,
                    RoomOperationResult::RoomJoined(roster_duplicate_room_joined_payload(
                        room_joined_payload(room_joined()),
                    )),
                ),
            );
            assert!(
                matches!(
                    outcome.events.as_slice(),
                    [SignalFishEvent::ProtocolViolation { .. }]
                ),
                "{policy:?}: the drifting baseline stays suppressed as a violation"
            );
            assert_eq!(
                core.room_role(),
                None,
                "{policy:?}: a rejected baseline never applies membership"
            );
            match policy {
                ProtocolViolationPolicy::Observe => {
                    assert!(!core.snapshot.quarantined);
                }
                ProtocolViolationPolicy::Quarantine => {
                    assert!(core.snapshot.quarantined);
                }
                _ => unreachable!("matrix covers Observe and Quarantine only"),
            }

            // The answer settled the admitted join even though its payload was
            // rejected: the fence is gone, so the caller can retry immediately.
            let (_, retry_id) = prepare_and_admit(
                &mut core,
                ClientOperation::JoinRoom(JoinRoomParams::new("game", "local")),
            );
            let recovery = process(
                &mut core,
                correlated_result(
                    retry_id,
                    RoomOperationResult::RoomJoined(room_joined_payload(room_joined())),
                ),
            );
            assert!(matches!(
                recovery.events.as_slice(),
                [SignalFishEvent::RoomJoined { .. }]
            ));
            assert_eq!(core.room_role(), Some(RoomRole::Player));
        }

        // The same seam covers legacy-mode admissions: the uncorrelated
        // baseline is equally suppressed yet must not strand its fence.
        let mut legacy = ClientCore::new(
            Some(GameDataEncoding::Json),
            ProtocolViolationPolicy::Observe,
            false,
        );
        let _ = process(&mut legacy, authenticated());
        let _ = process(&mut legacy, protocol_info(None));
        let (command, admission) = legacy
            .prepare_with_admission(ClientOperation::JoinRoom(JoinRoomParams::new(
                "game", "local",
            )))
            .expect("legacy join should prepare");
        assert!(matches!(
            command,
            CoreCommand::Message(ClientMessage::JoinRoom { .. })
        ));
        legacy.record_admission(admission);
        let outcome = process(
            &mut legacy,
            ServerMessage::RoomJoined(roster_duplicate_room_joined_payload(room_joined_payload(
                room_joined_v2(),
            ))),
        );
        assert!(matches!(
            outcome.events.as_slice(),
            [SignalFishEvent::ProtocolViolation { .. }]
        ));
        assert!(legacy.pending_room_operation.is_none());

        // ...and reconnect fences drain their credential queue exactly once.
        let mut core = correlated_outside(ProtocolViolationPolicy::Observe);
        let (_, reconnect_id) = prepare_and_admit(
            &mut core,
            ClientOperation::Reconnect(
                PlayerId::from_u128(LOCAL),
                RoomId::from_u128(10),
                "submitted-token".into(),
            ),
        );
        assert_eq!(core.pending_reconnects.len(), 1);
        let mut payload = match reconnected(vec![]) {
            ServerMessage::Reconnected(payload) => payload,
            _ => unreachable!("reconnected helper always returns Reconnected"),
        };
        payload.current_players.push(player(PEER));
        let outcome = process(
            &mut core,
            correlated_result(reconnect_id, RoomOperationResult::Reconnected(payload)),
        );
        assert!(matches!(
            outcome.events.as_slice(),
            [SignalFishEvent::ProtocolViolation { .. }]
        ));
        assert!(core.pending_room_operation.is_none());
        assert!(core.pending_reconnects.is_empty());
    }

    #[test]
    fn correlated_result_kind_matrix_covers_every_room_operation() {
        let room_joined = match room_joined() {
            ServerMessage::RoomJoined(payload) => RoomOperationResult::RoomJoined(payload),
            _ => unreachable!("room_joined helper always returns RoomJoined"),
        };
        let spectator_joined = match spectator_joined() {
            ServerMessage::SpectatorJoined(payload) => {
                RoomOperationResult::SpectatorJoined(payload)
            }
            _ => unreachable!("spectator_joined helper always returns SpectatorJoined"),
        };
        let reconnected = match reconnected(vec![]) {
            ServerMessage::Reconnected(payload) => RoomOperationResult::Reconnected(payload),
            _ => unreachable!("reconnected helper always returns Reconnected"),
        };
        let cases = [
            (PendingRoomOperation::JoinPlayer, room_joined),
            (
                PendingRoomOperation::JoinPlayer,
                RoomOperationResult::RoomJoinFailed {
                    reason: "failed".into(),
                    error_code: None,
                },
            ),
            (
                PendingRoomOperation::LeavePlayer,
                RoomOperationResult::RoomLeft,
            ),
            (PendingRoomOperation::ReconnectPlayer, reconnected),
            (
                PendingRoomOperation::ReconnectPlayer,
                RoomOperationResult::ReconnectionFailed {
                    reason: "failed".into(),
                    error_code: crate::ErrorCode::ReconnectionFailed,
                },
            ),
            (PendingRoomOperation::JoinSpectator, spectator_joined),
            (
                PendingRoomOperation::JoinSpectator,
                RoomOperationResult::SpectatorJoinFailed {
                    reason: "failed".into(),
                    error_code: None,
                },
            ),
            (
                PendingRoomOperation::LeaveSpectator,
                RoomOperationResult::SpectatorLeft {
                    room_id: Some(RoomId::from_u128(10)),
                    room_code: Some("ROOM".into()),
                    reason: Some(crate::protocol::SpectatorStateChangeReason::VoluntaryLeave),
                    current_spectators: vec![],
                },
            ),
        ];
        for (pending, result) in &cases {
            assert!(
                room_operation_result_matches(*pending, result),
                "{pending:?} must accept {}",
                room_operation_result_name(result)
            );
        }
        for pending in [
            PendingRoomOperation::JoinPlayer,
            PendingRoomOperation::LeavePlayer,
            PendingRoomOperation::ReconnectPlayer,
            PendingRoomOperation::JoinSpectator,
            PendingRoomOperation::LeaveSpectator,
        ] {
            assert!(room_operation_result_matches(
                pending,
                &RoomOperationResult::OperationFailed {
                    reason: "failed".into(),
                    error_code: None,
                }
            ));
            assert!(!room_operation_result_matches(
                pending,
                &RoomOperationResult::SpectatorLeft {
                    room_id: Some(RoomId::from_u128(10)),
                    room_code: Some("ROOM".into()),
                    reason: Some(crate::protocol::SpectatorStateChangeReason::Removed),
                    current_spectators: vec![],
                }
            ));
        }
    }

    #[test]
    fn correlated_results_remain_forbidden_inside_reconnect_replay() {
        let mut core = correlated_outside(ProtocolViolationPolicy::Observe);
        let (_, operation_id) = prepare_and_admit(
            &mut core,
            ClientOperation::Reconnect(
                PlayerId::from_u128(LOCAL),
                RoomId::from_u128(10),
                "submitted-token".into(),
            ),
        );
        let nested = correlated_result(
            RoomOperationId::from_u128(0xaaaa),
            RoomOperationResult::RoomLeft,
        );
        let ServerMessage::Reconnected(payload) = reconnected(vec![nested]) else {
            unreachable!("reconnected helper always returns Reconnected")
        };
        let outcome = process(
            &mut core,
            correlated_result(operation_id, RoomOperationResult::Reconnected(payload)),
        );
        assert_lifecycle_violation_containing(&outcome, "non-replayable RoomOperationResult");
        assert_eq!(
            core.pending_room_operation
                .as_ref()
                .and_then(|pending| pending.operation_id),
            Some(operation_id)
        );
        assert_eq!(core.pending_reconnects.len(), 1);
    }

    #[test]
    fn autonomous_spectator_exit_remains_valid_during_a_correlated_leave() {
        let mut core = correlated_outside(ProtocolViolationPolicy::Quarantine);
        let (_, join_id) = prepare_and_admit(
            &mut core,
            ClientOperation::JoinAsSpectator("game".into(), "ROOM".into(), "viewer".into()),
        );
        let ServerMessage::SpectatorJoined(payload) = spectator_joined() else {
            unreachable!("spectator_joined helper always returns SpectatorJoined")
        };
        let joined = process(
            &mut core,
            correlated_result(join_id, RoomOperationResult::SpectatorJoined(payload)),
        );
        assert!(matches!(
            joined.events.as_slice(),
            [SignalFishEvent::SpectatorJoined { .. }]
        ));
        let (_, _leave_id) = prepare_and_admit(&mut core, ClientOperation::LeaveSpectator);

        let outcome = process(
            &mut core,
            ServerMessage::SpectatorLeft {
                room_id: Some(RoomId::from_u128(10)),
                room_code: Some("ROOM".into()),
                reason: Some(crate::protocol::SpectatorStateChangeReason::Removed),
                current_spectators: vec![],
            },
        );
        assert!(matches!(
            outcome.events.as_slice(),
            [SignalFishEvent::SpectatorLeft { .. }]
        ));
        assert_eq!(core.room_role(), None);
        assert!(core.pending_room_operation.is_none());
    }

    #[test]
    fn authoritative_exit_absorbs_the_one_overtaken_leave_reply_under_every_policy() {
        for policy in [
            ProtocolViolationPolicy::Observe,
            ProtocolViolationPolicy::Quarantine,
            ProtocolViolationPolicy::Disconnect,
        ] {
            for late_reply in [
                RoomOperationResult::SpectatorLeft {
                    room_id: Some(RoomId::from_u128(10)),
                    room_code: Some("ROOM".into()),
                    reason: Some(crate::protocol::SpectatorStateChangeReason::VoluntaryLeave),
                    current_spectators: vec![],
                },
                RoomOperationResult::OperationFailed {
                    reason: "leave rejected after removal".into(),
                    error_code: None,
                },
            ] {
                let mut core = correlated_outside(policy);
                let (_, join_id) = prepare_and_admit(
                    &mut core,
                    ClientOperation::JoinAsSpectator("game".into(), "ROOM".into(), "viewer".into()),
                );
                let ServerMessage::SpectatorJoined(payload) = spectator_joined() else {
                    unreachable!("spectator_joined helper always returns SpectatorJoined")
                };
                let joined = process(
                    &mut core,
                    correlated_result(join_id, RoomOperationResult::SpectatorJoined(payload)),
                );
                assert!(matches!(
                    joined.events.as_slice(),
                    [SignalFishEvent::SpectatorJoined { .. }]
                ));

                let (_, leave_id) = prepare_and_admit(&mut core, ClientOperation::LeaveSpectator);

                // The authoritative exit wins the wire race against the
                // server's mandated reply to the voluntary leave.
                let removed = process(
                    &mut core,
                    ServerMessage::SpectatorLeft {
                        room_id: Some(RoomId::from_u128(10)),
                        room_code: Some("ROOM".into()),
                        reason: Some(crate::protocol::SpectatorStateChangeReason::Removed),
                        current_spectators: vec![],
                    },
                );
                assert!(matches!(
                    removed.events.as_slice(),
                    [SignalFishEvent::SpectatorLeft { .. }]
                ));
                assert_eq!(core.room_role(), None);
                assert_eq!(
                    core.absorbed_spectator_leave
                        .as_ref()
                        .and_then(|absorbed| absorbed.pending.operation_id),
                    Some(leave_id),
                    "the acknowledged leave reply must be awaited exactly once"
                );

                let absorbed_reply =
                    process(&mut core, correlated_result(leave_id, late_reply.clone()));
                assert!(
                    absorbed_reply.events.is_empty(),
                    "the superseded reply must be silent under {policy:?}"
                );
                assert!(!absorbed_reply.disconnect);
                assert!(core.absorbed_spectator_leave.is_none());

                // A duplicate of the same reply is no longer expected and
                // keeps violating.
                let duplicate = process(&mut core, correlated_result(leave_id, late_reply));
                assert_lifecycle_violation_containing(
                    &duplicate,
                    "without a pending room operation",
                );
                assert_eq!(
                    duplicate.disconnect,
                    policy == ProtocolViolationPolicy::Disconnect
                );
            }
        }
    }

    #[test]
    fn overtaken_correlated_leave_reply_preserves_a_fresh_join_fence() {
        let mut core = correlated_outside(ProtocolViolationPolicy::Disconnect);
        let (_, initial_join_id) = prepare_and_admit(
            &mut core,
            ClientOperation::JoinAsSpectator("game".into(), "ROOM".into(), "viewer".into()),
        );
        let ServerMessage::SpectatorJoined(initial_payload) = spectator_joined() else {
            unreachable!("spectator_joined helper always returns SpectatorJoined")
        };
        let _ = process(
            &mut core,
            correlated_result(
                initial_join_id,
                RoomOperationResult::SpectatorJoined(initial_payload),
            ),
        );
        let (_, leave_id) = prepare_and_admit(&mut core, ClientOperation::LeaveSpectator);
        let _ = process(
            &mut core,
            ServerMessage::SpectatorLeft {
                room_id: Some(RoomId::from_u128(10)),
                room_code: Some("ROOM".into()),
                reason: Some(crate::protocol::SpectatorStateChangeReason::Removed),
                current_spectators: vec![],
            },
        );

        let (_, fresh_join_id) = prepare_and_admit(
            &mut core,
            ClientOperation::JoinAsSpectator("game".into(), "ROOM".into(), "viewer".into()),
        );
        let absorbed = process(
            &mut core,
            correlated_result(
                leave_id,
                RoomOperationResult::OperationFailed {
                    reason: "leave was superseded by removal".into(),
                    error_code: None,
                },
            ),
        );
        assert!(absorbed.events.is_empty());
        assert!(!absorbed.disconnect);
        assert_eq!(
            core.pending_room_operation
                .as_ref()
                .and_then(|pending| pending.operation_id),
            Some(fresh_join_id),
            "the old terminal reply must not consume the fresh join fence"
        );

        let ServerMessage::SpectatorJoined(fresh_payload) = spectator_joined() else {
            unreachable!("spectator_joined helper always returns SpectatorJoined")
        };
        let joined = process(
            &mut core,
            correlated_result(
                fresh_join_id,
                RoomOperationResult::SpectatorJoined(fresh_payload),
            ),
        );
        assert!(matches!(
            joined.events.as_slice(),
            [SignalFishEvent::SpectatorJoined { .. }]
        ));
        assert!(!joined.disconnect);
    }

    #[test]
    fn overtaken_leave_reply_requires_the_prior_room_identity() {
        let mut core = correlated_outside(ProtocolViolationPolicy::Observe);
        let (_, join_id) = prepare_and_admit(
            &mut core,
            ClientOperation::JoinAsSpectator("game".into(), "ROOM".into(), "viewer".into()),
        );
        let ServerMessage::SpectatorJoined(payload) = spectator_joined() else {
            unreachable!("spectator_joined helper always returns SpectatorJoined")
        };
        let _ = process(
            &mut core,
            correlated_result(join_id, RoomOperationResult::SpectatorJoined(payload)),
        );
        let (_, leave_id) = prepare_and_admit(&mut core, ClientOperation::LeaveSpectator);
        let _ = process(
            &mut core,
            ServerMessage::SpectatorLeft {
                room_id: Some(RoomId::from_u128(10)),
                room_code: Some("ROOM".into()),
                reason: Some(crate::protocol::SpectatorStateChangeReason::RoomClosed),
                current_spectators: vec![],
            },
        );

        for wrong_identity in [
            RoomOperationResult::SpectatorLeft {
                room_id: Some(RoomId::from_u128(999)),
                room_code: Some("ROOM".into()),
                reason: Some(crate::protocol::SpectatorStateChangeReason::VoluntaryLeave),
                current_spectators: vec![],
            },
            RoomOperationResult::SpectatorLeft {
                room_id: Some(RoomId::from_u128(10)),
                room_code: Some("OTHER".into()),
                reason: Some(crate::protocol::SpectatorStateChangeReason::VoluntaryLeave),
                current_spectators: vec![],
            },
        ] {
            let rejected = process(&mut core, correlated_result(leave_id, wrong_identity));
            assert_lifecycle_violation_containing(&rejected, "without a pending room operation");
            assert!(
                core.absorbed_spectator_leave.is_some(),
                "wrong-room replies must not consume the one valid allowance"
            );
        }

        let absorbed = process(
            &mut core,
            correlated_result(
                leave_id,
                RoomOperationResult::SpectatorLeft {
                    room_id: Some(RoomId::from_u128(10)),
                    room_code: Some("ROOM".into()),
                    reason: Some(crate::protocol::SpectatorStateChangeReason::VoluntaryLeave),
                    current_spectators: vec![],
                },
            ),
        );
        assert!(absorbed.events.is_empty());
        assert!(core.absorbed_spectator_leave.is_none());
    }

    #[test]
    fn malformed_joined_reason_never_consumes_the_overtaken_leave_allowance() {
        let mut core = correlated_outside(ProtocolViolationPolicy::Observe);
        let (_, join_id) = prepare_and_admit(
            &mut core,
            ClientOperation::JoinAsSpectator("game".into(), "ROOM".into(), "viewer".into()),
        );
        let ServerMessage::SpectatorJoined(payload) = spectator_joined() else {
            unreachable!("spectator_joined helper always returns SpectatorJoined")
        };
        assert!(matches!(
            process(
                &mut core,
                correlated_result(join_id, RoomOperationResult::SpectatorJoined(payload))
            )
            .events
            .as_slice(),
            [SignalFishEvent::SpectatorJoined { .. }]
        ));
        let (_, leave_id) = prepare_and_admit(&mut core, ClientOperation::LeaveSpectator);
        assert!(
            process(
                &mut core,
                ServerMessage::SpectatorLeft {
                    room_id: Some(RoomId::from_u128(10)),
                    room_code: Some("ROOM".into()),
                    reason: Some(crate::protocol::SpectatorStateChangeReason::Removed),
                    current_spectators: vec![],
                },
            )
            .events
            .len()
                == 1,
            "the authoritative exit tears the spectator out and arms the allowance"
        );

        // A malformed joined-reason exit is not a voluntary-leave answer:
        // the absorb seam must leave it alone, and with no pending operation
        // left it violates at normalization instead of eating the awaited
        // reply slot.
        let malformed = process(
            &mut core,
            correlated_result(
                leave_id,
                RoomOperationResult::SpectatorLeft {
                    room_id: Some(RoomId::from_u128(10)),
                    room_code: Some("ROOM".into()),
                    reason: Some(crate::protocol::SpectatorStateChangeReason::Joined),
                    current_spectators: vec![],
                },
            ),
        );
        assert_lifecycle_violation_containing(&malformed, "without a pending room operation");
        assert!(
            core.absorbed_spectator_leave.is_some(),
            "the allowance survives an unclassifiable exit frame"
        );
        assert_eq!(core.room_role(), None);

        let true_reply = process(
            &mut core,
            correlated_result(leave_id, spectator_left_voluntary()),
        );
        assert!(true_reply.events.is_empty());
        assert!(core.absorbed_spectator_leave.is_none());
    }

    fn spectator_left_voluntary() -> RoomOperationResult {
        RoomOperationResult::SpectatorLeft {
            room_id: Some(RoomId::from_u128(10)),
            room_code: Some("ROOM".into()),
            reason: Some(crate::protocol::SpectatorStateChangeReason::VoluntaryLeave),
            current_spectators: vec![],
        }
    }

    #[test]
    fn authoritative_exit_absorbs_the_late_uncorrelated_leave_reply() {
        let mut core = ClientCore::new(
            Some(GameDataEncoding::Json),
            ProtocolViolationPolicy::Disconnect,
            true,
        );
        assert_eq!(process(&mut core, authenticated()).events.len(), 1);
        assert_eq!(process(&mut core, protocol_info(None)).events.len(), 1);

        let (command, admission) = core
            .prepare_with_admission(ClientOperation::JoinAsSpectator(
                "game".into(),
                "ROOM".into(),
                "viewer".into(),
            ))
            .expect("spectator join should prepare");
        assert!(matches!(
            command,
            CoreCommand::Message(ClientMessage::JoinAsSpectator { .. })
        ));
        core.record_admission(admission);
        // Strip the v3 delivery stamps: a legacy v2 connection must not see
        // epoch/seq baselines on authoritative snapshots.
        let ServerMessage::SpectatorJoined(mut payload) = spectator_joined() else {
            unreachable!("spectator_joined helper always returns SpectatorJoined")
        };
        for member in &mut payload.current_players {
            member.epoch = None;
            member.seq = None;
        }
        let joined_outcome = process(&mut core, ServerMessage::SpectatorJoined(payload));
        for event in &joined_outcome.events {
            if let SignalFishEvent::ProtocolViolation { diagnostic, .. } = event {
                panic!("uncorrelated join violated: {diagnostic}");
            }
        }
        assert!(matches!(
            joined_outcome.events.as_slice(),
            [SignalFishEvent::SpectatorJoined { .. }]
        ));

        let (command, admission) = core
            .prepare_with_admission(ClientOperation::LeaveSpectator)
            .expect("spectator leave should prepare");
        assert!(matches!(
            command,
            CoreCommand::Message(ClientMessage::LeaveSpectator)
        ));
        core.record_admission(admission);

        let removed = process(
            &mut core,
            ServerMessage::SpectatorLeft {
                room_id: Some(RoomId::from_u128(10)),
                room_code: Some("ROOM".into()),
                reason: Some(crate::protocol::SpectatorStateChangeReason::RoomClosed),
                current_spectators: vec![],
            },
        );
        assert!(matches!(
            removed.events.as_slice(),
            [SignalFishEvent::SpectatorLeft { .. }]
        ));
        assert_eq!(
            core.absorbed_spectator_leave
                .as_ref()
                .map(|absorbed| absorbed.pending.kind),
            Some(PendingRoomOperation::LeaveSpectator)
        );

        let (command, admission) = core
            .prepare_with_admission(ClientOperation::JoinAsSpectator(
                "game".into(),
                "ROOM".into(),
                "viewer".into(),
            ))
            .expect("a fresh spectator join should prepare");
        assert!(matches!(
            command,
            CoreCommand::Message(ClientMessage::JoinAsSpectator { .. })
        ));
        core.record_admission(admission);

        let wrong_room_reply = process(
            &mut core,
            ServerMessage::SpectatorLeft {
                room_id: Some(RoomId::from_u128(999)),
                room_code: Some("ROOM".into()),
                reason: Some(crate::protocol::SpectatorStateChangeReason::VoluntaryLeave),
                current_spectators: vec![],
            },
        );
        assert_lifecycle_violation_containing(
            &wrong_room_reply,
            "is invalid while authenticated=true and membership=None",
        );
        assert!(core.absorbed_spectator_leave.is_some());
        assert_eq!(
            core.pending_room_operation
                .as_ref()
                .map(|pending| pending.kind),
            Some(PendingRoomOperation::JoinSpectator)
        );

        let absorbed_reply = process(
            &mut core,
            ServerMessage::SpectatorLeft {
                room_id: Some(RoomId::from_u128(10)),
                room_code: Some("ROOM".into()),
                reason: Some(crate::protocol::SpectatorStateChangeReason::VoluntaryLeave),
                current_spectators: vec![],
            },
        );
        assert!(absorbed_reply.events.is_empty());
        assert!(!absorbed_reply.disconnect);
        assert!(core.absorbed_spectator_leave.is_none());
        assert_eq!(
            core.pending_room_operation
                .as_ref()
                .map(|pending| pending.kind),
            Some(PendingRoomOperation::JoinSpectator),
            "the old leave reply must not disturb the fresh join fence"
        );

        let ServerMessage::SpectatorJoined(mut fresh_payload) = spectator_joined() else {
            unreachable!("spectator_joined helper always returns SpectatorJoined")
        };
        for member in &mut fresh_payload.current_players {
            member.epoch = None;
            member.seq = None;
        }
        let fresh_join = process(&mut core, ServerMessage::SpectatorJoined(fresh_payload));
        assert!(matches!(
            fresh_join.events.as_slice(),
            [SignalFishEvent::SpectatorJoined { .. }]
        ));

        let duplicate = process(
            &mut core,
            ServerMessage::SpectatorLeft {
                room_id: Some(RoomId::from_u128(10)),
                room_code: Some("ROOM".into()),
                reason: Some(crate::protocol::SpectatorStateChangeReason::VoluntaryLeave),
                current_spectators: vec![],
            },
        );
        assert_lifecycle_violation_containing(
            &duplicate,
            "arrived without an admitted LeaveSpectator operation",
        );
        assert!(duplicate.disconnect);
    }

    #[test]
    fn authoritative_exit_without_a_pending_leave_still_violates_on_late_results() {
        let mut core = correlated_outside(ProtocolViolationPolicy::Quarantine);
        let (_, join_id) = prepare_and_admit(
            &mut core,
            ClientOperation::JoinAsSpectator("game".into(), "ROOM".into(), "viewer".into()),
        );
        let ServerMessage::SpectatorJoined(payload) = spectator_joined() else {
            unreachable!("spectator_joined helper always returns SpectatorJoined")
        };
        let _ = process(
            &mut core,
            correlated_result(join_id, RoomOperationResult::SpectatorJoined(payload)),
        );

        let removed = process(
            &mut core,
            ServerMessage::SpectatorLeft {
                room_id: Some(RoomId::from_u128(10)),
                room_code: Some("ROOM".into()),
                reason: Some(crate::protocol::SpectatorStateChangeReason::Removed),
                current_spectators: vec![],
            },
        );
        assert!(matches!(
            removed.events.as_slice(),
            [SignalFishEvent::SpectatorLeft { .. }]
        ));
        assert_eq!(core.room_role(), None);
        assert!(core.absorbed_spectator_leave.is_none());

        let spurious = process(
            &mut core,
            correlated_result(
                RoomOperationId::from_u128(0xaaaa),
                RoomOperationResult::SpectatorLeft {
                    room_id: Some(RoomId::from_u128(10)),
                    room_code: Some("ROOM".into()),
                    reason: Some(crate::protocol::SpectatorStateChangeReason::VoluntaryLeave),
                    current_spectators: vec![],
                },
            ),
        );
        assert_lifecycle_violation_containing(&spurious, "without a pending room operation");
    }

    #[test]
    fn stale_correlated_ids_never_release_the_current_fence_under_any_policy() {
        for policy in [
            ProtocolViolationPolicy::Observe,
            ProtocolViolationPolicy::Quarantine,
            ProtocolViolationPolicy::Disconnect,
        ] {
            let mut core = correlated_outside(policy);
            let (_, current_id) = prepare_and_admit(
                &mut core,
                ClientOperation::JoinRoom(JoinRoomParams::new("game", "local")),
            );
            let outcome = process(
                &mut core,
                correlated_result(
                    RoomOperationId::from_u128(0xaaaa),
                    RoomOperationResult::RoomJoinFailed {
                        reason: "stale".into(),
                        error_code: None,
                    },
                ),
            );
            assert_lifecycle_violation_containing(&outcome, "did not match");
            assert_eq!(
                outcome.disconnect,
                policy == ProtocolViolationPolicy::Disconnect
            );
            assert_eq!(
                core.pending_room_operation
                    .as_ref()
                    .and_then(|pending| pending.operation_id),
                Some(current_id)
            );
        }
    }

    #[test]
    fn physical_disconnect_clears_correlation_scope_and_pending_identity() {
        let mut core = correlated_outside(ProtocolViolationPolicy::Observe);
        let _ = prepare_and_admit(
            &mut core,
            ClientOperation::JoinRoom(JoinRoomParams::new("game", "local")),
        );
        assert!(core.room_operation_ids);
        assert!(core.pending_room_operation.is_some());

        let disconnected = core.disconnect(Some("transport closed".into()));
        assert!(matches!(disconnected, SignalFishEvent::Disconnected { .. }));
        assert!(!core.room_operation_ids);
        assert!(core.pending_room_operation.is_none());
        assert!(core.pending_reconnects.is_empty());
    }

    #[test]
    fn malformed_or_uncorrelated_errors_never_release_a_correlated_fence() {
        let mut core = correlated_outside(ProtocolViolationPolicy::Observe);
        let (_, operation_id) = prepare_and_admit(
            &mut core,
            ClientOperation::JoinRoom(JoinRoomParams::new("game", "local")),
        );
        let malformed = core.process_frame(TransportFrame::Text(format!(
            r#"{{"type":"RoomOperationResult","data":{{"operation_id":"{operation_id}","result":{{"type":"RoomJoinFailed","data":null}}}}}}"#
        )));
        assert!(matches!(
            malformed.events.as_slice(),
            [SignalFishEvent::DecodeFailed { .. }]
        ));
        assert_eq!(
            core.pending_room_operation
                .as_ref()
                .and_then(|pending| pending.operation_id),
            Some(operation_id)
        );

        let outcome = process(
            &mut core,
            ServerMessage::Error {
                message: "uncorrelated".into(),
                error_code: Some(crate::ErrorCode::NotInRoom),
            },
        );
        assert!(matches!(
            outcome.events.as_slice(),
            [SignalFishEvent::Error { .. }]
        ));
        assert_eq!(
            core.pending_room_operation
                .as_ref()
                .and_then(|pending| pending.operation_id),
            Some(operation_id)
        );
    }

    #[test]
    fn decoded_game_data_count_includes_stale_and_quarantined_receipts() {
        let game_data = |seq| ServerMessage::GameData {
            from_player: PlayerId::from_u128(PEER),
            data: serde_json::json!({"seq": seq}),
            seq: Some(seq),
            epoch: Some(1),
            class: Some(DeliveryClass::Reliable),
            key: None,
        };
        let mut core = v3_room(ProtocolViolationPolicy::Quarantine);

        let applied = process(&mut core, game_data(1));
        assert!(matches!(
            applied.events.as_slice(),
            [SignalFishEvent::GameData { .. }]
        ));

        let _ = process(
            &mut core,
            ServerMessage::PlayerLeft {
                player_id: PlayerId::from_u128(PEER),
                epoch: Some(1),
                final_seq: Some(2),
            },
        );
        let _ = process(
            &mut core,
            ServerMessage::PlayerReconnected {
                player_id: PlayerId::from_u128(PEER),
                epoch: Some(2),
            },
        );
        let stale = process(&mut core, game_data(2));
        assert!(stale.events.is_empty());

        core.snapshot.quarantined = true;
        let quarantined = process(
            &mut core,
            ServerMessage::GameData {
                from_player: PlayerId::from_u128(PEER),
                data: serde_json::json!({"seq": 1}),
                seq: Some(1),
                epoch: Some(2),
                class: Some(DeliveryClass::Reliable),
                key: None,
            },
        );
        assert!(quarantined.events.is_empty());
        assert_eq!(core.stats().game_data_received, 3);

        core.record_admission(ClientCore::admission_for(&ClientOperation::LeaveRoom));
        let _ = process(&mut core, ServerMessage::RoomLeft);
        let _ = core.disconnect(None);
        assert_eq!(
            core.stats().game_data_received,
            3,
            "lifetime counters survive room and connection teardown"
        );
        assert_eq!(
            ClientCore::new(None, ProtocolViolationPolicy::Quarantine, false).stats(),
            ClientStats::default(),
            "a new client starts with fresh counters"
        );

        let binary = V2BinaryGameDataFrame {
            from_player: PlayerId::from_u128(PEER),
            encoding: GameDataEncoding::MessagePack,
            payload: vec![1, 2, 3],
        };
        let mut binary_core = ClientCore::new(
            Some(GameDataEncoding::MessagePack),
            ProtocolViolationPolicy::Quarantine,
            false,
        );
        let _ = process(&mut binary_core, authenticated());
        let ServerMessage::ProtocolInfo(mut binary_protocol) = protocol_info(None) else {
            unreachable!("protocol_info helper always returns ProtocolInfo")
        };
        binary_protocol.game_data_formats =
            vec![GameDataEncoding::Json, GameDataEncoding::MessagePack];
        let _ = process(
            &mut binary_core,
            ServerMessage::ProtocolInfo(binary_protocol),
        );
        binary_core.record_admission(ClientCore::admission_for(&ClientOperation::JoinRoom(
            JoinRoomParams::new("game", "local"),
        )));
        let _ = process(&mut binary_core, room_joined_v2());
        binary_core.snapshot.quarantined = true;
        let rejected = binary_core.process_frame(TransportFrame::Binary(
            rmp_serde::to_vec_named(&binary).expect("binary receipt should serialize"),
        ));
        assert!(rejected
            .events
            .iter()
            .all(|event| !matches!(event, SignalFishEvent::GameDataBinary { .. })));
        assert_eq!(
            binary_core.stats(),
            ClientStats {
                game_data_received: 1,
                ..ClientStats::default()
            },
            "successful binary decode counts before quarantine suppression"
        );
    }

    fn spectator_joined() -> ServerMessage {
        ServerMessage::SpectatorJoined(Box::new(SpectatorJoinedPayload {
            room_id: RoomId::from_u128(10),
            room_code: "ROOM".into(),
            spectator_id: PlayerId::from_u128(99),
            game_name: "game".into(),
            current_players: vec![player(LOCAL), player(PEER)],
            current_spectators: vec![],
            lobby_state: LobbyState::Lobby,
            reason: None,
        }))
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum MembershipError {
        None,
        NotInRoom,
        AlreadyInRoom,
        NeedsPlayer,
        NeedsSpectator,
    }

    fn membership_error(result: crate::error::Result<()>) -> MembershipError {
        match result {
            Err(crate::SignalFishError::NotInRoom) => MembershipError::NotInRoom,
            Err(crate::SignalFishError::AlreadyInRoom) => MembershipError::AlreadyInRoom,
            Err(crate::SignalFishError::WrongRoomRole {
                required: RoomRole::Player,
                actual: RoomRole::Spectator,
            }) => MembershipError::NeedsPlayer,
            Err(crate::SignalFishError::WrongRoomRole {
                required: RoomRole::Spectator,
                actual: RoomRole::Player,
            }) => MembershipError::NeedsSpectator,
            _ => MembershipError::None,
        }
    }

    fn operation_matrix() -> Vec<(
        &'static str,
        ClientOperation,
        MembershipError,
        MembershipError,
        MembershipError,
    )> {
        let player_only = |name, operation| {
            (
                name,
                operation,
                MembershipError::NotInRoom,
                MembershipError::None,
                MembershipError::NeedsPlayer,
            )
        };
        vec![
            (
                "join room",
                ClientOperation::JoinRoom(JoinRoomParams::new("game", "local")),
                MembershipError::None,
                MembershipError::AlreadyInRoom,
                MembershipError::AlreadyInRoom,
            ),
            player_only("leave room", ClientOperation::LeaveRoom),
            player_only(
                "reliable data",
                ClientOperation::GameData(
                    serde_json::json!({"value": 1}),
                    GameDataDelivery::Reliable,
                ),
            ),
            player_only(
                "latest data",
                ClientOperation::GameData(
                    serde_json::json!({"value": 1}),
                    GameDataDelivery::Latest { key: 7 },
                ),
            ),
            player_only("binary data", ClientOperation::Binary(vec![1])),
            player_only("ready", ClientOperation::SetReady),
            player_only("start", ClientOperation::StartGame),
            player_only("request authority", ClientOperation::RequestAuthority(true)),
            player_only(
                "connection info",
                ClientOperation::ProvideConnectionInfo(ConnectionInfo::Direct {
                    host: "127.0.0.1".into(),
                    port: 7_777,
                }),
            ),
            (
                "reconnect",
                ClientOperation::Reconnect(
                    PlayerId::from_u128(LOCAL),
                    RoomId::from_u128(10),
                    "token".into(),
                ),
                MembershipError::None,
                MembershipError::AlreadyInRoom,
                MembershipError::AlreadyInRoom,
            ),
            (
                "join spectator",
                ClientOperation::JoinAsSpectator("game".into(), "ROOM".into(), "viewer".into()),
                MembershipError::None,
                MembershipError::AlreadyInRoom,
                MembershipError::AlreadyInRoom,
            ),
            (
                "leave spectator",
                ClientOperation::LeaveSpectator,
                MembershipError::NotInRoom,
                MembershipError::NeedsSpectator,
                MembershipError::None,
            ),
            (
                "ping",
                ClientOperation::Ping,
                MembershipError::None,
                MembershipError::None,
                MembershipError::None,
            ),
            player_only(
                "signal",
                ClientOperation::Signal(
                    PlayerId::from_u128(PEER),
                    SignalGeneration::Current,
                    PeerSignal::Offer("sdp".into()),
                ),
            ),
            player_only(
                "raw signal",
                ClientOperation::RawSignal(
                    PlayerId::from_u128(PEER),
                    SignalGeneration::Current,
                    serde_json::json!({"Custom": true}),
                ),
            ),
            player_only(
                "transport status",
                ClientOperation::TransportStatus(TransportKind::WebRtc, true),
            ),
        ]
    }

    #[test]
    fn operation_membership_matrix_is_exhaustive_and_role_specific() {
        for (name, operation, outside, player, spectator) in operation_matrix() {
            let mut outside_core = ClientCore::new(
                Some(GameDataEncoding::Json),
                ProtocolViolationPolicy::Observe,
                true,
            );
            assert_eq!(process(&mut outside_core, authenticated()).events.len(), 1);
            assert_eq!(
                process(&mut outside_core, protocol_info(Some(3)))
                    .events
                    .len(),
                1
            );
            assert_eq!(
                membership_error(outside_core.validate(&operation)),
                outside,
                "{name}"
            );

            let player_core = v3_room(ProtocolViolationPolicy::Observe);
            assert_eq!(
                membership_error(player_core.validate(&operation)),
                player,
                "{name}"
            );

            let spectator_core = v3_spectator(ProtocolViolationPolicy::Observe);
            assert_eq!(
                membership_error(spectator_core.validate(&operation)),
                spectator,
                "{name}"
            );
        }
    }

    #[test]
    fn unauthenticated_core_rejects_fencing_room_operations_before_admission() {
        let fencing_operations = [
            (
                "join room",
                ClientOperation::JoinRoom(JoinRoomParams::new("game", "local")),
            ),
            ("leave room", ClientOperation::LeaveRoom),
            (
                "reconnect",
                ClientOperation::Reconnect(
                    PlayerId::from_u128(LOCAL),
                    RoomId::from_u128(10),
                    "token".into(),
                ),
            ),
            (
                "join spectator",
                ClientOperation::JoinAsSpectator("game".into(), "ROOM".into(), "viewer".into()),
            ),
            ("leave spectator", ClientOperation::LeaveSpectator),
        ];
        for (name, operation) in fencing_operations {
            let mut core = ClientCore::new(
                Some(GameDataEncoding::Json),
                ProtocolViolationPolicy::Observe,
                true,
            );
            assert!(
                matches!(
                    core.validate(&operation),
                    Err(crate::SignalFishError::NotAuthenticated)
                ),
                "{name} must be refused before authentication"
            );
            assert_eq!(
                core.pending_room_operation, None,
                "{name} must not arm its admission fence while unauthenticated"
            );

            assert_eq!(process(&mut core, authenticated()).events.len(), 1);
            if matches!(
                operation,
                ClientOperation::JoinRoom(_)
                    | ClientOperation::Reconnect(..)
                    | ClientOperation::JoinAsSpectator(..)
            ) {
                core.validate(&operation).unwrap_or_else(|error| {
                    panic!("{name} must validate after authentication: {error:?}")
                });
            }
        }

        let core = ClientCore::new(
            Some(GameDataEncoding::Json),
            ProtocolViolationPolicy::Observe,
            true,
        );
        core.validate(&ClientOperation::Ping)
            .expect("ping remains valid before authentication");
        assert!(
            matches!(
                core.validate(&ClientOperation::GameData(
                    serde_json::json!({"value": 1}),
                    GameDataDelivery::Reliable,
                )),
                Err(crate::SignalFishError::NotInRoom)
            ),
            "non-fencing operations keep their existing pre-authentication behavior"
        );
    }

    #[test]
    fn admitted_room_transitions_fence_fifo_commands_and_failures_roll_back() {
        let mut joining = ClientCore::new(
            Some(GameDataEncoding::Json),
            ProtocolViolationPolicy::Observe,
            true,
        );
        let _ = process(&mut joining, authenticated());
        let _ = process(&mut joining, protocol_info(Some(3)));
        let join = ClientOperation::JoinRoom(JoinRoomParams::new("game", "local"));
        joining.validate(&join).expect("first join is valid");
        joining.record_admission(ClientCore::admission_for(&join));
        assert!(matches!(
            joining.validate(&ClientOperation::JoinRoom(JoinRoomParams::new(
                "game", "local"
            ))),
            Err(crate::SignalFishError::RoomOperationPending)
        ));
        let _ = process(
            &mut joining,
            ServerMessage::RoomJoinFailed {
                reason: "full".into(),
                error_code: None,
            },
        );
        joining
            .validate(&ClientOperation::JoinRoom(JoinRoomParams::new(
                "game", "local",
            )))
            .expect("failed join clears the admission fence");

        let mut leaving = v3_room(ProtocolViolationPolicy::Observe);
        let leave = ClientOperation::LeaveRoom;
        leaving.validate(&leave).expect("player may leave");
        leaving.record_admission(ClientCore::admission_for(&leave));
        assert!(matches!(
            leaving.validate(&ClientOperation::GameData(
                serde_json::json!({"after": "leave"}),
                GameDataDelivery::Reliable,
            )),
            Err(crate::SignalFishError::RoomOperationPending)
        ));
        let _ = process(&mut leaving, ServerMessage::RoomLeft);
        assert_eq!(leaving.room_role(), None);
        assert!(leaving.snapshot().player_id.is_none());
        assert!(matches!(
            leaving.validate(&ClientOperation::GameData(
                serde_json::json!({"after": "left"}),
                GameDataDelivery::Reliable,
            )),
            Err(crate::SignalFishError::NotInRoom)
        ));
    }

    #[test]
    fn unsolicited_room_operation_responses_never_mutate_membership() {
        let outside_responses = [
            room_joined(),
            ServerMessage::RoomJoinFailed {
                reason: "late failure".into(),
                error_code: None,
            },
            spectator_joined(),
            ServerMessage::SpectatorJoinFailed {
                reason: "late failure".into(),
                error_code: None,
            },
            reconnected(vec![]),
            ServerMessage::ReconnectionFailed {
                reason: "late failure".into(),
                error_code: crate::ErrorCode::ReconnectionFailed,
            },
        ];

        for response in outside_responses {
            let mut core = ClientCore::new(
                Some(GameDataEncoding::Json),
                ProtocolViolationPolicy::Observe,
                true,
            );
            let _ = process(&mut core, authenticated());
            let _ = process(&mut core, protocol_info(Some(3)));
            let before = core.snapshot();

            let outcome = process(&mut core, response);

            assert_lifecycle_violation(&outcome);
            assert_eq!(core.snapshot(), before);
            assert_eq!(core.pending_room_operation, None);
        }

        let mut player = v3_room(ProtocolViolationPolicy::Observe);
        let before = player.snapshot();
        assert_lifecycle_violation(&process(&mut player, ServerMessage::RoomLeft));
        assert_eq!(player.snapshot(), before);

        let mut spectator = v3_spectator(ProtocolViolationPolicy::Observe);
        let before = spectator.snapshot();
        assert_lifecycle_violation(&process(
            &mut spectator,
            ServerMessage::SpectatorLeft {
                room_id: Some(RoomId::from_u128(10)),
                room_code: Some("ROOM".into()),
                reason: None,
                current_spectators: vec![],
            },
        ));
        assert_eq!(spectator.snapshot(), before);
    }

    #[test]
    fn completed_room_operations_do_not_authorize_duplicate_or_delayed_responses() {
        let mut joined = ClientCore::new(
            Some(GameDataEncoding::Json),
            ProtocolViolationPolicy::Observe,
            true,
        );
        let _ = process(&mut joined, authenticated());
        let _ = process(&mut joined, protocol_info(Some(3)));
        joined.record_admission(ClientCore::admission_for(&ClientOperation::JoinRoom(
            JoinRoomParams::new("game", "local"),
        )));
        let _ = process(&mut joined, room_joined());
        let before = joined.snapshot();
        assert_lifecycle_violation(&process(&mut joined, room_joined()));
        assert_eq!(joined.snapshot(), before);
        assert_eq!(joined.pending_room_operation, None);

        joined.record_admission(ClientCore::admission_for(&ClientOperation::LeaveRoom));
        let _ = process(&mut joined, ServerMessage::RoomLeft);
        let before = joined.snapshot();
        assert_lifecycle_violation(&process(&mut joined, ServerMessage::RoomLeft));
        assert_eq!(joined.snapshot(), before);

        let mut failed_join = ClientCore::new(
            Some(GameDataEncoding::Json),
            ProtocolViolationPolicy::Observe,
            true,
        );
        let _ = process(&mut failed_join, authenticated());
        let _ = process(&mut failed_join, protocol_info(Some(3)));
        failed_join.record_admission(ClientCore::admission_for(&ClientOperation::JoinRoom(
            JoinRoomParams::new("game", "local"),
        )));
        let failure = ServerMessage::RoomJoinFailed {
            reason: "full".into(),
            error_code: Some(crate::ErrorCode::RoomFull),
        };
        let _ = process(&mut failed_join, failure.clone());
        let before = failed_join.snapshot();
        assert_lifecycle_violation(&process(&mut failed_join, failure));
        assert_eq!(failed_join.snapshot(), before);
        assert_eq!(failed_join.pending_room_operation, None);

        let mut failed_reconnect = ClientCore::new(
            Some(GameDataEncoding::Json),
            ProtocolViolationPolicy::Observe,
            true,
        );
        let _ = process(&mut failed_reconnect, authenticated());
        let _ = process(&mut failed_reconnect, protocol_info(Some(3)));
        failed_reconnect.record_reconnect_admitted(
            PlayerId::from_u128(LOCAL),
            RoomId::from_u128(10),
            "submitted-token".into(),
        );
        let failure = ServerMessage::ReconnectionFailed {
            reason: "expired".into(),
            error_code: crate::ErrorCode::ReconnectionExpired,
        };
        let _ = process(&mut failed_reconnect, failure.clone());
        let before = failed_reconnect.snapshot();
        assert_lifecycle_violation(&process(&mut failed_reconnect, failure));
        assert_eq!(failed_reconnect.snapshot(), before);
        assert_eq!(failed_reconnect.pending_room_operation, None);
    }

    #[test]
    fn authoritative_spectator_exits_do_not_require_a_voluntary_leave() {
        for policy in [
            ProtocolViolationPolicy::Observe,
            ProtocolViolationPolicy::Quarantine,
            ProtocolViolationPolicy::Disconnect,
        ] {
            for reason in [
                crate::protocol::SpectatorStateChangeReason::Disconnected,
                crate::protocol::SpectatorStateChangeReason::Removed,
                crate::protocol::SpectatorStateChangeReason::RoomClosed,
            ] {
                let mut core = v3_spectator(policy);
                let outcome = process(
                    &mut core,
                    ServerMessage::SpectatorLeft {
                        room_id: Some(RoomId::from_u128(10)),
                        room_code: Some("ROOM".into()),
                        reason: Some(reason),
                        current_spectators: vec![],
                    },
                );
                assert!(matches!(
                    outcome.events.as_slice(),
                    [SignalFishEvent::SpectatorLeft { .. }]
                ));
                assert!(!outcome.disconnect);
                assert_eq!(core.room_role(), None);
                assert!(!core.snapshot().quarantined);
            }
        }

        let mut invalid = v3_spectator(ProtocolViolationPolicy::Observe);
        invalid.record_admission(ClientCore::admission_for(&ClientOperation::LeaveSpectator));
        let outcome = process(
            &mut invalid,
            ServerMessage::SpectatorLeft {
                room_id: Some(RoomId::from_u128(10)),
                room_code: Some("ROOM".into()),
                reason: Some(crate::protocol::SpectatorStateChangeReason::Joined),
                current_spectators: vec![],
            },
        );
        assert_lifecycle_violation_containing(&outcome, "joined reason");
        assert_eq!(invalid.room_role(), Some(RoomRole::Spectator));

        // The wire authority leaves `room_id` optional: an authoritative exit
        // from the current room may omit the identity, while a named room
        // must still match.
        let mut omitted_identity = v3_spectator(ProtocolViolationPolicy::Observe);
        let outcome = process(
            &mut omitted_identity,
            ServerMessage::SpectatorLeft {
                room_id: None,
                room_code: None,
                reason: Some(crate::protocol::SpectatorStateChangeReason::RoomClosed),
                current_spectators: vec![],
            },
        );
        assert!(matches!(
            outcome.events.as_slice(),
            [SignalFishEvent::SpectatorLeft { .. }]
        ));
        assert!(!outcome.disconnect);
        assert_eq!(omitted_identity.room_role(), None);
        assert!(!omitted_identity.snapshot().quarantined);

        let mut mismatched_identity = v3_spectator(ProtocolViolationPolicy::Observe);
        let outcome = process(
            &mut mismatched_identity,
            ServerMessage::SpectatorLeft {
                room_id: Some(RoomId::from_u128(999)),
                room_code: None,
                reason: Some(crate::protocol::SpectatorStateChangeReason::RoomClosed),
                current_spectators: vec![],
            },
        );
        assert_lifecycle_violation_containing(&outcome, "identify the current room");
        assert_eq!(mismatched_identity.room_role(), Some(RoomRole::Spectator));
    }

    #[test]
    fn pending_room_responses_are_correlated_and_unattributed_errors_stay_fenced() {
        let mut player_join = ClientCore::new(
            Some(GameDataEncoding::Json),
            ProtocolViolationPolicy::Observe,
            true,
        );
        let _ = process(&mut player_join, authenticated());
        let _ = process(&mut player_join, protocol_info(Some(3)));
        let join = ClientOperation::JoinRoom(JoinRoomParams::new("game", "local"));
        player_join.record_admission(ClientCore::admission_for(&join));
        let before = player_join.snapshot();
        let outcome = process(&mut player_join, spectator_joined());
        assert_lifecycle_violation(&outcome);
        assert_eq!(player_join.snapshot(), before);
        assert_eq!(
            player_join
                .pending_room_operation
                .as_ref()
                .map(|pending| pending.kind),
            Some(PendingRoomOperation::JoinPlayer)
        );

        let mut spectator_join = ClientCore::new(
            Some(GameDataEncoding::Json),
            ProtocolViolationPolicy::Observe,
            true,
        );
        let _ = process(&mut spectator_join, authenticated());
        let _ = process(&mut spectator_join, protocol_info(Some(3)));
        let join = ClientOperation::JoinAsSpectator("game".into(), "ROOM".into(), "viewer".into());
        spectator_join.record_admission(ClientCore::admission_for(&join));
        let before = spectator_join.snapshot();
        let outcome = process(&mut spectator_join, room_joined());
        assert_lifecycle_violation(&outcome);
        assert_eq!(spectator_join.snapshot(), before);
        assert_eq!(
            spectator_join
                .pending_room_operation
                .as_ref()
                .map(|pending| pending.kind),
            Some(PendingRoomOperation::JoinSpectator)
        );

        let mut player_leave = v3_room(ProtocolViolationPolicy::Observe);
        player_leave.record_admission(ClientCore::admission_for(&ClientOperation::LeaveRoom));
        let outcome = process(
            &mut player_leave,
            ServerMessage::Error {
                message: "leave failed; retry".into(),
                error_code: Some(crate::ErrorCode::NotInRoom),
            },
        );
        assert!(matches!(
            outcome.events.as_slice(),
            [SignalFishEvent::Error { .. }]
        ));
        assert_eq!(player_leave.room_role(), Some(RoomRole::Player));
        assert!(matches!(
            player_leave.validate(&ClientOperation::GameData(
                serde_json::json!({"after": "unattributed-error"}),
                GameDataDelivery::Reliable,
            )),
            Err(crate::SignalFishError::RoomOperationPending)
        ));

        let mut spectator_leave = v3_spectator(ProtocolViolationPolicy::Observe);
        spectator_leave
            .record_admission(ClientCore::admission_for(&ClientOperation::LeaveSpectator));
        let before = spectator_leave.snapshot();
        let outcome = process(
            &mut spectator_leave,
            ServerMessage::SpectatorLeft {
                room_id: Some(RoomId::from_u128(999)),
                room_code: Some("OTHER".into()),
                reason: None,
                current_spectators: vec![],
            },
        );
        assert_lifecycle_violation(&outcome);
        assert_eq!(spectator_leave.snapshot(), before);
        assert_eq!(
            spectator_leave
                .pending_room_operation
                .as_ref()
                .map(|pending| pending.kind),
            Some(PendingRoomOperation::LeaveSpectator)
        );
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

    fn assert_lifecycle_violation_containing(outcome: &FrameOutcome, expected: &str) {
        assert_lifecycle_violation(outcome);
        let SignalFishEvent::ProtocolViolation { diagnostic, .. } = &outcome.events[0] else {
            unreachable!("assert_lifecycle_violation verified the event variant")
        };
        assert!(
            diagnostic.contains(expected),
            "expected diagnostic containing {expected:?}, found {diagnostic:?}"
        );
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
                match &message {
                    ServerMessage::RoomJoined(_) => {
                        core.record_admission(ClientCore::admission_for(
                            &ClientOperation::JoinRoom(JoinRoomParams::new("game", "local")),
                        ))
                    }
                    ServerMessage::RoomLeft => core
                        .record_admission(ClientCore::admission_for(&ClientOperation::LeaveRoom)),
                    _ => {}
                }
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
            let transport_before = core.snapshot().session_transport;
            let outcome = process(&mut core, ServerMessage::SessionPlan(Box::new(invalid)));
            assert_lifecycle_violation(&outcome);
            assert_eq!(core.snapshot(), before);
            assert_eq!(core.session_peers, peers_before);
            assert_eq!(core.snapshot().session_transport, transport_before);
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
                core.record_admission(ClientCore::admission_for(&ClientOperation::JoinRoom(
                    JoinRoomParams::new("game", "local"),
                )));
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
                assert_lifecycle_violation_containing(&outcome, "exactly once");
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
    fn authority_baselines_and_changes_are_cross_field_validated_transactionally() {
        let invalid_baselines = [
            ("local authority flag", {
                let ServerMessage::RoomJoined(mut payload) = room_joined() else {
                    unreachable!("room_joined helper always returns RoomJoined")
                };
                payload.is_authority = false;
                ServerMessage::RoomJoined(payload)
            }),
            ("multiple authority players", {
                let ServerMessage::RoomJoined(mut payload) = room_joined() else {
                    unreachable!("room_joined helper always returns RoomJoined")
                };
                payload.current_players[1].is_authority = true;
                ServerMessage::RoomJoined(payload)
            }),
        ];
        for (diagnostic, message) in invalid_baselines {
            let mut core = ClientCore::new(
                Some(GameDataEncoding::Json),
                ProtocolViolationPolicy::Observe,
                true,
            );
            let _ = process(&mut core, authenticated());
            let _ = process(&mut core, protocol_info(Some(3)));
            core.record_admission(ClientCore::admission_for(&ClientOperation::JoinRoom(
                JoinRoomParams::new("game", "local"),
            )));
            let before = core.snapshot();
            let outcome = process(&mut core, message);
            assert_lifecycle_violation_containing(&outcome, diagnostic);
            assert_eq!(core.snapshot(), before);
            assert_eq!(core.authority_player, None);
        }

        let mut core = v3_room(ProtocolViolationPolicy::Observe);
        let invalid_changes = [
            ServerMessage::AuthorityChanged {
                authority_player: Some(PlayerId::from_u128(PEER)),
                you_are_authority: true,
            },
            ServerMessage::AuthorityChanged {
                authority_player: Some(PlayerId::from_u128(999)),
                you_are_authority: false,
            },
        ];
        for message in invalid_changes {
            let outcome = process(&mut core, message);
            assert_lifecycle_violation(&outcome);
            assert_eq!(core.authority_player, Some(PlayerId::from_u128(LOCAL)));
            core.validate(&ClientOperation::RequestAuthority(false))
                .expect("invalid change must not revoke local authority");
        }

        let mut second_authority = player(99);
        second_authority.is_authority = true;
        let outcome = process(
            &mut core,
            ServerMessage::PlayerJoined {
                player: second_authority,
            },
        );
        assert_lifecycle_violation(&outcome);
        assert_eq!(core.authority_player, Some(PlayerId::from_u128(LOCAL)));

        let outcome = process(
            &mut core,
            ServerMessage::AuthorityChanged {
                authority_player: Some(PlayerId::from_u128(PEER)),
                you_are_authority: false,
            },
        );
        assert!(matches!(
            outcome.events.as_slice(),
            [SignalFishEvent::AuthorityChanged { .. }]
        ));
        assert!(matches!(
            core.validate(&ClientOperation::RequestAuthority(false)),
            Err(crate::SignalFishError::AuthorityRequired)
        ));
        assert!(matches!(
            core.validate(&ClientOperation::StartGame),
            Err(crate::SignalFishError::AuthorityRequired)
        ));

        let _ = process(
            &mut core,
            ServerMessage::PlayerLeft {
                player_id: PlayerId::from_u128(PEER),
                epoch: Some(1),
                final_seq: Some(0),
            },
        );
        core.validate(&ClientOperation::StartGame)
            .expect("any player may start when the room has no authority");
    }

    #[test]
    fn generationless_server_04_plan_remains_valid_when_its_shape_is_canonical() {
        let mut core = v3_room(ProtocolViolationPolicy::Observe);
        let mut legacy = plan(Topology::Mesh, TransportKind::WebRtc);
        legacy.generation = None;
        for _ in 0..2 {
            let outcome = process(
                &mut core,
                ServerMessage::SessionPlan(Box::new(legacy.clone())),
            );
            assert!(matches!(
                outcome.events.as_slice(),
                [SignalFishEvent::SessionPlan {
                    generation: None,
                    ..
                }]
            ));
        }
        assert!(core.retired_session_generations.is_empty());
    }

    #[test]
    fn superseded_session_plan_generation_is_rejected_transactionally_for_every_policy() {
        let generation_a = SessionGeneration::from_u128(10);
        let generation_b = SessionGeneration::from_u128(11);
        let mut plan_a = plan(Topology::Mesh, TransportKind::WebRtc);
        plan_a.generation = Some(generation_a);
        plan_a.peers = vec![peer(4)];
        let mut plan_b = plan(Topology::Host, TransportKind::WebRtc);
        plan_b.generation = Some(generation_b);
        plan_b.host = Some(PlayerId::from_u128(LOCAL));

        for policy in [
            ProtocolViolationPolicy::Observe,
            ProtocolViolationPolicy::Quarantine,
            ProtocolViolationPolicy::Disconnect,
        ] {
            let mut core = v3_room(policy);
            assert!(matches!(
                process(
                    &mut core,
                    ServerMessage::SessionPlan(Box::new(plan_a.clone()))
                )
                .events
                .as_slice(),
                [SignalFishEvent::SessionPlan { .. }]
            ));
            assert!(matches!(
                process(
                    &mut core,
                    ServerMessage::SessionPlan(Box::new(plan_b.clone()))
                )
                .events
                .as_slice(),
                [SignalFishEvent::SessionPlan { .. }]
            ));

            let before = core.snapshot();
            let peers_before = core.session_peers.clone();
            #[cfg(feature = "tokio-runtime")]
            let revision_before = core.session_plan_revision;
            let outcome = process(
                &mut core,
                ServerMessage::SessionPlan(Box::new(plan_a.clone())),
            );
            assert_lifecycle_violation_containing(&outcome, "already superseded");
            assert_eq!(
                outcome.disconnect,
                policy == ProtocolViolationPolicy::Disconnect,
                "{policy:?}"
            );
            assert_eq!(core.snapshot().session_generation, Some(generation_b));
            assert_eq!(core.snapshot().session_topology, before.session_topology);
            assert_eq!(core.snapshot().session_transport, before.session_transport);
            assert_eq!(core.session_peers, peers_before);
            #[cfg(feature = "tokio-runtime")]
            assert_eq!(core.session_plan_revision, revision_before);
            assert_eq!(
                core.snapshot().quarantined,
                policy == ProtocolViolationPolicy::Quarantine
            );

            if policy != ProtocolViolationPolicy::Disconnect {
                let stale_signal = process(
                    &mut core,
                    ServerMessage::Signal {
                        from: PlayerId::from_u128(4),
                        generation: Some(generation_a),
                        signal: serde_json::json!({"Offer": "stale"}),
                    },
                );
                assert!(stale_signal.events.is_empty(), "{policy:?}");

                let current_signal = process(
                    &mut core,
                    ServerMessage::Signal {
                        from: PlayerId::from_u128(PEER),
                        generation: Some(generation_b),
                        signal: serde_json::json!({"Offer": "current"}),
                    },
                );
                assert!(matches!(
                    current_signal.events.as_slice(),
                    [SignalFishEvent::SignalReceived { .. }]
                ));
            }
        }
    }

    #[test]
    fn replayed_webrtc_plan_cannot_replace_the_current_direct_transport() {
        let mut core = v3_room(ProtocolViolationPolicy::Observe);
        let mut webrtc = plan(Topology::Mesh, TransportKind::WebRtc);
        webrtc.generation = Some(SessionGeneration::from_u128(10));
        let mut direct = plan(Topology::Host, TransportKind::Direct);
        direct.generation = Some(SessionGeneration::from_u128(11));
        let _ = process(
            &mut core,
            ServerMessage::SessionPlan(Box::new(webrtc.clone())),
        );
        let _ = process(&mut core, ServerMessage::SessionPlan(Box::new(direct)));

        let outcome = process(&mut core, ServerMessage::SessionPlan(Box::new(webrtc)));
        assert_lifecycle_violation_containing(&outcome, "already superseded");
        assert_eq!(core.snapshot().session_topology, Some(Topology::Host));
        assert_eq!(
            core.snapshot().session_transport,
            Some(TransportKind::Direct)
        );
    }

    #[test]
    fn duplicate_current_session_plan_generation_remains_authoritative() {
        let mut core = v3_room(ProtocolViolationPolicy::Observe);
        let current = plan(Topology::Mesh, TransportKind::WebRtc);
        for _ in 0..2 {
            let outcome = process(
                &mut core,
                ServerMessage::SessionPlan(Box::new(current.clone())),
            );
            assert!(matches!(
                outcome.events.as_slice(),
                [SignalFishEvent::SessionPlan { .. }]
            ));
        }
        assert!(core.retired_session_generations.is_empty());
    }

    #[test]
    fn session_plan_replay_guard_retains_more_than_the_previous_generation() {
        let mut core = v3_room(ProtocolViolationPolicy::Observe);
        let mut plan = plan(Topology::Mesh, TransportKind::WebRtc);
        for generation in [10, 11, 12] {
            plan.generation = Some(SessionGeneration::from_u128(generation));
            let outcome = process(
                &mut core,
                ServerMessage::SessionPlan(Box::new(plan.clone())),
            );
            assert!(matches!(
                outcome.events.as_slice(),
                [SignalFishEvent::SessionPlan { .. }]
            ));
        }
        assert_eq!(core.retired_session_generations.len(), 2);

        plan.generation = Some(SessionGeneration::from_u128(10));
        let outcome = process(&mut core, ServerMessage::SessionPlan(Box::new(plan)));
        assert_lifecycle_violation_containing(&outcome, "already superseded");
        assert_eq!(
            core.snapshot().session_generation,
            Some(SessionGeneration::from_u128(12))
        );
    }

    #[test]
    fn retired_session_generation_fence_bounds_churn_and_keeps_recent_replays_fenced() {
        let mut core = v3_room(ProtocolViolationPolicy::Observe);
        let mut plan = plan(Topology::Mesh, TransportKind::WebRtc);
        // Generation churn well past the fence: the retained set stays
        // bounded while every recent-generation replay stays fenced.
        for generation in 10..(10 + RETIRED_SESSION_GENERATION_FENCE as u128 * 3) {
            plan.generation = Some(SessionGeneration::from_u128(generation));
            let outcome = process(
                &mut core,
                ServerMessage::SessionPlan(Box::new(plan.clone())),
            );
            assert!(matches!(
                outcome.events.as_slice(),
                [SignalFishEvent::SessionPlan { .. }]
            ));
            assert!(core.retired_session_generations.len() <= RETIRED_SESSION_GENERATION_FENCE);
        }
        assert_eq!(
            core.retired_session_generations.len(),
            RETIRED_SESSION_GENERATION_FENCE
        );
        // The newest retired generation is still fenced.
        let latest = core.snapshot().session_generation;
        let newest_retired =
            SessionGeneration::from_u128(10 + RETIRED_SESSION_GENERATION_FENCE as u128 * 3 - 2);
        assert_ne!(Some(newest_retired), latest);
        plan.generation = Some(newest_retired);
        let outcome = process(
            &mut core,
            ServerMessage::SessionPlan(Box::new(plan.clone())),
        );
        assert_lifecycle_violation_containing(&outcome, "already superseded");
        assert_eq!(core.snapshot().session_generation, latest);

        // A replay older than the fence degrades to a fresh authoritative
        // plan instead of staying fenced forever (documented contract).
        let evicted_head = SessionGeneration::from_u128(
            10 + RETIRED_SESSION_GENERATION_FENCE as u128 * 3
                - RETIRED_SESSION_GENERATION_FENCE as u128
                - 2,
        );
        assert!(!core.retired_session_generations.contains(&evicted_head));
        plan.generation = Some(evicted_head);
        let outcome = process(&mut core, ServerMessage::SessionPlan(Box::new(plan)));
        assert!(matches!(
            outcome.events.as_slice(),
            [SignalFishEvent::SessionPlan { .. }]
        ));
        assert_eq!(core.snapshot().session_generation, Some(evicted_head));
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
                core.record_reconnect_admitted(
                    PlayerId::from_u128(LOCAL),
                    RoomId::from_u128(10),
                    "submitted-token".into(),
                );
                let before = core.snapshot();
                let outcome = process(&mut core, reconnected(vec![nested.clone()]));
                assert_lifecycle_violation_containing(&outcome, "non-replayable");
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
    fn authoritative_reconnect_resets_retired_session_generations_transactionally() {
        let retired = SessionGeneration::from_u128(10);
        let reconnecting = || {
            let mut core = ClientCore::new(
                Some(GameDataEncoding::Json),
                ProtocolViolationPolicy::Observe,
                true,
            );
            let _ = process(&mut core, authenticated());
            let _ = process(&mut core, protocol_info(Some(3)));
            core.retired_session_generations.push_back(retired);
            core.record_reconnect_admitted(
                PlayerId::from_u128(LOCAL),
                RoomId::from_u128(10),
                "submitted-token".into(),
            );
            core
        };

        let mut accepted = reconnecting();
        let outcome = process(&mut accepted, reconnected(vec![]));
        assert!(matches!(
            outcome.events.as_slice(),
            [SignalFishEvent::Reconnected { .. }]
        ));
        assert!(accepted.retired_session_generations.is_empty());
        let mut fresh_plan = plan(Topology::Mesh, TransportKind::WebRtc);
        fresh_plan.generation = Some(retired);
        let outcome = process(
            &mut accepted,
            ServerMessage::SessionPlan(Box::new(fresh_plan)),
        );
        assert!(matches!(
            outcome.events.as_slice(),
            [SignalFishEvent::SessionPlan { .. }]
        ));

        let mut rejected = reconnecting();
        let ServerMessage::Reconnected(mut invalid) = reconnected(vec![]) else {
            unreachable!("reconnected helper always returns Reconnected")
        };
        invalid.replay = None;
        let outcome = process(&mut rejected, ServerMessage::Reconnected(invalid));
        assert_lifecycle_violation_containing(&outcome, "replay completeness");
        assert!(rejected.retired_session_generations.contains(&retired));
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
                assert_eq!(core.room_role(), None, "{name}");
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
            assert_eq!(core.room_role(), Some(RoomRole::Player));
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
    fn in_room_reconnect_violation_preserves_the_existing_frontier() {
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

    /// Same-generation WebRtc→relay replan: peers dropped by the replacement
    /// hold no authority under the still-live generation, so their in-flight
    /// same-generation signals must stay benign (retired at replacement,
    /// which reads the pre-replacement transport), not lifecycle violations.
    #[test]
    fn same_generation_relay_replan_retires_dropped_webrtc_plan_peers() {
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
            // The relay replacement keeps the current generation.
            let _ = process(
                &mut core,
                ServerMessage::SessionPlan(Box::new(plan(Topology::Relay, TransportKind::Relay))),
            );
            assert_eq!(
                core.snapshot().session_generation,
                Some(SessionGeneration::from_u128(GENERATION))
            );

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
    fn departed_peer_inflight_signal_is_a_suppressed_race_under_every_policy() {
        const LIVE_PEER: u128 = 4;
        let player_left = ServerMessage::PlayerLeft {
            player_id: PlayerId::from_u128(PEER),
            epoch: Some(1),
            final_seq: Some(1),
        };
        let departed_signal = ServerMessage::Signal {
            from: PlayerId::from_u128(PEER),
            generation: Some(SessionGeneration::from_u128(GENERATION)),
            signal: serde_json::json!({"Answer": "queued-before-departure"}),
        };
        let live_signal = || ServerMessage::Signal {
            from: PlayerId::from_u128(LIVE_PEER),
            generation: Some(SessionGeneration::from_u128(GENERATION)),
            signal: serde_json::json!({"Offer": "still-live"}),
        };
        for policy in [
            ProtocolViolationPolicy::Quarantine,
            ProtocolViolationPolicy::Disconnect,
            ProtocolViolationPolicy::Observe,
        ] {
            for player_left_first in [true, false] {
                let mut core = v3_room(policy);
                let mut webrtc = plan(Topology::Mesh, TransportKind::WebRtc);
                webrtc.peers.push(peer(LIVE_PEER));
                let _ = process(&mut core, ServerMessage::SessionPlan(Box::new(webrtc)));
                // PEER was an active sender whose final frames raced our
                // view of its departure.
                let _ = process(
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

                if player_left_first {
                    let _ = process(&mut core, player_left.clone());
                    assert!(core
                        .retired_signal_peers
                        .contains(&PlayerId::from_u128(PEER)));
                    let outcome = process(&mut core, departed_signal.clone());
                    assert!(
                        outcome.events.is_empty(),
                        "{policy:?}: {:#?}",
                        outcome.events
                    );
                    assert!(!outcome.disconnect, "{policy:?}");
                    assert!(!core.snapshot().quarantined, "{policy:?}");
                } else {
                    let outcome = process(&mut core, departed_signal.clone());
                    assert!(
                        matches!(
                            outcome.events.as_slice(),
                            [SignalFishEvent::SignalReceived { .. }]
                        ),
                        "{policy:?}: {:#?}",
                        outcome.events
                    );
                    let _ = process(&mut core, player_left.clone());
                }

                // The session stays healthy either way: a current-generation
                // signal from a still-rostered peer is delivered and no
                // quarantine latched over the relay floor.
                let outcome = process(&mut core, live_signal());
                assert!(
                    matches!(
                        outcome.events.as_slice(),
                        [SignalFishEvent::SignalReceived { .. }]
                    ),
                    "{policy:?}: {:#?}",
                    outcome.events
                );
                assert!(!core.snapshot().quarantined, "{policy:?}");
            }
        }
    }

    #[test]
    fn same_generation_plan_replacement_suppresses_dropped_peer_signals() {
        const LIVE_PEER: u128 = 4;
        for policy in [
            ProtocolViolationPolicy::Quarantine,
            ProtocolViolationPolicy::Disconnect,
            ProtocolViolationPolicy::Observe,
        ] {
            let mut core = v3_room(policy);
            let mut webrtc = plan(Topology::Mesh, TransportKind::WebRtc);
            webrtc.peers.push(peer(LIVE_PEER));
            let _ = process(&mut core, ServerMessage::SessionPlan(Box::new(webrtc)));

            let mut replanned = plan(Topology::Mesh, TransportKind::WebRtc);
            replanned.peers = vec![peer(LIVE_PEER)];
            let _ = process(&mut core, ServerMessage::SessionPlan(Box::new(replanned)));
            assert!(core
                .retired_signal_peers
                .contains(&PlayerId::from_u128(PEER)));

            let dropped = process(
                &mut core,
                ServerMessage::Signal {
                    from: PlayerId::from_u128(PEER),
                    generation: Some(SessionGeneration::from_u128(GENERATION)),
                    signal: serde_json::json!({"IceCandidate": "in-flight"}),
                },
            );
            assert!(dropped.events.is_empty(), "{policy:?}");
            assert!(!dropped.disconnect, "{policy:?}");
            let retained = process(
                &mut core,
                ServerMessage::Signal {
                    from: PlayerId::from_u128(LIVE_PEER),
                    generation: Some(SessionGeneration::from_u128(GENERATION)),
                    signal: serde_json::json!({"Offer": "retained"}),
                },
            );
            assert!(
                matches!(
                    retained.events.as_slice(),
                    [SignalFishEvent::SignalReceived { .. }]
                ),
                "{policy:?}: {:#?}",
                retained.events
            );
        }
    }

    #[test]
    fn new_peer_reauthorization_clears_departure_retirement_for_current_generation() {
        let mut core = v3_room(ProtocolViolationPolicy::Observe);
        let _ = process(
            &mut core,
            ServerMessage::SessionPlan(Box::new(plan(Topology::Mesh, TransportKind::WebRtc))),
        );
        let _ = process(
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
        let _ = process(
            &mut core,
            ServerMessage::PlayerLeft {
                player_id: PlayerId::from_u128(PEER),
                epoch: Some(1),
                final_seq: Some(1),
            },
        );
        assert!(core
            .retired_signal_peers
            .contains(&PlayerId::from_u128(PEER)));

        let _ = process(
            &mut core,
            ServerMessage::PlayerJoined {
                player: PlayerInfo {
                    id: PlayerId::from_u128(PEER),
                    name: "peer".into(),
                    is_authority: false,
                    is_ready: false,
                    connected_at: "2026-01-01T00:00:00Z".into(),
                    connection_info: None,
                    epoch: Some(2),
                    seq: Some(0),
                },
            },
        );
        let _ = process(
            &mut core,
            ServerMessage::NewPeer {
                peer_id: PlayerId::from_u128(PEER),
                you_initiate: true,
            },
        );
        assert!(core.retired_signal_peers.is_empty());

        let outcome = process(
            &mut core,
            ServerMessage::Signal {
                from: PlayerId::from_u128(PEER),
                generation: Some(SessionGeneration::from_u128(GENERATION)),
                signal: serde_json::json!({"Offer": "rejoined"}),
            },
        );
        assert!(matches!(
            outcome.events.as_slice(),
            [SignalFishEvent::SignalReceived { .. }]
        ));
    }

    /// A generation-changing plan must not carry peer retirement into the
    /// new generation: dropped peers never held authority under it, so their
    /// current-generation signals are genuine violations (their
    /// superseded-generation frames still die in the generation check).
    #[test]
    fn generation_change_does_not_carry_retirement_to_the_new_generation() {
        let mut core = v3_room(ProtocolViolationPolicy::Quarantine);
        let _ = process(
            &mut core,
            ServerMessage::SessionPlan(Box::new(plan(Topology::Mesh, TransportKind::WebRtc))),
        );
        assert!(core.retired_signal_peers.is_empty());

        let mut replanned = plan(Topology::Mesh, TransportKind::WebRtc);
        replanned.generation = Some(SessionGeneration::from_u128(GENERATION + 1));
        replanned.peers.clear();
        let _ = process(&mut core, ServerMessage::SessionPlan(Box::new(replanned)));
        // The generation change clears retirements without re-retiring the
        // dropped peer under the new generation.
        assert!(core.retired_signal_peers.is_empty());

        // A stale superseded-generation frame stays a benign race.
        let stale = process(
            &mut core,
            ServerMessage::Signal {
                from: PlayerId::from_u128(PEER),
                generation: Some(SessionGeneration::from_u128(GENERATION)),
                signal: serde_json::json!({"Offer": "superseded"}),
            },
        );
        assert!(stale.events.is_empty(), "{:#?}", stale.events);

        // A current-generation signal from a sender who never held this
        // generation's authority is a violation, not silent suppression.
        let outcome = process(
            &mut core,
            ServerMessage::Signal {
                from: PlayerId::from_u128(PEER),
                generation: Some(SessionGeneration::from_u128(GENERATION + 1)),
                signal: serde_json::json!({"Offer": "never-authorized-here"}),
            },
        );
        assert_lifecycle_violation(&outcome);
        assert!(core.snapshot().quarantined);
    }

    #[test]
    fn unknown_sender_signal_after_departure_remains_a_lifecycle_violation() {
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
            let _ = process(
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
            let _ = process(
                &mut core,
                ServerMessage::PlayerLeft {
                    player_id: PlayerId::from_u128(PEER),
                    epoch: Some(1),
                    final_seq: Some(1),
                },
            );

            let outcome = process(
                &mut core,
                ServerMessage::Signal {
                    from: PlayerId::from_u128(3),
                    generation: Some(SessionGeneration::from_u128(GENERATION)),
                    signal: serde_json::json!({"Offer": "never-authorized"}),
                },
            );
            assert_lifecycle_violation_containing(
                &outcome,
                "not in the authoritative session peer set",
            );
            match policy {
                ProtocolViolationPolicy::Quarantine => {
                    assert!(core.snapshot().quarantined);
                    assert!(!outcome.disconnect);
                }
                ProtocolViolationPolicy::Disconnect => {
                    assert!(outcome.disconnect);
                }
                ProtocolViolationPolicy::Observe => {
                    assert!(!core.snapshot().quarantined);
                    assert!(!outcome.disconnect);
                }
            }
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
            .retired_signal_peers
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
        assert!(!core.retired_signal_peers.is_empty());
        assert_eq!(core.snapshot().session_topology, Some(Topology::Relay));
        assert_eq!(
            core.snapshot().session_transport,
            Some(TransportKind::Relay)
        );

        core.record_admission(ClientCore::admission_for(&ClientOperation::LeaveRoom));
        let _ = process(&mut core, ServerMessage::RoomLeft);
        assert!(core.retired_signal_peers.is_empty());
        assert!(core.snapshot().session_topology.is_none());
        assert!(core.snapshot().session_transport.is_none());

        let mut generated_room = v3_room(ProtocolViolationPolicy::Observe);
        let _ = process(
            &mut generated_room,
            ServerMessage::SessionPlan(Box::new(plan(Topology::Mesh, TransportKind::WebRtc))),
        );
        let mut generated_replacement = plan(Topology::Mesh, TransportKind::WebRtc);
        generated_replacement.generation = Some(SessionGeneration::from_u128(GENERATION + 1));
        let _ = process(
            &mut generated_room,
            ServerMessage::SessionPlan(Box::new(generated_replacement)),
        );
        assert!(!generated_room.retired_session_generations.is_empty());
        generated_room.record_admission(ClientCore::admission_for(&ClientOperation::LeaveRoom));
        let _ = process(&mut generated_room, ServerMessage::RoomLeft);
        assert!(generated_room.retired_session_generations.is_empty());

        let mut disconnecting = v3_room(ProtocolViolationPolicy::Observe);
        let _ = process(
            &mut disconnecting,
            ServerMessage::SessionPlan(Box::new(plan(Topology::Mesh, TransportKind::WebRtc))),
        );
        let mut replacement = plan(Topology::Mesh, TransportKind::WebRtc);
        replacement.generation = Some(SessionGeneration::from_u128(GENERATION + 1));
        let _ = process(
            &mut disconnecting,
            ServerMessage::SessionPlan(Box::new(replacement)),
        );
        assert!(!disconnecting.retired_session_generations.is_empty());
        assert_eq!(
            disconnecting.snapshot().session_topology,
            Some(Topology::Mesh)
        );
        assert_eq!(
            disconnecting.snapshot().session_transport,
            Some(TransportKind::WebRtc)
        );
        let _ = disconnecting.disconnect(None);
        assert!(disconnecting.retired_session_generations.is_empty());
        assert!(disconnecting.snapshot().session_topology.is_none());
        assert!(disconnecting.snapshot().session_transport.is_none());
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
                    match &message {
                        ServerMessage::RoomJoined(_) => {
                            core.record_admission(ClientCore::admission_for(
                                &ClientOperation::JoinRoom(JoinRoomParams::new("game", "local")),
                            ))
                        }
                        ServerMessage::RoomLeft => core.record_admission(
                            ClientCore::admission_for(&ClientOperation::LeaveRoom),
                        ),
                        _ => {}
                    }
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

    #[test]
    fn dequeue_serialization_failure_releases_only_the_matching_fence() {
        let cases: [(&str, ClientOperation, ClientMessage); 5] = [
            (
                "join",
                ClientOperation::JoinRoom(JoinRoomParams::new("game", "local")),
                ClientMessage::JoinRoom {
                    game_name: "game".into(),
                    player_name: "local".into(),
                    room_code: None,
                    max_players: None,
                    supports_authority: None,
                    relay_transport: None,
                },
            ),
            (
                "leave",
                ClientOperation::LeaveRoom,
                ClientMessage::LeaveRoom,
            ),
            (
                "reconnect",
                ClientOperation::Reconnect(
                    PlayerId::from_u128(LOCAL),
                    RoomId::from_u128(10),
                    "token".into(),
                ),
                ClientMessage::Reconnect {
                    player_id: PlayerId::from_u128(LOCAL),
                    room_id: RoomId::from_u128(10),
                    auth_token: "token".into(),
                },
            ),
            (
                "spectator join",
                ClientOperation::JoinAsSpectator("game".into(), "ROOM".into(), "viewer".into()),
                ClientMessage::JoinAsSpectator {
                    game_name: "game".into(),
                    room_code: "ROOM".into(),
                    spectator_name: "viewer".into(),
                },
            ),
            (
                "spectator leave",
                ClientOperation::LeaveSpectator,
                ClientMessage::LeaveSpectator,
            ),
        ];

        for (name, operation, message) in cases.iter() {
            let mut core = ClientCore::new(
                Some(GameDataEncoding::Json),
                ProtocolViolationPolicy::Observe,
                true,
            );
            core.record_admission(ClientCore::admission_for(operation));
            assert!(
                core.pending_room_operation.is_some(),
                "{name}: admission must arm the fence"
            );
            if matches!(operation, ClientOperation::Reconnect(..)) {
                assert_eq!(core.pending_reconnects.len(), 1);
            }

            core.dequeue_serialization_failed(message);
            assert!(
                core.pending_room_operation.is_none(),
                "{name}: dequeue failure must release the fence"
            );
            assert!(
                core.pending_reconnects.is_empty(),
                "{name}: reconnect bookkeeping must be released with the fence"
            );
            // A mismatched message kind must never release someone else's
            // fence.
            core.record_admission(ClientCore::admission_for(operation));
            core.dequeue_serialization_failed(&ClientMessage::Ping);
            assert!(
                core.pending_room_operation.is_some(),
                "{name}: an unrelated message must not release the fence"
            );
        }

        // After `room_operation_ids` negotiation the same five operations are
        // enqueued wrapped in a correlated `RoomOperation` envelope; a
        // dequeue-time serialization failure of that envelope must release
        // the fence exactly like the unwrapped shapes above.
        for (name, operation, unwrapped) in cases.iter().map(|(n, o, m)| (n, o, m.clone())) {
            let operation_id = RoomOperationId::from_u128(0xaaaa);
            let envelope = correlate_room_operation(unwrapped, operation_id);

            // Negotiated mode, with the fence carrying the same operation id
            // the envelope was built with — the exact state
            // `prepare_with_admission` produces.
            let mut core = ClientCore::new_with_room_operation_ids(
                Some(GameDataEncoding::Json),
                ProtocolViolationPolicy::Observe,
                true,
                true,
            );
            core.record_admission(ClientCore::admission_for(operation));
            if let Some(pending) = core.pending_room_operation.as_mut() {
                pending.operation_id = Some(operation_id);
            }
            assert!(
                core.pending_room_operation.is_some(),
                "{name}: correlated admission must arm the fence"
            );

            core.dequeue_serialization_failed(&envelope);
            assert!(
                core.pending_room_operation.is_none(),
                "{name}: a correlated dequeue failure must release the fence"
            );
            assert!(
                core.pending_reconnects.is_empty(),
                "{name}: correlated reconnect bookkeeping must be released with the fence"
            );

            // The kind match is still exact for envelopes.
            core.record_admission(ClientCore::admission_for(operation));
            let unrelated = correlate_room_operation(
                match operation {
                    ClientOperation::JoinRoom(_) => ClientMessage::LeaveRoom,
                    _ => ClientMessage::JoinRoom {
                        game_name: "other".into(),
                        player_name: "local".into(),
                        room_code: None,
                        max_players: None,
                        supports_authority: None,
                        relay_transport: None,
                    },
                },
                RoomOperationId::from_u128(0xbbbb),
            );
            core.dequeue_serialization_failed(&unrelated);
            assert!(
                core.pending_room_operation.is_some(),
                "{name}: a different correlated operation must not release the fence"
            );
        }
    }
}
