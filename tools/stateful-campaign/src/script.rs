//! Scenario script model: a deterministic list of steps the runner replays,
//! plus the generator's knowledge about each delivered frame (used by the
//! per-frame event-expectation oracle).

use signal_fish_client::protocol::{PlayerId, ServerMessage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EchoId {
    /// Use the operation id the client actually sent (read from the outbound log).
    Match,
    /// Use a wrong, unrelated UUID.
    Wrong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EchoKind {
    JoinOk,
    JoinFailed,
    LeaveOk,
    ReconnectOk,
    ReconnectFailed,
    SpectatorJoinOk,
    SpectatorJoinFailed,
    SpectatorLeaveOk,
    OperationFailed,
}

impl EchoKind {
    pub fn name(self) -> &'static str {
        match self {
            EchoKind::JoinOk => "RoomJoined",
            EchoKind::JoinFailed => "RoomJoinFailed",
            EchoKind::LeaveOk => "RoomLeft",
            EchoKind::ReconnectOk => "Reconnected",
            EchoKind::ReconnectFailed => "ReconnectionFailed",
            EchoKind::SpectatorJoinOk => "SpectatorJoined",
            EchoKind::SpectatorJoinFailed => "SpectatorJoinFailed",
            EchoKind::SpectatorLeaveOk => "SpectatorLeft",
            EchoKind::OperationFailed => "OperationFailed",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Cmd {
    JoinRoom,
    JoinRoomMax(u8),
    LeaveRoom,
    SendGameData(serde_json::Value),
    SendGameDataLatest(u32),
    SendGameDataVolatile,
    SendBinaryGameData(usize),
    SetReady,
    StartGame,
    RequestAuthority(bool),
    ProvideConnectionInfo,
    Reconnect(PlayerId, PlayerId),
    JoinAsSpectator,
    LeaveSpectator,
    Ping,
    SendSignal,
    SendRawSignal,
    ReportTransportStatus,
}

impl Cmd {
    pub fn name(&self) -> &'static str {
        match self {
            Cmd::JoinRoom | Cmd::JoinRoomMax(_) => "join_room",
            Cmd::LeaveRoom => "leave_room",
            Cmd::SendGameData(_) => "send_game_data",
            Cmd::SendGameDataLatest(_) => "send_game_data(Latest)",
            Cmd::SendGameDataVolatile => "send_game_data(Volatile)",
            Cmd::SendBinaryGameData(_) => "send_binary_game_data",
            Cmd::SetReady => "set_ready",
            Cmd::StartGame => "start_game",
            Cmd::RequestAuthority(_) => "request_authority",
            Cmd::ProvideConnectionInfo => "provide_connection_info",
            Cmd::Reconnect(..) => "reconnect",
            Cmd::JoinAsSpectator => "join_as_spectator",
            Cmd::LeaveSpectator => "leave_spectator",
            Cmd::Ping => "ping",
            Cmd::SendSignal => "send_signal",
            Cmd::SendRawSignal => "send_raw_signal",
            Cmd::ReportTransportStatus => "report_transport_status",
        }
    }

    /// The room-operation fence this command arms at successful queue
    /// admission (mirrors `ClientCore`'s pending-operation model).
    pub fn fence(self: &Cmd) -> Option<FenceKind> {
        match self {
            Cmd::JoinRoom | Cmd::JoinRoomMax(_) => Some(FenceKind::JoinPlayer),
            Cmd::LeaveRoom => Some(FenceKind::LeavePlayer),
            Cmd::Reconnect(..) => Some(FenceKind::ReconnectPlayer),
            Cmd::JoinAsSpectator => Some(FenceKind::JoinSpectator),
            Cmd::LeaveSpectator => Some(FenceKind::LeaveSpectator),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceKind {
    JoinPlayer,
    LeavePlayer,
    ReconnectPlayer,
    JoinSpectator,
    LeaveSpectator,
}

/// Generator knowledge about a delivered game-data frame's v3 stamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StampMode {
    /// Fresh monotonic sequence for the sender (well formed).
    #[default]
    Valid,
    /// Deliberately stale/replayed sequence (well formed; a stamp
    /// violation — backward or non-positive sequence).
    Stale,
    /// Both stamps zero (schema-valid; stamp-invalid for the core).
    Zero,
    /// Stamps omitted (schema-valid; v2-legal, v3-invalid).
    None,
}

impl StampMode {
    pub fn well_formed(self) -> bool {
        matches!(self, StampMode::Valid | StampMode::Stale)
    }
}

/// Extra generator knowledge attached to a delivery step.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameMeta {
    /// `GameData`/`GameDataBinary` stamp shape (ignored for other frames).
    pub stamp: StampMode,
    /// The frame deliberately exceeds a validated bound (hostile face).
    pub bound_breaking: bool,
}

#[allow(clippy::large_enum_variant)] // scripts own their messages by design
#[derive(Debug, Clone)]
pub enum Step {
    /// Deliver a schema-valid `ServerMessage` text frame.
    Deliver(ServerMessage, FrameMeta),
    /// Deliver a handcrafted raw (always schema-invalid) text frame.
    DeliverRaw(&'static str),
    /// Deliver a schema-valid physical binary frame (v3 MessagePack envelope).
    /// The generator guarantees the envelope decodes; `FrameMeta.stamp`
    /// classifies the embedded stamps.
    DeliverBinary(Vec<u8>, FrameMeta),
    /// Deliver a `RoomOperationResult` echoing (or not) the client's last sent operation id.
    DeliverEcho(EchoKind, EchoId),
    /// Issue one public-API client command (refusals are recorded, not fatal).
    Cmd(Cmd),
    /// Call `poll()` `n` times.
    Poll(usize),
    /// Graceful close boundary.
    Close,
    /// Server-initiated transport close (poll_recv -> Ready(None) forever).
    PeerClose,
    /// Arm the transport's send-side `Pending`-refusal face: each frame is
    /// refused with `Pending` for its first N offers before acceptance.
    SetSendDelay(usize),
    /// Terminal transport error face: `poll_recv` returns
    /// `Ready(Some(Err(_)))` (and `poll_send` too when `fail_send`).
    TransportKill { fail_recv: bool, fail_send: bool },
}

impl Step {
    pub fn render(&self) -> String {
        match self {
            Step::Deliver(msg, meta) => {
                let json = match serde_json::to_string(msg) {
                    Ok(json) => json,
                    Err(_) => "<ser fail>".to_string(),
                };
                format!(
                    "Deliver({}{}) {json}",
                    msg_variant_name(msg),
                    meta_suffix(meta)
                )
            }
            Step::DeliverRaw(raw) => format!("DeliverRaw({raw})"),
            Step::DeliverBinary(bytes, meta) => {
                format!(
                    "DeliverBinary(<{} bytes>{})",
                    bytes.len(),
                    meta_suffix(meta)
                )
            }
            Step::DeliverEcho(kind, id) => match id {
                EchoId::Match => format!("DeliverEcho({}, operation_id=match)", kind.name()),
                EchoId::Wrong => format!("DeliverEcho({}, operation_id=WRONG)", kind.name()),
            },
            Step::Cmd(cmd) => format!("Cmd({})", cmd.name()),
            Step::Poll(n) => format!("Poll({n})"),
            Step::Close => "Close".to_string(),
            Step::PeerClose => "PeerClose(transport Ready(None))".to_string(),
            Step::SetSendDelay(n) => format!("SetSendDelay({n})"),
            Step::TransportKill {
                fail_recv,
                fail_send,
            } => format!(
                "TransportKill(recv=Ready(Some(Err)):{fail_recv} send=Ready(Err):{fail_send})"
            ),
        }
    }
}

fn meta_suffix(meta: &FrameMeta) -> String {
    let mut parts = Vec::new();
    match meta.stamp {
        StampMode::Valid => parts.push("stamp=valid"),
        StampMode::Stale => parts.push("stamp=stale"),
        StampMode::Zero => parts.push("stamp=zero"),
        StampMode::None => parts.push("stamp=none"),
    }
    if meta.bound_breaking {
        parts.push("bound-break");
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" {{{}}}", parts.join(","))
    }
}

pub fn msg_variant_name(msg: &ServerMessage) -> &'static str {
    match msg {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKind {
    /// `enable_v3()`: negotiated v3, requests room_operation_ids capability.
    V3,
    /// v3 relay floor with explicit version only (no transports/topologies lists).
    V3VersionOnly,
    /// Frozen v2 relay floor (no negotiation fields at all).
    V2,
    /// Explicit protocol_version = 2.
    V2Explicit,
}

impl ConfigKind {
    pub fn name(self) -> &'static str {
        match self {
            ConfigKind::V3 => "v3",
            ConfigKind::V3VersionOnly => "v3-version-only",
            ConfigKind::V2 => "v2",
            ConfigKind::V2Explicit => "v2-explicit",
        }
    }

    /// Mirrors `SignalFishConfig::requests_room_operation_ids`.
    pub fn requests_room_operation_ids(self) -> bool {
        matches!(self, ConfigKind::V3 | ConfigKind::V3VersionOnly)
    }

    pub fn is_v3(self) -> bool {
        matches!(self, ConfigKind::V3 | ConfigKind::V3VersionOnly)
    }
}

#[derive(Debug, Clone)]
pub struct Script {
    pub seed: u64,
    pub index: usize,
    pub archetype: &'static str,
    pub config_kind: ConfigKind,
    /// Whether ProtocolInfo in this script echoes the room_operation_ids capability.
    pub echo_room_ops: bool,
    /// Small command-queue override for send-pressure scripts.
    pub small_command_capacity: Option<usize>,
    pub steps: Vec<Step>,
}
