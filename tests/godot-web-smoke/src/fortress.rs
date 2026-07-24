use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fortress_rollback::network::codec::{decode_message, encode};
use fortress_rollback::{
    compute_checksum, Config, DesyncDetection, FortressEvent, FortressRequest, Frame, Message,
    NonBlockingSocket, P2PSession, PlayerHandle, PlayerType, SessionBuilder, SessionMetrics,
    SessionState, SyncHealth,
};
use godot::prelude::*;
use serde::{Deserialize, Serialize};
use signal_fish_client::protocol::{GameDataEncoding, PlayerId, PlayerInfo};
use signal_fish_client::{
    JoinRoomParams, SignalFishConfig, SignalFishError, SignalFishEvent, SignalFishPollingClient,
};
use signal_fish_client_godot::GodotWebSocketTransport;

const SERVER_URL: &str = "ws://127.0.0.1:3536/v2/ws";
const APP_ID: &str = "e2e-test-app";
const GAME_NAME: &str = "godot-fortress-issue-61";
const DEFAULT_TARGET_FRAMES: i32 = 600;
const SETTLEMENT_FRAMES: i32 = 20;
const MAX_RELAY_QUEUE: usize = 256;
const IMPAIRMENT_START_FRAME: i32 = 120;
const INPUT_DELAY_FRAMES: i32 = 2;
const IMPAIRMENT_MAX_HOLD_FRAMES: i32 = 8;
const CLEAN_LAG_LIMIT: u64 = 8;
const IMPAIRED_LAG_LIMIT: u64 = 13;
const PREDICTION_WINDOW_FRAMES: usize = 20;
// Browser startup includes renderer/JIT warm-up that is distinct from sustained
// rollback behavior. Keep that phase explicitly bounded by the prediction
// window, then enforce the scenario lag limit from frame 61 onward.
const CONFIRMATION_LAG_WARMUP_FRAMES: i32 = 60;
const SIMULATION_FPS: usize = 18;
const RELAY_ENVELOPE_MAGIC: &[u8; 4] = b"SFF1";
const STARTUP_ENVELOPE_MAGIC: &[u8; 4] = b"SFS1";
const STARTUP_LEAD_MS: u64 = 2_000;
const STARTUP_MAX_LEAD: Duration = Duration::from_secs(3);
const STARTUP_MIN_PROPOSAL_LEAD: Duration = Duration::from_millis(1_250);
const STARTUP_MIN_ACK_LEAD: Duration = Duration::from_millis(750);
const STARTUP_MIN_COMMIT_LEAD: Duration = Duration::from_millis(250);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
// GitHub's shared runners have measured at roughly 23 rendered frames/second
// under two independent Chromium/Godot processes. Keep a generous wall-clock
// guard while the frame/checksum/queue oracles remain exact.
const DEFAULT_SESSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(40);
const TEARDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

type Client = SignalFishPollingClient<GodotWebSocketTransport>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
struct TestInput {
    value: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
struct TestState {
    frame: i32,
    value: u64,
}

#[derive(Debug)]
struct TestConfig;

impl Config for TestConfig {
    type Input = TestInput;
    type State = TestState;
    type Address = PlayerId;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupControl {
    Proposal { start_unix_ms: u64 },
    Ack { start_unix_ms: u64 },
    Commit { start_unix_ms: u64 },
}

impl StartupControl {
    fn start_unix_ms(self) -> u64 {
        match self {
            Self::Proposal { start_unix_ms }
            | Self::Ack { start_unix_ms }
            | Self::Commit { start_unix_ms } => start_unix_ms,
        }
    }
}

fn startup_control_allowed(
    role: &str,
    control: StartupControl,
    expected_start_unix_ms: Option<u64>,
    proposal_sent: bool,
    ack_sent: bool,
) -> bool {
    let exact_deadline = expected_start_unix_ms == Some(control.start_unix_ms());
    match (role, control) {
        ("a", StartupControl::Proposal { .. }) => {
            expected_start_unix_ms.is_none() || exact_deadline
        }
        ("a", StartupControl::Commit { .. }) => ack_sent && exact_deadline,
        ("b", StartupControl::Ack { .. }) => proposal_sent && exact_deadline,
        _ => false,
    }
}

fn startup_control_lead_valid(
    deadline: Option<Instant>,
    now: Instant,
    minimum_lead: Duration,
    already_received: bool,
) -> bool {
    already_received
        || deadline.is_some_and(|deadline| deadline.saturating_duration_since(now) >= minimum_lead)
}

#[derive(Default)]
struct RelayQueues {
    inbound: VecDeque<(PlayerId, Message)>,
    outbound: VecDeque<(PlayerId, Vec<u8>)>,
    encoded: u64,
    decoded: u64,
    malformed: u64,
    ignored_nonlocal: u64,
    post_ready_drained: u64,
    dropped: u64,
    retries: u64,
    peak_inbound: usize,
    peak_outbound: usize,
    local_frame: Option<i32>,
    remote_frame: Option<i32>,
}

#[derive(Clone)]
struct RelaySocket {
    queues: Rc<RefCell<RelayQueues>>,
}

impl NonBlockingSocket<PlayerId> for RelaySocket {
    fn send_to(&mut self, message: &Message, destination: &PlayerId) {
        let mut queues = self.queues.borrow_mut();
        if queues.outbound.len() >= MAX_RELAY_QUEUE {
            queues.dropped = queues.dropped.saturating_add(1);
            return;
        }
        match encode(message) {
            Ok(encoded) => {
                let payload = encode_relay_envelope(
                    destination.as_bytes(),
                    queues.local_frame.unwrap_or(Frame::NULL.as_i32()),
                    &encoded,
                );
                queues.outbound.push_back((*destination, payload));
                queues.encoded = queues.encoded.saturating_add(1);
                queues.peak_outbound = queues.peak_outbound.max(queues.outbound.len());
            }
            Err(_) => queues.dropped = queues.dropped.saturating_add(1),
        }
    }

    fn receive_all_messages(&mut self) -> Vec<(PlayerId, Message)> {
        self.queues.borrow_mut().inbound.drain(..).collect()
    }
}

pub(super) struct FortressScenario {
    role: String,
    requested_room_code: Option<String>,
    target_frames: i32,
    settlement_frame_limit: i32,
    session_timeout: std::time::Duration,
    poll_hitch_frame: Option<i32>,
    poll_hitch_callbacks_remaining: u8,
    poll_hitch_completed: bool,
    poll_hitch_frames_advanced: u64,
    joined_room_code: Option<String>,
    client: Option<Client>,
    local_id: Option<PlayerId>,
    remote_id: Option<PlayerId>,
    players: Vec<PlayerInfo>,
    session: Option<P2PSession<TestConfig>>,
    session_finished: bool,
    final_metrics: Option<SessionMetrics>,
    final_sync_in_sync: bool,
    pre_impairment_metrics: Option<SessionMetrics>,
    local_handle: Option<PlayerHandle>,
    remote_handle: Option<PlayerHandle>,
    relay: Rc<RefCell<RelayQueues>>,
    game: TestState,
    checksum_through: i32,
    confirmed_checksum: u64,
    target_state_checksum: Option<u128>,
    startup_started_at: Option<Instant>,
    started: Option<Instant>,
    simulation_elapsed_ms: Option<u128>,
    next_advance_at: Option<Instant>,
    startup_barrier_completed: bool,
    startup_start_unix_ms: Option<u64>,
    startup_start_at: Option<Instant>,
    startup_proposal_sent: bool,
    startup_proposal_received: bool,
    startup_ack_sent: bool,
    startup_ack_received: bool,
    startup_commit_sent: bool,
    startup_commit_received: bool,
    startup_barrier_release_local_frame: Option<i32>,
    startup_barrier_elapsed_ms: Option<u128>,
    startup_release_lateness_ms: Option<u128>,
    game_ready: bool,
    max_poll_us: u64,
    last_time_series_frame: i32,
    last_accepted: u64,
    multi_frame_poll: bool,
    confirmation_lag_warmup_max: u64,
    confirmation_lag_steady_max: u64,
    completed: bool,
    target_confirmed: bool,
    game_ready_at: Option<Instant>,
    settle_started: Option<Instant>,
    closing_success: bool,
    impairment_activated: bool,
    impairment_released: bool,
    save_requests: u64,
    load_requests: u64,
    advance_requests: u64,
    desync_events: u64,
    peer_left_observed: bool,
    peer_left_epoch: Option<u32>,
    peer_left_final_seq: Option<u64>,
    fatal: Option<String>,
}

impl FortressScenario {
    pub(super) fn from_user_args(args: &[String]) -> Option<Self> {
        let role = argument(args, "--fortress-role")?;
        let requested_room_code = argument(args, "--room-code");
        let target_frames = argument(args, "--target-frames")
            .and_then(|value| value.parse::<i32>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_TARGET_FRAMES);
        let settlement_frame_limit = target_frames.saturating_add(SETTLEMENT_FRAMES);
        let session_timeout = argument(args, "--session-timeout-ms")
            .and_then(|value| value.parse::<u64>().ok())
            .map(std::time::Duration::from_millis)
            .unwrap_or(DEFAULT_SESSION_TIMEOUT);
        let poll_hitch_frame = argument(args, "--poll-hitch-frame")
            .and_then(|value| value.parse::<i32>().ok())
            .filter(|value| *value >= 0);
        let server_url = argument(args, "--server-url").unwrap_or_else(|| SERVER_URL.into());
        let configuration_error = match (role.as_str(), requested_room_code.as_ref()) {
            ("a", None) | ("b", Some(_)) => None,
            ("a", Some(_)) => Some("role a must create a room without --room-code".to_string()),
            ("b", None) => Some("role b requires the creator's --room-code".to_string()),
            _ => Some(format!("unsupported Fortress role: {role}")),
        };
        let (client, fatal) = if let Some(error) = configuration_error {
            godot_error!("SIGNAL_FISH_FORTRESS startup-error {error}");
            (None, Some(error))
        } else {
            match GodotWebSocketTransport::connect(&server_url) {
                Ok(transport) => {
                    let mut config = SignalFishConfig::new(APP_ID).enable_v3();
                    config.platform = Some(format!("godot-fortress-{role}"));
                    config.game_data_format = Some(GameDataEncoding::MessagePack);
                    (Some(SignalFishPollingClient::new(transport, config)), None)
                }
                Err(error) => {
                    godot_error!("SIGNAL_FISH_FORTRESS startup-error {error}");
                    (None, Some(format!("transport startup failed: {error}")))
                }
            }
        };
        godot_print!("SIGNAL_FISH_FORTRESS fixture-ready role={role}");
        Some(Self {
            role,
            requested_room_code,
            target_frames,
            settlement_frame_limit,
            session_timeout,
            poll_hitch_frame,
            poll_hitch_callbacks_remaining: u8::from(poll_hitch_frame.is_some()) * 6,
            poll_hitch_completed: false,
            poll_hitch_frames_advanced: 0,
            joined_room_code: None,
            client,
            local_id: None,
            remote_id: None,
            players: Vec::new(),
            session: None,
            session_finished: false,
            final_metrics: None,
            final_sync_in_sync: false,
            pre_impairment_metrics: None,
            local_handle: None,
            remote_handle: None,
            relay: Rc::new(RefCell::new(RelayQueues::default())),
            game: TestState::default(),
            checksum_through: -1,
            confirmed_checksum: 0,
            target_state_checksum: None,
            startup_started_at: None,
            started: None,
            simulation_elapsed_ms: None,
            next_advance_at: None,
            startup_barrier_completed: false,
            startup_start_unix_ms: None,
            startup_start_at: None,
            startup_proposal_sent: false,
            startup_proposal_received: false,
            startup_ack_sent: false,
            startup_ack_received: false,
            startup_commit_sent: false,
            startup_commit_received: false,
            startup_barrier_release_local_frame: None,
            startup_barrier_elapsed_ms: None,
            startup_release_lateness_ms: None,
            game_ready: false,
            max_poll_us: 0,
            last_time_series_frame: -60,
            last_accepted: 0,
            multi_frame_poll: false,
            confirmation_lag_warmup_max: 0,
            confirmation_lag_steady_max: 0,
            completed: false,
            target_confirmed: false,
            game_ready_at: None,
            settle_started: None,
            closing_success: false,
            impairment_activated: false,
            impairment_released: false,
            save_requests: 0,
            load_requests: 0,
            advance_requests: 0,
            desync_events: 0,
            peer_left_observed: false,
            peer_left_epoch: None,
            peer_left_final_seq: None,
            fatal,
        })
    }

    pub(super) fn process(&mut self) -> bool {
        if self.completed {
            return true;
        }
        let skipped_poll_for_hitch = self.should_skip_poll_for_hitch();
        let events = if skipped_poll_for_hitch {
            Vec::new()
        } else {
            self.poll_client_once()
        };
        for event in events {
            self.handle_event(event);
        }
        if self.fatal.is_none() && !self.closing_success {
            self.start_session_if_ready();
            if !(self.game_ready && self.peer_left_observed) {
                let advances_before = self.advance_requests;
                let visual_frames_before = self
                    .session
                    .as_ref()
                    .map_or(0, |session| session.metrics().visual_frames);
                self.drive_session();
                if skipped_poll_for_hitch {
                    let visual_frames_after = self
                        .session
                        .as_ref()
                        .map_or(visual_frames_before, |session| {
                            session.metrics().visual_frames
                        });
                    self.poll_hitch_frames_advanced = self
                        .poll_hitch_frames_advanced
                        .saturating_add(visual_frames_after.saturating_sub(visual_frames_before));
                }
                debug_assert!(self.advance_requests >= advances_before);
            }
            self.stop_game_session_if_ready();
            self.drain_post_ready_relay();
            self.pump_outbound();
        }
        if self.fatal.is_none() {
            self.drive_successful_close();
        }
        if self.fatal.is_some() {
            self.finish(false);
        }
        self.completed
    }

    fn poll_client_once(&mut self) -> Vec<SignalFishEvent> {
        let Some(client) = &mut self.client else {
            return Vec::new();
        };
        let started = Instant::now();
        let events = client.poll();
        self.max_poll_us = self
            .max_poll_us
            .max(started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64);
        let accepted = client.transport_diagnostics().accepted_frames;
        self.multi_frame_poll |= accepted.saturating_sub(self.last_accepted) > 1;
        self.last_accepted = accepted;
        events
    }

    fn should_skip_poll_for_hitch(&mut self) -> bool {
        let Some(hitch_frame) = self.poll_hitch_frame else {
            return false;
        };
        let current_frame = self
            .session
            .as_ref()
            .map_or(-1, |session| session.current_frame().as_i32());
        if current_frame < hitch_frame || self.poll_hitch_callbacks_remaining == 0 {
            return false;
        }
        self.poll_hitch_callbacks_remaining = self.poll_hitch_callbacks_remaining.saturating_sub(1);
        if self.poll_hitch_callbacks_remaining == 0 {
            self.poll_hitch_completed = true;
        }
        true
    }

    fn handle_event(&mut self, event: SignalFishEvent) {
        match event {
            SignalFishEvent::Authenticated { .. } => {
                let mut params = JoinRoomParams::new(GAME_NAME, format!("Fortress-{}", self.role));
                if let Some(room_code) = self.requested_room_code.as_deref() {
                    params = params.with_room_code(room_code);
                }
                if let Some(client) = &mut self.client {
                    if let Err(error) = client.join_room(params) {
                        self.fail(format!("join failed: {error}"));
                    }
                }
            }
            SignalFishEvent::RoomJoined {
                room_code,
                player_id,
                current_players,
                ..
            } => {
                if self.role == "b"
                    && self.requested_room_code.as_deref() != Some(room_code.as_str())
                {
                    self.fail(format!(
                        "joined room {room_code}, expected {}",
                        self.requested_room_code.as_deref().unwrap_or("<missing>")
                    ));
                    return;
                }
                self.local_id = Some(player_id);
                self.players = current_players;
                self.joined_room_code = Some(room_code.clone());
                godot_print!(
                    "SIGNAL_FISH_FORTRESS room-joined role={} room_code={room_code} roster_count={}",
                    self.role,
                    self.players.len()
                );
            }
            SignalFishEvent::PlayerJoined { player } => {
                if !self.players.iter().any(|existing| existing.id == player.id) {
                    self.players.push(player);
                }
            }
            SignalFishEvent::PlayerLeft {
                player_id,
                epoch,
                final_seq,
            } if Some(player_id) == self.remote_id => {
                if epoch.is_none_or(|value| value == 0) || final_seq.is_none_or(|value| value == 0)
                {
                    self.fail("PlayerLeft omitted its v3 terminal watermark".to_string());
                    return;
                }
                self.peer_left_observed = true;
                self.peer_left_epoch = epoch;
                self.peer_left_final_seq = final_seq;
            }
            SignalFishEvent::GameDataBinary {
                from_player,
                encoding,
                payload,
                seq,
                epoch,
            } => self.receive_relay(from_player, encoding, payload, seq, epoch),
            SignalFishEvent::ProtocolViolation { diagnostic, .. } => {
                self.fail(format!("Signal Fish protocol violation: {diagnostic}"));
            }
            SignalFishEvent::DecodeFailed { error, .. } => {
                self.fail(format!("Signal Fish decode failure: {error}"));
            }
            SignalFishEvent::Disconnected { reason, .. }
                if !self.completed && !self.closing_success =>
            {
                self.fail(format!("unexpected disconnect: {reason:?}"));
            }
            _ => {}
        }
    }

    fn receive_relay(
        &mut self,
        sender: PlayerId,
        encoding: GameDataEncoding,
        payload: Vec<u8>,
        seq: Option<u64>,
        epoch: Option<u32>,
    ) {
        let Some(local_id) = self.local_id else {
            return;
        };
        let valid_sender = self.players.iter().any(|player| player.id == sender);
        let valid_envelope = encoding == GameDataEncoding::MessagePack
            && seq.is_some_and(|value| value > 0)
            && epoch.is_some_and(|value| value > 0)
            && valid_sender;
        if !valid_envelope {
            self.note_malformed();
            return;
        }
        if let Some((destination, control)) = decode_startup_envelope(&payload) {
            if destination != local_id.as_bytes() {
                let ignored = self.relay.borrow().ignored_nonlocal.saturating_add(1);
                self.relay.borrow_mut().ignored_nonlocal = ignored;
                return;
            }
            let mut relay = self.relay.borrow_mut();
            relay.decoded = relay.decoded.saturating_add(1);
            drop(relay);
            if self.remote_id != Some(sender) {
                self.fail("startup control did not originate from the session peer".to_string());
                return;
            }
            self.handle_startup_control(control);
            return;
        }
        let Some((destination, advertised_frame, encoded)) = decode_relay_envelope(&payload) else {
            self.note_malformed();
            return;
        };
        if destination != local_id.as_bytes() {
            let ignored = self.relay.borrow().ignored_nonlocal.saturating_add(1);
            self.relay.borrow_mut().ignored_nonlocal = ignored;
            return;
        }
        {
            let mut queues = self.relay.borrow_mut();
            queues.remote_frame = Some(
                queues
                    .remote_frame
                    .map_or(advertised_frame, |current| current.max(advertised_frame)),
            );
        }
        match decode_message(encoded) {
            Ok((message, consumed)) if consumed == encoded.len() => {
                let mut queues = self.relay.borrow_mut();
                if queues.inbound.len() >= MAX_RELAY_QUEUE {
                    queues.dropped = queues.dropped.saturating_add(1);
                } else {
                    queues.inbound.push_back((sender, message));
                    queues.decoded = queues.decoded.saturating_add(1);
                    queues.peak_inbound = queues.peak_inbound.max(queues.inbound.len());
                }
            }
            _ => self.note_malformed(),
        }
    }

    fn handle_startup_control(&mut self, control: StartupControl) {
        let start_unix_ms = control.start_unix_ms();
        if !startup_control_allowed(
            &self.role,
            control,
            self.startup_start_unix_ms,
            self.startup_proposal_sent,
            self.startup_ack_sent,
        ) {
            self.fail(format!(
                "unexpected startup control for role {}: {control:?}",
                self.role
            ));
            return;
        }
        match (&*self.role, control) {
            ("a", StartupControl::Proposal { .. }) => {
                if self.startup_start_at.is_none() {
                    match instant_for_unix_ms(start_unix_ms) {
                        Some(deadline) => {
                            let remaining = deadline.saturating_duration_since(Instant::now());
                            if !(STARTUP_MIN_PROPOSAL_LEAD..=STARTUP_MAX_LEAD).contains(&remaining)
                            {
                                self.fail(
                                    "startup proposal has an unsafe deadline lead".to_string(),
                                );
                                return;
                            }
                            self.startup_start_unix_ms = Some(start_unix_ms);
                            self.startup_start_at = Some(deadline);
                        }
                        None => {
                            self.fail("startup proposal arrived after its deadline".to_string());
                            return;
                        }
                    }
                }
                self.startup_proposal_received = true;
            }
            ("a", StartupControl::Commit { .. })
                if self.startup_ack_sent && self.startup_start_unix_ms == Some(start_unix_ms) =>
            {
                if !startup_control_lead_valid(
                    self.startup_start_at,
                    Instant::now(),
                    STARTUP_MIN_COMMIT_LEAD,
                    self.startup_commit_received,
                ) {
                    self.fail("startup commit arrived too near its common deadline".to_string());
                    return;
                }
                self.startup_commit_received = true;
            }
            ("b", StartupControl::Ack { .. })
                if self.startup_proposal_sent
                    && self.startup_start_unix_ms == Some(start_unix_ms) =>
            {
                if !startup_control_lead_valid(
                    self.startup_start_at,
                    Instant::now(),
                    STARTUP_MIN_ACK_LEAD,
                    self.startup_ack_received,
                ) {
                    self.fail(
                        "startup acknowledgement arrived too near its common deadline".to_string(),
                    );
                    return;
                }
                self.startup_ack_received = true;
            }
            _ => self.fail("startup control admission mismatch".to_string()),
        }
    }

    fn note_malformed(&mut self) {
        let malformed = self.relay.borrow().malformed.saturating_add(1);
        self.relay.borrow_mut().malformed = malformed;
    }

    fn start_session_if_ready(&mut self) {
        let Some(local_id) = self.local_id else {
            return;
        };
        if self.session.is_some() || self.session_finished {
            return;
        }
        let mut remote_ids = self
            .players
            .iter()
            .map(|player| player.id)
            .filter(|player_id| *player_id != local_id)
            .collect::<Vec<_>>();
        remote_ids.sort_unstable();
        remote_ids.dedup();
        let [remote_id] = remote_ids.as_slice() else {
            return;
        };
        let remote_id = *remote_id;
        let mut roster = [local_id, remote_id];
        roster.sort_unstable();
        let local_index = usize::from(roster[1] == local_id);
        let remote_index = 1usize.saturating_sub(local_index);
        let local_handle = PlayerHandle::new(local_index);
        let remote_handle = PlayerHandle::new(remote_index);
        let builder = SessionBuilder::<TestConfig>::new()
            .with_num_players(2)
            .and_then(|builder| builder.with_fps(SIMULATION_FPS))
            .and_then(|builder| builder.with_input_delay(INPUT_DELAY_FRAMES as usize))
            // Keep Fortress's internal stop threshold above the declared
            // constrained-network lag limit. Scenario oracles independently
            // enforce tighter clean/impaired bounds, so acceptable lag does
            // not manufacture a wait or stall by exhausting this window.
            .map(|builder| builder.with_max_prediction_window(PREDICTION_WINDOW_FRAMES))
            .and_then(|builder| builder.add_player(PlayerType::Local, local_handle))
            .and_then(|builder| builder.add_player(PlayerType::Remote(remote_id), remote_handle))
            .map(|builder| {
                builder.with_desync_detection_mode(DesyncDetection::On { interval: 60 })
            });
        match builder.and_then(|builder| {
            builder.start_p2p_session(RelaySocket {
                queues: Rc::clone(&self.relay),
            })
        }) {
            Ok(session) => {
                self.session = Some(session);
                self.local_handle = Some(local_handle);
                self.remote_handle = Some(remote_handle);
                self.remote_id = Some(remote_id);
                self.startup_started_at = Some(Instant::now());
                godot_print!(
                    "SIGNAL_FISH_FORTRESS session-created role={} local_handle={local_index}",
                    self.role
                );
            }
            Err(error) => self.fail(format!("Fortress session creation failed: {error}")),
        }
    }

    fn drive_session(&mut self) {
        let Some(session) = &mut self.session else {
            return;
        };
        self.relay.borrow_mut().local_frame = Some(session.current_frame().as_i32());
        session.poll_remote_clients();
        let confirmed = session.confirmed_frame().as_i32().min(self.target_frames);
        while self.checksum_through < confirmed {
            let frame = self.checksum_through.saturating_add(1);
            match session.confirmed_inputs_for_frame(Frame::new(frame)) {
                Ok(inputs) => {
                    self.confirmed_checksum = inputs.iter().enumerate().fold(
                        self.confirmed_checksum.wrapping_mul(1_099_511_628_211),
                        |checksum, (index, input)| {
                            checksum
                                .wrapping_add(u64::from(input.value))
                                .wrapping_add((index as u64).wrapping_mul(16777619))
                                .wrapping_add(frame as u64)
                        },
                    );
                    self.checksum_through = frame;
                }
                Err(error) => {
                    self.fatal = Some(format!(
                        "confirmed input checksum failed at frame {frame}: {error}"
                    ));
                    return;
                }
            }
        }
        for event in session.events() {
            if let event @ (FortressEvent::DesyncDetected { .. }
            | FortressEvent::Disconnected { .. }
            | FortressEvent::IncompatibleSession { .. }
            | FortressEvent::SyncTimeout { .. }) = event
            {
                if matches!(event, FortressEvent::DesyncDetected { .. }) {
                    self.desync_events = self.desync_events.saturating_add(1);
                }
                self.fatal = Some(format!("Fortress terminal event: {event:?}"));
                return;
            }
        }
        let metrics = session.metrics();
        let sample_frame = session.current_frame().as_i32();
        if confirmation_lag_is_warmup(sample_frame) {
            self.confirmation_lag_warmup_max = self
                .confirmation_lag_warmup_max
                .max(metrics.confirmation_lag_current);
        } else {
            self.confirmation_lag_steady_max = self
                .confirmation_lag_steady_max
                .max(metrics.confirmation_lag_current);
        }
        if sample_frame >= self.last_time_series_frame.saturating_add(60) {
            self.last_time_series_frame = sample_frame;
            let queue_depth = self
                .client
                .as_ref()
                .map_or(0, |client| client.polling_stats().current_queue_depth);
            let queue_age_ms = self.client.as_ref().map_or(0.0, |client| {
                client
                    .queue_age_stats()
                    .current_oldest_queue_age
                    .as_secs_f64()
                    * 1_000.0
            });
            let sample = serde_json::json!({
                "role": self.role,
                "elapsed_ms": self.started.map_or(0, |started| started.elapsed().as_millis()),
                "current_frame": sample_frame,
                "confirmed_frame": session.confirmed_frame().as_i32(),
                "confirmation_lag": metrics.confirmation_lag_current,
                "confirmation_lag_max": metrics.confirmation_lag_max,
                "queue_depth": queue_depth,
                "queue_age_ms": queue_age_ms,
            });
            godot_print!("SIGNAL_FISH_FORTRESS sample {sample}");
        }
        let sync_health = self
            .remote_handle
            .and_then(|handle| session.sync_health(handle));
        if self
            .started
            .is_some_and(|started| started.elapsed() >= self.session_timeout)
        {
            self.fatal = Some(format!(
                "Fortress settlement timed out: confirmed={} checksum_through={} target_checksum={} sync_health={sync_health:?} checks={}",
                session.confirmed_frame().as_i32(),
                self.checksum_through,
                self.target_state_checksum.is_some(),
                metrics.checksums_compared,
            ));
            return;
        }
        let now = Instant::now();
        if !self.startup_barrier_completed
            && self
                .startup_started_at
                .is_some_and(|started| started.elapsed() >= STARTUP_TIMEOUT)
        {
            self.fatal = Some("causal startup handshake timed out".to_string());
            return;
        }
        if session.current_state() != SessionState::Running {
            return;
        }
        if self.role == "b" && !self.startup_proposal_sent {
            let Some(start_unix_ms) =
                unix_time_millis().and_then(|now_ms| now_ms.checked_add(STARTUP_LEAD_MS))
            else {
                self.fatal = Some("system clock cannot represent startup deadline".to_string());
                return;
            };
            let Some(start_at) = instant_for_unix_ms(start_unix_ms) else {
                self.fatal = Some("startup deadline cannot be mapped monotonically".to_string());
                return;
            };
            let Some(remote_id) = self.remote_id else {
                self.fatal = Some("missing remote player for startup proposal".to_string());
                return;
            };
            let control = StartupControl::Proposal { start_unix_ms };
            if !queue_startup_control(&self.relay, remote_id, control) {
                self.fatal = Some("startup proposal exceeded the relay queue bound".to_string());
                return;
            }
            self.startup_start_unix_ms = Some(start_unix_ms);
            self.startup_start_at = Some(start_at);
            self.startup_proposal_sent = true;
            return;
        }
        if self.role == "a" && self.startup_proposal_received && !self.startup_ack_sent {
            let (Some(remote_id), Some(start_unix_ms), Some(start_at)) = (
                self.remote_id,
                self.startup_start_unix_ms,
                self.startup_start_at,
            ) else {
                self.fatal = Some("startup proposal state is incomplete".to_string());
                return;
            };
            if now >= start_at {
                self.fatal = Some("startup proposal left no time for acknowledgement".to_string());
                return;
            }
            if !queue_startup_control(
                &self.relay,
                remote_id,
                StartupControl::Ack { start_unix_ms },
            ) {
                self.fatal =
                    Some("startup acknowledgement exceeded the relay queue bound".to_string());
                return;
            }
            self.startup_ack_sent = true;
            return;
        }
        if self.role == "b" && self.startup_ack_received && !self.startup_commit_sent {
            let (Some(remote_id), Some(start_unix_ms), Some(start_at)) = (
                self.remote_id,
                self.startup_start_unix_ms,
                self.startup_start_at,
            ) else {
                self.fatal = Some("startup acknowledgement state is incomplete".to_string());
                return;
            };
            if now >= start_at {
                self.fatal = Some("startup acknowledgement missed the common deadline".to_string());
                return;
            }
            if !queue_startup_control(
                &self.relay,
                remote_id,
                StartupControl::Commit { start_unix_ms },
            ) {
                self.fatal = Some("startup commit exceeded the relay queue bound".to_string());
                return;
            }
            self.startup_commit_sent = true;
            return;
        }
        let startup_handshake_ready = match &*self.role {
            "a" => self.startup_ack_sent && self.startup_commit_received,
            "b" => {
                self.startup_proposal_sent && self.startup_ack_received && self.startup_commit_sent
            }
            _ => false,
        };
        match startup_release_due(now, self.startup_start_at, startup_handshake_ready) {
            Ok(false) => return,
            Ok(true) => {}
            Err(message) => {
                self.fatal = Some(format!("{message} for role {}", self.role));
                return;
            }
        }
        let Some(start_at) = self.startup_start_at else {
            self.fatal = Some("startup release lost the common deadline".to_string());
            return;
        };
        if !self.startup_barrier_completed {
            self.startup_barrier_completed = true;
            self.startup_barrier_release_local_frame = Some(session.current_frame().as_i32());
            self.startup_barrier_elapsed_ms = self
                .startup_started_at
                .map(|started| started.elapsed().as_millis());
            self.startup_release_lateness_ms =
                Some(now.saturating_duration_since(start_at).as_millis());
            self.started = Some(now);
            // Anchor both independent cadence schedules to the same mapped
            // deadline rather than each browser's first later callback.
            self.next_advance_at = Some(start_at);
            if let Some(client) = &mut self.client {
                client.reset_queue_age_peak();
            }
        }

        let target_is_confirmed = session.confirmed_frame().as_i32() >= self.target_frames
            && self.checksum_through >= self.target_frames;
        if target_is_confirmed && !self.target_confirmed {
            self.target_confirmed = true;
            self.simulation_elapsed_ms = self.started.map(|started| started.elapsed().as_millis());
        }

        if target_is_confirmed
            && self.target_state_checksum.is_some()
            && sync_health == Some(SyncHealth::InSync)
            && metrics.checksums_compared >= 10
            && metrics.checksums_mismatched == 0
            && metrics.events_discarded_total == 0
        {
            if !self.game_ready {
                self.game_ready = true;
                self.game_ready_at = Some(Instant::now());
            }
            return;
        }

        let current = session.current_frame().as_i32();
        if current >= IMPAIRMENT_START_FRAME && self.pre_impairment_metrics.is_none() {
            self.pre_impairment_metrics = Some(session.metrics());
        }
        if self.role == "b" && !self.impairment_activated && current >= IMPAIRMENT_START_FRAME {
            // Align at the switch boundary before changing B's delayed input.
            // This setup synchronization does not pace measured steady-state
            // gameplay and is bounded by the session timeout/oracles.
            if self
                .relay
                .borrow()
                .remote_frame
                .is_none_or(|remote| remote < IMPAIRMENT_START_FRAME)
            {
                return;
            }
            self.impairment_activated = true;
        }
        if !simulation_advance_due(now, &mut self.next_advance_at) {
            return;
        }
        if current < self.settlement_frame_limit {
            let Some(local_handle) = self.local_handle else {
                self.fatal = Some("missing local Fortress handle".to_string());
                return;
            };
            // The input packet produced by this advance causally proves the
            // advertised post-advance watermark at the receiving peer.
            self.relay.borrow_mut().local_frame = Some(current.saturating_add(1));
            // Prediction stays exactly correct before the switch. Player B's
            // value change makes A's last-value prediction wrong, so the later
            // nonzero rollback delta is causally tied to that remote input.
            let input = TestInput {
                value: u32::from(self.role == "b" && self.impairment_activated),
            };
            if let Err(error) = session.add_local_input(local_handle, input) {
                self.fatal = Some(format!("add_local_input failed: {error}"));
                return;
            }
            match session.advance_frame() {
                Ok(requests) => {
                    for request in requests {
                        match request {
                            FortressRequest::SaveGameState { cell, frame } => {
                                if self.game.frame != frame.as_i32() {
                                    self.fatal = Some(format!(
                                        "save frame mismatch: game={} request={}",
                                        self.game.frame,
                                        frame.as_i32()
                                    ));
                                    return;
                                }
                                let checksum = match compute_checksum(&self.game) {
                                    Ok(checksum) => checksum,
                                    Err(error) => {
                                        self.fatal =
                                            Some(format!("state checksum failed: {error}"));
                                        return;
                                    }
                                };
                                if !cell.save(frame, Some(self.game.clone()), Some(checksum)) {
                                    self.fatal = Some(format!(
                                        "Fortress rejected state save for frame {}",
                                        frame.as_i32()
                                    ));
                                    return;
                                }
                                self.save_requests = self.save_requests.saturating_add(1);
                                if frame.as_i32() == self.target_frames {
                                    self.target_state_checksum = Some(checksum);
                                }
                            }
                            FortressRequest::LoadGameState { cell, frame } => {
                                if let Some(state) = cell.load() {
                                    if state.frame != frame.as_i32() {
                                        self.fatal = Some(format!(
                                            "load frame mismatch: state={} request={}",
                                            state.frame,
                                            frame.as_i32()
                                        ));
                                        return;
                                    }
                                    self.game = state;
                                    self.load_requests = self.load_requests.saturating_add(1);
                                } else {
                                    self.fatal =
                                        Some("Fortress requested missing state".to_string());
                                    return;
                                }
                            }
                            FortressRequest::AdvanceFrame { inputs } => {
                                for (index, (input, status)) in inputs.iter().enumerate() {
                                    if !matches!(
                                        status,
                                        fortress_rollback::InputStatus::Disconnected
                                    ) {
                                        self.game.value = self.game.value.wrapping_add(
                                            u64::from(input.value).wrapping_mul(index as u64 + 1),
                                        );
                                    }
                                }
                                self.game.frame = self.game.frame.saturating_add(1);
                                self.advance_requests = self.advance_requests.saturating_add(1);
                            }
                        }
                    }
                }
                Err(error) => self.fatal = Some(format!("advance_frame failed: {error}")),
            }
        }
    }

    fn pump_outbound(&mut self) {
        let current_frame = self
            .session
            .as_ref()
            .map_or(0, |session| session.current_frame().as_i32());
        if self.role == "b" && self.impairment_activated && !self.impairment_released {
            let predicted_through = IMPAIRMENT_START_FRAME
                .saturating_add(INPUT_DELAY_FRAMES)
                .saturating_add(1);
            // Retain B's FIFO until A's causal post-advance watermark proves
            // it simulated the changed delayed-input frame speculatively.
            if self
                .relay
                .borrow()
                .remote_frame
                .is_none_or(|remote| remote < predicted_through)
            {
                if current_frame
                    >= IMPAIRMENT_START_FRAME.saturating_add(IMPAIRMENT_MAX_HOLD_FRAMES)
                {
                    self.fatal = Some(
                        "deterministic rollback relay hold exceeded its frame bound".to_string(),
                    );
                }
                return;
            }
            self.impairment_released = true;
        }
        let Some(client) = &mut self.client else {
            return;
        };
        loop {
            let payload = self
                .relay
                .borrow()
                .outbound
                .front()
                .map(|(_, payload)| payload.clone());
            let Some(payload) = payload else {
                break;
            };
            match client.send_binary_game_data(payload) {
                Ok(()) => {
                    self.relay.borrow_mut().outbound.pop_front();
                }
                Err(SignalFishError::SendBufferFull { .. }) => {
                    let mut queues = self.relay.borrow_mut();
                    queues.retries = queues.retries.saturating_add(1);
                    break;
                }
                Err(error) => {
                    self.fatal = Some(format!("relay send failed: {error}"));
                    break;
                }
            }
        }
    }

    fn stop_game_session_if_ready(&mut self) {
        if !self.game_ready || self.session_finished {
            return;
        }
        if let Some(session) = self.session.take() {
            self.final_metrics = Some(session.metrics());
            self.final_sync_in_sync = self
                .remote_handle
                .is_some_and(|handle| session.sync_health(handle) == Some(SyncHealth::InSync));
        }
        self.session_finished = true;
    }

    fn drain_post_ready_relay(&mut self) {
        if !self.session_finished {
            return;
        }
        let mut queues = self.relay.borrow_mut();
        let drained = u64::try_from(queues.inbound.len()).unwrap_or(u64::MAX);
        queues.inbound.clear();
        queues.post_ready_drained = queues.post_ready_drained.saturating_add(drained);
    }

    fn fail(&mut self, message: String) {
        if self.fatal.is_none() {
            self.fatal = Some(message);
        }
    }

    fn drive_successful_close(&mut self) {
        if !self.game_ready {
            return;
        }
        let queues_empty = {
            let queues = self.relay.borrow();
            queues.inbound.is_empty() && queues.outbound.is_empty()
        };
        let client_queue_empty = self
            .client
            .as_ref()
            .is_none_or(|client| client.polling_stats().current_queue_depth == 0);
        if !queues_empty || !client_queue_empty {
            self.settle_started = None;
            return;
        }

        if self.role == "a" && !self.peer_left_observed {
            self.settle_started = None;
            if self
                .game_ready_at
                .is_some_and(|ready| ready.elapsed() >= TEARDOWN_TIMEOUT)
            {
                self.fail("creator did not observe the joiner's terminal PlayerLeft".to_string());
            }
            return;
        }

        let settled_at = self.settle_started.get_or_insert_with(Instant::now);
        // The joiner closes first. The creator closes only after observing the
        // exact v3 terminal watermark, then gives its final acknowledgement a
        // short interval to leave the polling queue. Closure itself is still
        // gated on an exactly empty relay and SDK boundary above.
        let settle_ms = if self.role == "a" { 150 } else { 500 };
        if settled_at.elapsed() < std::time::Duration::from_millis(settle_ms) {
            return;
        }
        let Some(client) = &mut self.client else {
            self.finish(true);
            return;
        };
        if !self.closing_success {
            client.close();
            self.closing_success = true;
            return;
        }
        if !client.is_closing() {
            self.finish(true);
        }
    }

    fn finish(&mut self, passed: bool) {
        if self.completed {
            return;
        }
        let queues = self.relay.borrow();
        let metrics = self
            .final_metrics
            .or_else(|| self.session.as_ref().map(|session| session.metrics()));
        let sync_in_sync = self.final_sync_in_sync
            || self.session.as_ref().is_some_and(|session| {
                self.remote_handle
                    .and_then(|handle| session.sync_health(handle))
                    == Some(SyncHealth::InSync)
            });
        let polling = self.client.as_ref().map(|client| client.polling_stats());
        let queue_age = self.client.as_ref().map(|client| client.queue_age_stats());
        let transport = self
            .client
            .as_ref()
            .map(|client| client.transport_diagnostics());
        let admission_watermark_violations = self.client.as_ref().map_or(0, |client| {
            client.transport().admission_watermark_violations()
        });
        let total_elapsed_ms = self
            .started
            .map_or(0, |started| started.elapsed().as_millis());
        let final_inbound_depth = queues.inbound.len();
        let final_outbound_depth = queues.outbound.len();
        let queue_depth = polling.map_or(0, |stats| stats.current_queue_depth);
        let current_queue_age = queue_age.map_or(std::time::Duration::ZERO, |stats| {
            stats.current_oldest_queue_age
        });
        let peak_queue_age = queue_age.map_or(std::time::Duration::ZERO, |stats| {
            stats.peak_oldest_queue_age
        });
        let checksums_mismatched = metrics.map_or(0, |metrics| metrics.checksums_mismatched);
        let events_discarded = metrics.map_or(0, |metrics| metrics.events_discarded_total);
        let relay_messages_per_simulated_frame = metrics.map_or(0.0, |metrics| {
            queues.encoded.saturating_add(queues.decoded) as f64
                / metrics.visual_frames.max(1) as f64
        });
        let observed_simulation_fps = self
            .simulation_elapsed_ms
            .filter(|elapsed| *elapsed > 0)
            .map_or(0.0, |elapsed| {
                self.target_frames.max(0) as f64 * 1_000.0 / elapsed as f64
            });
        let lag_limit = scenario_lag_limit(self.poll_hitch_frame, self.settlement_frame_limit);
        let startup_barrier_valid = self.startup_barrier_completed
            && self.startup_barrier_release_local_frame == Some(0)
            && self.startup_start_unix_ms.is_some_and(|value| value > 0)
            && self.startup_barrier_elapsed_ms.is_some()
            && self
                .startup_release_lateness_ms
                .is_some_and(|value| value <= 100)
            && match &*self.role {
                "a" => {
                    self.startup_proposal_received
                        && self.startup_ack_sent
                        && self.startup_commit_received
                }
                "b" => {
                    self.startup_proposal_sent
                        && self.startup_ack_received
                        && self.startup_commit_sent
                }
                _ => false,
            };
        let invariant_passed = self.game_ready
            && self.target_state_checksum.is_some()
            && startup_barrier_valid
            && final_inbound_depth == 0
            && final_outbound_depth == 0
            && queue_depth == 0
            && current_queue_age == std::time::Duration::ZERO
            && peak_queue_age <= std::time::Duration::from_millis(500)
            && queues.dropped == 0
            && queues.malformed == 0
            && checksums_mismatched == 0
            && events_discarded == 0
            && metrics.is_some_and(|metrics| {
                metrics.confirmation_lag_current <= lag_limit
                    && self.confirmation_lag_steady_max <= lag_limit
                    && self.confirmation_lag_warmup_max <= PREDICTION_WINDOW_FRAMES as u64
                    && metrics.confirmation_lag_max <= PREDICTION_WINDOW_FRAMES as u64
                    && metrics.confirmation_lag_max
                        == self
                            .confirmation_lag_warmup_max
                            .max(self.confirmation_lag_steady_max)
                    && metrics.stall_count == 0
                // Advisory frame-advantage wait recommendations are intentionally not
                // required to be zero. This fixed-cadence driver never acts on them,
                // and fortress-rollback emits one whenever transient frame advantage
                // reaches MIN_RECOMMENDATION (3) — inside the confirmation-lag bound
                // asserted just above. Requiring zero contradicted that lag bound and
                // flaked on browser/relay timing jitter. `wait_recommendation_count`
                // is still reported in the summary for observability; `stall_count`
                // (a real progress stall) stays strict.
            })
            && relay_messages_per_simulated_frame >= 2.0;
        let summary = serde_json::json!({
            "passed": passed && self.fatal.is_none() && invariant_passed,
            "role": self.role,
            "requested_room_code": self.requested_room_code,
            "joined_room_code": self.joined_room_code,
            "local_id": self.local_id.map(|id| id.to_string()),
            "remote_id": self.remote_id.map(|id| id.to_string()),
            "target_frames": self.target_frames,
            "settlement_frame_limit": self.settlement_frame_limit,
            "session_timeout_ms": self.session_timeout.as_millis(),
            "game_frame": self.game.frame,
            // Preserve exact integer values when the browser runner parses JSON.
            "confirmed_input_checksum": self.confirmed_checksum.to_string(),
            "target_state_checksum": self.target_state_checksum.map(|checksum| checksum.to_string()),
            "checksum_through": self.checksum_through,
            "speculative_value": self.game.value,
            "simulation_elapsed_ms": self.simulation_elapsed_ms,
            "simulation_target_fps": SIMULATION_FPS,
            "observed_simulation_fps": observed_simulation_fps,
            "total_elapsed_ms": total_elapsed_ms,
            "startup_barrier_completed": self.startup_barrier_completed,
            "startup_start_unix_ms": self.startup_start_unix_ms,
            "startup_proposal_sent": self.startup_proposal_sent,
            "startup_proposal_received": self.startup_proposal_received,
            "startup_ack_sent": self.startup_ack_sent,
            "startup_ack_received": self.startup_ack_received,
            "startup_commit_sent": self.startup_commit_sent,
            "startup_commit_received": self.startup_commit_received,
            "startup_barrier_release_local_frame": self.startup_barrier_release_local_frame,
            "startup_barrier_elapsed_ms": self.startup_barrier_elapsed_ms,
            "startup_release_lateness_ms": self.startup_release_lateness_ms,
            "max_poll_us": self.max_poll_us,
            "multi_frame_poll": self.multi_frame_poll,
            "game_ready": self.game_ready,
            "sync_in_sync": sync_in_sync,
            "frames_advanced": metrics.map_or(0, |metrics| metrics.frames_advanced),
            "visual_frames": metrics.map_or(0, |metrics| metrics.visual_frames),
            "resimulated_frames": metrics.map_or(0, |metrics| metrics.resimulated_frames),
            "rollback_count": metrics.map_or(0, |metrics| metrics.rollback_count),
            "max_rollback_depth": metrics.map_or(0, |metrics| metrics.max_rollback_depth),
            "prediction_miss_count": metrics.map_or(0, |metrics| metrics.prediction_miss_count),
            "stall_count": metrics.map_or(0, |metrics| metrics.stall_count),
            "wait_recommendation_count": metrics.map_or(0, |metrics| metrics.wait_recommendations),
            "confirmation_lag_current": metrics.map_or(0, |metrics| metrics.confirmation_lag_current),
            "confirmation_lag_max": metrics.map_or(0, |metrics| metrics.confirmation_lag_max),
            "confirmation_lag_warmup_frames": CONFIRMATION_LAG_WARMUP_FRAMES,
            "confirmation_lag_warmup_max": self.confirmation_lag_warmup_max,
            "confirmation_lag_steady_max": self.confirmation_lag_steady_max,
            "checksums_compared": metrics.map_or(0, |metrics| metrics.checksums_compared),
            "checksums_matched": metrics.map_or(0, |metrics| metrics.checksums_matched),
            "checksums_mismatched": checksums_mismatched,
            "events_discarded": events_discarded,
            "save_requests": self.save_requests,
            "load_requests": self.load_requests,
            "advance_requests": self.advance_requests,
            "desync_events": self.desync_events,
            "pre_impairment_rollback_count": self.pre_impairment_metrics.map_or(0, |metrics| metrics.rollback_count),
            "pre_impairment_resimulated_frames": self.pre_impairment_metrics.map_or(0, |metrics| metrics.resimulated_frames),
            "pre_impairment_prediction_miss_count": self.pre_impairment_metrics.map_or(0, |metrics| metrics.prediction_miss_count),
            "relay_encoded": queues.encoded,
            "relay_decoded": queues.decoded,
            "relay_retries": queues.retries,
            "relay_dropped": queues.dropped,
            "relay_malformed": queues.malformed,
            "relay_ignored_nonlocal": queues.ignored_nonlocal,
            "relay_post_ready_drained": queues.post_ready_drained,
            "relay_peak_inbound": queues.peak_inbound,
            "relay_peak_outbound": queues.peak_outbound,
            "relay_inbound_depth": final_inbound_depth,
            "relay_outbound_depth": final_outbound_depth,
            "queue_depth": queue_depth,
            "peak_queue_depth": polling.map_or(0, |stats| stats.peak_queue_depth),
            "current_queue_age_ms": current_queue_age.as_secs_f64() * 1_000.0,
            "peak_queue_age_ms": peak_queue_age.as_secs_f64() * 1_000.0,
            "relay_messages_per_simulated_frame": relay_messages_per_simulated_frame,
            "accepted_frames": transport.map_or(0, |stats| stats.accepted_frames),
            "watermark_hits": transport.map_or(0, |stats| stats.watermark_hits),
            "backend_capacity_hits": transport.map_or(0, |stats| stats.backend_capacity_hits),
            "admission_watermark_violations": admission_watermark_violations,
            "impairment_activated": self.impairment_activated,
            "impairment_released": self.impairment_released,
            "poll_hitch_frame": self.poll_hitch_frame,
            "poll_hitch_completed": self.poll_hitch_completed,
            "poll_hitch_frames_advanced": self.poll_hitch_frames_advanced,
            "peer_left_observed": self.peer_left_observed,
            "peer_left_epoch": self.peer_left_epoch,
            "peer_left_final_seq": self.peer_left_final_seq,
            "fatal": self.fatal,
        });
        let final_sample = serde_json::json!({
            "role": self.role,
            "elapsed_ms": total_elapsed_ms,
            "current_frame": self.game.frame,
            "confirmed_frame": self.checksum_through,
            "confirmation_lag": metrics.map_or(0, |metrics| metrics.confirmation_lag_current),
            "confirmation_lag_max": metrics.map_or(0, |metrics| metrics.confirmation_lag_max),
            "queue_depth": queue_depth,
            "queue_age_ms": current_queue_age.as_secs_f64() * 1_000.0,
            "final": true,
        });
        godot_print!("SIGNAL_FISH_FORTRESS sample {final_sample}");
        if summary["passed"].as_bool() == Some(true) {
            godot_print!("SIGNAL_FISH_FORTRESS summary {summary}");
        } else {
            godot_error!("SIGNAL_FISH_FORTRESS summary {summary}");
        }
        self.completed = true;
    }
}

fn simulation_frame_period() -> std::time::Duration {
    std::time::Duration::from_nanos(1_000_000_000 / SIMULATION_FPS as u64)
}

fn scenario_lag_limit(poll_hitch_frame: Option<i32>, settlement_frame_limit: i32) -> u64 {
    if poll_hitch_frame.is_some_and(|frame| (0..settlement_frame_limit).contains(&frame)) {
        IMPAIRED_LAG_LIMIT
    } else {
        CLEAN_LAG_LIMIT
    }
}

fn confirmation_lag_is_warmup(current_frame: i32) -> bool {
    current_frame <= CONFIRMATION_LAG_WARMUP_FRAMES
}

fn simulation_advance_due(now: Instant, next_advance_at: &mut Option<Instant>) -> bool {
    let period = simulation_frame_period();
    let Some(deadline) = next_advance_at else {
        *next_advance_at = now.checked_add(period);
        return true;
    };
    if now < *deadline {
        return false;
    }
    let scheduled = deadline.checked_add(period).unwrap_or(now);
    // Preserve elapsed deadline debt so a delayed browser process can catch
    // up instead of retaining permanent frame advantage. The caller invokes
    // this once per rendered callback, bounding recovery to one frame there.
    *deadline = scheduled;
    true
}

fn unix_time_millis() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
}

fn instant_for_unix_ms(start_unix_ms: u64) -> Option<Instant> {
    let remaining_ms = start_unix_ms.checked_sub(unix_time_millis()?)?;
    if remaining_ms == 0 {
        return None;
    }
    Instant::now().checked_add(Duration::from_millis(remaining_ms))
}

fn startup_release_due(
    now: Instant,
    start_at: Option<Instant>,
    handshake_ready: bool,
) -> Result<bool, &'static str> {
    let Some(start_at) = start_at else {
        return if handshake_ready {
            Err("startup handshake omitted the common deadline")
        } else {
            Ok(false)
        };
    };
    if !handshake_ready {
        return if now >= start_at {
            Err("startup handshake was incomplete at the common deadline")
        } else {
            Ok(false)
        };
    }
    Ok(now >= start_at)
}

fn queue_startup_control(
    relay: &Rc<RefCell<RelayQueues>>,
    remote_id: PlayerId,
    control: StartupControl,
) -> bool {
    let payload = encode_startup_envelope(remote_id.as_bytes(), control);
    let mut queues = relay.borrow_mut();
    if queues.outbound.len() >= MAX_RELAY_QUEUE {
        queues.dropped = queues.dropped.saturating_add(1);
        return false;
    }
    queues.outbound.push_back((remote_id, payload));
    queues.encoded = queues.encoded.saturating_add(1);
    queues.peak_outbound = queues.peak_outbound.max(queues.outbound.len());
    true
}

fn encode_startup_envelope(destination: &[u8; 16], control: StartupControl) -> Vec<u8> {
    let kind = match control {
        StartupControl::Proposal { .. } => 1,
        StartupControl::Ack { .. } => 2,
        StartupControl::Commit { .. } => 3,
    };
    let mut payload = Vec::with_capacity(29);
    payload.extend_from_slice(destination);
    payload.extend_from_slice(STARTUP_ENVELOPE_MAGIC);
    payload.push(kind);
    payload.extend_from_slice(&control.start_unix_ms().to_le_bytes());
    payload
}

fn decode_startup_envelope(payload: &[u8]) -> Option<(&[u8], StartupControl)> {
    let (destination, header) = payload.split_at_checked(16)?;
    if header.len() != 13 || header.get(..4)? != STARTUP_ENVELOPE_MAGIC {
        return None;
    }
    let start_unix_ms = u64::from_le_bytes(header.get(5..13)?.try_into().ok()?);
    if start_unix_ms == 0 {
        return None;
    }
    let control = match *header.get(4)? {
        1 => StartupControl::Proposal { start_unix_ms },
        2 => StartupControl::Ack { start_unix_ms },
        3 => StartupControl::Commit { start_unix_ms },
        _ => return None,
    };
    Some((destination, control))
}

fn encode_relay_envelope(destination: &[u8; 16], advertised_frame: i32, encoded: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(
        destination
            .len()
            .saturating_add(RELAY_ENVELOPE_MAGIC.len())
            .saturating_add(std::mem::size_of::<i32>())
            .saturating_add(encoded.len()),
    );
    payload.extend_from_slice(destination);
    payload.extend_from_slice(RELAY_ENVELOPE_MAGIC);
    payload.extend_from_slice(&advertised_frame.to_le_bytes());
    payload.extend_from_slice(encoded);
    payload
}

fn decode_relay_envelope(payload: &[u8]) -> Option<(&[u8], i32, &[u8])> {
    let (destination, remainder) = payload.split_at_checked(16)?;
    let (header, encoded) = remainder.split_at_checked(8)?;
    if header.get(..4)? != RELAY_ENVELOPE_MAGIC {
        return None;
    }
    let advertised_frame = i32::from_le_bytes(header.get(4..8)?.try_into().ok()?);
    Some((destination, advertised_frame, encoded))
}

fn argument(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find_map(|pair| (pair.first().map(String::as_str) == Some(name)).then(|| pair[1].clone()))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{
        confirmation_lag_is_warmup, decode_relay_envelope, decode_startup_envelope,
        encode_relay_envelope, encode_startup_envelope, scenario_lag_limit, simulation_advance_due,
        simulation_frame_period, startup_control_allowed, startup_control_lead_valid,
        startup_release_due, StartupControl,
    };

    #[test]
    fn startup_envelope_round_trips_deadline_controls_and_rejects_corruption() {
        let destination = [9; 16];
        for control in [
            StartupControl::Proposal {
                start_unix_ms: 123_456,
            },
            StartupControl::Ack {
                start_unix_ms: 123_456,
            },
            StartupControl::Commit {
                start_unix_ms: 123_456,
            },
        ] {
            let payload = encode_startup_envelope(&destination, control);
            assert_eq!(
                decode_startup_envelope(&payload),
                Some((destination.as_slice(), control))
            );
        }

        let mut invalid_kind = encode_startup_envelope(
            &destination,
            StartupControl::Proposal {
                start_unix_ms: 123_456,
            },
        );
        invalid_kind[20] = u8::MAX;
        assert_eq!(decode_startup_envelope(&invalid_kind), None);
        assert_eq!(decode_startup_envelope(&invalid_kind[..28]), None);
    }

    #[test]
    fn startup_transition_gate_rejects_wrong_stage_timestamp_and_missed_deadline() {
        let start = Instant::now();
        let proposal = StartupControl::Proposal { start_unix_ms: 100 };
        let ack = StartupControl::Ack { start_unix_ms: 100 };
        let commit = StartupControl::Commit { start_unix_ms: 100 };

        assert!(startup_control_allowed("a", proposal, None, false, false));
        assert!(startup_control_allowed(
            "a",
            proposal,
            Some(100),
            false,
            true
        ));
        assert!(!startup_control_allowed(
            "a",
            StartupControl::Proposal { start_unix_ms: 101 },
            Some(100),
            false,
            true
        ));
        assert!(!startup_control_allowed(
            "a",
            commit,
            Some(100),
            false,
            false
        ));
        assert!(startup_control_allowed("a", commit, Some(100), false, true));
        assert!(startup_control_allowed("b", ack, Some(100), true, false));
        assert!(!startup_control_allowed(
            "b",
            StartupControl::Ack { start_unix_ms: 101 },
            Some(100),
            true,
            false
        ));
        assert!(!startup_control_allowed(
            "b",
            proposal,
            Some(100),
            true,
            false
        ));

        assert_eq!(
            startup_release_due(start, Some(start), false),
            Err("startup handshake was incomplete at the common deadline")
        );
        assert_eq!(startup_release_due(start, Some(start), true), Ok(true));
        assert_eq!(
            startup_release_due(start, Some(start + Duration::from_millis(1)), false),
            Ok(false)
        );
        assert_eq!(
            startup_release_due(start, None, true),
            Err("startup handshake omitted the common deadline")
        );

        let after_deadline = start + Duration::from_millis(1);
        assert!(!startup_control_lead_valid(
            Some(start),
            after_deadline,
            Duration::from_millis(250),
            false
        ));
        assert!(startup_control_lead_valid(
            Some(start),
            after_deadline,
            Duration::from_millis(250),
            true
        ));
    }

    #[test]
    fn fixed_cadence_preserves_deadline_debt_and_bounds_each_recovery_callback() {
        let start = Instant::now();
        let period = simulation_frame_period();
        let mut deadline = None;

        assert!(simulation_advance_due(start, &mut deadline));
        assert!(!simulation_advance_due(
            start + Duration::from_millis(40),
            &mut deadline
        ));
        assert!(simulation_advance_due(
            start + Duration::from_millis(60),
            &mut deadline
        ));
        assert_eq!(deadline, start.checked_add(period.saturating_mul(2)));

        let after_pause = start + Duration::from_secs(5);
        assert!(simulation_advance_due(after_pause, &mut deadline));
        assert_eq!(deadline, start.checked_add(period.saturating_mul(3)));
        assert!(deadline.is_some_and(|deadline| deadline < after_pause));

        // Each invocation advances exactly one deadline even while overdue;
        // the rendered-callback caller therefore cannot burst several frames.
        assert!(simulation_advance_due(after_pause, &mut deadline));
        assert_eq!(deadline, start.checked_add(period.saturating_mul(4)));
    }

    #[test]
    fn clean_and_impaired_self_oracles_use_the_runner_lag_limits() {
        assert_eq!(scenario_lag_limit(None, 620), 8);
        assert_eq!(scenario_lag_limit(Some(240), 620), 13);
        assert_eq!(scenario_lag_limit(Some(240), 3_620), 13);
        assert_eq!(scenario_lag_limit(Some(-1), 620), 8);
        assert_eq!(scenario_lag_limit(Some(620), 620), 8);
    }

    #[test]
    fn confirmation_lag_warmup_has_an_explicit_frame_boundary() {
        assert!(confirmation_lag_is_warmup(-1));
        assert!(confirmation_lag_is_warmup(0));
        assert!(confirmation_lag_is_warmup(59));
        assert!(confirmation_lag_is_warmup(60));
        assert!(!confirmation_lag_is_warmup(61));
        assert!(!confirmation_lag_is_warmup(3_600));
    }

    #[test]
    fn relay_envelope_round_trips_frame_watermark_and_rejects_corruption() {
        let destination = [7; 16];
        let encoded = [1, 2, 3, 4];
        let payload = encode_relay_envelope(&destination, 123, &encoded);

        assert_eq!(
            decode_relay_envelope(&payload),
            Some((destination.as_slice(), 123, encoded.as_slice()))
        );
        assert_eq!(decode_relay_envelope(&payload[..20]), None);

        let mut corrupt = payload;
        if let Some(byte) = corrupt.get_mut(16) {
            *byte ^= 0xff;
        }
        assert_eq!(decode_relay_envelope(&corrupt), None);
    }
}
