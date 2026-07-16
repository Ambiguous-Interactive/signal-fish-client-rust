use godot::prelude::*;
use signal_fish_client::protocol::GameDataEncoding;
use signal_fish_client::{
    GodotWebSocketTransport, JoinRoomParams, SignalFishConfig, SignalFishEvent,
    SignalFishPollingClient,
};
use std::time::{Duration, Instant};

const SERVER_URL: &str = "ws://127.0.0.1:3536/v2/ws";
const APP_ID: &str = "e2e-test-app";
const BINARY_PAYLOAD: &[u8] = &[0, 1, 2, 255];
const LOAD_SECONDS: u64 = 16;
const LOAD_RATE_PER_CLIENT: u64 = 136;
const LOAD_TARGET_PER_CLIENT: u64 = LOAD_SECONDS * LOAD_RATE_PER_CLIENT;
const LOAD_MAX_QUEUE_DEPTH_PER_CLIENT: u64 = 32;

type Client = SignalFishPollingClient<GodotWebSocketTransport>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PairKind {
    Json,
    Binary,
}

impl PairKind {
    fn label(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Binary => "binary",
        }
    }

    fn game_name(self) -> &'static str {
        match self {
            Self::Json => "godot-web-smoke-json",
            Self::Binary => "godot-web-smoke-binary",
        }
    }

    fn encoding(self) -> GameDataEncoding {
        match self {
            Self::Json => GameDataEncoding::Json,
            Self::Binary => GameDataEncoding::MessagePack,
        }
    }
}

struct SmokePair {
    kind: PairKind,
    first: Option<Client>,
    second: Option<Client>,
    room_code: Option<String>,
    relay_sent: bool,
    relay_received: bool,
    pong_received: bool,
    closing: bool,
    shutdown_done: bool,
    close_attributed: bool,
    server_close_ready_logged: bool,
    burst_accepted_before: Option<u64>,
    burst_received: usize,
    burst_one_poll: bool,
    burst_poll_checked: bool,
    load_started: Option<Instant>,
    load_finished_at: Option<Instant>,
    offered_a: u64,
    offered_b: u64,
    received_a: u64,
    received_b: u64,
    latencies_us: Vec<u64>,
    max_poll_us: u64,
    multi_frame_poll: bool,
    last_accepted_a: u64,
    last_accepted_b: u64,
    last_accepted_bytes_a: u64,
    last_accepted_bytes_b: u64,
    last_sample_at: Option<Instant>,
    last_latency_us: u64,
    sample_last_accepted: u64,
    sample_last_received: u64,
    max_poll_work_frames: u64,
    max_poll_work_bytes: u64,
    max_poll_receive_frames: u64,
    poll_count: u64,
    load_error: bool,
    peak_aggregate_depth: u64,
    other_pair_max_poll_us: u64,
}

impl SmokePair {
    fn new(kind: PairKind) -> Self {
        Self {
            kind,
            first: connect_client(kind, "a"),
            second: None,
            room_code: None,
            relay_sent: false,
            relay_received: false,
            pong_received: false,
            closing: false,
            shutdown_done: false,
            close_attributed: false,
            server_close_ready_logged: false,
            burst_accepted_before: None,
            burst_received: 0,
            burst_one_poll: false,
            burst_poll_checked: false,
            load_started: None,
            load_finished_at: None,
            offered_a: 0,
            offered_b: 0,
            received_a: 0,
            received_b: 0,
            latencies_us: Vec::new(),
            max_poll_us: 0,
            multi_frame_poll: false,
            last_accepted_a: 0,
            last_accepted_b: 0,
            last_accepted_bytes_a: 0,
            last_accepted_bytes_b: 0,
            last_sample_at: None,
            last_latency_us: 0,
            sample_last_accepted: 0,
            sample_last_received: 0,
            max_poll_work_frames: 0,
            max_poll_work_bytes: 0,
            max_poll_receive_frames: 0,
            poll_count: 0,
            load_error: false,
            peak_aggregate_depth: 0,
            other_pair_max_poll_us: 0,
        }
    }

    fn poll(&mut self) {
        if self.shutdown_done || self.close_attributed {
            return;
        }
        if self.closing {
            self.drive_close();
            return;
        }

        let first_events = poll_measured(
            &mut self.first,
            &mut self.max_poll_us,
            &mut self.multi_frame_poll,
            &mut self.last_accepted_a,
            &mut self.max_poll_work_frames,
            &mut self.last_accepted_bytes_a,
            &mut self.max_poll_work_bytes,
            &mut self.max_poll_receive_frames,
            &mut self.poll_count,
        );
        self.check_binary_burst_acceptance();
        for event in first_events {
            self.handle_first(event);
        }

        if self.second.is_none() && self.room_code.is_some() {
            self.second = connect_client(self.kind, "b");
        }

        let second_events = poll_measured(
            &mut self.second,
            &mut self.max_poll_us,
            &mut self.multi_frame_poll,
            &mut self.last_accepted_b,
            &mut self.max_poll_work_frames,
            &mut self.last_accepted_bytes_b,
            &mut self.max_poll_work_bytes,
            &mut self.max_poll_receive_frames,
            &mut self.poll_count,
        );
        for event in second_events {
            self.handle_second(event);
        }

        if self.kind == PairKind::Json && self.relay_received && self.pong_received {
            if self.load_started.is_none() {
                let now = Instant::now();
                if let Some(client) = self.first.as_mut() {
                    client.reset_queue_age_peak();
                }
                if let Some(client) = self.second.as_mut() {
                    client.reset_queue_age_peak();
                }
                self.load_started = Some(now);
                self.last_sample_at = Some(now);
                self.max_poll_us = 0;
                self.multi_frame_poll = false;
                self.last_accepted_a = self
                    .first
                    .as_ref()
                    .map_or(0, |client| client.transport_diagnostics().accepted_frames);
                self.last_accepted_b = self
                    .second
                    .as_ref()
                    .map_or(0, |client| client.transport_diagnostics().accepted_frames);
                self.last_accepted_bytes_a = self
                    .first
                    .as_ref()
                    .map_or(0, |client| client.transport_diagnostics().accepted_bytes);
                self.last_accepted_bytes_b = self
                    .second
                    .as_ref()
                    .map_or(0, |client| client.transport_diagnostics().accepted_bytes);
                self.sample_last_accepted =
                    self.last_accepted_a.saturating_add(self.last_accepted_b);
                self.sample_last_received = 0;
                self.max_poll_work_frames = 0;
                self.max_poll_work_bytes = 0;
                self.max_poll_receive_frames = 0;
                self.poll_count = 0;
                godot_print!("SIGNAL_FISH_SMOKE load-started");
            }
            self.drive_load();
        }
        if self.kind == PairKind::Binary
            && self.relay_received
            && self.pong_received
            && !self.server_close_ready_logged
        {
            godot_print!("SIGNAL_FISH_SMOKE binary-ready-for-server-close");
            self.server_close_ready_logged = true;
        }
    }

    fn handle_first(&mut self, event: SignalFishEvent) {
        let label = self.kind.label();
        match event {
            SignalFishEvent::Connected => {
                godot_print!("SIGNAL_FISH_SMOKE {label}-connected-first");
            }
            SignalFishEvent::Authenticated { .. } => {
                godot_print!("SIGNAL_FISH_SMOKE {label}-authenticated-first");
                if let Some(client) = &mut self.first {
                    if let Err(error) = client.ping() {
                        godot_error!("SIGNAL_FISH_SMOKE {label}-ping-error {error}");
                    }
                    if let Err(error) = client.join_room(JoinRoomParams::new(
                        self.kind.game_name(),
                        format!("Godot-{label}-A"),
                    )) {
                        godot_error!("SIGNAL_FISH_SMOKE {label}-join-first-error {error}");
                    }
                }
            }
            SignalFishEvent::Pong => {
                godot_print!("SIGNAL_FISH_SMOKE {label}-pong-ok");
                self.pong_received = true;
            }
            SignalFishEvent::RoomJoined { room_code, .. } => {
                godot_print!("SIGNAL_FISH_SMOKE {label}-joined-first");
                self.room_code = Some(room_code);
            }
            SignalFishEvent::GameData { data, .. }
                if data.get("load_sender").and_then(serde_json::Value::as_str) == Some("b") =>
            {
                self.record_load_receive(&data, false);
            }
            SignalFishEvent::Disconnected { reason, .. } => {
                self.handle_disconnect("first", reason.as_deref());
            }
            _ => {}
        }
    }

    fn handle_second(&mut self, event: SignalFishEvent) {
        let label = self.kind.label();
        match event {
            SignalFishEvent::Connected => {
                godot_print!("SIGNAL_FISH_SMOKE {label}-connected-second");
            }
            SignalFishEvent::Authenticated { .. } => {
                godot_print!("SIGNAL_FISH_SMOKE {label}-authenticated-second");
                if let (Some(client), Some(room_code)) =
                    (&mut self.second, self.room_code.as_deref())
                {
                    let params =
                        JoinRoomParams::new(self.kind.game_name(), format!("Godot-{label}-B"))
                            .with_room_code(room_code);
                    if let Err(error) = client.join_room(params) {
                        godot_error!("SIGNAL_FISH_SMOKE {label}-join-second-error {error}");
                    }
                }
            }
            SignalFishEvent::RoomJoined { .. } if !self.relay_sent => {
                godot_print!("SIGNAL_FISH_SMOKE {label}-joined-second");
                self.send_relay();
            }
            SignalFishEvent::GameData { data, .. }
                if self.kind == PairKind::Json
                    && data.get("smoke").and_then(serde_json::Value::as_str)
                        == Some("text-relay") =>
            {
                godot_print!("SIGNAL_FISH_SMOKE text-relay-ok");
                self.relay_received = true;
            }
            SignalFishEvent::GameData { data, .. }
                if data.get("load_sender").and_then(serde_json::Value::as_str) == Some("a") =>
            {
                self.record_load_receive(&data, true);
            }
            SignalFishEvent::GameDataBinary {
                payload, encoding, ..
            } if self.kind == PairKind::Binary
                && encoding == GameDataEncoding::MessagePack
                && payload == BINARY_PAYLOAD =>
            {
                self.burst_received += 1;
                if self.burst_received == 4 {
                    godot_print!("SIGNAL_FISH_SMOKE binary-relay-ok");
                    self.relay_received = true;
                }
            }
            SignalFishEvent::Disconnected { reason, .. } => {
                self.handle_disconnect("second", reason.as_deref());
            }
            _ => {}
        }
    }

    fn send_relay(&mut self) {
        let Some(client) = &mut self.first else {
            return;
        };
        let result = match self.kind {
            PairKind::Json => client.send_game_data(serde_json::json!({
                "smoke": "text-relay"
            })),
            PairKind::Binary => {
                self.burst_accepted_before = Some(client.transport_diagnostics().accepted_frames);
                let mut result = Ok(());
                for _ in 0..4 {
                    if let Err(error) = client.send_binary_game_data(BINARY_PAYLOAD.to_vec()) {
                        result = Err(error);
                        break;
                    }
                }
                result
            }
        };
        if let Err(error) = result {
            godot_error!(
                "SIGNAL_FISH_SMOKE {}-relay-send-error {error}",
                self.kind.label()
            );
        } else {
            self.relay_sent = true;
        }
    }

    fn check_binary_burst_acceptance(&mut self) {
        if self.kind != PairKind::Binary || self.burst_poll_checked {
            return;
        }
        let Some(before) = self.burst_accepted_before else {
            return;
        };
        self.burst_poll_checked = true;
        let Some(client) = &self.first else {
            return;
        };
        let accepted = client
            .transport_diagnostics()
            .accepted_frames
            .saturating_sub(before);
        if accepted == 4 && client.polling_stats().current_queue_depth == 0 {
            self.burst_one_poll = true;
            godot_print!("SIGNAL_FISH_SMOKE binary-four-one-poll");
        } else {
            godot_error!(
                "SIGNAL_FISH_SMOKE binary-four-one-poll-error accepted={accepted} queue={}",
                client.polling_stats().current_queue_depth
            );
        }
    }

    fn drive_load(&mut self) {
        if let Some(finished_at) = self.load_finished_at {
            if Instant::now().saturating_duration_since(finished_at) >= Duration::from_secs(1) {
                close_client(&mut self.first);
                close_client(&mut self.second);
                self.closing = true;
            }
            return;
        }
        let Some(started) = self.load_started else {
            return;
        };
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(started);
        let due = (elapsed
            .as_micros()
            .saturating_mul(u128::from(LOAD_RATE_PER_CLIENT))
            / 1_000_000)
            .min(u128::from(LOAD_TARGET_PER_CLIENT)) as u64;

        self.load_error |= offer_load(&mut self.first, "a", &mut self.offered_a, due, elapsed);
        self.load_error |= offer_load(&mut self.second, "b", &mut self.offered_b, due, elapsed);
        let (current_depth, _, _, _) =
            aggregate_diagnostics(self.first.as_ref(), self.second.as_ref());
        self.peak_aggregate_depth = self.peak_aggregate_depth.max(current_depth);

        if self
            .last_sample_at
            .is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_millis(250))
            || (elapsed >= Duration::from_secs(LOAD_SECONDS)
                && self.offered_a == LOAD_TARGET_PER_CLIENT
                && self.offered_b == LOAD_TARGET_PER_CLIENT
                && self.received_a == LOAD_TARGET_PER_CLIENT
                && self.received_b == LOAD_TARGET_PER_CLIENT)
        {
            let last_sample_at = self.last_sample_at.unwrap_or(now);
            self.last_sample_at = Some(now);
            let (queue_depth, peak_depth, buffered, accepted) =
                aggregate_diagnostics(self.first.as_ref(), self.second.as_ref());
            let (oldest_queue_age_us, peak_oldest_queue_age_us) =
                aggregate_queue_ages(self.first.as_ref(), self.second.as_ref());
            self.peak_aggregate_depth = self.peak_aggregate_depth.max(queue_depth);
            let (send_budget_exhaustions, receive_budget_exhaustions) =
                aggregate_budget_exhaustions(self.first.as_ref(), self.second.as_ref());
            let received = self.received_a.saturating_add(self.received_b);
            let interval_ms = now
                .saturating_duration_since(last_sample_at)
                .as_millis()
                .max(1);
            let accepted_per_second =
                u128::from(accepted.saturating_sub(self.sample_last_accepted))
                    .saturating_mul(1_000)
                    .checked_div(interval_ms)
                    .unwrap_or(0);
            let received_per_second =
                u128::from(received.saturating_sub(self.sample_last_received))
                    .saturating_mul(1_000)
                    .checked_div(interval_ms)
                    .unwrap_or(0);
            let sample = serde_json::json!({
                "elapsed_ms": elapsed.as_millis(),
                "command_depth": queue_depth,
                "peak_depth": peak_depth,
                "oldest_queue_age_us": oldest_queue_age_us,
                "peak_oldest_queue_age_us": peak_oldest_queue_age_us,
                "buffered_bytes": buffered,
                "accepted_frames": accepted,
                "received_frames": received,
                "accepted_per_second": accepted_per_second,
                "received_per_second": received_per_second,
                "offered_frames": self.offered_a + self.offered_b,
                "poll_max_us": self.max_poll_us,
                "poll_work_frames": self.max_poll_work_frames,
                "poll_work_bytes": self.max_poll_work_bytes,
                "poll_receive_frames": self.max_poll_receive_frames,
                "poll_count": self.poll_count,
                "send_budget_exhaustions": send_budget_exhaustions,
                "receive_budget_exhaustions": receive_budget_exhaustions,
                "latest_latency_us": self.last_latency_us,
            });
            godot_print!("SIGNAL_FISH_LOAD sample {sample}");
            self.sample_last_accepted = accepted;
            self.sample_last_received = received;
            self.max_poll_work_frames = 0;
            self.max_poll_work_bytes = 0;
            self.max_poll_receive_frames = 0;
        }

        let queues_empty = self
            .first
            .as_ref()
            .is_none_or(|client| client.polling_stats().current_queue_depth == 0)
            && self
                .second
                .as_ref()
                .is_none_or(|client| client.polling_stats().current_queue_depth == 0);
        if elapsed >= Duration::from_secs(LOAD_SECONDS)
            && self.offered_a == LOAD_TARGET_PER_CLIENT
            && self.offered_b == LOAD_TARGET_PER_CLIENT
            && self.received_a == LOAD_TARGET_PER_CLIENT
            && self.received_b == LOAD_TARGET_PER_CLIENT
            && queues_empty
        {
            self.finish_load();
        } else if elapsed >= Duration::from_secs(LOAD_SECONDS + 5) {
            godot_error!(
                "SIGNAL_FISH_SMOKE load-drain-error offered={}/{} received={}/{}",
                self.offered_a,
                self.offered_b,
                self.received_a,
                self.received_b
            );
            self.finish_load();
        }
    }

    fn record_load_receive(&mut self, data: &serde_json::Value, from_a: bool) {
        let (Some(started), Some(sequence), Some(sent_us)) = (
            self.load_started,
            data.get("seq").and_then(serde_json::Value::as_u64),
            data.get("sent_us").and_then(serde_json::Value::as_u64),
        ) else {
            return;
        };
        let expected = if from_a {
            self.received_a
        } else {
            self.received_b
        };
        if sequence != expected {
            self.load_error = true;
            godot_error!(
                "SIGNAL_FISH_SMOKE load-order-error sender={} expected={expected} actual={sequence}",
                if from_a { "a" } else { "b" }
            );
        }
        if from_a {
            self.received_a = self.received_a.saturating_add(1);
        } else {
            self.received_b = self.received_b.saturating_add(1);
        }
        let now_us = Instant::now()
            .saturating_duration_since(started)
            .as_micros()
            .min(u128::from(u64::MAX)) as u64;
        let latency = now_us.saturating_sub(sent_us);
        self.last_latency_us = latency;
        self.latencies_us.push(latency);
    }

    fn finish_load(&mut self) {
        if self.closing || self.load_finished_at.is_some() {
            return;
        }
        self.latencies_us.sort_unstable();
        let p99_index = self
            .latencies_us
            .len()
            .saturating_mul(99)
            .div_ceil(100)
            .saturating_sub(1);
        let p99_us = self
            .latencies_us
            .get(p99_index)
            .copied()
            .unwrap_or(u64::MAX);
        let (queue_depth, peak_depth, buffered, accepted) =
            aggregate_diagnostics(self.first.as_ref(), self.second.as_ref());
        let admission_hits = [self.first.as_ref(), self.second.as_ref()]
            .into_iter()
            .flatten()
            .map(|client| {
                let diagnostics = client.transport_diagnostics();
                diagnostics
                    .watermark_hits
                    .saturating_add(diagnostics.backend_capacity_hits)
            })
            .fold(0u64, u64::saturating_add);
        let buffering_safe = [self.first.as_ref(), self.second.as_ref()]
            .into_iter()
            .flatten()
            .all(|client| {
                let diagnostics = client.transport_diagnostics();
                diagnostics.peak_buffered_bytes <= diagnostics.effective_watermark_bytes
            });
        let per_client_peak_depth = [self.first.as_ref(), self.second.as_ref()]
            .map(|client| client.map_or(0, |client| client.polling_stats().peak_queue_depth));
        let per_client_peak_oldest_queue_age_us = [self.first.as_ref(), self.second.as_ref()]
            .map(|client| {
                client.map_or(0, |client| {
                    client
                        .queue_age_stats()
                        .peak_oldest_queue_age
                        .as_micros()
                        .min(u128::from(u64::MAX)) as u64
                })
            });
        let passed = self.offered_a == LOAD_TARGET_PER_CLIENT
            && self.offered_b == LOAD_TARGET_PER_CLIENT
            && self.received_a == LOAD_TARGET_PER_CLIENT
            && self.received_b == LOAD_TARGET_PER_CLIENT
            && queue_depth == 0
            && self.peak_aggregate_depth <= 64
            && per_client_peak_depth.into_iter().all(|depth| depth <= 64)
            && per_client_peak_oldest_queue_age_us
                .into_iter()
                .all(|age_us| age_us <= 500_000)
            && admission_hits == 0
            && self.multi_frame_poll
            && self.max_poll_us.max(self.other_pair_max_poll_us) < 50_000
            && p99_us <= 500_000
            && buffering_safe;
        let per_client_peak_buffered = [self.first.as_ref(), self.second.as_ref()].map(|client| {
            client.map_or(0, |client| {
                client.transport_diagnostics().peak_buffered_bytes
            })
        });
        let per_client_watermark = [self.first.as_ref(), self.second.as_ref()].map(|client| {
            client.map_or(0, |client| {
                client.transport_diagnostics().effective_watermark_bytes
            })
        });
        let passed = passed && !self.load_error;
        let summary = serde_json::json!({
            "passed": passed,
            "offered_per_client": [self.offered_a, self.offered_b],
            "received_per_client": [self.received_a, self.received_b],
            "final_queue_depth": queue_depth,
            "peak_queue_depth": peak_depth,
            "peak_aggregate_queue_depth": self.peak_aggregate_depth,
            "per_client_peak_queue_depth": per_client_peak_depth,
            "per_client_peak_oldest_queue_age_us": per_client_peak_oldest_queue_age_us,
            "buffered_bytes": buffered,
            "accepted_frames": accepted,
            "admission_hits": admission_hits,
            "multi_frame_poll": self.multi_frame_poll,
            "max_poll_us": self.max_poll_us.max(self.other_pair_max_poll_us),
            "p99_latency_us": p99_us,
            "buffering_safe": buffering_safe,
            "per_client_peak_buffered_bytes": per_client_peak_buffered,
            "per_client_effective_watermark_bytes": per_client_watermark,
            "one_frame_escape_bytes": 0,
            "load_error": self.load_error,
        });
        if passed {
            godot_print!("SIGNAL_FISH_SMOKE load-summary {summary}");
        } else {
            godot_error!("SIGNAL_FISH_SMOKE load-summary {summary}");
        }
        self.load_finished_at = Some(Instant::now());
    }

    fn handle_disconnect(&mut self, peer: &str, reason: Option<&str>) {
        let label = self.kind.label();
        if self.kind == PairKind::Binary
            && self.relay_received
            && reason.is_some_and(|reason| {
                reason.contains("code=Some(4000)") || reason.contains("code=4000")
            })
        {
            godot_print!("SIGNAL_FISH_SMOKE close-attribution-ok {peer}");
            self.close_attributed = true;
            close_client(&mut self.first);
            close_client(&mut self.second);
        } else if !self.closing {
            godot_error!("SIGNAL_FISH_SMOKE {label}-unexpected-disconnect {peer} {reason:?}");
        }
    }

    fn drive_close(&mut self) {
        if let Some(client) = &mut self.first {
            let _ = client.poll();
        }
        if let Some(client) = &mut self.second {
            let _ = client.poll();
        }
        let first_done = self
            .first
            .as_ref()
            .is_none_or(|client| !client.is_closing());
        let second_done = self
            .second
            .as_ref()
            .is_none_or(|client| !client.is_closing());
        if first_done && second_done {
            godot_print!("SIGNAL_FISH_SMOKE json-shutdown-ok");
            self.shutdown_done = true;
        }
    }
}

#[derive(GodotClass)]
#[class(base = Node)]
struct SignalFishSmoke {
    base: Base<Node>,
    json: SmokePair,
    binary: SmokePair,
    complete: bool,
}

#[godot_api]
impl INode for SignalFishSmoke {
    fn init(base: Base<Node>) -> Self {
        Self {
            base,
            json: SmokePair::new(PairKind::Json),
            binary: SmokePair::new(PairKind::Binary),
            complete: false,
        }
    }

    fn ready(&mut self) {
        godot_print!("SIGNAL_FISH_SMOKE fixture-ready");
    }

    fn process(&mut self, _delta: f64) {
        if self.complete {
            return;
        }
        self.binary.poll();
        self.json.other_pair_max_poll_us = self.binary.max_poll_us;
        self.json.poll();
        if self.json.shutdown_done && self.binary.close_attributed {
            godot_print!("SIGNAL_FISH_SMOKE complete");
            self.complete = true;
            if let Some(mut tree) = self.base().get_tree() {
                tree.quit();
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn poll_measured(
    client: &mut Option<Client>,
    max_poll_us: &mut u64,
    multi_frame_poll: &mut bool,
    last_accepted: &mut u64,
    max_poll_work_frames: &mut u64,
    last_accepted_bytes: &mut u64,
    max_poll_work_bytes: &mut u64,
    max_poll_receive_frames: &mut u64,
    poll_count: &mut u64,
) -> Vec<SignalFishEvent> {
    let Some(client) = client else {
        return Vec::new();
    };
    let started = Instant::now();
    let events = client.poll();
    *poll_count = poll_count.saturating_add(1);
    let elapsed_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
    *max_poll_us = (*max_poll_us).max(elapsed_us);
    let accepted = client.transport_diagnostics().accepted_frames;
    let accepted_delta = accepted.saturating_sub(*last_accepted);
    *multi_frame_poll |= accepted_delta > 1;
    *max_poll_work_frames = (*max_poll_work_frames).max(accepted_delta);
    let accepted_bytes = client.transport_diagnostics().accepted_bytes;
    *max_poll_work_bytes =
        (*max_poll_work_bytes).max(accepted_bytes.saturating_sub(*last_accepted_bytes));
    *max_poll_receive_frames =
        (*max_poll_receive_frames).max(u64::try_from(events.len()).unwrap_or(u64::MAX));
    *last_accepted = accepted;
    *last_accepted_bytes = accepted_bytes;
    events
}

fn offer_load(
    client: &mut Option<Client>,
    sender: &str,
    offered: &mut u64,
    due: u64,
    elapsed: Duration,
) -> bool {
    let Some(client) = client else {
        return true;
    };
    let available =
        LOAD_MAX_QUEUE_DEPTH_PER_CLIENT.saturating_sub(client.polling_stats().current_queue_depth);
    let bounded_due = due.min(offered.saturating_add(available));
    while *offered < bounded_due {
        let sent_us = elapsed.as_micros().min(u128::from(u64::MAX)) as u64;
        let data = serde_json::json!({
            "load_sender": sender,
            "seq": *offered,
            "sent_us": sent_us,
        });
        match client.send_game_data(data) {
            Ok(()) => *offered = offered.saturating_add(1),
            Err(error) => {
                godot_error!("SIGNAL_FISH_SMOKE load-admission-error sender={sender} {error}");
                return true;
            }
        }
    }
    false
}

fn aggregate_budget_exhaustions(first: Option<&Client>, second: Option<&Client>) -> (u64, u64) {
    [first, second]
        .into_iter()
        .flatten()
        .fold((0u64, 0u64), |(send, receive), client| {
            let stats = client.polling_stats();
            (
                send.saturating_add(stats.send_budget_exhaustions),
                receive.saturating_add(stats.receive_budget_exhaustions),
            )
        })
}

fn aggregate_queue_ages(first: Option<&Client>, second: Option<&Client>) -> (u64, u64) {
    [first, second].into_iter().flatten().fold(
        (0u64, 0u64),
        |(current_max, peak_max), client| {
            let stats = client.queue_age_stats();
            let current = stats
                .current_oldest_queue_age
                .as_micros()
                .min(u128::from(u64::MAX)) as u64;
            let peak = stats
                .peak_oldest_queue_age
                .as_micros()
                .min(u128::from(u64::MAX)) as u64;
            (current_max.max(current), peak_max.max(peak))
        },
    )
}

fn aggregate_diagnostics(first: Option<&Client>, second: Option<&Client>) -> (u64, u64, u64, u64) {
    [first, second].into_iter().flatten().fold(
        (0u64, 0u64, 0u64, 0u64),
        |(queue, peak, buffered, accepted), client| {
            let polling = client.polling_stats();
            let transport = client.transport_diagnostics();
            (
                queue.saturating_add(polling.current_queue_depth),
                peak.saturating_add(polling.peak_queue_depth),
                buffered.saturating_add(transport.current_buffered_bytes),
                accepted.saturating_add(transport.accepted_frames),
            )
        },
    )
}

fn connect_client(kind: PairKind, suffix: &str) -> Option<Client> {
    match GodotWebSocketTransport::connect(SERVER_URL) {
        Ok(transport) => {
            let mut config = SignalFishConfig::new(APP_ID).enable_v3();
            config.platform = Some(format!("godot-smoke-{}-{suffix}", kind.label()));
            config.game_data_format = Some(kind.encoding());
            Some(SignalFishPollingClient::new(transport, config))
        }
        Err(error) => {
            godot_error!("SIGNAL_FISH_SMOKE {}-transport-error {error}", kind.label());
            None
        }
    }
}

fn close_client(client: &mut Option<Client>) {
    if let Some(client) = client {
        client.close();
    }
}

struct SmokeExtension;

// The CI negative-control build enables this feature to force the raw
// Emscripten WebSocket imports into an otherwise valid Godot GDExtension.
// Official templates cannot resolve those optional JavaScript-library symbols.
#[cfg(feature = "raw-emscripten-proof")]
#[allow(deprecated)]
fn exercise_raw_emscripten_import() {
    let _ = signal_fish_client::EmscriptenWebSocketTransport::connect(SERVER_URL);
}

#[gdextension]
unsafe impl ExtensionLibrary for SmokeExtension {
    fn on_level_init(_level: InitLevel) {
        #[cfg(feature = "raw-emscripten-proof")]
        exercise_raw_emscripten_import();
    }
}
