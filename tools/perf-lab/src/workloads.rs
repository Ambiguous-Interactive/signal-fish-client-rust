#![cfg(feature = "perf")]
#![forbid(unsafe_code)]

use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::task::{Context, Poll};
use std::time::Duration;

use serde::{Deserialize, Serialize, Serializer};
use sha2::{Digest as _, Sha256};
use signal_fish_client::protocol::{
    GameDataEncoding, LobbyState, PlayerId, PlayerInfo, ProtocolInfoPayload, RateLimitInfo,
    ReconnectedPayload, RoomJoinedPayload,
};
use signal_fish_client::{
    DeliveryClass, DeliveryCountersByClass, DeliveryGap, DeliveryGapReason, DeliveryReportPayload,
    GameDataDelivery, JoinRoomParams, LatestDeliveryCounters, MessageTransport,
    PollingClientOptions, PollingClosePolicy, PollingWorkBudget, ReplayStatus, RoomRole,
    SenderWatermark, ServerMessage, SignalFishConfig, SignalFishError, SignalFishEvent,
    SignalFishPollingClient, Transport, TransportFrame,
};

const LOCAL_PLAYER: u128 = 100;
const ROOM_ID: u128 = 200;
const FIRST_PEER: u128 = 1_000;
const EVENT_KIND_COUNT: usize = 36;
const MAX_DRIVE_POLLS: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Inbound,
    Outbound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workload {
    Lobby {
        players: usize,
    },
    JsonRelay {
        direction: Direction,
        payload_bytes: usize,
        messages: usize,
    },
    BinaryRelay {
        direction: Direction,
        payload_bytes: usize,
        messages: usize,
    },
    Latest,
    Volatile,
    AuthorizedGap,
    Reconnect {
        players: usize,
    },
    PollingReadyFrameBurst,
    PollingReadyByteBurst,
    PollingPendingRecovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkloadSpec {
    pub id: &'static str,
    pub workload: Workload,
}

pub const WORKLOADS: [WorkloadSpec; 28] = [
    WorkloadSpec {
        id: "lobby/2",
        workload: Workload::Lobby { players: 2 },
    },
    WorkloadSpec {
        id: "lobby/8",
        workload: Workload::Lobby { players: 8 },
    },
    WorkloadSpec {
        id: "lobby/16",
        workload: Workload::Lobby { players: 16 },
    },
    WorkloadSpec {
        id: "json/in/256/single",
        workload: Workload::JsonRelay {
            direction: Direction::Inbound,
            payload_bytes: 256,
            messages: 1,
        },
    },
    WorkloadSpec {
        id: "json/in/256/burst64",
        workload: Workload::JsonRelay {
            direction: Direction::Inbound,
            payload_bytes: 256,
            messages: 64,
        },
    },
    WorkloadSpec {
        id: "json/in/4096/single",
        workload: Workload::JsonRelay {
            direction: Direction::Inbound,
            payload_bytes: 4_096,
            messages: 1,
        },
    },
    WorkloadSpec {
        id: "json/in/4096/burst64",
        workload: Workload::JsonRelay {
            direction: Direction::Inbound,
            payload_bytes: 4_096,
            messages: 64,
        },
    },
    WorkloadSpec {
        id: "json/out/256/single",
        workload: Workload::JsonRelay {
            direction: Direction::Outbound,
            payload_bytes: 256,
            messages: 1,
        },
    },
    WorkloadSpec {
        id: "json/out/256/burst64",
        workload: Workload::JsonRelay {
            direction: Direction::Outbound,
            payload_bytes: 256,
            messages: 64,
        },
    },
    WorkloadSpec {
        id: "json/out/4096/single",
        workload: Workload::JsonRelay {
            direction: Direction::Outbound,
            payload_bytes: 4_096,
            messages: 1,
        },
    },
    WorkloadSpec {
        id: "json/out/4096/burst64",
        workload: Workload::JsonRelay {
            direction: Direction::Outbound,
            payload_bytes: 4_096,
            messages: 64,
        },
    },
    WorkloadSpec {
        id: "binary/in/256/single",
        workload: Workload::BinaryRelay {
            direction: Direction::Inbound,
            payload_bytes: 256,
            messages: 1,
        },
    },
    WorkloadSpec {
        id: "binary/in/256/burst64",
        workload: Workload::BinaryRelay {
            direction: Direction::Inbound,
            payload_bytes: 256,
            messages: 64,
        },
    },
    WorkloadSpec {
        id: "binary/in/4096/single",
        workload: Workload::BinaryRelay {
            direction: Direction::Inbound,
            payload_bytes: 4_096,
            messages: 1,
        },
    },
    WorkloadSpec {
        id: "binary/in/4096/burst64",
        workload: Workload::BinaryRelay {
            direction: Direction::Inbound,
            payload_bytes: 4_096,
            messages: 64,
        },
    },
    WorkloadSpec {
        id: "binary/out/256/single",
        workload: Workload::BinaryRelay {
            direction: Direction::Outbound,
            payload_bytes: 256,
            messages: 1,
        },
    },
    WorkloadSpec {
        id: "binary/out/256/burst64",
        workload: Workload::BinaryRelay {
            direction: Direction::Outbound,
            payload_bytes: 256,
            messages: 64,
        },
    },
    WorkloadSpec {
        id: "binary/out/4096/single",
        workload: Workload::BinaryRelay {
            direction: Direction::Outbound,
            payload_bytes: 4_096,
            messages: 1,
        },
    },
    WorkloadSpec {
        id: "binary/out/4096/burst64",
        workload: Workload::BinaryRelay {
            direction: Direction::Outbound,
            payload_bytes: 4_096,
            messages: 64,
        },
    },
    WorkloadSpec {
        id: "classified/latest",
        workload: Workload::Latest,
    },
    WorkloadSpec {
        id: "classified/volatile",
        workload: Workload::Volatile,
    },
    WorkloadSpec {
        id: "classified/authorized-gap",
        workload: Workload::AuthorizedGap,
    },
    WorkloadSpec {
        id: "reconnect/2",
        workload: Workload::Reconnect { players: 2 },
    },
    WorkloadSpec {
        id: "reconnect/8",
        workload: Workload::Reconnect { players: 8 },
    },
    WorkloadSpec {
        id: "reconnect/16",
        workload: Workload::Reconnect { players: 16 },
    },
    WorkloadSpec {
        id: "polling/ready-frame-burst",
        workload: Workload::PollingReadyFrameBurst,
    },
    WorkloadSpec {
        id: "polling/ready-byte-burst",
        workload: Workload::PollingReadyByteBurst,
    },
    WorkloadSpec {
        id: "polling/pending-recovery",
        workload: Workload::PollingPendingRecovery,
    },
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkloadLedger {
    pub workload: &'static str,
    pub logical_operations: u64,
    pub polls: u64,
    pub send_attempts: u64,
    pub pending_sends: u64,
    pub inbound_frames: u64,
    pub inbound_bytes: u64,
    pub outbound_frames: u64,
    pub outbound_bytes: u64,
    pub inbound_wire_sha256: [u8; 32],
    pub outbound_wire_sha256: [u8; 32],
    #[serde(serialize_with = "serialize_event_counts")]
    pub event_counts: [u64; EVENT_KIND_COUNT],
    pub event_payload_fingerprint: u64,
    pub snapshot_fingerprint: u64,
    pub roster_players: u64,
    pub replayed_events: u64,
    pub sender_watermarks: u64,
    pub reported_gap_sequences: u64,
    pub game_data_sent: u64,
    pub game_data_received: u64,
    pub messages_undecodable: u64,
    pub send_budget_exhaustions: u64,
    pub receive_budget_exhaustions: u64,
    pub final_queue_depth: u64,
    pub current_queue_age_ns: u64,
    pub peak_queue_age_ns: u64,
    pub connected: bool,
    pub transport_ready: bool,
    pub authenticated: bool,
    pub negotiated_protocol_version: Option<u16>,
    pub in_room: bool,
    pub quarantined: bool,
}

#[derive(Debug)]
pub struct WorkloadFixture {
    spec: WorkloadSpec,
    client: SignalFishPollingClient<DeterministicTransport>,
    commands: VecDeque<Command>,
    measured_events: Vec<SignalFishEvent>,
    measured_ingress_open: Rc<Cell<bool>>,
    inbound_wire_sha256: [u8; 32],
    inbound_bytes: u64,
    setup_events: EventAccumulator,
    baseline_send_attempts: u64,
    baseline_pending_sends: u64,
    baseline_received_frames: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawOutcome {
    polls: u64,
    event_count: u64,
}

#[derive(Debug, Deserialize)]
struct ProtocolBaselines {
    schema: u32,
    records: Vec<ProtocolBaseline>,
}

#[derive(Debug, Deserialize)]
struct ProtocolBaseline {
    workload: String,
    sha256: [u8; 32],
}

#[derive(Debug)]
enum Command {
    Json(serde_json::Value, GameDataDelivery),
    Binary(Vec<u8>),
    Reconnect {
        player_id: PlayerId,
        room_id: PlayerId,
        token: String,
    },
}

#[derive(Debug, Default)]
struct DeterministicTransport {
    setup_incoming: VecDeque<TransportFrame>,
    setup_room_ingress_open: Rc<Cell<bool>>,
    measured_incoming: VecDeque<TransportFrame>,
    measured_ingress_open: Rc<Cell<bool>>,
    sent: Vec<TransportFrame>,
    send_attempts: u64,
    pending_sends: u64,
    received_frames: u64,
    pending_after_accepted_sends: Option<u64>,
    pending_returned: bool,
    closed: bool,
}

impl Transport for DeterministicTransport {
    fn poll_send(
        &mut self,
        _cx: &mut Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> Poll<Result<(), SignalFishError>> {
        self.send_attempts = self.send_attempts.saturating_add(1);
        if self.pending_after_accepted_sends == Some(usize_to_u64(self.sent.len()))
            && frame.is_some()
            && !self.pending_returned
        {
            self.pending_returned = true;
            self.pending_sends = self.pending_sends.saturating_add(1);
            return Poll::Pending;
        }
        if let Some(accepted) = frame.take() {
            self.sent.push(accepted);
        }
        Poll::Ready(Ok(()))
    }

    fn poll_recv(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<TransportFrame, SignalFishError>>> {
        let setup_open = self.received_frames < 2 || self.setup_room_ingress_open.get();
        let next = setup_open
            .then(|| self.setup_incoming.pop_front())
            .flatten()
            .or_else(|| {
                self.measured_ingress_open
                    .get()
                    .then(|| self.measured_incoming.pop_front())
                    .flatten()
            });
        match next {
            Some(frame) => {
                self.received_frames = self.received_frames.saturating_add(1);
                Poll::Ready(Some(Ok(frame)))
            }
            None => Poll::Pending,
        }
    }

    fn poll_close(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), SignalFishError>> {
        self.closed = true;
        Poll::Ready(Ok(()))
    }

    fn abort(&mut self) {
        self.closed = true;
        self.setup_incoming.clear();
        self.measured_incoming.clear();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EventAccumulator {
    counts: [u64; EVENT_KIND_COUNT],
    payload_fingerprint: u64,
    roster_players: u64,
    replayed_events: u64,
    sender_watermarks: u64,
    reported_gap_sequences: u64,
}

impl Default for EventAccumulator {
    fn default() -> Self {
        Self {
            counts: [0; EVENT_KIND_COUNT],
            payload_fingerprint: 0xcbf2_9ce4_8422_2325,
            roster_players: 0,
            replayed_events: 0,
            sender_watermarks: 0,
            reported_gap_sequences: 0,
        }
    }
}

impl EventAccumulator {
    fn observe(&mut self, event: SignalFishEvent) -> Result<(), String> {
        self.fingerprint_event_payload(&event)?;
        let index = match event {
            SignalFishEvent::Connected => 0,
            SignalFishEvent::Disconnected { .. } => 1,
            SignalFishEvent::DecodeFailed { .. } => 2,
            SignalFishEvent::ProtocolViolation { diagnostic, .. } => {
                return Err(format!(
                    "protocol violation in workload fixture: {diagnostic}"
                ));
            }
            SignalFishEvent::Authenticated { .. } => 4,
            SignalFishEvent::ProtocolInfo(_) => 5,
            SignalFishEvent::AuthenticationError { .. } => 6,
            SignalFishEvent::RoomJoined {
                current_players, ..
            } => {
                self.roster_players = usize_to_u64(current_players.len());
                7
            }
            SignalFishEvent::RoomJoinFailed { .. } => 8,
            SignalFishEvent::RoomLeft => 9,
            SignalFishEvent::PlayerJoined { .. } => 10,
            SignalFishEvent::PlayerLeft { .. } => 11,
            SignalFishEvent::GameData {
                from_player,
                data,
                seq,
                epoch,
                class,
                key,
            } => {
                self.fingerprint(from_player.as_bytes());
                self.fingerprint(&seq.unwrap_or_default().to_le_bytes());
                self.fingerprint(&epoch.unwrap_or_default().to_le_bytes());
                self.fingerprint(&[delivery_class_byte(class)]);
                self.fingerprint(&key.unwrap_or_default().to_le_bytes());
                fingerprint_json(&mut self.payload_fingerprint, &data)?;
                12
            }
            SignalFishEvent::GameDataBinary {
                from_player,
                encoding,
                payload,
                seq,
                epoch,
            } => {
                self.fingerprint(from_player.as_bytes());
                self.fingerprint(&[encoding_byte(encoding)]);
                self.fingerprint(&seq.unwrap_or_default().to_le_bytes());
                self.fingerprint(&epoch.unwrap_or_default().to_le_bytes());
                self.fingerprint(&payload);
                13
            }
            SignalFishEvent::AuthorityChanged { .. } => 14,
            SignalFishEvent::AuthorityResponse { .. } => 15,
            SignalFishEvent::LobbyStateChanged { .. } => 16,
            SignalFishEvent::GameStarting { .. } => 17,
            SignalFishEvent::SessionPlan { .. } => 18,
            SignalFishEvent::NewPeer { .. } => 19,
            SignalFishEvent::SignalReceived { .. } => 20,
            SignalFishEvent::PeerTransportStatus { .. } => 21,
            SignalFishEvent::RelayStats { .. } => 22,
            SignalFishEvent::GoingAway { .. } => 23,
            SignalFishEvent::DeliveryReport(report) => {
                for gap in report.gaps {
                    let count = gap
                        .to_seq
                        .checked_sub(gap.from_seq)
                        .and_then(|length| length.checked_add(1))
                        .ok_or_else(|| "DeliveryReport gap length overflowed".to_string())?;
                    self.reported_gap_sequences = self.reported_gap_sequences.saturating_add(count);
                }
                24
            }
            SignalFishEvent::Pong => 25,
            SignalFishEvent::Reconnected {
                current_players,
                missed_events,
                sender_watermarks,
                ..
            } => {
                self.roster_players = usize_to_u64(current_players.len());
                self.replayed_events = usize_to_u64(missed_events.len());
                self.sender_watermarks = usize_to_u64(sender_watermarks.len());
                26
            }
            SignalFishEvent::ReconnectionFailed { .. } => 27,
            SignalFishEvent::PlayerReconnected { .. } => 28,
            SignalFishEvent::SpectatorJoined { .. } => 29,
            SignalFishEvent::SpectatorJoinFailed { .. } => 30,
            SignalFishEvent::SpectatorLeft { .. } => 31,
            SignalFishEvent::NewSpectatorJoined { .. } => 32,
            SignalFishEvent::SpectatorDisconnected { .. } => 33,
            SignalFishEvent::Error { .. } => 34,
            SignalFishEvent::RoomOperationFailed { .. } => 35,
        };
        let count = self
            .counts
            .get_mut(index)
            .ok_or_else(|| format!("event index {index} is outside ledger"))?;
        *count = count.saturating_add(1);
        self.fingerprint(&[
            u8::try_from(index).map_err(|_| format!("event index {index} does not fit u8"))?
        ]);
        Ok(())
    }

    fn fingerprint(&mut self, bytes: &[u8]) {
        fingerprint_bytes(&mut self.payload_fingerprint, bytes);
    }

    fn fingerprint_event_payload(&mut self, event: &SignalFishEvent) -> Result<(), String> {
        match event {
            SignalFishEvent::Authenticated {
                app_name,
                organization,
                rate_limits,
            } => fingerprint_serializable(
                &mut self.payload_fingerprint,
                4,
                &(app_name, organization, rate_limits),
            ),
            SignalFishEvent::ProtocolInfo(payload) => {
                fingerprint_serializable(&mut self.payload_fingerprint, 5, payload)
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
            } => fingerprint_serializable(
                &mut self.payload_fingerprint,
                7,
                &(
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
                ),
            ),
            SignalFishEvent::DeliveryReport(report) => {
                fingerprint_serializable(&mut self.payload_fingerprint, 24, report)
            }
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
                fingerprint_serializable(
                    &mut self.payload_fingerprint,
                    26,
                    &(
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
                    ),
                )?;
                fingerprint_serializable(
                    &mut self.payload_fingerprint,
                    36,
                    &(replay, sender_watermarks, reconnection_token),
                )?;
                for missed in missed_events {
                    self.fingerprint_event_payload(missed)?;
                }
                Ok(())
            }
            SignalFishEvent::AuthorityChanged {
                authority_player,
                you_are_authority,
            } => fingerprint_serializable(
                &mut self.payload_fingerprint,
                14,
                &(authority_player, you_are_authority),
            ),
            SignalFishEvent::LobbyStateChanged {
                lobby_state,
                ready_players,
                all_ready,
            } => fingerprint_serializable(
                &mut self.payload_fingerprint,
                16,
                &(lobby_state, ready_players, all_ready),
            ),
            _ => Ok(()),
        }
    }
}

pub fn prepare_and_warm(spec: WorkloadSpec) -> Result<WorkloadFixture, String> {
    let mut config = SignalFishConfig::new("perf_lab")
        .enable_v3()
        .with_command_channel_capacity(256);
    // The default sdk_version would embed the crate version in the
    // Authenticate wire bytes and invalidate every pinned protocol-ledger
    // digest on each release bump. The lab pins protocol behavior, not
    // release metadata, so the field stays omitted like the ProtocolInfo
    // fixture below.
    config.sdk_version = None;
    let mut options = PollingClientOptions::default();
    let mut setup_incoming = VecDeque::new();
    let mut measured_incoming = VecDeque::new();
    let mut commands = VecDeque::new();
    let mut pending_game_send_once = false;

    push_text(&mut setup_incoming, authenticated_message())?;
    let binary = matches!(spec.workload, Workload::BinaryRelay { .. });
    if binary {
        config.game_data_format = Some(GameDataEncoding::MessagePack);
    }
    push_text(&mut setup_incoming, protocol_info_message())?;

    match spec.workload {
        Workload::Lobby { players } => {
            push_text(&mut measured_incoming, room_message(players))?;
        }
        Workload::JsonRelay {
            direction,
            payload_bytes,
            messages,
        } => {
            push_text(&mut setup_incoming, room_message(2))?;
            let payload = serde_json::Value::String("j".repeat(payload_bytes));
            for seq in 1..=messages {
                match direction {
                    Direction::Inbound => push_text(
                        &mut measured_incoming,
                        ServerMessage::GameData {
                            from_player: peer_id(0),
                            data: payload.clone(),
                            seq: Some(usize_to_u64(seq)),
                            epoch: Some(1),
                            class: Some(DeliveryClass::Reliable),
                            key: None,
                        },
                    )?,
                    Direction::Outbound => commands
                        .push_back(Command::Json(payload.clone(), GameDataDelivery::Reliable)),
                }
            }
        }
        Workload::BinaryRelay {
            direction,
            payload_bytes,
            messages,
        } => {
            push_text(&mut setup_incoming, room_message(2))?;
            let payload = vec![0x5a; payload_bytes];
            for seq in 1..=messages {
                match direction {
                    Direction::Inbound => {
                        let frame = signal_fish_client::V3BinaryGameDataFrame {
                            from_player: peer_id(0),
                            encoding: GameDataEncoding::MessagePack,
                            payload: payload.clone(),
                            seq: usize_to_u64(seq),
                            epoch: 1,
                        };
                        let wire = rmp_serde::to_vec_named(&frame)
                            .map_err(|error| format!("serialize v3 binary fixture: {error}"))?;
                        measured_incoming.push_back(TransportFrame::Binary(wire));
                    }
                    Direction::Outbound => commands.push_back(Command::Binary(payload.clone())),
                }
            }
        }
        Workload::Latest => {
            push_text(&mut setup_incoming, room_message(2))?;
            for seq in 0..64 {
                commands.push_back(Command::Json(
                    serde_json::json!({ "frame": seq, "payload": "l".repeat(256) }),
                    GameDataDelivery::Latest { key: 7 },
                ));
            }
        }
        Workload::Volatile => {
            push_text(&mut setup_incoming, room_message(2))?;
            for seq in 0..64 {
                commands.push_back(Command::Json(
                    serde_json::json!({ "frame": seq, "payload": "v".repeat(256) }),
                    GameDataDelivery::Volatile,
                ));
            }
        }
        Workload::AuthorizedGap => {
            push_text(&mut setup_incoming, room_message(2))?;
            push_text(
                &mut measured_incoming,
                ServerMessage::GameData {
                    from_player: peer_id(0),
                    data: serde_json::json!({ "frame": 1 }),
                    seq: Some(1),
                    epoch: Some(1),
                    class: Some(DeliveryClass::Latest),
                    key: Some(7),
                },
            )?;
            push_text(
                &mut measured_incoming,
                ServerMessage::DeliveryReport(Box::new(DeliveryReportPayload {
                    per_class: DeliveryCountersByClass {
                        latest: LatestDeliveryCounters {
                            superseded: 1,
                            ..LatestDeliveryCounters::default()
                        },
                        ..DeliveryCountersByClass::default()
                    },
                    gaps: vec![DeliveryGap {
                        from_player: peer_id(0),
                        epoch: 1,
                        from_seq: 2,
                        to_seq: 2,
                        reason: DeliveryGapReason::LatestSuperseded,
                    }],
                })),
            )?;
            push_text(
                &mut measured_incoming,
                ServerMessage::GameData {
                    from_player: peer_id(0),
                    data: serde_json::json!({ "frame": 3 }),
                    seq: Some(3),
                    epoch: Some(1),
                    class: Some(DeliveryClass::Latest),
                    key: Some(7),
                },
            )?;
        }
        Workload::Reconnect { players } => {
            push_text(&mut measured_incoming, reconnect_message(players))?;
            commands.push_back(Command::Reconnect {
                player_id: local_id(),
                room_id: PlayerId::from_u128(ROOM_ID),
                token: "submitted-token".to_string(),
            });
        }
        Workload::PollingReadyFrameBurst => {
            push_text(&mut setup_incoming, room_message(2))?;
            options = PollingClientOptions {
                work_budget: PollingWorkBudget {
                    send_frames: 8,
                    send_bytes: 2_048,
                    receive_frames: 64,
                    receive_bytes: 64 * 1_024,
                },
                close_policy: PollingClosePolicy::Abandon,
            };
            for seq in 0..17 {
                commands.push_back(Command::Json(
                    serde_json::json!({ "frame": seq, "payload": "b".repeat(32) }),
                    GameDataDelivery::Reliable,
                ));
            }
        }
        Workload::PollingReadyByteBurst => {
            push_text(&mut setup_incoming, room_message(2))?;
            options = PollingClientOptions {
                work_budget: PollingWorkBudget {
                    send_frames: 64,
                    send_bytes: 1_024,
                    receive_frames: 64,
                    receive_bytes: 64 * 1_024,
                },
                close_policy: PollingClosePolicy::Abandon,
            };
            for seq in 0..4 {
                commands.push_back(Command::Json(
                    serde_json::json!({ "frame": seq, "payload": "b".repeat(700) }),
                    GameDataDelivery::Reliable,
                ));
            }
        }
        Workload::PollingPendingRecovery => {
            push_text(&mut setup_incoming, room_message(2))?;
            commands.push_back(Command::Json(
                serde_json::json!({ "frame": 1, "payload": "p".repeat(256) }),
                GameDataDelivery::Reliable,
            ));
            pending_game_send_once = true;
        }
    }

    let inbound_wire_sha256 = digest_frames(setup_incoming.iter().chain(measured_incoming.iter()));
    let inbound_bytes = setup_incoming
        .iter()
        .chain(measured_incoming.iter())
        .fold(0u64, |total, frame| total.saturating_add(frame_len(frame)));
    let measured_ingress_open = Rc::new(Cell::new(false));
    let setup_room_ingress_open = Rc::new(Cell::new(false));
    let expected_sent = commands.len().saturating_add(2);
    let transport = DeterministicTransport {
        setup_incoming,
        setup_room_ingress_open: Rc::clone(&setup_room_ingress_open),
        measured_incoming,
        measured_ingress_open: Rc::clone(&measured_ingress_open),
        sent: Vec::with_capacity(expected_sent),
        pending_after_accepted_sends: pending_game_send_once.then_some(2),
        ..DeterministicTransport::default()
    };
    let mut client = SignalFishPollingClient::new_with_options(transport, config, options);
    let mut setup_events = EventAccumulator::default();
    let mut setup_polls = 0;
    observe_poll(&mut client, &mut setup_events, &mut setup_polls)?;
    if !matches!(spec.workload, Workload::Reconnect { .. }) {
        client
            .join_room(JoinRoomParams::new("performance-lab", "local"))
            .map_err(|error| format!("{} room admission failed: {error}", spec.id))?;
        setup_room_ingress_open.set(true);
        observe_poll(&mut client, &mut setup_events, &mut setup_polls)?;
    }
    if !client.transport().setup_incoming.is_empty()
        || client.transport().received_frames != expected_setup_frames(spec.workload)
    {
        return Err(format!("{} setup did not drain exactly", spec.id));
    }
    validate_setup_events(spec, &setup_events)?;
    client.reset_queue_age_peak();
    let baseline_send_attempts = client.transport().send_attempts;
    let baseline_pending_sends = client.transport().pending_sends;
    let baseline_received_frames = client.transport().received_frames;
    Ok(WorkloadFixture {
        spec,
        client,
        commands,
        measured_events: Vec::with_capacity(expected_measured_events(spec.workload)),
        measured_ingress_open,
        inbound_wire_sha256,
        inbound_bytes,
        setup_events,
        baseline_send_attempts,
        baseline_pending_sends,
        baseline_received_frames,
    })
}

pub fn run_measured(fixture: &mut WorkloadFixture) -> Result<RawOutcome, String> {
    fixture.measured_ingress_open.set(true);
    let mut polls = 0u64;
    while let Some(command) = fixture.commands.pop_front() {
        match command {
            Command::Json(data, delivery) => fixture
                .client
                .send_game_data_with_delivery(data, delivery)
                .map_err(|error| {
                    format!("{} command admission failed: {error}", fixture.spec.id)
                })?,
            Command::Binary(payload) => fixture
                .client
                .send_binary_game_data(payload)
                .map_err(|error| format!("{} binary admission failed: {error}", fixture.spec.id))?,
            Command::Reconnect {
                player_id,
                room_id,
                token,
            } => fixture
                .client
                .reconnect(player_id, room_id, token)
                .map_err(|error| {
                    format!("{} reconnect admission failed: {error}", fixture.spec.id)
                })?,
        }
    }

    let mut observed_idle = false;
    for _ in 0..MAX_DRIVE_POLLS {
        let batch = fixture.client.poll();
        polls = polls.saturating_add(1);
        let batch_empty = batch.is_empty();
        for event in batch {
            fixture.measured_events.push(event);
        }
        let transport_idle = fixture.client.transport().measured_incoming.is_empty();
        let outbound_idle = fixture.client.polling_stats().current_queue_depth == 0;
        if batch_empty && transport_idle && outbound_idle {
            observed_idle = true;
            break;
        }
    }
    if !observed_idle {
        return Err(format!(
            "{} did not quiesce within {MAX_DRIVE_POLLS} polls",
            fixture.spec.id
        ));
    }
    Ok(RawOutcome {
        polls,
        event_count: usize_to_u64(fixture.measured_events.len()),
    })
}

pub fn finish_and_verify(
    fixture: WorkloadFixture,
    outcome: RawOutcome,
) -> Result<WorkloadLedger, String> {
    finish_and_verify_inner(fixture, outcome, true)
}

pub fn finish_without_protocol_pin(
    fixture: WorkloadFixture,
    outcome: RawOutcome,
) -> Result<WorkloadLedger, String> {
    finish_and_verify_inner(fixture, outcome, false)
}

fn finish_and_verify_inner(
    mut fixture: WorkloadFixture,
    outcome: RawOutcome,
    enforce_protocol_pin: bool,
) -> Result<WorkloadLedger, String> {
    let expected_events = usize_to_u64(expected_measured_events(fixture.spec.workload));
    if outcome.event_count != usize_to_u64(fixture.measured_events.len())
        || outcome.event_count != expected_events
    {
        return Err(format!(
            "{} measured events: expected {expected_events}, found {}",
            fixture.spec.id, outcome.event_count
        ));
    }
    let mut measured_events = EventAccumulator {
        payload_fingerprint: fixture.setup_events.payload_fingerprint,
        ..EventAccumulator::default()
    };
    for event in fixture.measured_events.drain(..) {
        measured_events.observe(event)?;
    }
    let stats = fixture.client.stats();
    let polling = fixture.client.polling_stats();
    let queue_age = fixture.client.queue_age_stats();
    let snapshot = fixture.client.snapshot();
    let transport = fixture.client.transport();
    let measured_inbound_frames = transport
        .received_frames
        .saturating_sub(fixture.baseline_received_frames);
    let expected_measured_inbound = expected_inbound_frames(fixture.spec.workload)
        .saturating_sub(expected_setup_frames(fixture.spec.workload));
    if measured_inbound_frames != expected_measured_inbound {
        return Err(format!(
            "{} measured inbound frames: expected {expected_measured_inbound}, found {measured_inbound_frames}",
            fixture.spec.id
        ));
    }
    let outbound_wire_sha256 = digest_frames(transport.sent.iter());
    let outbound_bytes = transport
        .sent
        .iter()
        .fold(0u64, |total, frame| total.saturating_add(frame_len(frame)));
    let mut event_counts = fixture.setup_events.counts;
    for (total, measured) in event_counts.iter_mut().zip(measured_events.counts) {
        *total = total.saturating_add(measured);
    }
    let ledger = WorkloadLedger {
        workload: fixture.spec.id,
        logical_operations: logical_operations(fixture.spec.workload),
        polls: outcome.polls,
        send_attempts: transport
            .send_attempts
            .saturating_sub(fixture.baseline_send_attempts),
        pending_sends: transport
            .pending_sends
            .saturating_sub(fixture.baseline_pending_sends),
        inbound_frames: transport.received_frames,
        inbound_bytes: fixture.inbound_bytes,
        outbound_frames: usize_to_u64(transport.sent.len()),
        outbound_bytes,
        inbound_wire_sha256: fixture.inbound_wire_sha256,
        outbound_wire_sha256,
        event_counts,
        event_payload_fingerprint: measured_events.payload_fingerprint,
        snapshot_fingerprint: snapshot_fingerprint(&snapshot)?,
        roster_players: measured_events
            .roster_players
            .max(fixture.setup_events.roster_players),
        replayed_events: measured_events.replayed_events,
        sender_watermarks: measured_events.sender_watermarks,
        reported_gap_sequences: measured_events.reported_gap_sequences,
        game_data_sent: stats.game_data_sent,
        game_data_received: stats.game_data_received,
        messages_undecodable: stats.messages_undecodable,
        send_budget_exhaustions: polling.send_budget_exhaustions,
        receive_budget_exhaustions: polling.receive_budget_exhaustions,
        final_queue_depth: polling.current_queue_depth,
        current_queue_age_ns: duration_ns(queue_age.current_oldest_queue_age),
        peak_queue_age_ns: duration_ns(queue_age.peak_oldest_queue_age),
        connected: snapshot.connected,
        transport_ready: snapshot.transport_ready,
        authenticated: snapshot.authenticated,
        negotiated_protocol_version: snapshot.negotiated_protocol_version,
        in_room: snapshot.room_role.is_some(),
        quarantined: snapshot.quarantined,
    };
    if enforce_protocol_pin {
        validate_ledger(fixture.spec, &ledger)?;
    } else {
        validate_ledger_semantics(fixture.spec, &ledger)?;
    }
    Ok(ledger)
}

pub fn execute_once(spec: WorkloadSpec) -> Result<WorkloadLedger, String> {
    let mut fixture = prepare_and_warm(spec)?;
    let outcome = run_measured(&mut fixture)?;
    finish_and_verify(fixture, outcome)
}

pub fn execute_once_without_protocol_pin(spec: WorkloadSpec) -> Result<WorkloadLedger, String> {
    let mut fixture = prepare_and_warm(spec)?;
    let outcome = run_measured(&mut fixture)?;
    finish_without_protocol_pin(fixture, outcome)
}

pub fn deterministic_ledger_digest(ledger: &WorkloadLedger) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(ledger.workload.as_bytes());
    for value in [
        ledger.logical_operations,
        ledger.polls,
        ledger.send_attempts,
        ledger.pending_sends,
        ledger.inbound_frames,
        ledger.inbound_bytes,
        ledger.outbound_frames,
        ledger.outbound_bytes,
        ledger.roster_players,
        ledger.replayed_events,
        ledger.sender_watermarks,
        ledger.reported_gap_sequences,
        ledger.game_data_sent,
        ledger.game_data_received,
        ledger.messages_undecodable,
        ledger.send_budget_exhaustions,
        ledger.receive_budget_exhaustions,
        ledger.final_queue_depth,
        ledger.event_payload_fingerprint,
        ledger.snapshot_fingerprint,
    ] {
        hasher.update(value.to_le_bytes());
    }
    hasher.update(ledger.inbound_wire_sha256);
    hasher.update(ledger.outbound_wire_sha256);
    for count in ledger.event_counts {
        hasher.update(count.to_le_bytes());
    }
    hasher.update(
        ledger
            .negotiated_protocol_version
            .unwrap_or_default()
            .to_le_bytes(),
    );
    hasher.update([
        u8::from(ledger.connected),
        u8::from(ledger.transport_ready),
        u8::from(ledger.authenticated),
        u8::from(ledger.in_room),
        u8::from(ledger.quarantined),
    ]);
    hasher.finalize().into()
}

pub fn validate_ledger(spec: WorkloadSpec, ledger: &WorkloadLedger) -> Result<(), String> {
    validate_ledger_semantics(spec, ledger)?;
    validate_protocol_baseline(spec, ledger)
}

fn validate_ledger_semantics(spec: WorkloadSpec, ledger: &WorkloadLedger) -> Result<(), String> {
    if ledger.workload != spec.id {
        return Err(format!(
            "ledger workload mismatch: expected {}, found {}",
            spec.id, ledger.workload
        ));
    }
    if ledger.messages_undecodable != 0 || event_count(ledger, 2)? != 0 {
        return Err(format!("{} decoded malformed input", spec.id));
    }
    if event_count(ledger, 3)? != 0 || ledger.quarantined {
        return Err(format!("{} triggered accountability quarantine", spec.id));
    }
    if !ledger.connected
        || !ledger.transport_ready
        || !ledger.authenticated
        || ledger.negotiated_protocol_version != Some(3)
        || ledger.final_queue_depth != 0
    {
        return Err(format!("{} ended in an invalid client state", spec.id));
    }
    if event_count(ledger, 0)? != 1 || event_count(ledger, 4)? != 1 || event_count(ledger, 5)? != 1
    {
        return Err(format!("{} setup event ledger is not exact", spec.id));
    }

    let expected_inbound_frames = expected_inbound_frames(spec.workload);
    if ledger.inbound_frames != expected_inbound_frames {
        return Err(format!(
            "{} inbound frame count: expected {expected_inbound_frames}, found {}",
            spec.id, ledger.inbound_frames
        ));
    }
    let expected_outbound_frames = expected_outbound_frames(spec.workload);
    if ledger.outbound_frames != expected_outbound_frames {
        return Err(format!(
            "{} outbound frame count: expected {expected_outbound_frames}, found {}",
            spec.id, ledger.outbound_frames
        ));
    }
    let expected_total_events = usize_to_u64(expected_setup_event_count(spec.workload))
        .saturating_add(usize_to_u64(expected_measured_events(spec.workload)));
    let total_events = ledger
        .event_counts
        .iter()
        .fold(0u64, |total, count| total.saturating_add(*count));
    if total_events != expected_total_events {
        return Err(format!(
            "{} total event count: expected {expected_total_events}, found {total_events}",
            spec.id
        ));
    }
    let expected_send_exhaustions = expected_send_budget_exhaustions(spec.workload);
    let expected_receive_exhaustions = expected_receive_budget_exhaustions(spec.workload);
    if ledger.send_budget_exhaustions != expected_send_exhaustions
        || ledger.receive_budget_exhaustions != expected_receive_exhaustions
    {
        return Err(format!(
            "{} budget ledger: expected send/receive {expected_send_exhaustions}/{expected_receive_exhaustions}, found {}/{}",
            spec.id, ledger.send_budget_exhaustions, ledger.receive_budget_exhaustions
        ));
    }
    if ledger.current_queue_age_ns != 0 {
        return Err(format!("{} retained a nonzero queue age", spec.id));
    }

    match spec.workload {
        Workload::Lobby { players } => {
            expect_room(spec, ledger, players)?;
            expect_game_counts(spec, ledger, 0, 0)?;
        }
        Workload::JsonRelay {
            direction,
            messages,
            ..
        } => {
            expect_room(spec, ledger, 2)?;
            let messages = usize_to_u64(messages);
            match direction {
                Direction::Inbound => {
                    expect_game_counts(spec, ledger, 0, messages)?;
                    expect_event_count(spec, ledger, 12, messages)?;
                }
                Direction::Outbound => {
                    expect_game_counts(spec, ledger, messages, 0)?;
                    expect_event_count(spec, ledger, 12, 0)?;
                }
            }
        }
        Workload::BinaryRelay {
            direction,
            messages,
            ..
        } => {
            expect_room(spec, ledger, 2)?;
            let messages = usize_to_u64(messages);
            match direction {
                Direction::Inbound => {
                    expect_game_counts(spec, ledger, 0, messages)?;
                    expect_event_count(spec, ledger, 13, messages)?;
                }
                Direction::Outbound => {
                    expect_game_counts(spec, ledger, messages, 0)?;
                    expect_event_count(spec, ledger, 13, 0)?;
                }
            }
        }
        Workload::Latest | Workload::Volatile => {
            expect_room(spec, ledger, 2)?;
            expect_game_counts(spec, ledger, 64, 0)?;
        }
        Workload::AuthorizedGap => {
            expect_room(spec, ledger, 2)?;
            expect_game_counts(spec, ledger, 0, 2)?;
            expect_event_count(spec, ledger, 12, 2)?;
            expect_event_count(spec, ledger, 24, 1)?;
            if ledger.reported_gap_sequences != 1 {
                return Err(format!("{} did not account for exactly one gap", spec.id));
            }
        }
        Workload::Reconnect { players } => {
            expect_event_count(spec, ledger, 7, 0)?;
            expect_event_count(spec, ledger, 26, 1)?;
            if ledger.roster_players != usize_to_u64(players)
                || ledger.sender_watermarks != usize_to_u64(players)
                || ledger.replayed_events != 2
                || !ledger.in_room
            {
                return Err(format!("{} reconnect evidence is incomplete", spec.id));
            }
            expect_game_counts(spec, ledger, 0, 0)?;
        }
        Workload::PollingReadyFrameBurst => {
            expect_room(spec, ledger, 2)?;
            expect_game_counts(spec, ledger, 17, 0)?;
        }
        Workload::PollingReadyByteBurst => {
            expect_room(spec, ledger, 2)?;
            expect_game_counts(spec, ledger, 4, 0)?;
        }
        Workload::PollingPendingRecovery => {
            expect_room(spec, ledger, 2)?;
            expect_game_counts(spec, ledger, 1, 0)?;
            if ledger.pending_sends != 1 {
                return Err(format!(
                    "{} expected one pre-acceptance Pending, found {}",
                    spec.id, ledger.pending_sends
                ));
            }
        }
    }
    Ok(())
}

fn validate_protocol_baseline(spec: WorkloadSpec, ledger: &WorkloadLedger) -> Result<(), String> {
    let baselines: ProtocolBaselines =
        serde_json::from_str(include_str!("../protocol-baselines.json"))
            .map_err(|error| format!("decode protocol baselines: {error}"))?;
    if baselines.schema != 1 || baselines.records.len() != WORKLOADS.len() {
        return Err("protocol baseline registry is incomplete".to_string());
    }
    let baseline = baselines
        .records
        .iter()
        .find(|baseline| baseline.workload == spec.id)
        .ok_or_else(|| format!("missing protocol baseline for {}", spec.id))?;
    let observed = deterministic_ledger_digest(ledger);
    if observed == baseline.sha256 {
        Ok(())
    } else {
        Err(format!(
            "{} protocol ledger digest changed: expected {:?}, observed {:?}",
            spec.id, baseline.sha256, observed
        ))
    }
}

fn observe_poll(
    client: &mut SignalFishPollingClient<DeterministicTransport>,
    events: &mut EventAccumulator,
    polls: &mut u64,
) -> Result<(), String> {
    let batch = client.poll();
    *polls = polls.saturating_add(1);
    for event in batch {
        events.observe(event)?;
    }
    Ok(())
}

fn expect_room(spec: WorkloadSpec, ledger: &WorkloadLedger, players: usize) -> Result<(), String> {
    expect_event_count(spec, ledger, 7, 1)?;
    if ledger.roster_players != usize_to_u64(players) || !ledger.in_room {
        return Err(format!("{} room roster evidence is incomplete", spec.id));
    }
    Ok(())
}

fn expect_game_counts(
    spec: WorkloadSpec,
    ledger: &WorkloadLedger,
    sent: u64,
    received: u64,
) -> Result<(), String> {
    if ledger.game_data_sent != sent || ledger.game_data_received != received {
        return Err(format!(
            "{} traffic ledger mismatch: expected sent/received {sent}/{received}, found {}/{}",
            spec.id, ledger.game_data_sent, ledger.game_data_received
        ));
    }
    Ok(())
}

fn expect_event_count(
    spec: WorkloadSpec,
    ledger: &WorkloadLedger,
    index: usize,
    expected: u64,
) -> Result<(), String> {
    let found = ledger
        .event_counts
        .get(index)
        .copied()
        .ok_or_else(|| format!("event index {index} is outside ledger"))?;
    if found != expected {
        return Err(format!(
            "{} event index {index}: expected {expected}, found {found}",
            spec.id
        ));
    }
    Ok(())
}

fn event_count(ledger: &WorkloadLedger, index: usize) -> Result<u64, String> {
    ledger
        .event_counts
        .get(index)
        .copied()
        .ok_or_else(|| format!("event index {index} is outside ledger"))
}

fn authenticated_message() -> ServerMessage {
    ServerMessage::Authenticated {
        app_name: "performance-lab".to_string(),
        organization: None,
        rate_limits: RateLimitInfo {
            per_minute: 1_000,
            per_hour: 10_000,
            per_day: 100_000,
        },
    }
}

fn protocol_info_message() -> ServerMessage {
    ServerMessage::ProtocolInfo(ProtocolInfoPayload {
        platform: None,
        sdk_version: None,
        minimum_version: None,
        recommended_version: None,
        capabilities: Vec::new(),
        notes: None,
        game_data_formats: vec![GameDataEncoding::Json, GameDataEncoding::MessagePack],
        player_name_rules: None,
        protocol_version: Some(3),
        min_protocol_version: Some(2),
        max_protocol_version: Some(3),
        transports: Some(vec![MessageTransport::Websocket]),
        max_outbound_message_size: Some(8 * 1024 * 1024),
    })
}

fn room_message(players: usize) -> ServerMessage {
    let players = players_fixture(players);
    ServerMessage::RoomJoined(Box::new(RoomJoinedPayload {
        room_id: PlayerId::from_u128(ROOM_ID),
        room_code: "PERF01".to_string(),
        player_id: local_id(),
        game_name: "performance-lab".to_string(),
        max_players: u8::try_from(players.len()).unwrap_or(u8::MAX),
        supports_authority: true,
        current_players: players,
        is_authority: true,
        lobby_state: LobbyState::Lobby,
        ready_players: Vec::new(),
        relay_type: "websocket".to_string(),
        current_spectators: Vec::new(),
        ice_servers: Vec::new(),
        reconnection_token: Some("initial-token".to_string()),
    }))
}

fn reconnect_message(players: usize) -> ServerMessage {
    let current_players = players_fixture(players);
    let sender_watermarks = current_players
        .iter()
        .map(|player| SenderWatermark {
            player_id: player.id,
            epoch: 1,
            seq: 0,
        })
        .collect();
    ServerMessage::Reconnected(Box::new(ReconnectedPayload {
        room_id: PlayerId::from_u128(ROOM_ID),
        room_code: "PERF01".to_string(),
        player_id: local_id(),
        game_name: "performance-lab".to_string(),
        max_players: u8::try_from(current_players.len()).unwrap_or(u8::MAX),
        supports_authority: true,
        current_players,
        is_authority: true,
        lobby_state: LobbyState::Finalized,
        ready_players: Vec::new(),
        relay_type: "websocket".to_string(),
        current_spectators: Vec::new(),
        ice_servers: Vec::new(),
        missed_events: vec![
            ServerMessage::LobbyStateChanged {
                lobby_state: LobbyState::Finalized,
                ready_players: Vec::new(),
                all_ready: false,
            },
            ServerMessage::AuthorityChanged {
                authority_player: Some(local_id()),
                you_are_authority: true,
            },
        ],
        replay: Some(ReplayStatus::Complete),
        sender_watermarks,
        reconnection_token: Some("rotated-token".to_string()),
    }))
}

#[allow(clippy::arithmetic_side_effects)] // `index` starts at 1: subtraction cannot underflow
fn players_fixture(players: usize) -> Vec<PlayerInfo> {
    (0..players)
        .map(|index| {
            let id = if index == 0 {
                local_id()
            } else {
                peer_id(index - 1)
            };
            PlayerInfo {
                id,
                name: if index == 0 {
                    "local".to_string()
                } else {
                    format!("peer-{index}")
                },
                is_authority: index == 0,
                is_ready: true,
                connected_at: "2026-01-01T00:00:00Z".to_string(),
                connection_info: None,
                epoch: Some(1),
                seq: Some(0),
            }
        })
        .collect()
}

fn local_id() -> PlayerId {
    PlayerId::from_u128(LOCAL_PLAYER)
}

fn peer_id(index: usize) -> PlayerId {
    PlayerId::from_u128(FIRST_PEER.saturating_add(index as u128))
}

fn push_text(
    incoming: &mut VecDeque<TransportFrame>,
    message: ServerMessage,
) -> Result<(), String> {
    let json = serde_json::to_string(&message)
        .map_err(|error| format!("serialize server fixture: {error}"))?;
    incoming.push_back(TransportFrame::Text(json));
    Ok(())
}

fn digest_frames<'a>(frames: impl IntoIterator<Item = &'a TransportFrame>) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for frame in frames {
        match frame {
            TransportFrame::Text(text) => {
                hasher.update([0]);
                hasher.update(usize_to_u64(text.len()).to_le_bytes());
                hasher.update(text.as_bytes());
            }
            TransportFrame::Binary(bytes) => {
                hasher.update([1]);
                hasher.update(usize_to_u64(bytes.len()).to_le_bytes());
                hasher.update(bytes);
            }
        }
    }
    hasher.finalize().into()
}

fn frame_len(frame: &TransportFrame) -> u64 {
    match frame {
        TransportFrame::Text(text) => usize_to_u64(text.len()),
        TransportFrame::Binary(bytes) => usize_to_u64(bytes.len()),
    }
}

fn logical_operations(workload: Workload) -> u64 {
    match workload {
        Workload::Lobby { .. } | Workload::Reconnect { .. } | Workload::AuthorizedGap => 1,
        Workload::JsonRelay { messages, .. } | Workload::BinaryRelay { messages, .. } => {
            usize_to_u64(messages)
        }
        Workload::Latest | Workload::Volatile => 64,
        Workload::PollingReadyFrameBurst => 17,
        Workload::PollingReadyByteBurst => 4,
        Workload::PollingPendingRecovery => 1,
    }
}

fn expected_inbound_frames(workload: Workload) -> u64 {
    match workload {
        Workload::Lobby { .. }
        | Workload::Latest
        | Workload::Volatile
        | Workload::PollingReadyFrameBurst
        | Workload::PollingReadyByteBurst
        | Workload::PollingPendingRecovery => 3,
        Workload::JsonRelay {
            direction: Direction::Inbound,
            messages,
            ..
        }
        | Workload::BinaryRelay {
            direction: Direction::Inbound,
            messages,
            ..
        } => 3u64.saturating_add(usize_to_u64(messages)),
        Workload::JsonRelay {
            direction: Direction::Outbound,
            ..
        }
        | Workload::BinaryRelay {
            direction: Direction::Outbound,
            ..
        } => 3,
        Workload::AuthorizedGap => 6,
        Workload::Reconnect { .. } => 3,
    }
}

fn expected_outbound_frames(workload: Workload) -> u64 {
    let workload_frames = match workload {
        Workload::JsonRelay {
            direction: Direction::Outbound,
            messages,
            ..
        }
        | Workload::BinaryRelay {
            direction: Direction::Outbound,
            messages,
            ..
        } => usize_to_u64(messages),
        Workload::Latest | Workload::Volatile => 64,
        Workload::PollingPendingRecovery => 1,
        Workload::PollingReadyFrameBurst => 17,
        Workload::PollingReadyByteBurst => 4,
        _ => 0,
    };
    2u64.saturating_add(workload_frames)
}

fn expected_send_budget_exhaustions(workload: Workload) -> u64 {
    match workload {
        Workload::JsonRelay {
            direction: Direction::Outbound,
            payload_bytes: 4_096,
            messages: 64,
        } => 4,
        Workload::BinaryRelay {
            direction: Direction::Outbound,
            payload_bytes: 4_096,
            messages: 64,
        }
        | Workload::PollingReadyByteBurst => 3,
        Workload::PollingReadyFrameBurst => 2,
        _ => 0,
    }
}

fn expected_receive_budget_exhaustions(workload: Workload) -> u64 {
    match workload {
        Workload::JsonRelay {
            direction: Direction::Inbound,
            payload_bytes: 4_096,
            messages: 64,
        }
        | Workload::BinaryRelay {
            direction: Direction::Inbound,
            payload_bytes: 4_096,
            messages: 64,
        } => 4,
        _ => 0,
    }
}

fn delivery_class_byte(class: Option<DeliveryClass>) -> u8 {
    match class {
        None => 0,
        Some(DeliveryClass::Reliable) => 1,
        Some(DeliveryClass::Latest) => 2,
        Some(DeliveryClass::Volatile) => 3,
    }
}

fn encoding_byte(encoding: GameDataEncoding) -> u8 {
    match encoding {
        GameDataEncoding::Json => 0,
        GameDataEncoding::MessagePack => 1,
        GameDataEncoding::Rkyv => 2,
    }
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn duration_ns(value: Duration) -> u64 {
    u64::try_from(value.as_nanos()).unwrap_or(u64::MAX)
}

fn serialize_event_counts<S>(
    counts: &[u64; EVENT_KIND_COUNT],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    counts.as_slice().serialize(serializer)
}

fn fingerprint_bytes(fingerprint: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *fingerprint ^= u64::from(*byte);
        *fingerprint = fingerprint.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

fn fingerprint_json(fingerprint: &mut u64, value: &serde_json::Value) -> Result<(), String> {
    match value {
        serde_json::Value::Null => fingerprint_bytes(fingerprint, &[0]),
        serde_json::Value::Bool(value) => {
            fingerprint_bytes(fingerprint, &[1, u8::from(*value)]);
        }
        serde_json::Value::Number(value) => {
            fingerprint_bytes(fingerprint, &[2]);
            if let Some(number) = value.as_i64() {
                fingerprint_bytes(fingerprint, &[0]);
                fingerprint_bytes(fingerprint, &number.to_le_bytes());
            } else if let Some(number) = value.as_u64() {
                fingerprint_bytes(fingerprint, &[1]);
                fingerprint_bytes(fingerprint, &number.to_le_bytes());
            } else if let Some(number) = value.as_f64() {
                fingerprint_bytes(fingerprint, &[2]);
                fingerprint_bytes(fingerprint, &number.to_bits().to_le_bytes());
            } else {
                return Err("JSON number had no supported representation".to_string());
            }
        }
        serde_json::Value::String(value) => {
            fingerprint_bytes(fingerprint, &[3]);
            fingerprint_bytes(fingerprint, &usize_to_u64(value.len()).to_le_bytes());
            fingerprint_bytes(fingerprint, value.as_bytes());
        }
        serde_json::Value::Array(values) => {
            fingerprint_bytes(fingerprint, &[4]);
            fingerprint_bytes(fingerprint, &usize_to_u64(values.len()).to_le_bytes());
            for value in values {
                fingerprint_json(fingerprint, value)?;
            }
        }
        serde_json::Value::Object(values) => {
            fingerprint_bytes(fingerprint, &[5]);
            fingerprint_bytes(fingerprint, &usize_to_u64(values.len()).to_le_bytes());
            for (key, value) in values {
                fingerprint_bytes(fingerprint, &usize_to_u64(key.len()).to_le_bytes());
                fingerprint_bytes(fingerprint, key.as_bytes());
                fingerprint_json(fingerprint, value)?;
            }
        }
    }
    Ok(())
}

fn fingerprint_serializable<T: Serialize>(
    fingerprint: &mut u64,
    kind: u8,
    value: &T,
) -> Result<(), String> {
    fingerprint_bytes(fingerprint, &[kind]);
    let value = serde_json::to_value(value)
        .map_err(|error| format!("serialize event fingerprint: {error}"))?;
    fingerprint_json(fingerprint, &value)
}

fn snapshot_fingerprint(snapshot: &signal_fish_client::ClientSnapshot) -> Result<u64, String> {
    let mut fingerprint = 0xcbf2_9ce4_8422_2325;
    fingerprint_serializable(
        &mut fingerprint,
        40,
        &(
            snapshot.connected,
            snapshot.transport_ready,
            snapshot.authenticated,
            snapshot.negotiated_protocol_version,
            snapshot.requested_game_data_format,
            snapshot.effective_game_data_format,
            snapshot.player_id,
            snapshot.room_id,
            &snapshot.room_code,
        ),
    )?;
    fingerprint_bytes(
        &mut fingerprint,
        &[match snapshot.room_role {
            None => 0,
            Some(RoomRole::Player) => 1,
            Some(RoomRole::Spectator) => 2,
        }],
    );
    fingerprint_serializable(
        &mut fingerprint,
        41,
        &(
            snapshot.session_generation,
            snapshot.session_topology,
            snapshot.session_transport,
            &snapshot.reconnection_token,
            snapshot.quarantined,
        ),
    )?;
    Ok(fingerprint)
}

fn expected_setup_frames(workload: Workload) -> u64 {
    match workload {
        Workload::Lobby { .. } | Workload::Reconnect { .. } => 2,
        _ => 3,
    }
}

fn expected_measured_events(workload: Workload) -> usize {
    match workload {
        Workload::Lobby { .. } | Workload::Reconnect { .. } => 1,
        Workload::JsonRelay {
            direction: Direction::Inbound,
            messages,
            ..
        }
        | Workload::BinaryRelay {
            direction: Direction::Inbound,
            messages,
            ..
        } => messages,
        Workload::AuthorizedGap => 3,
        _ => 0,
    }
}

fn expected_setup_event_count(workload: Workload) -> usize {
    if matches!(
        workload,
        Workload::Lobby { .. } | Workload::Reconnect { .. }
    ) {
        3
    } else {
        4
    }
}

fn validate_setup_events(spec: WorkloadSpec, events: &EventAccumulator) -> Result<(), String> {
    for (index, expected) in [(0, 1), (4, 1), (5, 1)] {
        let found = events
            .counts
            .get(index)
            .copied()
            .ok_or_else(|| format!("setup event index {index} is outside ledger"))?;
        if found != expected {
            return Err(format!(
                "{} setup event index {index}: expected {expected}, found {found}",
                spec.id
            ));
        }
    }
    let expected_room = u64::from(!matches!(
        spec.workload,
        Workload::Lobby { .. } | Workload::Reconnect { .. }
    ));
    let room_events = events
        .counts
        .get(7)
        .copied()
        .ok_or_else(|| "room setup event index is outside ledger".to_string())?;
    if room_events != expected_room {
        return Err(format!("{} setup room event count is not exact", spec.id));
    }
    if events
        .counts
        .iter()
        .enumerate()
        .any(|(index, count)| !matches!(index, 0 | 4 | 5 | 7) && *count != 0)
    {
        return Err(format!("{} setup emitted an unexpected event", spec.id));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_workload_has_exact_non_vacuous_evidence() -> Result<(), String> {
        for spec in WORKLOADS {
            let ledger = execute_once(spec)?;
            assert!(ledger.logical_operations > 0, "{} is vacuous", spec.id);
            assert!(ledger.inbound_frames > 0, "{} read no frames", spec.id);
            assert!(ledger.outbound_frames > 0, "{} wrote no frames", spec.id);
            assert_ne!(
                ledger.inbound_wire_sha256, [0; 32],
                "{} input digest",
                spec.id
            );
            assert_ne!(
                ledger.outbound_wire_sha256, [0; 32],
                "{} output digest",
                spec.id
            );
        }
        Ok(())
    }

    #[test]
    fn workload_ids_are_unique() {
        for (index, spec) in WORKLOADS.iter().enumerate() {
            assert!(
                WORKLOADS
                    .iter()
                    .skip(index + 1)
                    .all(|other| other.id != spec.id),
                "duplicate workload id {}",
                spec.id
            );
        }
    }

    #[test]
    fn exact_verifier_rejects_corrupted_evidence() -> Result<(), String> {
        let spec = WORKLOADS[3];
        let ledger = execute_once(spec)?;

        let mut wrong_traffic = ledger.clone();
        wrong_traffic.game_data_received = wrong_traffic.game_data_received.saturating_add(1);
        assert!(validate_ledger(spec, &wrong_traffic).is_err());

        let mut wrong_event = ledger.clone();
        let game_data_events = wrong_event
            .event_counts
            .get_mut(12)
            .ok_or_else(|| "game-data event slot is missing".to_string())?;
        *game_data_events = game_data_events.saturating_add(1);
        assert!(validate_ledger(spec, &wrong_event).is_err());

        let mut unexpected_event = ledger.clone();
        let pong_events = unexpected_event
            .event_counts
            .get_mut(25)
            .ok_or_else(|| "Pong event slot is missing".to_string())?;
        *pong_events = pong_events.saturating_add(1);
        assert!(validate_ledger(spec, &unexpected_event).is_err());

        let mut wrong_snapshot = ledger.clone();
        wrong_snapshot.authenticated = false;
        assert!(validate_ledger(spec, &wrong_snapshot).is_err());

        let mut wrong_queue = ledger.clone();
        wrong_queue.final_queue_depth = 1;
        assert!(validate_ledger(spec, &wrong_queue).is_err());

        let mut wrong_wire = ledger.clone();
        wrong_wire.inbound_wire_sha256 = [0; 32];
        assert!(validate_ledger(spec, &wrong_wire).is_err());

        let mut wrong_payload = ledger.clone();
        wrong_payload.event_payload_fingerprint =
            wrong_payload.event_payload_fingerprint.saturating_add(1);
        assert!(validate_ledger(spec, &wrong_payload).is_err());

        let mut wrong_snapshot_fingerprint = ledger;
        wrong_snapshot_fingerprint.snapshot_fingerprint = wrong_snapshot_fingerprint
            .snapshot_fingerprint
            .saturating_add(1);
        assert!(validate_ledger(spec, &wrong_snapshot_fingerprint).is_err());
        Ok(())
    }

    #[test]
    fn reconnect_verifier_rejects_replay_and_watermark_corruption() -> Result<(), String> {
        let spec = WORKLOADS[23];
        let ledger = execute_once(spec)?;

        let mut wrong_replay = ledger.clone();
        wrong_replay.replayed_events = wrong_replay.replayed_events.saturating_add(1);
        assert!(validate_ledger(spec, &wrong_replay).is_err());

        let mut wrong_watermarks = ledger;
        wrong_watermarks.sender_watermarks = wrong_watermarks.sender_watermarks.saturating_sub(1);
        assert!(validate_ledger(spec, &wrong_watermarks).is_err());
        Ok(())
    }

    #[test]
    fn deterministic_digest_covers_protocol_evidence_but_not_wall_clock_age() -> Result<(), String>
    {
        let spec = WORKLOADS[25];
        let ledger = execute_once(spec)?;
        let expected = deterministic_ledger_digest(&ledger);

        let mut changed_protocol = ledger.clone();
        changed_protocol.send_budget_exhaustions =
            changed_protocol.send_budget_exhaustions.saturating_add(1);
        assert_ne!(deterministic_ledger_digest(&changed_protocol), expected);

        let mut changed_clock = ledger;
        changed_clock.peak_queue_age_ns = changed_clock.peak_queue_age_ns.saturating_add(1);
        assert_eq!(deterministic_ledger_digest(&changed_clock), expected);
        Ok(())
    }
}
