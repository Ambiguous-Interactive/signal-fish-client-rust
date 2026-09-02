//! Scenario generator: deterministic, schema-valid hostile server-message
//! sequences mixed with random client commands. Every delivered game-data
//! frame carries [`FrameMeta`] so the expectation oracle can distinguish
//! "documented suppression" from "silent swallow".

use std::collections::HashMap;

use signal_fish_client::protocol::{
    DeliveryClass, DeliveryGap, DeliveryGapReason, DeliveryReportPayload, DirectEndpoint,
    GameDataEncoding, IceServer, LatestDeliveryCounters, LobbyState, PeerConnectionInfo, PlayerId,
    PlayerInfo, ProtocolInfoPayload, RateLimitInfo, ReliableDeliveryCounters, ReplayStatus, RoomId,
    RoomJoinedPayload, SenderWatermark, ServerMessage, SessionPeer, SessionPlanPayload,
    SpectatorInfo, SpectatorJoinedPayload, SpectatorStateChangeReason, Topology, TransportKind,
    VolatileDeliveryCounters,
};
use signal_fish_client::ErrorCode;
use uuid::Uuid;

use crate::rng::Rng;
use crate::script::{Cmd, ConfigKind, EchoId, EchoKind, FrameMeta, Script, StampMode, Step};

pub const DELIVERY_REPORT_MAX_GAPS: usize = 256;

pub struct Ctx {
    pub self_id: PlayerId,
    pub room_id: RoomId,
    pub peer_a: PlayerId,
    pub peer_b: PlayerId,
    pub peer_c: PlayerId,
    pub unknown: PlayerId,
    pub spectator_a: PlayerId,
    pub gen: [u128; 6],
    pub room_code: String,
    pub game_name: String,
    pub sender_seq: HashMap<PlayerId, u64>,
    pub sender_epoch: HashMap<PlayerId, u32>,
}

impl Ctx {
    fn new(rng: &mut Rng) -> Self {
        let n = |rng: &mut Rng, salt: u64| Uuid::from_u128(rng.uuid_u128() ^ u128::from(salt));
        Self {
            self_id: n(rng, 1),
            room_id: n(rng, 2),
            peer_a: n(rng, 3),
            peer_b: n(rng, 4),
            peer_c: n(rng, 5),
            unknown: n(rng, 6),
            spectator_a: n(rng, 7),
            gen: [
                rng.uuid_u128(),
                rng.uuid_u128(),
                rng.uuid_u128(),
                rng.uuid_u128(),
                rng.uuid_u128(),
                rng.uuid_u128(),
            ],
            room_code: if rng.chance(10) {
                String::new()
            } else {
                format!("R{}", rng.below(1_000_000))
            },
            game_name: if rng.chance(5) {
                String::new()
            } else {
                "hostile-game".to_string()
            },
            sender_seq: HashMap::new(),
            sender_epoch: HashMap::new(),
        }
    }

    pub fn gen_uuid(&self, k: usize) -> Uuid {
        let slot = k.checked_rem(self.gen.len()).unwrap_or(0);
        Uuid::from_u128(self.gen.get(slot).copied().unwrap_or(self.gen[0]))
    }

    /// Next monotonic stamp for a sender (hostile variants mutate afterwards).
    pub fn next_seq(&mut self, sender: PlayerId) -> u64 {
        let next = self.sender_seq.entry(sender).or_insert(0);
        *next = next.wrapping_add(1);
        *next
    }

    pub fn epoch_of(&self, sender: PlayerId) -> u32 {
        *self.sender_epoch.get(&sender).unwrap_or(&1)
    }
}

fn player_info(
    _ctx: &Ctx,
    id: PlayerId,
    name: &str,
    epoch: Option<u32>,
    seq: Option<u64>,
) -> PlayerInfo {
    PlayerInfo {
        id,
        name: name.to_string(),
        is_authority: false,
        is_ready: false,
        connected_at: "2026-09-02T00:00:00Z".to_string(),
        connection_info: None,
        epoch,
        seq,
    }
}

fn spectator_info(id: PlayerId, name: &str) -> SpectatorInfo {
    SpectatorInfo {
        id,
        name: name.to_string(),
        connected_at: "2026-09-02T00:00:00Z".to_string(),
    }
}

fn rate_limits(huge: bool) -> RateLimitInfo {
    if huge {
        RateLimitInfo {
            per_minute: u64::MAX,
            per_hour: u64::MAX,
            per_day: u64::MAX,
        }
    } else {
        RateLimitInfo {
            per_minute: 60,
            per_hour: 1000,
            per_day: 10_000,
        }
    }
}

fn protocol_info(
    rng: &mut Rng,
    _ctx: &Ctx,
    config: ConfigKind,
    echo_room_ops: bool,
    allow_bound_breaking: bool,
) -> (ServerMessage, FrameMeta) {
    let v3 = config.is_v3();
    let mut capabilities: Vec<String> = Vec::new();
    if v3 && echo_room_ops {
        capabilities.push("room_operation_ids".to_string());
    }
    if rng.chance(15) {
        capabilities.push("unknown_future_capability".to_string());
    }
    let v3_fields = v3.then_some(3u16);
    // 10% of v3 negotiation frames advertise an outbound bound beyond the
    // authority's validated range (a bound-breaking hostile face).
    let bound_breaking = v3 && allow_bound_breaking && rng.below(10) == 9;
    let max_outbound = if v3_fields.is_some() {
        match bound_breaking {
            true => usize::MAX,
            false if rng.below(9) == 0 => 67_108_864, // authority bound maximum (valid)
            false => 8 * 1024 * 1024,
        }
    } else {
        // The v2 face must omit the field entirely (validated).
        usize::MAX
    };
    let max_outbound = if v3_fields.is_some() {
        Some(max_outbound)
    } else {
        None
    };
    (
        ServerMessage::ProtocolInfo(ProtocolInfoPayload {
            platform: rng.chance(50).then(|| "rust".to_string()),
            sdk_version: rng.chance(50).then(|| "0.0.0-hostile".to_string()),
            minimum_version: v3_fields.map(|_| "0.4.0".to_string()),
            recommended_version: rng.chance(30).then(|| "0.8.0".to_string()),
            capabilities,
            notes: rng.chance(10).then(String::new),
            game_data_formats: if rng.chance(20) {
                vec![GameDataEncoding::Json]
            } else {
                vec![GameDataEncoding::Json, GameDataEncoding::MessagePack]
            },
            player_name_rules: rng.chance(20).then(|| {
                signal_fish_client::protocol::PlayerNameRulesPayload {
                    max_length: 32,
                    min_length: 1,
                    allow_unicode_alphanumeric: true,
                    allow_spaces: false,
                    allow_leading_trailing_whitespace: false,
                    allowed_symbols: vec!["_".to_string()],
                    additional_allowed_characters: None,
                }
            }),
            protocol_version: v3_fields,
            min_protocol_version: v3_fields.map(|_| 2),
            max_protocol_version: v3_fields.map(|_| 3),
            transports: v3_fields
                .map(|_| vec![signal_fish_client::protocol::MessageTransport::Websocket]),
            max_outbound_message_size: max_outbound,
        }),
        FrameMeta {
            stamp: StampMode::None,
            bound_breaking,
        },
    )
}

fn authenticated(rng: &mut Rng) -> ServerMessage {
    ServerMessage::Authenticated {
        app_name: if rng.chance(10) {
            String::new()
        } else {
            "hostile-app".to_string()
        },
        organization: rng.chance(30).then(|| "org".to_string()),
        rate_limits: rate_limits(rng.chance(10)),
    }
}

fn room_joined(
    rng: &mut Rng,
    ctx: &Ctx,
    roster: &[PlayerInfo],
    max_players: u8,
    with_token: bool,
) -> ServerMessage {
    ServerMessage::RoomJoined(Box::new(RoomJoinedPayload {
        room_id: ctx.room_id,
        room_code: ctx.room_code.clone(),
        player_id: ctx.self_id,
        game_name: ctx.game_name.clone(),
        max_players,
        supports_authority: rng.chance(60),
        current_players: roster.to_vec(),
        is_authority: rng.chance(30),
        lobby_state: rng
            .pick(&[
                LobbyState::Waiting,
                LobbyState::Lobby,
                LobbyState::Finalized,
            ])
            .clone(),
        ready_players: vec![],
        relay_type: "websocket".to_string(),
        current_spectators: vec![],
        ice_servers: vec![],
        reconnection_token: with_token.then(|| format!("tok-{}", ctx.gen[0])),
    }))
}

fn game_data(
    rng: &mut Rng,
    ctx: &mut Ctx,
    from: PlayerId,
    mode: StampMode,
) -> (ServerMessage, FrameMeta) {
    let (seq, epoch) = match mode {
        StampMode::Valid => {
            let s = ctx.next_seq(from);
            (Some(s), Some(ctx.epoch_of(from)))
        }
        StampMode::Stale => {
            let s = ctx.sender_seq.entry(from).or_insert(2);
            (Some(s.saturating_sub(1)), Some(ctx.epoch_of(from)))
        }
        StampMode::Zero => (Some(0), Some(0)),
        StampMode::None => (None, None),
    };
    let class = match rng.below(4) {
        0 => Some(DeliveryClass::Reliable),
        1 => Some(DeliveryClass::Latest),
        2 => Some(DeliveryClass::Volatile),
        _ => None,
    };
    let key = if matches!(class, Some(DeliveryClass::Latest)) {
        Some(rng.next_u64() as u32)
    } else {
        None
    };
    (
        ServerMessage::GameData {
            from_player: from,
            data: hostile_json_value(rng),
            seq,
            epoch,
            class,
            key,
        },
        FrameMeta {
            stamp: mode,
            bound_breaking: false,
        },
    )
}

fn hostile_json_value(rng: &mut Rng) -> serde_json::Value {
    match rng.below(6) {
        0 => serde_json::json!({ "k": rng.next_u64() }),
        1 => serde_json::json!([]),
        2 => serde_json::json!({}),
        3 => serde_json::json!(format!("huge-{}", u64::MAX)),
        4 => serde_json::Value::Null,
        _ => serde_json::json!({ "nested": { "deep": [1, 2, { "x": true }] } }),
    }
}

fn counters_with_superseded(total: u64) -> signal_fish_client::protocol::DeliveryCountersByClass {
    signal_fish_client::protocol::DeliveryCountersByClass {
        reliable: ReliableDeliveryCounters::default(),
        latest: LatestDeliveryCounters {
            superseded: total,
            ..LatestDeliveryCounters::default()
        },
        volatile: VolatileDeliveryCounters::default(),
    }
}

fn singleton_gap(sender: PlayerId, epoch: u32, seq: u64, reason: DeliveryGapReason) -> DeliveryGap {
    DeliveryGap {
        from_player: sender,
        epoch,
        from_seq: seq,
        to_seq: seq,
        reason,
    }
}

/// Consistent batch of 256 singleton gaps with matching cumulative counters.
fn consistent_report(ctx: &Ctx, sender: PlayerId, batch: u64) -> ServerMessage {
    let epoch = ctx.epoch_of(sender);
    let gaps: Vec<DeliveryGap> = (0..DELIVERY_REPORT_MAX_GAPS)
        .map(|i| {
            let seq = batch
                .saturating_sub(1)
                .saturating_mul(DELIVERY_REPORT_MAX_GAPS as u64)
                .saturating_add(i as u64)
                .saturating_add(1);
            singleton_gap(sender, epoch, seq, DeliveryGapReason::LatestSuperseded)
        })
        .collect();
    ServerMessage::DeliveryReport(Box::new(DeliveryReportPayload {
        gaps,
        per_class: counters_with_superseded(batch.saturating_mul(DELIVERY_REPORT_MAX_GAPS as u64)),
    }))
}

fn hostile_report(rng: &mut Rng, ctx: &Ctx) -> (ServerMessage, FrameMeta) {
    let sender = *rng.pick(&[ctx.peer_a, ctx.peer_b, ctx.unknown]);
    let epoch = ctx.epoch_of(sender);
    // Modes 0, 1, 3, 4 are structurally invalid; mode 2 (an unsupported-format
    // run) is validity-ambiguous, so it is not marked bound-breaking.
    let (gaps, bound_breaking) = match rng.below(5) {
        // Overlapping ranges.
        0 => (
            vec![
                singleton_gap(sender, epoch, 5, DeliveryGapReason::LatestSuperseded),
                singleton_gap(sender, epoch, 5, DeliveryGapReason::LatestSuperseded),
                DeliveryGap {
                    from_player: sender,
                    epoch,
                    from_seq: 10,
                    to_seq: 20,
                    reason: DeliveryGapReason::VolatileDropped,
                },
                DeliveryGap {
                    from_player: sender,
                    epoch,
                    from_seq: 15,
                    to_seq: 25,
                    reason: DeliveryGapReason::VolatileDropped,
                },
            ],
            true,
        ),
        // Reversed / miscounted ranges.
        1 => (
            vec![DeliveryGap {
                from_player: sender,
                epoch,
                from_seq: 30,
                to_seq: 10,
                reason: DeliveryGapReason::LatestDroppedFull,
            }],
            true,
        ),
        // Unsupported-format ranges with a later (possibly legal) advisory.
        2 => (
            (0..8usize)
                .map(|i| {
                    singleton_gap(
                        sender,
                        epoch,
                        (i as u64).saturating_add(1),
                        DeliveryGapReason::UnsupportedFormat,
                    )
                })
                .collect(),
            false,
        ),
        // Wrong-sender / wrong-epoch gaps.
        3 => (
            vec![
                singleton_gap(ctx.unknown, 999, 1, DeliveryGapReason::LatestSuperseded),
                singleton_gap(sender, 0, 2, DeliveryGapReason::VolatileDropped),
            ],
            true,
        ),
        // Over the per-report cap.
        _ => (
            (0..=DELIVERY_REPORT_MAX_GAPS)
                .map(|i| {
                    singleton_gap(
                        sender,
                        epoch,
                        (i as u64).saturating_add(1),
                        DeliveryGapReason::LatestSuperseded,
                    )
                })
                .collect(),
            true,
        ),
    };
    (
        ServerMessage::DeliveryReport(Box::new(DeliveryReportPayload {
            gaps,
            per_class: counters_with_superseded(rng.next_u64().checked_rem(1000).unwrap_or(0)),
        })),
        FrameMeta {
            stamp: StampMode::None,
            bound_breaking,
        },
    )
}

fn relay_stats(_rng: &mut Rng, mode: u8) -> (ServerMessage, FrameMeta) {
    // Modes 1 (zero interval) and 2 (interval change plus saturated counters)
    // are bound-breaking hostile faces; modes 0 and 3 are valid heartbeats.
    let bound_breaking = matches!(mode, 1 | 2);
    let interval_ms = match mode {
        1 => 0,
        2 => 2000,
        _ => 1000,
    };
    let (sent, dropped, backpressure) = match mode {
        2 => (u64::MAX, u64::MAX, u64::MAX),
        3 => (5, 3, 1),
        _ => (10, 0, 0),
    };
    (
        ServerMessage::RelayStats {
            interval_ms,
            sent_to_you: sent,
            dropped_for_you: dropped,
            backpressure_events: backpressure,
        },
        FrameMeta {
            stamp: StampMode::None,
            bound_breaking,
        },
    )
}

const PAIRS: [(Topology, TransportKind); 4] = [
    (Topology::Relay, TransportKind::Relay),
    (Topology::Host, TransportKind::Direct),
    (Topology::Host, TransportKind::WebRtc),
    (Topology::Mesh, TransportKind::WebRtc),
];

fn session_plan(
    rng: &mut Rng,
    ctx: &Ctx,
    generation: Option<Uuid>,
    pair: usize,
    peers: &[PlayerId],
    zero_port: bool,
) -> ServerMessage {
    let slot = pair.checked_rem(PAIRS.len()).unwrap_or(0);
    let (topology, transport) = PAIRS
        .get(slot)
        .copied()
        .unwrap_or((Topology::Relay, TransportKind::Relay));
    let host = matches!(topology, Topology::Host).then(|| *peers.first().unwrap_or(&ctx.peer_a));
    let direct_endpoint = matches!(
        (topology, transport),
        (Topology::Host, TransportKind::Direct)
    )
    .then(|| DirectEndpoint {
        host: "203.0.113.7".to_string(),
        port: if zero_port { 0 } else { 27015 },
    });
    let ice = if matches!(transport, TransportKind::WebRtc) {
        vec![IceServer {
            urls: vec!["stun:stun.invalid:3478".to_string()],
            username: None,
            credential: None,
        }]
    } else {
        Vec::new()
    };
    ServerMessage::SessionPlan(Box::new(SessionPlanPayload {
        generation,
        topology,
        transport,
        host,
        direct_endpoint,
        peers: peers
            .iter()
            .map(|id| SessionPeer {
                player_id: *id,
                player_name: format!("peer-{id}"),
                is_authority: matches!(topology, Topology::Host) && Some(*id) == host,
                initiate: rng.chance(50),
            })
            .collect(),
        ice_servers: ice,
        fallback: TransportKind::Relay,
    }))
}

fn reconnected(
    rng: &mut Rng,
    ctx: &Ctx,
    roster: &[PlayerInfo],
    replay: Option<ReplayStatus>,
    watermarks: bool,
) -> ServerMessage {
    ServerMessage::Reconnected(Box::new(signal_fish_client::protocol::ReconnectedPayload {
        room_id: ctx.room_id,
        room_code: ctx.room_code.clone(),
        player_id: ctx.self_id,
        game_name: ctx.game_name.clone(),
        max_players: 8,
        supports_authority: true,
        current_players: roster.to_vec(),
        is_authority: rng.chance(30),
        lobby_state: LobbyState::Finalized,
        ready_players: vec![],
        relay_type: "websocket".to_string(),
        current_spectators: vec![],
        ice_servers: vec![],
        missed_events: vec![
            ServerMessage::Pong,
            ServerMessage::Error {
                message: "replayed".to_string(),
                error_code: None,
            },
        ],
        replay,
        sender_watermarks: if watermarks {
            roster
                .iter()
                .filter_map(|p| {
                    p.epoch.map(|e| SenderWatermark {
                        player_id: p.id,
                        epoch: e,
                        seq: p.seq.unwrap_or(0),
                    })
                })
                .collect()
        } else {
            Vec::new()
        },
        reconnection_token: Some(format!("rotated-{}", ctx.gen[1])),
    }))
}

fn spectator_joined(
    rng: &mut Rng,
    ctx: &Ctx,
    spectator_id: PlayerId,
    reason: Option<SpectatorStateChangeReason>,
) -> ServerMessage {
    ServerMessage::SpectatorJoined(Box::new(SpectatorJoinedPayload {
        room_id: ctx.room_id,
        room_code: ctx.room_code.clone(),
        spectator_id,
        game_name: ctx.game_name.clone(),
        current_players: vec![player_info(ctx, ctx.self_id, "self", None, None)],
        current_spectators: vec![spectator_info(spectator_id, "spec")],
        lobby_state: rng.pick(&[LobbyState::Waiting, LobbyState::Lobby]).clone(),
        reason,
    }))
}

fn error_msg(rng: &mut Rng) -> ServerMessage {
    let code = match rng.below(8) {
        0 => Some(ErrorCode::InternalError),
        1 => Some(ErrorCode::SlowConsumer),
        2 => Some(ErrorCode::RateLimitExceeded),
        3 => Some(ErrorCode::UnsupportedGameDataFormat),
        4 => Some(ErrorCode::ConnectionIdleTimeout),
        5 => None,
        6 => Some(ErrorCode::ServerDraining),
        _ => Some(ErrorCode::ActivityTimeout),
    };
    ServerMessage::Error {
        message: if rng.chance(10) {
            String::new()
        } else {
            "hostile error".to_string()
        },
        error_code: code,
    }
}

fn going_away(rng: &mut Rng) -> ServerMessage {
    ServerMessage::GoingAway {
        deadline_ms: rng.next_u64(),
        retry_after_secs: rng
            .chance(50)
            .then(|| rng.next_u64().checked_rem(3600).unwrap_or(0)),
    }
}

fn lobby_changed(rng: &mut Rng, ctx: &Ctx) -> ServerMessage {
    ServerMessage::LobbyStateChanged {
        lobby_state: rng
            .pick(&[
                LobbyState::Waiting,
                LobbyState::Lobby,
                LobbyState::Finalized,
            ])
            .clone(),
        ready_players: if rng.chance(50) {
            vec![ctx.peer_a]
        } else {
            Vec::new()
        },
        all_ready: rng.chance(50),
    }
}

fn game_starting(rng: &mut Rng, ctx: &Ctx) -> ServerMessage {
    ServerMessage::GameStarting {
        peer_connections: vec![PeerConnectionInfo {
            player_id: ctx.peer_a,
            player_name: "peer-a".to_string(),
            is_authority: true,
            relay_type: "websocket".to_string(),
            connection_info: if rng.chance(50) {
                Some(signal_fish_client::protocol::ConnectionInfo::Direct {
                    host: "203.0.113.9".to_string(),
                    port: 7777,
                })
            } else {
                None
            },
        }],
    }
}

fn authority_changed(rng: &mut Rng, ctx: &Ctx) -> ServerMessage {
    ServerMessage::AuthorityChanged {
        authority_player: if rng.chance(30) {
            None
        } else {
            Some(ctx.self_id)
        },
        you_are_authority: rng.chance(50),
    }
}

fn authority_response(rng: &mut Rng) -> ServerMessage {
    ServerMessage::AuthorityResponse {
        granted: rng.chance(50),
        reason: rng.chance(50).then(|| "hostile".to_string()),
        error_code: None,
    }
}

fn signal_from(
    _rng: &mut Rng,
    _ctx: &Ctx,
    from: PlayerId,
    generation: Option<Uuid>,
) -> ServerMessage {
    ServerMessage::Signal {
        from,
        generation,
        signal: serde_json::json!({ "Offer": format!("v=0 host {}", from) }),
    }
}

fn new_peer(rng: &mut Rng, ctx: &Ctx) -> ServerMessage {
    ServerMessage::NewPeer {
        peer_id: *rng.pick(&[ctx.peer_b, ctx.peer_c, ctx.unknown]),
        you_initiate: rng.chance(50),
    }
}

fn peer_transport_status(rng: &mut Rng, ctx: &Ctx) -> ServerMessage {
    ServerMessage::PeerTransportStatus {
        peer_id: *rng.pick(&[ctx.peer_a, ctx.peer_b]),
        transport: *rng.pick(&[
            TransportKind::Relay,
            TransportKind::Direct,
            TransportKind::WebRtc,
        ]),
        connected: rng.chance(50),
    }
}

fn gamedata_binary(
    rng: &mut Rng,
    ctx: &mut Ctx,
    from: PlayerId,
    mode: StampMode,
) -> (ServerMessage, FrameMeta) {
    let len = 1usize.saturating_add(rng.below(64));
    let payload: Vec<u8> = (0..len)
        .map(|_| rng.next_u64().checked_rem(256).unwrap_or(0) as u8)
        .collect();
    let (seq, epoch) = match mode {
        StampMode::Valid => {
            let s = ctx.next_seq(from);
            (Some(s), Some(ctx.epoch_of(from)))
        }
        StampMode::Stale => (Some(1), Some(1)),
        StampMode::Zero => (Some(0), Some(0)),
        StampMode::None => (None, None),
    };
    (
        ServerMessage::GameDataBinary {
            from_player: from,
            encoding: *rng.pick(&[GameDataEncoding::MessagePack, GameDataEncoding::Json]),
            payload,
            seq,
            epoch,
        },
        FrameMeta {
            stamp: mode,
            bound_breaking: false,
        },
    )
}

fn player_left(_rng: &mut Rng, who: PlayerId, mode: u8) -> (ServerMessage, FrameMeta) {
    let (epoch, final_seq) = match mode {
        0 => (Some(2), Some(u64::MAX)),
        1 => (Some(1), Some(0)),
        2 => (None, None),
        _ => (Some(0), Some(0)),
    };
    (
        ServerMessage::PlayerLeft {
            player_id: who,
            epoch,
            final_seq,
        },
        FrameMeta {
            stamp: StampMode::None,
            bound_breaking: false,
        },
    )
}

// ── Physical v3 MessagePack envelope (hand-built, no rmp dep) ────────

/// Hand-build a canonical protocol-v3 MessagePack binary envelope.
pub fn v3_binary_envelope(from: [u8; 16], payload: &[u8], seq: u64, epoch: u32) -> Vec<u8> {
    fn fixstr(out: &mut Vec<u8>, s: &str) {
        // Callers pass static keys shorter than 16 bytes.
        out.push(0xA0 | s.len() as u8);
        out.extend_from_slice(s.as_bytes());
    }
    fn bin8(out: &mut Vec<u8>, bytes: &[u8]) {
        out.push(0xC4);
        out.push(bytes.len() as u8);
        out.extend_from_slice(bytes);
    }
    fn uint(out: &mut Vec<u8>, v: u64) {
        if v < 128 {
            out.push(v as u8);
        } else if v <= u64::from(u8::MAX) {
            out.push(0xCC);
            out.push(v as u8);
        } else if v <= u64::from(u16::MAX) {
            out.push(0xCD);
            out.extend_from_slice(&(v as u16).to_be_bytes());
        } else if v <= u64::from(u32::MAX) {
            out.push(0xCE);
            out.extend_from_slice(&(v as u32).to_be_bytes());
        } else {
            out.push(0xCF);
            out.extend_from_slice(&v.to_be_bytes());
        }
    }
    let mut out = Vec::new();
    out.push(0x85); // fixmap, 5 fields
    fixstr(&mut out, "from_player");
    bin8(&mut out, &from);
    fixstr(&mut out, "encoding");
    fixstr(&mut out, "message_pack");
    fixstr(&mut out, "payload");
    bin8(&mut out, payload);
    fixstr(&mut out, "seq");
    uint(&mut out, seq);
    fixstr(&mut out, "epoch");
    uint(&mut out, u64::from(epoch));
    out
}

// ── Raw (schema-invalid) fixtures ───────────────────────────────────

pub const RAW_UNKNOWN_TYPE: &str = r#"{"type":"TotallyUnknown","data":{}}"#;
pub const RAW_UNKNOWN_ERROR_CODE: &str =
    r#"{"type":"Error","data":{"message":"m","error_code":"NOT_A_REAL_CODE_4242"}}"#;
pub const RAW_NOT_JSON: &str = "not json at all";
pub const RAW_BAD_SHAPE: &str = r#"{"type":"PlayerJoined","data":{"player":"nope"}}"#;
pub const RAW_NEGATIVE_U64: &str = r#"{"type":"RelayStats","data":{"interval_ms":-5,"sent_to_you":0,"dropped_for_you":0,"backpressure_events":0}}"#;
pub const RAW_OVERLIMIT_STRING: &str = r#"{"type":"Authenticated","data":{"app_name":1,"rate_limits":{"per_minute":1,"per_hour":2,"per_day":3}}}"#;

// ── Command menu ────────────────────────────────────────────────────

pub fn random_cmd(rng: &mut Rng, ctx: &Ctx) -> Cmd {
    match rng.below(17) {
        0 => Cmd::JoinRoom,
        1 => Cmd::JoinRoomMax(1usize.saturating_add(rng.below(8)) as u8),
        2 => Cmd::LeaveRoom,
        3 => Cmd::SendGameData(hostile_json_value(rng)),
        4 => Cmd::SendGameDataLatest(rng.next_u64() as u32),
        5 => Cmd::SendGameDataVolatile,
        6 => Cmd::SendBinaryGameData(1usize.saturating_add(rng.below(128))),
        7 => Cmd::SetReady,
        8 => Cmd::StartGame,
        9 => Cmd::RequestAuthority(rng.chance(50)),
        10 => Cmd::ProvideConnectionInfo,
        11 => Cmd::Reconnect(ctx.self_id, ctx.room_id),
        12 => Cmd::JoinAsSpectator,
        13 => Cmd::LeaveSpectator,
        14 => Cmd::Ping,
        15 if rng.chance(50) => Cmd::SendSignal,
        15 => Cmd::SendRawSignal,
        _ => Cmd::ReportTransportStatus,
    }
}

/// Sprinkle a hostile raw frame with low probability.
fn maybe_raw(rng: &mut Rng, steps: &mut Vec<Step>) {
    if rng.chance(4) {
        steps.push(Step::DeliverRaw(rng.pick(&[
            RAW_UNKNOWN_TYPE,
            RAW_UNKNOWN_ERROR_CODE,
            RAW_NOT_JSON,
            RAW_BAD_SHAPE,
            RAW_NEGATIVE_U64,
            RAW_OVERLIMIT_STRING,
        ])));
        steps.push(Step::Poll(1));
    }
}

/// One random command + poll with some probability.
fn maybe_cmd(rng: &mut Rng, ctx: &Ctx, steps: &mut Vec<Step>, percent: u64) {
    if rng.chance(percent) {
        steps.push(Step::Cmd(random_cmd(rng, ctx)));
        steps.push(Step::Poll(1));
    }
}

fn deliver(steps: &mut Vec<Step>, msg: ServerMessage, meta: FrameMeta) {
    steps.push(Step::Deliver(msg, meta));
    steps.push(Step::Poll(1));
}

/// Shared prologue: Authenticated → ProtocolInfo.
fn prologue(
    rng: &mut Rng,
    ctx: &Ctx,
    config: ConfigKind,
    echo_room_ops: bool,
    steps: &mut Vec<Step>,
) {
    prologue_with(rng, ctx, config, echo_room_ops, true, steps);
}

/// Prologue with controllable negotiation hostility: the send-pressure
/// archetype needs a completed negotiation, so it disables the
/// bound-breaking face.
fn prologue_with(
    rng: &mut Rng,
    ctx: &Ctx,
    config: ConfigKind,
    echo_room_ops: bool,
    allow_bound_breaking: bool,
    steps: &mut Vec<Step>,
) {
    deliver(steps, authenticated(rng), FrameMeta::default());
    let (info, meta) = protocol_info(rng, ctx, config, echo_room_ops, allow_bound_breaking);
    deliver(steps, info, meta);
}

/// Join flow matching the negotiated envelope mode.
fn join_flow(rng: &mut Rng, ctx: &Ctx, echo_room_ops: bool, steps: &mut Vec<Step>) {
    if echo_room_ops && rng.chance(80) {
        steps.push(Step::Cmd(Cmd::JoinRoom));
        steps.push(Step::Poll(1));
        let kind = if rng.chance(85) {
            EchoKind::JoinOk
        } else {
            EchoKind::JoinFailed
        };
        let id = if rng.chance(85) {
            EchoId::Match
        } else {
            EchoId::Wrong
        };
        steps.push(Step::DeliverEcho(kind, id));
        steps.push(Step::Poll(1));
    } else {
        steps.push(Step::Cmd(Cmd::JoinRoom));
        steps.push(Step::Poll(1));
        deliver(
            steps,
            room_joined(rng, ctx, &[player_info_placeholder(ctx)], 8, true),
            FrameMeta::default(),
        );
    }
}

fn player_info_placeholder(ctx: &Ctx) -> PlayerInfo {
    player_info(ctx, ctx.self_id, "self", Some(1), Some(0))
}

// ── Archetypes ──────────────────────────────────────────────────────

fn arch_journey(
    rng: &mut Rng,
    ctx: &mut Ctx,
    config: ConfigKind,
    echo_room_ops: bool,
) -> Vec<Step> {
    let mut steps = Vec::new();
    let v3 = config.is_v3();
    prologue(rng, ctx, config, echo_room_ops, &mut steps);
    join_flow(rng, ctx, echo_room_ops, &mut steps);

    // Roster churn.
    for peer in [ctx.peer_a, ctx.peer_b] {
        deliver(
            &mut steps,
            ServerMessage::PlayerJoined {
                player: player_info(ctx, peer, "peer", Some(1), Some(0)),
            },
            FrameMeta::default(),
        );
    }
    deliver(&mut steps, lobby_changed(rng, ctx), FrameMeta::default());
    if rng.chance(50) {
        deliver(
            &mut steps,
            authority_changed(rng, ctx),
            FrameMeta::default(),
        );
    }
    if !v3 {
        // The v2 stampless game-data face (its stamped variants are the
        // hostile UnexpectedMetadata face exercised by the accountability
        // archetype's v2 branch).
        let (msg, meta) = game_data(rng, ctx, ctx.peer_a, StampMode::None);
        deliver(&mut steps, msg, meta);
    }

    // Readiness + start.
    steps.push(Step::Cmd(Cmd::SetReady));
    steps.push(Step::Poll(1));
    deliver(&mut steps, lobby_changed(rng, ctx), FrameMeta::default());
    steps.push(Step::Cmd(Cmd::StartGame));
    steps.push(Step::Poll(1));
    deliver(&mut steps, game_starting(rng, ctx), FrameMeta::default());

    if v3 {
        // Authoritative plan churn: fresh gen, duplicate, replay.
        let g0 = ctx.gen_uuid(0);
        deliver(
            &mut steps,
            session_plan(rng, ctx, Some(g0), 0, &[ctx.peer_a, ctx.peer_b], false),
            FrameMeta::default(),
        );
        deliver(
            &mut steps,
            session_plan(rng, ctx, Some(g0), 0, &[ctx.peer_a, ctx.peer_b], false),
            FrameMeta::default(),
        );
        let g1 = ctx.gen_uuid(1);
        deliver(
            &mut steps,
            session_plan(rng, ctx, Some(g1), 3, &[ctx.peer_a, ctx.peer_b], false),
            FrameMeta::default(),
        );
        deliver(
            &mut steps,
            signal_from(rng, ctx, ctx.peer_a, Some(g1)),
            FrameMeta::default(),
        );
        deliver(
            &mut steps,
            signal_from(rng, ctx, ctx.peer_a, Some(g0)),
            FrameMeta::default(),
        );
        deliver(
            &mut steps,
            signal_from(rng, ctx, ctx.self_id, Some(g1)),
            FrameMeta::default(),
        );
        deliver(
            &mut steps,
            signal_from(rng, ctx, ctx.unknown, None),
            FrameMeta::default(),
        );
        deliver(&mut steps, new_peer(rng, ctx), FrameMeta::default());
        deliver(
            &mut steps,
            peer_transport_status(rng, ctx),
            FrameMeta::default(),
        );
        // Stamped game data + reports + relay stats.
        for _ in 0..4usize.saturating_add(rng.below(8)) {
            let (msg, meta) = game_data(rng, ctx, ctx.peer_a, StampMode::Valid);
            deliver(&mut steps, msg, meta);
            if rng.chance(30) {
                let (stale, stale_meta) = game_data(rng, ctx, ctx.peer_a, StampMode::Stale);
                deliver(&mut steps, stale, stale_meta);
            }
        }
        // Physically valid v3 binary envelopes (real binary-frame face).
        for _ in 0..2usize.saturating_add(rng.below(3)) {
            let seq = ctx.next_seq(ctx.peer_a);
            let env = v3_binary_envelope(ctx.peer_a.into_bytes(), b"hostile-payload", seq, 1);
            steps.push(Step::DeliverBinary(
                env,
                FrameMeta {
                    stamp: StampMode::Valid,
                    bound_breaking: false,
                },
            ));
            steps.push(Step::Poll(1));
        }
        // Text-delivered GameDataBinary frames (representation mismatch by design).
        let (gb1, gb1_meta) = gamedata_binary(rng, ctx, ctx.peer_a, StampMode::Valid);
        deliver(&mut steps, gb1, gb1_meta);
        let (gb2, gb2_meta) = gamedata_binary(rng, ctx, ctx.peer_a, StampMode::Zero);
        deliver(&mut steps, gb2, gb2_meta);
        deliver(
            &mut steps,
            consistent_report(ctx, ctx.peer_a, 1),
            FrameMeta::default(),
        );
        let (rs0, rs0_meta) = relay_stats(rng, 0);
        deliver(&mut steps, rs0, rs0_meta);
        let (rs3, rs3_meta) = relay_stats(rng, 3);
        deliver(&mut steps, rs3, rs3_meta);
        if rng.chance(30) {
            let (rs2, rs2_meta) = relay_stats(rng, 2);
            deliver(&mut steps, rs2, rs2_meta);
        }
        deliver(&mut steps, going_away(rng), FrameMeta::default());
        // Server shutdown advisory is best-effort; the structured close follows.
        steps.push(Step::PeerClose);
        steps.push(Step::Poll(2));
    }

    maybe_cmd(rng, ctx, &mut steps, 40);
    maybe_raw(rng, &mut steps);

    // Reconnect journey tail: leave, reconnect echo, then plan refresh.
    deliver(&mut steps, ServerMessage::RoomLeft, FrameMeta::default());
    steps.push(Step::Cmd(Cmd::Reconnect(ctx.self_id, ctx.room_id)));
    steps.push(Step::Poll(1));
    if echo_room_ops && rng.chance(60) {
        let kind = if rng.chance(70) {
            EchoKind::ReconnectOk
        } else {
            EchoKind::ReconnectFailed
        };
        steps.push(Step::DeliverEcho(kind, EchoId::Match));
        steps.push(Step::Poll(1));
    } else {
        deliver(
            &mut steps,
            reconnected(
                rng,
                ctx,
                &[
                    player_info_placeholder(ctx),
                    player_info(ctx, ctx.peer_a, "a", Some(1), Some(0)),
                ],
                Some(ReplayStatus::Complete),
                v3,
            ),
            FrameMeta::default(),
        );
        if v3 {
            deliver(
                &mut steps,
                session_plan(rng, ctx, Some(ctx.gen_uuid(2)), 0, &[ctx.peer_a], false),
                FrameMeta::default(),
            );
        }
    }

    deliver(&mut steps, ServerMessage::Pong, FrameMeta::default());
    maybe_cmd(rng, ctx, &mut steps, 30);
    steps.push(Step::Close);
    steps
}

fn arch_roster_storm(
    rng: &mut Rng,
    ctx: &mut Ctx,
    config: ConfigKind,
    echo_room_ops: bool,
) -> Vec<Step> {
    let mut steps = Vec::new();
    let v3 = config.is_v3();
    prologue(rng, ctx, config, echo_room_ops, &mut steps);
    join_flow(rng, ctx, echo_room_ops, &mut steps);

    // Roster overflow: baseline self + several beyond max_players=2.
    for i in 0..8u32 {
        deliver(
            &mut steps,
            ServerMessage::PlayerJoined {
                player: player_info(
                    ctx,
                    Uuid::from_u128(u128::from(0xA000_u32).wrapping_add(u128::from(i))),
                    "storm",
                    Some(1),
                    Some(0),
                ),
            },
            // Beyond-capacity inserts are bound-breaking for i > small values;
            // the oracle treats roster inserts as validity-ambiguous anyway.
            FrameMeta::default(),
        );
        maybe_cmd(rng, ctx, &mut steps, 8);
    }
    // Reconnect-epoch announcement flood for one sender (>16 distinct).
    let sender = ctx.peer_a;
    for epoch in 2..=24u32 {
        deliver(
            &mut steps,
            ServerMessage::PlayerReconnected {
                player_id: sender,
                epoch: Some(epoch),
            },
            FrameMeta::default(),
        );
    }
    // Departure churn (issue #166 uncoverable-departure flood).
    if v3 {
        for epoch in 2..=20u32 {
            deliver(
                &mut steps,
                ServerMessage::PlayerLeft {
                    player_id: sender,
                    epoch: Some(epoch),
                    final_seq: Some(u64::MAX),
                },
                FrameMeta::default(),
            );
        }
    } else {
        for _ in 0..12 {
            let (msg, meta) = player_left(rng, sender, 2);
            deliver(&mut steps, msg, meta);
        }
    }
    // Unknown-player reconnect flood (trusted-server envelope, still bounded).
    for epoch in 1..=24u32 {
        deliver(
            &mut steps,
            ServerMessage::PlayerReconnected {
                player_id: ctx.unknown,
                epoch: Some(epoch),
            },
            FrameMeta::default(),
        );
    }
    maybe_raw(rng, &mut steps);
    if rng.chance(50) {
        steps.push(Step::PeerClose);
        steps.push(Step::Poll(2));
    } else {
        steps.push(Step::Close);
    }
    steps
}

fn arch_accountability(
    rng: &mut Rng,
    ctx: &mut Ctx,
    config: ConfigKind,
    echo_room_ops: bool,
) -> Vec<Step> {
    let mut steps = Vec::new();
    let v3 = config.is_v3();
    prologue(rng, ctx, config, echo_room_ops, &mut steps);
    join_flow(rng, ctx, echo_room_ops, &mut steps);
    deliver(
        &mut steps,
        ServerMessage::PlayerJoined {
            player: player_info(ctx, ctx.peer_a, "a", Some(1), Some(0)),
        },
        FrameMeta::default(),
    );

    if v3 {
        // Stamp chaos.
        for i in 0..12usize.saturating_add(rng.below(12)) {
            let mode = match i.checked_rem(5).unwrap_or(0) {
                0 => StampMode::Valid,
                1 => StampMode::Stale,
                2 => StampMode::Zero,
                3 => StampMode::None,
                _ => StampMode::Valid,
            };
            let (msg, meta) = game_data(rng, ctx, ctx.peer_a, mode);
            deliver(&mut steps, msg, meta);
        }
        // Physically valid v3 binary envelopes with mixed stamps.
        for i in 0..4u64 {
            let seq = ctx.next_seq(ctx.peer_a);
            let epoch = if i.checked_rem(2).unwrap_or(1) == 0 {
                1
            } else {
                9
            };
            let env = v3_binary_envelope(
                ctx.peer_a.into_bytes(),
                format!("payload-{i}").as_bytes(),
                if i == 3 { 1 } else { seq },
                epoch,
            );
            let replayed = i == 3;
            steps.push(Step::DeliverBinary(
                env,
                FrameMeta {
                    stamp: if replayed {
                        StampMode::Stale
                    } else {
                        StampMode::Valid
                    },
                    bound_breaking: false,
                },
            ));
            steps.push(Step::Poll(1));
        }
        // Text-delivered GameDataBinary (representation mismatch by design).
        for _ in 0..4 {
            let mode = if rng.chance(50) {
                StampMode::Valid
            } else {
                StampMode::Zero
            };
            let (msg, meta) = gamedata_binary(rng, ctx, ctx.peer_a, mode);
            deliver(&mut steps, msg, meta);
        }
        // Six consistent 256-gap batches saturate the 1024 retention bound
        // (4 batches would not reach it; the sixth forces the eviction face).
        for batch in 1..=6u64 {
            deliver(
                &mut steps,
                consistent_report(ctx, ctx.peer_a, batch),
                FrameMeta::default(),
            );
            maybe_cmd(rng, ctx, &mut steps, 5);
        }
        // Hostile report zoo.
        for _ in 0..6usize.saturating_add(rng.below(6)) {
            let (msg, meta) = hostile_report(rng, ctx);
            deliver(&mut steps, msg, meta);
        }
        // Unsupported-format advisory causality.
        deliver(&mut steps, error_msg(rng), FrameMeta::default());
        deliver(&mut steps, error_msg(rng), FrameMeta::default());
        deliver(
            &mut steps,
            consistent_report(ctx, ctx.peer_a, 7),
            FrameMeta::default(),
        );
        deliver(&mut steps, error_msg(rng), FrameMeta::default());
        // RelayStats invariant attacks.
        let (rs0, rs0_meta) = relay_stats(rng, 0);
        deliver(&mut steps, rs0, rs0_meta);
        let (rs2, rs2_meta) = relay_stats(rng, 2);
        deliver(&mut steps, rs2, rs2_meta);
        let (rs3, rs3_meta) = relay_stats(rng, 3);
        deliver(&mut steps, rs3, rs3_meta);
        let (rs1, rs1_meta) = relay_stats(rng, 1);
        deliver(&mut steps, rs1, rs1_meta);
    } else {
        // v2 exposure of v3-only metadata.
        for _ in 0..6 {
            let (msg, meta) = player_left(rng, ctx.peer_a, 0);
            deliver(&mut steps, msg, meta);
        }
        let (rs0, rs0_meta) = relay_stats(rng, 0);
        deliver(&mut steps, rs0, rs0_meta);
        deliver(
            &mut steps,
            consistent_report(ctx, ctx.peer_a, 1),
            FrameMeta::default(),
        );
    }
    maybe_raw(rng, &mut steps);
    steps.push(Step::Close);
    steps
}

fn arch_spectator_churn(
    rng: &mut Rng,
    ctx: &mut Ctx,
    config: ConfigKind,
    echo_room_ops: bool,
) -> Vec<Step> {
    let mut steps = Vec::new();
    prologue(rng, ctx, config, echo_room_ops, &mut steps);
    steps.push(Step::Cmd(Cmd::JoinAsSpectator));
    steps.push(Step::Poll(1));
    if echo_room_ops && rng.chance(70) {
        let kind = if rng.chance(80) {
            EchoKind::SpectatorJoinOk
        } else {
            EchoKind::SpectatorJoinFailed
        };
        let id = if rng.chance(85) {
            EchoId::Match
        } else {
            EchoId::Wrong
        };
        steps.push(Step::DeliverEcho(kind, id));
        steps.push(Step::Poll(1));
    } else {
        deliver(
            &mut steps,
            spectator_joined(rng, ctx, ctx.spectator_a, None),
            FrameMeta::default(),
        );
    }

    // Spectator churn storms.
    for i in 0..20usize.saturating_add(rng.below(20)) {
        match rng.below(5) {
            0 => deliver(
                &mut steps,
                ServerMessage::NewSpectatorJoined {
                    spectator: spectator_info(
                        Uuid::from_u128(u128::from(0xB000_u32).wrapping_add(u128::from(i as u32))),
                        "spec",
                    ),
                    current_spectators: vec![],
                    reason: Some(SpectatorStateChangeReason::Joined),
                },
                FrameMeta::default(),
            ),
            1 => deliver(
                &mut steps,
                ServerMessage::SpectatorDisconnected {
                    spectator_id: Uuid::from_u128(
                        u128::from(0xB000_u32).wrapping_add(u128::from(i as u32)),
                    ),
                    reason: Some(SpectatorStateChangeReason::Disconnected),
                    current_spectators: vec![],
                },
                FrameMeta::default(),
            ),
            2 => deliver(
                &mut steps,
                ServerMessage::SpectatorJoinFailed {
                    reason: "full".to_string(),
                    error_code: Some(ErrorCode::TooManySpectators),
                },
                FrameMeta::default(),
            ),
            3 => deliver(
                &mut steps,
                spectator_joined(
                    rng,
                    ctx,
                    ctx.spectator_a,
                    Some(SpectatorStateChangeReason::Joined),
                ),
                FrameMeta::default(),
            ),
            _ => {
                steps.push(Step::Cmd(Cmd::Ping));
                steps.push(Step::Poll(1));
                deliver(&mut steps, ServerMessage::Pong, FrameMeta::default());
            }
        }
    }
    // Authoritative exit then a late voluntary-leave echo (the absorbed-race face).
    deliver(
        &mut steps,
        ServerMessage::SpectatorLeft {
            room_id: Some(ctx.room_id),
            room_code: Some(ctx.room_code.clone()),
            reason: Some(SpectatorStateChangeReason::Removed),
            current_spectators: vec![],
        },
        FrameMeta::default(),
    );
    steps.push(Step::Cmd(Cmd::LeaveSpectator));
    steps.push(Step::Poll(1));
    let late_id = if rng.chance(50) {
        EchoId::Match
    } else {
        EchoId::Wrong
    };
    steps.push(Step::DeliverEcho(EchoKind::SpectatorLeaveOk, late_id));
    steps.push(Step::Poll(1));
    // Rejoin as spectator, then authoritative room-closed exit.
    steps.push(Step::Cmd(Cmd::JoinAsSpectator));
    steps.push(Step::Poll(1));
    deliver(
        &mut steps,
        spectator_joined(
            rng,
            ctx,
            ctx.spectator_a,
            Some(SpectatorStateChangeReason::Joined),
        ),
        FrameMeta::default(),
    );
    deliver(
        &mut steps,
        ServerMessage::SpectatorLeft {
            room_id: Some(ctx.room_id),
            room_code: Some(ctx.room_code.clone()),
            reason: Some(SpectatorStateChangeReason::RoomClosed),
            current_spectators: vec![],
        },
        FrameMeta::default(),
    );
    maybe_raw(rng, &mut steps);
    steps.push(Step::Close);
    steps
}

fn arch_plan_churn(
    rng: &mut Rng,
    ctx: &mut Ctx,
    config: ConfigKind,
    echo_room_ops: bool,
) -> Vec<Step> {
    let mut steps = Vec::new();
    let v3 = config.is_v3();
    prologue(rng, ctx, config, echo_room_ops, &mut steps);
    join_flow(rng, ctx, echo_room_ops, &mut steps);
    deliver(
        &mut steps,
        ServerMessage::PlayerJoined {
            player: player_info(ctx, ctx.peer_a, "a", Some(1), Some(0)),
        },
        FrameMeta::default(),
    );
    steps.push(Step::Cmd(Cmd::SetReady));
    steps.push(Step::Poll(1));
    steps.push(Step::Cmd(Cmd::StartGame));
    steps.push(Step::Poll(1));
    deliver(&mut steps, game_starting(rng, ctx), FrameMeta::default());

    if v3 {
        // All four topology/transport pairs with fresh generations.
        for pair in 0..4usize {
            deliver(
                &mut steps,
                session_plan(
                    rng,
                    ctx,
                    Some(ctx.gen_uuid(pair)),
                    pair,
                    &[ctx.peer_a],
                    false,
                ),
                FrameMeta::default(),
            );
            deliver(
                &mut steps,
                signal_from(rng, ctx, ctx.peer_a, Some(ctx.gen_uuid(pair))),
                FrameMeta::default(),
            );
            maybe_cmd(rng, ctx, &mut steps, 15);
        }
        // Generation-less legacy plan.
        deliver(
            &mut steps,
            session_plan(rng, ctx, None, 0, &[ctx.peer_a], false),
            FrameMeta::default(),
        );
        deliver(
            &mut steps,
            signal_from(rng, ctx, ctx.peer_a, None),
            FrameMeta::default(),
        );
        // Unknown generation.
        deliver(
            &mut steps,
            session_plan(
                rng,
                ctx,
                Some(Uuid::from_u128(0xDEAD_BEEF)),
                3,
                &[ctx.peer_a, ctx.peer_b],
                false,
            ),
            FrameMeta::default(),
        );
        // Replay the fenced generation (older than the current one).
        deliver(
            &mut steps,
            session_plan(rng, ctx, Some(ctx.gen_uuid(0)), 0, &[ctx.peer_a], false),
            FrameMeta::default(),
        );
        // Zero-port direct endpoint (hostile lifecycle).
        deliver(
            &mut steps,
            session_plan(rng, ctx, Some(ctx.gen_uuid(5)), 1, &[ctx.peer_a], true),
            FrameMeta::default(),
        );
        // Plan naming an absent host + empty peers.
        deliver(
            &mut steps,
            session_plan(rng, ctx, Some(ctx.gen_uuid(4)), 3, &[], false),
            FrameMeta::default(),
        );
        // Signals from self / unknown / stale.
        deliver(
            &mut steps,
            signal_from(rng, ctx, ctx.self_id, Some(ctx.gen_uuid(4))),
            FrameMeta::default(),
        );
        deliver(
            &mut steps,
            signal_from(rng, ctx, ctx.unknown, Some(ctx.gen_uuid(4))),
            FrameMeta::default(),
        );
        deliver(
            &mut steps,
            signal_from(rng, ctx, ctx.peer_a, Some(ctx.gen_uuid(0))),
            FrameMeta::default(),
        );
        deliver(&mut steps, new_peer(rng, ctx), FrameMeta::default());
        deliver(
            &mut steps,
            peer_transport_status(rng, ctx),
            FrameMeta::default(),
        );
    }
    maybe_raw(rng, &mut steps);
    steps.push(Step::Close);
    steps
}

fn arch_raw_frames(
    rng: &mut Rng,
    ctx: &mut Ctx,
    config: ConfigKind,
    echo_room_ops: bool,
) -> Vec<Step> {
    let mut steps = Vec::new();
    prologue(rng, ctx, config, echo_room_ops, &mut steps);
    for _ in 0..24usize.saturating_add(rng.below(24)) {
        maybe_raw(rng, &mut steps);
        match rng.below(6) {
            0 => deliver(&mut steps, ServerMessage::Pong, FrameMeta::default()),
            1 => deliver(&mut steps, error_msg(rng), FrameMeta::default()),
            2 => deliver(
                &mut steps,
                room_joined(rng, ctx, &[player_info_placeholder(ctx)], 4, false),
                FrameMeta::default(),
            ),
            3 => deliver(
                &mut steps,
                ServerMessage::RoomJoinFailed {
                    reason: "hostile".to_string(),
                    error_code: Some(ErrorCode::RoomNotFound),
                },
                FrameMeta::default(),
            ),
            4 => deliver(
                &mut steps,
                ServerMessage::AuthenticationError {
                    error: "hostile".to_string(),
                    error_code: ErrorCode::Unauthorized,
                },
                FrameMeta::default(),
            ),
            _ => deliver(&mut steps, authority_response(rng), FrameMeta::default()),
        }
        maybe_cmd(rng, ctx, &mut steps, 25);
    }
    steps.push(Step::Close);
    steps
}

fn arch_command_storm(
    rng: &mut Rng,
    ctx: &mut Ctx,
    config: ConfigKind,
    echo_room_ops: bool,
) -> Vec<Step> {
    let mut steps = Vec::new();
    prologue(rng, ctx, config, echo_room_ops, &mut steps);
    if rng.chance(70) {
        join_flow(rng, ctx, echo_room_ops, &mut steps);
    }
    for _ in 0..60usize.saturating_add(rng.below(80)) {
        steps.push(Step::Cmd(random_cmd(rng, ctx)));
        steps.push(Step::Poll(1));
        if rng.chance(25) {
            let candidates = vec![
                ServerMessage::Pong,
                error_msg(rng),
                lobby_changed(rng, ctx),
                authority_response(rng),
                authority_changed(rng, ctx),
                ServerMessage::RoomLeft,
                ServerMessage::RoomJoinFailed {
                    reason: "x".into(),
                    error_code: None,
                },
                ServerMessage::SpectatorJoinFailed {
                    reason: "x".into(),
                    error_code: None,
                },
                ServerMessage::ReconnectionFailed {
                    reason: "x".into(),
                    error_code: ErrorCode::ReconnectionTokenInvalid,
                },
                going_away(rng),
            ];
            let pick = rng.below(candidates.len());
            let chosen = candidates
                .into_iter()
                .nth(pick)
                .unwrap_or(ServerMessage::Pong);
            deliver(&mut steps, chosen, FrameMeta::default());
        }
        if rng.chance(10) {
            if rng.below(2) == 0 {
                deliver(
                    &mut steps,
                    spectator_joined(rng, ctx, ctx.spectator_a, None),
                    FrameMeta::default(),
                );
            } else {
                deliver(
                    &mut steps,
                    reconnected(rng, ctx, &[player_info_placeholder(ctx)], None, false),
                    FrameMeta::default(),
                );
            }
        }
    }
    steps.push(Step::Close);
    steps
}

fn arch_echo_zoo(
    rng: &mut Rng,
    ctx: &mut Ctx,
    config: ConfigKind,
    echo_room_ops: bool,
) -> Vec<Step> {
    let mut steps = Vec::new();
    prologue(rng, ctx, config, echo_room_ops, &mut steps);
    // Cycle through every echo kind with both id choices, issuing the matching
    // command first where the state machine plausibly allows it.
    let cycle: [(Cmd, EchoKind); 9] = [
        (Cmd::JoinRoom, EchoKind::JoinOk),
        (Cmd::JoinRoom, EchoKind::JoinFailed),
        (Cmd::LeaveRoom, EchoKind::LeaveOk),
        (
            Cmd::Reconnect(ctx.self_id, ctx.room_id),
            EchoKind::ReconnectOk,
        ),
        (
            Cmd::Reconnect(ctx.self_id, ctx.room_id),
            EchoKind::ReconnectFailed,
        ),
        (Cmd::JoinAsSpectator, EchoKind::SpectatorJoinOk),
        (Cmd::JoinAsSpectator, EchoKind::SpectatorJoinFailed),
        (Cmd::LeaveSpectator, EchoKind::SpectatorLeaveOk),
        (Cmd::JoinRoom, EchoKind::OperationFailed),
    ];
    for (cmd, kind) in cycle {
        steps.push(Step::Cmd(cmd));
        steps.push(Step::Poll(1));
        let id = if rng.chance(70) {
            EchoId::Match
        } else {
            EchoId::Wrong
        };
        steps.push(Step::DeliverEcho(kind, id));
        steps.push(Step::Poll(2));
        maybe_cmd(rng, ctx, &mut steps, 20);
    }
    // Correlated results without any pending operation (lifecycle offenders).
    for _ in 0..4 {
        steps.push(Step::DeliverEcho(EchoKind::OperationFailed, EchoId::Wrong));
        steps.push(Step::Poll(1));
    }
    // Duplicate current answer.
    steps.push(Step::Cmd(Cmd::Ping));
    steps.push(Step::Poll(1));
    deliver(&mut steps, ServerMessage::Pong, FrameMeta::default());
    maybe_raw(rng, &mut steps);
    steps.push(Step::Close);
    steps
}

/// Terminal-transport-error archetype: a random prefix of normal frames, then
/// the transport turns hostile with a terminal `poll_recv` error (and sometimes
/// a terminal `poll_send` error). Asserts the documented terminal behavior:
/// transition to terminal state, exact `Disconnected` cause, refusal of later
/// commands, idempotent `close()`, no hang.
fn arch_transport_kill(
    rng: &mut Rng,
    ctx: &mut Ctx,
    config: ConfigKind,
    echo_room_ops: bool,
) -> Vec<Step> {
    let mut steps = Vec::new();
    prologue(rng, ctx, config, echo_room_ops, &mut steps);
    // Normal, phase-legal frames regardless of membership (post-auth menu).
    for _ in 0..3usize.saturating_add(rng.below(6)) {
        match rng.below(4) {
            0 => deliver(&mut steps, ServerMessage::Pong, FrameMeta::default()),
            1 => deliver(&mut steps, error_msg(rng), FrameMeta::default()),
            2 => deliver(&mut steps, authority_response(rng), FrameMeta::default()),
            _ => {
                let (msg, meta) = relay_stats(rng, 0);
                deliver(&mut steps, msg, meta)
            }
        }
        maybe_cmd(rng, ctx, &mut steps, 10);
    }
    // The kill: terminal recv error always; terminal send error sometimes.
    let (fail_recv, fail_send) = match rng.below(3) {
        0 => (true, false),
        1 => (false, true),
        _ => (true, true),
    };
    steps.push(Step::TransportKill {
        fail_recv,
        fail_send,
    });
    steps.push(Step::Poll(3));
    // Post-terminal commands must be refused, never hang, never emit events.
    steps.push(Step::Cmd(Cmd::Ping));
    steps.push(Step::Cmd(Cmd::SendGameData(
        serde_json::json!({ "post": "terminal" }),
    )));
    steps.push(Step::Cmd(Cmd::JoinRoom));
    steps.push(Step::Poll(2));
    // close() must be idempotent and silent after a terminal transport error.
    steps.push(Step::Close);
    steps
}

/// Send-side budget/pacing archetype (issue #219): fill a small command queue,
/// arm the transport's `Pending`-refusal face, then drain — asserting FIFO
/// delivery, capacity accounting, and the send ledger.
fn arch_send_pressure(rng: &mut Rng, ctx: &mut Ctx, config: ConfigKind) -> Vec<Step> {
    let mut steps = Vec::new();
    // Correlation stays off, the baseline omits the v3 reconnection token,
    // and negotiation stays valid, so the join completes under every
    // configuration; the archetype targets the send path.
    prologue_with(rng, ctx, config, false, false, &mut steps);
    steps.push(Step::Cmd(Cmd::JoinRoom));
    steps.push(Step::Poll(1));
    // v3 baselines carry paired stamps; v2 snapshots must omit them. The
    // local authority flag must agree with the roster entry so the join
    // always completes — the archetype targets the send path.
    let roster = vec![if config.is_v3() {
        player_info_placeholder(ctx)
    } else {
        player_info(ctx, ctx.self_id, "self", None, None)
    }];
    let baseline = room_joined(rng, ctx, &roster, 8, false);
    let baseline = match baseline {
        ServerMessage::RoomJoined(mut payload) => {
            payload.is_authority = false;
            if let Some(local) = payload
                .current_players
                .iter_mut()
                .find(|player| player.id == payload.player_id)
            {
                local.is_authority = false;
            }
            ServerMessage::RoomJoined(payload)
        }
        other => other,
    };
    deliver(&mut steps, baseline, FrameMeta::default());
    // Fill the (small) command queue past capacity: later sends must be
    // refused gracefully while earlier ones stay queued in order.
    for k in 0..96u32 {
        steps.push(Step::Cmd(Cmd::SendGameData(
            serde_json::json!({ "marker": k }),
        )));
    }
    // Arm the send-delay face: every frame is refused with `Pending` twice
    // before acceptance, so flushing takes multiple polls per frame.
    steps.push(Step::SetSendDelay(2));
    // Interleave drains with more sends (capacity frees up mid-drain).
    for wave in 0..3u32 {
        steps.push(Step::Poll(64));
        for k in 0..8u32 {
            let marker = 1000u32.wrapping_add(wave.wrapping_mul(100)).wrapping_add(k);
            steps.push(Step::Cmd(Cmd::SendGameData(
                serde_json::json!({ "marker": marker }),
            )));
            if rng.chance(20) {
                steps.push(Step::Cmd(Cmd::SendBinaryGameData(
                    1usize.saturating_add(rng.below(64)),
                )));
            }
        }
    }
    // Full drain: everything accepted must reach the outbound log exactly once.
    steps.push(Step::Poll(1024));
    deliver(&mut steps, ServerMessage::Pong, FrameMeta::default());
    deliver(&mut steps, ServerMessage::Pong, FrameMeta::default());
    steps.push(Step::Close);
    steps
}

pub fn generate_script(seed: u64, index: usize) -> Script {
    let mut rng =
        Rng::new(seed.rotate_left(17) ^ (index as u64).wrapping_mul(0x517C_C1B7_2722_0A95));
    let ctx_seed = rng.next_u64();
    let mut ctx = Ctx::new(&mut Rng::new(ctx_seed));
    let archetype_index = index % 12;
    let (archetype, config_kind, echo_room_ops) = match archetype_index {
        0 | 1 => ("journey", pick_config(&mut rng), rng.chance(75)),
        2 | 3 => ("roster_storm", pick_config(&mut rng), rng.chance(40)),
        4 => ("accountability", pick_config(&mut rng), rng.chance(40)),
        5 => ("spectator_churn", pick_config(&mut rng), rng.chance(70)),
        6 => ("plan_churn", ConfigKind::V3, rng.chance(70)),
        7 => ("raw_frames", pick_config(&mut rng), rng.chance(50)),
        8 => ("command_storm", pick_config(&mut rng), rng.chance(50)),
        9 => ("echo_zoo", pick_config(&mut rng), rng.chance(80)),
        10 => ("transport_kill", pick_config(&mut rng), rng.chance(60)),
        _ => ("send_pressure", pick_config(&mut rng), false),
    };
    let (steps, small_command_capacity) = match archetype {
        "journey" => (
            arch_journey(&mut rng, &mut ctx, config_kind, echo_room_ops),
            None,
        ),
        "roster_storm" => (
            arch_roster_storm(&mut rng, &mut ctx, config_kind, echo_room_ops),
            None,
        ),
        "accountability" => (
            arch_accountability(&mut rng, &mut ctx, config_kind, echo_room_ops),
            None,
        ),
        "spectator_churn" => (
            arch_spectator_churn(&mut rng, &mut ctx, config_kind, echo_room_ops),
            None,
        ),
        "plan_churn" => (
            arch_plan_churn(&mut rng, &mut ctx, config_kind, echo_room_ops),
            None,
        ),
        "raw_frames" => (
            arch_raw_frames(&mut rng, &mut ctx, config_kind, echo_room_ops),
            None,
        ),
        "command_storm" => (
            arch_command_storm(&mut rng, &mut ctx, config_kind, echo_room_ops),
            None,
        ),
        "echo_zoo" => (
            arch_echo_zoo(&mut rng, &mut ctx, config_kind, echo_room_ops),
            None,
        ),
        "transport_kill" => (
            arch_transport_kill(&mut rng, &mut ctx, config_kind, echo_room_ops),
            None,
        ),
        _ => (
            arch_send_pressure(&mut rng, &mut ctx, config_kind),
            Some(64usize),
        ),
    };
    Script {
        seed,
        index,
        archetype,
        config_kind,
        echo_room_ops,
        small_command_capacity,
        steps,
    }
}

fn pick_config(rng: &mut Rng) -> ConfigKind {
    match rng.below(8) {
        0..=3 => ConfigKind::V3,
        4 => ConfigKind::V3VersionOnly,
        5 | 6 => ConfigKind::V2,
        _ => ConfigKind::V2Explicit,
    }
}

// ── Canary fixtures (deterministic, for oracle self-tests) ──────────

impl Ctx {
    pub fn new_for_canary() -> Ctx {
        let mut rng = Rng::new(0xC0DEC0DE);
        Ctx::new(&mut rng)
    }
}

pub fn canary_authenticated() -> ServerMessage {
    ServerMessage::Authenticated {
        app_name: "canary".into(),
        organization: None,
        rate_limits: RateLimitInfo {
            per_minute: 1,
            per_hour: 2,
            per_day: 3,
        },
    }
}

pub fn canary_protocol_info() -> (ServerMessage, FrameMeta) {
    (
        ServerMessage::ProtocolInfo(ProtocolInfoPayload {
            platform: None,
            sdk_version: None,
            minimum_version: Some("0.4.0".into()),
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
        }),
        FrameMeta::default(),
    )
}

pub fn canary_room_joined(ctx: &Ctx) -> ServerMessage {
    ServerMessage::RoomJoined(Box::new(RoomJoinedPayload {
        room_id: ctx.room_id,
        room_code: ctx.room_code.clone(),
        player_id: ctx.self_id,
        game_name: ctx.game_name.clone(),
        max_players: 4,
        supports_authority: false,
        current_players: vec![player_info(ctx, ctx.self_id, "self", Some(1), Some(0))],
        is_authority: false,
        lobby_state: LobbyState::Waiting,
        ready_players: vec![],
        relay_type: "websocket".into(),
        current_spectators: vec![],
        ice_servers: vec![],
        reconnection_token: None,
    }))
}

pub fn canary_player_joined(ctx: &Ctx) -> ServerMessage {
    ServerMessage::PlayerJoined {
        player: player_info(ctx, ctx.peer_a, "peer-a", Some(1), Some(0)),
    }
}

pub fn canary_game_data(ctx: &mut Ctx) -> (ServerMessage, FrameMeta) {
    let seq = ctx.next_seq(ctx.peer_a);
    (
        ServerMessage::GameData {
            from_player: ctx.peer_a,
            data: serde_json::json!({ "canary": true }),
            seq: Some(seq),
            epoch: Some(1),
            class: Some(DeliveryClass::Reliable),
            key: None,
        },
        FrameMeta {
            stamp: StampMode::Valid,
            bound_breaking: false,
        },
    )
}
