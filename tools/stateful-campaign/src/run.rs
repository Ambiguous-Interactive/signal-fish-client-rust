//! Scenario runner + stateful oracle.
//!
//! Three oracle families run over every script:
//!
//! 1. the phase/terminal/coherence model (round 41),
//! 2. the per-frame event-expectation model (issue #219): every delivered
//!    frame must produce exactly one documented outcome multiset — a silent
//!    swallow of a non-suppressible event, a fabricated event, or a
//!    double-delivery is a finding,
//! 3. the stats/ledger equivalence model (issue #219): `ClientStats` counters
//!    must equal the harness's independent count of decoded frames, and the
//!    send-pressure archetype additionally asserts FIFO delivery and capacity
//!    accounting under `Pending`-refusing `poll_send` faces.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex as StdMutex;

use signal_fish_client::client::{
    GameDataDelivery, JoinRoomParams, ProtocolViolationPolicy, SignalFishConfig,
};
use signal_fish_client::event::SignalFishEvent;
use signal_fish_client::polling_client::SignalFishPollingClient;
use signal_fish_client::protocol::{
    ClientMessage, ConnectionInfo, DeliveryGapReason, LobbyState, PlayerId, ReconnectedPayload,
    ReplayStatus, RoomJoinedPayload, RoomOperationResult, ServerMessage, SpectatorJoinedPayload,
    SpectatorStateChangeReason, TransportKind,
};
use signal_fish_client::signal::PeerSignal;
#[allow(unused_imports)]
use signal_fish_client::SignalFishClientApi;

use crate::script::{
    Cmd, ConfigKind, EchoId, EchoKind, FenceKind, FrameMeta, Script, StampMode, Step,
};
use crate::transport::{lock, ScriptedTransport};

#[derive(Debug, Clone)]
pub struct Finding {
    pub category: String,
    pub detail: String,
    pub step_index: usize,
}

pub struct Outcome {
    pub findings: Vec<Finding>,
    pub events_seen: usize,
    pub frames_fed: usize,
    pub commands_refused: usize,
    pub commands_accepted: usize,
    pub violations: usize,
    /// True when a Disconnected event was observed (terminal teardown).
    pub terminal: bool,
    /// True when that teardown was violation-caused (not a peer close).
    pub violation_teardown: bool,
}

impl Outcome {
    fn new() -> Self {
        Self {
            findings: Vec::new(),
            events_seen: 0,
            frames_fed: 0,
            commands_refused: 0,
            commands_accepted: 0,
            violations: 0,
            terminal: false,
            violation_teardown: false,
        }
    }
}

/// Global coverage ledger (event/variant names emitted anywhere in the corpus).
pub static COVERAGE: StdMutex<BTreeSet<String>> = StdMutex::new(BTreeSet::new());

/// Global ledger of delivered `ServerMessage` variant names (wire coverage).
pub static DELIVERED: StdMutex<BTreeSet<String>> = StdMutex::new(BTreeSet::new());

fn note_delivered(name: &str) {
    lock(&DELIVERED).insert(name.to_string());
}

/// Watchdog heartbeat: incremented every step so a stalled library call is
/// detectable from the budget loop in `main`.
pub static HEARTBEAT: AtomicU64 = AtomicU64::new(0);
static VERBOSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
// Oracle strictness toggle: when `false`, every rejection branch is neutered
// and the oracle only counts events. `--selftest` proves every rejection
// canary is sensitive to this toggle (the deliberately-broken oracle must
// accept known-bad input). Settable via `STATEFUL_CAMPAIGN_BREAK_ORACLE=1`.
// Thread-local so the unit tests (canaries toggle it
// while the soak/smoke tests run concurrently) stay isolated; the binary
// drives everything on its main thread.
thread_local! {
    static ORACLE_STRICT: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Deliberately neuter the oracle (canary sensitivity proof).
pub fn set_oracle_neutered(broken: bool) {
    ORACLE_STRICT.with(|strict| strict.set(!broken));
}

fn oracle_strict() -> bool {
    ORACLE_STRICT.with(|strict| strict.get())
}

static DIFFERENTIAL_DONE: StdMutex<BTreeSet<(u64, usize)>> = StdMutex::new(BTreeSet::new());
pub static CURRENT_LABEL: StdMutex<String> = StdMutex::new(String::new());

fn heartbeat() {
    HEARTBEAT.fetch_add(1, Ordering::Relaxed);
}

fn note_coverage(name: &str) {
    lock(&COVERAGE).insert(name.to_string());
}

pub fn event_name(ev: &SignalFishEvent) -> &'static str {
    match ev {
        SignalFishEvent::Connected => "Connected",
        SignalFishEvent::Disconnected { .. } => "Disconnected",
        SignalFishEvent::DecodeFailed { .. } => "DecodeFailed",
        SignalFishEvent::ProtocolViolation { .. } => "ProtocolViolation",
        SignalFishEvent::Authenticated { .. } => "Authenticated",
        SignalFishEvent::ProtocolInfo(_) => "ProtocolInfo",
        SignalFishEvent::AuthenticationError { .. } => "AuthenticationError",
        SignalFishEvent::RoomJoined { .. } => "RoomJoined",
        SignalFishEvent::RoomJoinFailed { .. } => "RoomJoinFailed",
        SignalFishEvent::RoomLeft => "RoomLeft",
        SignalFishEvent::RoomOperationFailed { .. } => "RoomOperationFailed",
        SignalFishEvent::PlayerJoined { .. } => "PlayerJoined",
        SignalFishEvent::PlayerLeft { .. } => "PlayerLeft",
        SignalFishEvent::GameData { .. } => "GameData",
        SignalFishEvent::GameDataBinary { .. } => "GameDataBinary",
        SignalFishEvent::AuthorityChanged { .. } => "AuthorityChanged",
        SignalFishEvent::AuthorityResponse { .. } => "AuthorityResponse",
        SignalFishEvent::LobbyStateChanged { .. } => "LobbyStateChanged",
        SignalFishEvent::GameStarting { .. } => "GameStarting",
        SignalFishEvent::SessionPlan { .. } => "SessionPlan",
        SignalFishEvent::NewPeer { .. } => "NewPeer",
        SignalFishEvent::SignalReceived { .. } => "SignalReceived",
        SignalFishEvent::PeerTransportStatus { .. } => "PeerTransportStatus",
        SignalFishEvent::RelayStats { .. } => "RelayStats",
        SignalFishEvent::GoingAway { .. } => "GoingAway",
        SignalFishEvent::DeliveryReport(_) => "DeliveryReport",
        SignalFishEvent::Pong => "Pong",
        SignalFishEvent::Reconnected { .. } => "Reconnected",
        SignalFishEvent::ReconnectionFailed { .. } => "ReconnectionFailed",
        SignalFishEvent::PlayerReconnected { .. } => "PlayerReconnected",
        SignalFishEvent::SpectatorJoined { .. } => "SpectatorJoined",
        SignalFishEvent::SpectatorJoinFailed { .. } => "SpectatorJoinFailed",
        SignalFishEvent::SpectatorLeft { .. } => "SpectatorLeft",
        SignalFishEvent::NewSpectatorJoined { .. } => "NewSpectatorJoined",
        SignalFishEvent::SpectatorDisconnected { .. } => "SpectatorDisconnected",
        SignalFishEvent::Error { .. } => "Error",
        SignalFishEvent::Reconnecting { .. } => "Reconnecting",
        SignalFishEvent::ReconnectAbandoned { .. } => "ReconnectAbandoned",
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Player,
    Spectator,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TerminalCause {
    /// Disconnected followed a Disconnect-policy violation.
    Violation,
    /// Disconnected matched an armed peer-close face.
    PeerClose,
    /// Disconnected followed an armed terminal transport error.
    TransportError,
}

// ── Per-frame event-expectation oracle (issue #219) ─────────────────

/// One documented outcome for a delivered frame: an exact event-name multiset
/// plus a `ProtocolViolation` count. `Connected`/`Disconnected` are excluded
/// (the phase oracle owns them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AllowedOutcome {
    pub(crate) events: BTreeMap<&'static str, usize>,
    pub(crate) violations: usize,
}

fn event_multimap(name: &'static str) -> BTreeMap<&'static str, usize> {
    let mut map = BTreeMap::new();
    map.insert(name, 1);
    map
}

impl AllowedOutcome {
    fn empty() -> Self {
        Self {
            events: BTreeMap::new(),
            violations: 0,
        }
    }

    fn event(name: &'static str) -> Self {
        Self {
            events: event_multimap(name),
            violations: 0,
        }
    }

    fn violation() -> Self {
        Self {
            events: BTreeMap::new(),
            violations: 1,
        }
    }

    fn violation_then_event(name: &'static str) -> Self {
        Self {
            events: event_multimap(name),
            violations: 1,
        }
    }
}

/// The unresolved expectation for one delivered frame: the set of outcome
/// multisets the documented model permits.
pub(crate) struct Slot {
    pub(crate) alternatives: Vec<AllowedOutcome>,
    pub(crate) frame: &'static str,
    /// When the frame's outcome is a violation, the client retires the
    /// answered fence of this kind (accountability-invalid baselines).
    pub(crate) fence_retired_on_violation: Option<FenceKind>,
}

fn one_of(alternatives: Vec<AllowedOutcome>) -> Vec<AllowedOutcome> {
    alternatives
}

/// The exactly-one-event shorthand.
fn exactly(name: &'static str) -> Vec<AllowedOutcome> {
    one_of(vec![AllowedOutcome::event(name)])
}

/// Exactly one violation, nothing else (frame suppressed after diagnosis).
fn violation_only() -> Vec<AllowedOutcome> {
    one_of(vec![AllowedOutcome::violation()])
}

/// Event-or-violation validity-ambiguous faces (never silence). Under the
/// Observe policy an accountability-invalid frame is additionally delivered
/// diagnostically, so the violation+event face is legal there too.
fn event_or_violation(name: &'static str) -> Vec<AllowedOutcome> {
    one_of(vec![
        AllowedOutcome::event(name),
        AllowedOutcome::violation(),
        AllowedOutcome::violation_then_event(name),
    ])
}

impl Oracle {
    /// Game data: the frame must surface its event. Silence is legal only
    /// while a Quarantine-policy latch is active (the documented suppression
    /// path); a client that drops game data outside that latch fails the
    /// expectation oracle even though the stats counters stay honest.
    pub(crate) fn game_data_outcomes(&self, name: &'static str) -> Vec<AllowedOutcome> {
        if self.policy == ProtocolViolationPolicy::Quarantine && self.batch_quarantined {
            one_of(vec![AllowedOutcome::event(name), AllowedOutcome::empty()])
        } else {
            exactly(name)
        }
    }

    /// v2 game data: no stamp state exists, so most stamp faces cannot be
    /// predicted exactly; the frame must surface its event, an accountability
    /// violation, or (under Quarantine) documented suppression — never two
    /// events, never Observe silence.
    pub(crate) fn v2_game_data_outcomes(&self, name: &'static str) -> Vec<AllowedOutcome> {
        let mut alternatives = vec![AllowedOutcome::event(name)];
        alternatives.extend(violation_outcomes(self.policy, Some(name)));
        if self.policy == ProtocolViolationPolicy::Quarantine && self.batch_quarantined {
            alternatives.push(AllowedOutcome::empty());
        }
        one_of(alternatives)
    }

    /// Representation-mismatch faces (physical binary frames and v3
    /// text-delivered `GameDataBinary`): the representation gate always fires
    /// one violation, a second can stack from the re-validated stamps (an
    /// invalid stamp, or a frontier shifted by an earlier apply-anyway frame
    /// under Observe), and the Observe policy always delivers the decoded
    /// frame diagnostically — apply and apply-anyway both reach the event
    /// queue.
    pub(crate) fn binary_representation_outcomes(&self) -> Vec<AllowedOutcome> {
        let mut two_violations = AllowedOutcome::violation();
        two_violations.violations = 2;
        match self.policy {
            ProtocolViolationPolicy::Observe => {
                let mut two_with_event = AllowedOutcome::violation_then_event("GameDataBinary");
                two_with_event.violations = 2;
                one_of(vec![
                    AllowedOutcome::violation_then_event("GameDataBinary"),
                    two_with_event,
                ])
            }
            _ => one_of(vec![AllowedOutcome::violation(), two_violations]),
        }
    }
}

/// Accountability-invalid faces under the Observe policy additionally deliver
/// the frame diagnostically (violation + event); under Quarantine/Disconnect
/// the frame is suppressed after the violation.
fn violation_outcomes(
    policy: ProtocolViolationPolicy,
    event: Option<&'static str>,
) -> Vec<AllowedOutcome> {
    match (policy, event) {
        (ProtocolViolationPolicy::Observe, Some(name)) => one_of(vec![
            AllowedOutcome::violation(),
            AllowedOutcome::violation_then_event(name),
        ]),
        _ => violation_only(),
    }
}

/// Independent phase/suppression model driven only by observed events.
pub(crate) struct Oracle {
    pub(crate) policy: ProtocolViolationPolicy,
    pub(crate) config: ConfigKind,
    pub(crate) echo_room_ops: bool,
    pub(crate) connected_event_seen: bool,
    pub(crate) any_event_seen: bool,
    pub(crate) authenticated: bool,
    pub(crate) protocol_info_seen: bool,
    pub(crate) v3: bool,
    pub(crate) correlation: bool,
    pub(crate) plan_seen: bool,
    pub(crate) session_transport: Option<TransportKind>,
    pub(crate) session_generation: Option<PlayerId>,
    pub(crate) plan_peers: BTreeSet<PlayerId>,
    pub(crate) roster: BTreeSet<PlayerId>,
    pub(crate) local_id: Option<PlayerId>,
    pub(crate) room_id: Option<PlayerId>,
    pub(crate) room_code: Option<String>,
    pub(crate) membership: Option<Role>,
    pub(crate) room_finalized: bool,
    pub(crate) terminal: bool,
    pub(crate) disconnected_seen: bool,
    pub(crate) violations: usize,
    /// Intra-batch quarantine view under the Quarantine policy.
    pub(crate) batch_quarantined: bool,
    pub(crate) pending_disconnect: bool,
    /// Server-initiated close armed: a Disconnected event is legal under any policy.
    pub(crate) peer_close_armed: bool,
    /// A Disconnected event was caused by a violation (not a peer close).
    pub(crate) violation_teardown: bool,
    /// Terminal transport error armed: Disconnected with the error cause is legal.
    pub(crate) transport_error_armed: bool,
    /// Exact `Display` strings of the armed terminal transport errors; the
    /// client must copy one of them verbatim into the `Disconnected` reason.
    pub(crate) expected_transport_reasons: Vec<String>,
    /// Classification of the observed terminal `Disconnected` event.
    pub(crate) terminal_cause: Option<TerminalCause>,
    /// `reason` field captured from the observed `Disconnected` event.
    pub(crate) disconnect_reason: Option<String>,
    /// Violation observed since the last membership transition (Quarantine
    /// policy): `snapshot.quarantined` must be positively latched.
    pub(crate) quarantine_latched: bool,

    // Expectation-oracle state (mirrors the client's documented fences).
    pub(crate) fence: Option<FenceKind>,
    pub(crate) pending_reconnects: usize,
    /// Armed when an authoritative SpectatorLeft overtook a pending voluntary
    /// leave: exactly one matching late reply is silently absorbed. The id is
    /// captured by the runner from the outbound log right after the exit.
    pub(crate) absorbed_leave_armed: bool,
    pub(crate) absorbed_leave_id: Option<PlayerId>,
    /// Relay-stats validity tracking (interval constancy, monotone counters).
    pub(crate) relay_interval: Option<u64>,
    pub(crate) relay_counters: (u64, u64, u64),
    /// An unsupported-format gap armed the advisory for a causal Error frame.
    pub(crate) unsupported_format_advisory_armed: bool,

    /// Pending per-frame expectations (issue #219 oracle).
    pub(crate) slots: VecDeque<Slot>,
}

impl Oracle {
    pub(crate) fn new(
        policy: ProtocolViolationPolicy,
        config: ConfigKind,
        echo_room_ops: bool,
    ) -> Self {
        Self {
            policy,
            config,
            echo_room_ops,
            connected_event_seen: false,
            any_event_seen: false,
            authenticated: false,
            protocol_info_seen: false,
            v3: false,
            correlation: false,
            plan_seen: false,
            session_transport: None,
            session_generation: None,
            plan_peers: BTreeSet::new(),
            roster: BTreeSet::new(),
            local_id: None,
            room_id: None,
            room_code: None,
            membership: None,
            room_finalized: false,
            terminal: false,
            disconnected_seen: false,
            violations: 0,
            batch_quarantined: false,
            pending_disconnect: false,
            peer_close_armed: false,
            violation_teardown: false,
            transport_error_armed: false,
            expected_transport_reasons: Vec::new(),
            terminal_cause: None,
            disconnect_reason: None,
            quarantine_latched: false,
            fence: None,
            pending_reconnects: 0,
            absorbed_leave_armed: false,
            absorbed_leave_id: None,
            relay_interval: None,
            relay_counters: (0, 0, 0),
            unsupported_format_advisory_armed: false,
            slots: VecDeque::new(),
        }
    }

    pub(crate) fn arm_transport_error(&mut self, fail_recv: bool, fail_send: bool) {
        self.transport_error_armed = true;
        if fail_recv {
            self.expected_transport_reasons
                .push(crate::transport::RECV_ERROR_DISPLAY.to_string());
        }
        if fail_send {
            self.expected_transport_reasons
                .push(crate::transport::SEND_ERROR_DISPLAY.to_string());
        }
    }

    pub(crate) fn begin_batch(&mut self, quarantined: bool) {
        self.batch_quarantined = quarantined;
    }

    pub(crate) fn in_room(&self) -> bool {
        self.authenticated && self.membership.is_some()
    }

    pub(crate) fn is_player(&self) -> bool {
        self.authenticated && self.membership == Some(Role::Player)
    }

    pub(crate) fn self_sender(&self, id: &PlayerId) -> bool {
        self.local_id.is_some_and(|local| local == *id)
    }

    /// Compute the documented outcome set for a delivered frame BEFORE it is
    /// processed. Reactively-updated state (roster, plan, relay, fences) comes
    /// from previously observed events, so this mirrors the client's gates.
    pub(crate) fn expectation_for(
        &self,
        msg: &ServerMessage,
        meta: &FrameMeta,
    ) -> Vec<AllowedOutcome> {
        use ServerMessage as M;
        // The documented negotiation gate runs before every per-variant phase
        // gate: everything except the auth/negotiation/pong/error family
        // requires a completed ProtocolInfo.
        if !matches!(
            msg,
            M::Authenticated { .. }
                | M::AuthenticationError { .. }
                | M::ProtocolInfo(_)
                | M::Pong
                | M::Error { .. }
        ) && !self.protocol_info_seen
        {
            return violation_only();
        }
        match msg {
            M::Authenticated { .. } => {
                if !self.authenticated && self.membership.is_none() {
                    exactly("Authenticated")
                } else {
                    violation_only()
                }
            }
            M::AuthenticationError { .. } => {
                if !self.authenticated && self.membership.is_none() {
                    exactly("AuthenticationError")
                } else {
                    violation_only()
                }
            }
            M::ProtocolInfo(_) => {
                if !self.authenticated || self.membership.is_some() || self.protocol_info_seen {
                    return violation_only();
                }
                if meta.bound_breaking {
                    // Outbound bound beyond the authority's validated range.
                    return violation_only();
                }
                exactly("ProtocolInfo")
            }
            M::RoomJoined(payload) => {
                if !self.authenticated || self.membership.is_some() {
                    return violation_only();
                }
                if self.fence != Some(FenceKind::JoinPlayer) {
                    return violation_only();
                }
                if self.correlation {
                    // Unwrapped response while a correlated operation pends.
                    return violation_only();
                }
                if !baseline_roster_shape_valid(
                    payload.player_id,
                    payload.is_authority,
                    &payload.current_players,
                ) || !baseline_roster_stamps_valid(self.v3, &payload.current_players)
                {
                    return violation_only();
                }
                event_or_violation("RoomJoined")
            }
            M::RoomJoinFailed { .. } => {
                if !self.authenticated || self.fence != Some(FenceKind::JoinPlayer) {
                    return violation_only();
                }
                if self.correlation {
                    return violation_only();
                }
                exactly("RoomJoinFailed")
            }
            M::RoomLeft => {
                if !self.is_player() || self.fence != Some(FenceKind::LeavePlayer) {
                    return violation_only();
                }
                if self.correlation {
                    return violation_only();
                }
                exactly("RoomLeft")
            }
            M::PlayerJoined { player } => {
                if !self.in_room() {
                    return violation_only();
                }
                if self.v3 && v3_stamp_invalid(player.epoch, player.seq) {
                    return violation_only();
                }
                event_or_violation("PlayerJoined")
            }
            M::PlayerLeft {
                player_id: _,
                epoch,
                final_seq,
            } => {
                if !self.in_room() {
                    return violation_only();
                }
                let shape_invalid = (self.v3
                    && (epoch.is_none() != final_seq.is_none() || epoch == &Some(0)))
                    || (!self.v3 && (epoch.is_some() || final_seq.is_some()));
                if shape_invalid {
                    // Stamp-shape violations are accountability faces: the
                    // Observe policy delivers the frame diagnostically.
                    return violation_outcomes(self.policy, Some("PlayerLeft"));
                }
                event_or_violation("PlayerLeft")
            }
            M::GameData { seq, epoch, .. } => {
                if !self.in_room() {
                    return violation_only();
                }
                match (self.v3, seq.is_none() && epoch.is_none(), meta.stamp) {
                    (false, true, _) => self.v2_game_data_outcomes("GameData"),
                    (false, false, _) => violation_only(),
                    (true, _, StampMode::Stale) => {
                        // Backward/replayed sequence: a stamp violation.
                        violation_outcomes(self.policy, Some("GameData"))
                    }
                    (true, _, StampMode::Valid) => self.game_data_outcomes("GameData"),
                    (true, _, _) => violation_outcomes(self.policy, Some("GameData")),
                }
            }
            M::GameDataBinary { seq, epoch, .. } => {
                if !self.in_room() {
                    return violation_only();
                }
                if !self.v3 {
                    return match seq.is_none() && epoch.is_none() {
                        true => exactly("GameDataBinary"),
                        false => violation_only(),
                    };
                }
                // On v3 the text-delivered binary type is the documented
                // representation-mismatch face; an invalid stamp can stack a
                // second violation, and Observe delivers through both.
                self.binary_representation_outcomes()
            }
            M::AuthorityChanged { .. } => {
                if !self.in_room() {
                    return violation_only();
                }
                // Named-authority-in-roster and you-are faces stay
                // validity-ambiguous (the roster view is reactive only).
                event_or_violation("AuthorityChanged")
            }
            M::AuthorityResponse { .. } => {
                if !self.authenticated || !self.protocol_info_seen {
                    return violation_only();
                }
                exactly("AuthorityResponse")
            }
            M::LobbyStateChanged { .. } => {
                if !self.in_room() {
                    return violation_only();
                }
                exactly("LobbyStateChanged")
            }
            M::GameStarting { .. } => {
                if !self.in_room() {
                    return violation_only();
                }
                exactly("GameStarting")
            }
            M::Pong => exactly("Pong"),
            M::Reconnected(payload) => {
                if !self.authenticated || self.membership.is_some() {
                    return violation_only();
                }
                if self.fence != Some(FenceKind::ReconnectPlayer) || self.pending_reconnects == 0 {
                    return violation_only();
                }
                if self.correlation {
                    return violation_only();
                }
                if self.v3 {
                    // Replay is mandatory and the rotated token must differ
                    // from the submitted one (the generator never submits a
                    // "rotated-" token).
                    if payload.replay.is_none() || payload.reconnection_token.is_none() {
                        return violation_only();
                    }
                } else if payload.replay.is_some()
                    || !payload.sender_watermarks.is_empty()
                    || payload.reconnection_token.is_some()
                {
                    return violation_only();
                }
                event_or_violation("Reconnected")
            }
            M::ReconnectionFailed { .. } => {
                if !self.authenticated
                    || self.fence != Some(FenceKind::ReconnectPlayer)
                    || self.pending_reconnects == 0
                {
                    return violation_only();
                }
                if self.correlation {
                    return violation_only();
                }
                exactly("ReconnectionFailed")
            }
            M::PlayerReconnected { epoch, .. } => {
                if !self.in_room() {
                    return violation_only();
                }
                let shape_invalid = (self.v3 && (epoch.is_none() || epoch == &Some(0)))
                    || (!self.v3 && epoch.is_some());
                if shape_invalid {
                    violation_outcomes(self.policy, Some("PlayerReconnected"))
                } else {
                    event_or_violation("PlayerReconnected")
                }
            }
            M::SpectatorJoined(payload) => {
                if !self.authenticated || self.membership.is_some() {
                    return violation_only();
                }
                if self.fence != Some(FenceKind::JoinSpectator) {
                    return violation_only();
                }
                if self.correlation {
                    return violation_only();
                }
                if self.v3
                    && payload
                        .current_players
                        .iter()
                        .any(|p| v3_stamp_invalid(p.epoch, p.seq))
                {
                    return violation_only();
                }
                event_or_violation("SpectatorJoined")
            }
            M::SpectatorJoinFailed { .. } => {
                if !self.authenticated || self.fence != Some(FenceKind::JoinSpectator) {
                    return violation_only();
                }
                if self.correlation {
                    return violation_only();
                }
                exactly("SpectatorJoinFailed")
            }
            M::SpectatorLeft {
                room_id,
                room_code,
                reason,
                ..
            } => {
                if !self.authenticated || self.membership != Some(Role::Spectator) {
                    return violation_only();
                }
                match reason {
                    Some(SpectatorStateChangeReason::Joined) => violation_only(),
                    Some(
                        SpectatorStateChangeReason::Disconnected
                        | SpectatorStateChangeReason::Removed
                        | SpectatorStateChangeReason::RoomClosed,
                    ) => {
                        // A named room must match the joined one (echo joins
                        // make the joined room differ from the generator's).
                        let identity_matches =
                            match (room_id, room_code, &self.room_id, &self.room_code) {
                                (Some(named_id), _, Some(joined_id), _)
                                    if named_id != joined_id =>
                                {
                                    false
                                }
                                (None, Some(named_code), _, Some(joined_code))
                                    if !joined_code.is_empty() && named_code != joined_code =>
                                {
                                    false
                                }
                                _ => true,
                            };
                        if identity_matches {
                            exactly("SpectatorLeft")
                        } else {
                            violation_only()
                        }
                    }
                    Some(SpectatorStateChangeReason::VoluntaryLeave) | None => {
                        if self.correlation || self.fence != Some(FenceKind::LeaveSpectator) {
                            violation_only()
                        } else {
                            exactly("SpectatorLeft")
                        }
                    }
                }
            }
            M::NewSpectatorJoined { .. } => {
                if !self.in_room() {
                    return violation_only();
                }
                exactly("NewSpectatorJoined")
            }
            M::SpectatorDisconnected { .. } => {
                if !self.in_room() {
                    return violation_only();
                }
                exactly("SpectatorDisconnected")
            }
            M::Error { error_code, .. } => {
                if self.v3
                    && error_code == &Some(signal_fish_client::ErrorCode::UnsupportedGameDataFormat)
                {
                    // Causality face: the advisory tracking here is an
                    // approximation of the client's armed-range bookkeeping,
                    // so the violation faces stay legal even when this oracle
                    // believes the advisory is armed.
                    let mut alternatives = vec![AllowedOutcome::event("Error")];
                    alternatives.extend(violation_outcomes(self.policy, Some("Error")));
                    one_of(alternatives)
                } else {
                    exactly("Error")
                }
            }
            M::Signal {
                from, generation, ..
            } => {
                if !self.is_player() || !self.v3 || !self.plan_seen {
                    return violation_only();
                }
                if generation != &self.session_generation {
                    // Stale/unknown generation: silently suppressed. The
                    // documented suppression check precedes sender checks.
                    return one_of(vec![AllowedOutcome::empty()]);
                }
                if self.session_transport != Some(TransportKind::WebRtc) {
                    return violation_only();
                }
                if self.self_sender(from) || !self.plan_peers.contains(from) {
                    return violation_only();
                }
                exactly("SignalReceived")
            }
            M::NewPeer { peer_id, .. } => {
                if !self.is_player() || !self.v3 {
                    return violation_only();
                }
                if !self.plan_seen || self.session_transport != Some(TransportKind::WebRtc) {
                    return violation_only();
                }
                if self.self_sender(peer_id) || !self.roster.contains(peer_id) {
                    return violation_only();
                }
                exactly("NewPeer")
            }
            M::SessionPlan(_) => {
                if !self.is_player() || !self.v3 || !self.room_finalized {
                    return violation_only();
                }
                event_or_violation("SessionPlan")
            }
            M::PeerTransportStatus { peer_id, .. } => {
                if !self.is_player() || !self.v3 {
                    return violation_only();
                }
                if self.self_sender(peer_id) || !self.roster.contains(peer_id) {
                    return violation_only();
                }
                exactly("PeerTransportStatus")
            }
            M::RelayStats {
                interval_ms,
                sent_to_you,
                dropped_for_you,
                backpressure_events,
            } => {
                if !self.authenticated || !self.v3 {
                    return violation_only();
                }
                let counters = (*sent_to_you, *dropped_for_you, *backpressure_events);
                let moved_backward = counters.0 < self.relay_counters.0
                    || counters.1 < self.relay_counters.1
                    || counters.2 < self.relay_counters.2;
                if meta.bound_breaking
                    || *interval_ms == 0
                    || self.relay_interval.is_some_and(|seen| seen != *interval_ms)
                    || moved_backward
                {
                    return violation_outcomes(self.policy, Some("RelayStats"));
                }
                exactly("RelayStats")
            }
            M::GoingAway { .. } => {
                if !self.authenticated || !self.v3 {
                    return violation_only();
                }
                exactly("GoingAway")
            }
            M::DeliveryReport(_) => {
                if !self.in_room() || !self.v3 {
                    return violation_only();
                }
                if meta.bound_breaking {
                    return violation_outcomes(self.policy, Some("DeliveryReport"));
                }
                event_or_violation("DeliveryReport")
            }
            M::RoomOperationResult { .. } => {
                // Normalized before validation; never reaches the gates.
                violation_only()
            }
        }
    }

    /// Documented outcome set for a physical binary frame (the campaign's
    /// negotiated format is always Json, so the representation gate fires).
    pub(crate) fn expectation_for_binary(&self, _meta: &FrameMeta) -> Vec<AllowedOutcome> {
        if !self.in_room() || !self.protocol_info_seen {
            return violation_only();
        }
        if !self.v3 {
            // The v3-shaped envelope fails the v2 decoder after the
            // representation violation; Observe continues into DecodeFailed.
            return match self.policy {
                ProtocolViolationPolicy::Observe => one_of(vec![
                    AllowedOutcome {
                        events: event_multimap("DecodeFailed"),
                        violations: 1,
                    },
                    AllowedOutcome::violation(),
                ]),
                _ => violation_only(),
            };
        }
        self.binary_representation_outcomes()
    }

    /// Whether a violating baseline retires its answered fence: the round-12
    /// `retire_answered_room_operation` rule applies to accountability-invalid
    /// baselines; a definite lifecycle rejection keeps the fence armed.
    pub(crate) fn retire_on_violation(&self, msg: &ServerMessage) -> Option<FenceKind> {
        use ServerMessage as M;
        match msg {
            M::RoomJoined(payload) => {
                if !self.authenticated
                    || self.membership.is_some()
                    || self.fence != Some(FenceKind::JoinPlayer)
                    || self.correlation
                {
                    return None;
                }
                // v2 token/ICE exposure and roster-shape failures are
                // definite lifecycle rejections; stamp failures are the
                // accountability layer and retire the answered fence.
                if (!self.v3
                    && (payload.reconnection_token.is_some() || !payload.ice_servers.is_empty()))
                    || !baseline_roster_shape_valid(
                        payload.player_id,
                        payload.is_authority,
                        &payload.current_players,
                    )
                {
                    return None;
                }
                Some(FenceKind::JoinPlayer)
            }
            M::Reconnected(payload) => {
                if !self.authenticated
                    || self.membership.is_some()
                    || self.fence != Some(FenceKind::ReconnectPlayer)
                    || self.pending_reconnects == 0
                    || self.correlation
                {
                    return None;
                }
                if self.v3 {
                    if payload.replay.is_none() || payload.reconnection_token.is_none() {
                        return None;
                    }
                } else if payload.replay.is_some()
                    || !payload.sender_watermarks.is_empty()
                    || payload.reconnection_token.is_some()
                {
                    return None;
                }
                Some(FenceKind::ReconnectPlayer)
            }
            M::SpectatorJoined(_payload) => {
                if !self.authenticated
                    || self.membership.is_some()
                    || self.fence != Some(FenceKind::JoinSpectator)
                    || self.correlation
                {
                    return None;
                }
                Some(FenceKind::JoinSpectator)
            }
            _ => None,
        }
    }

    /// Compute the documented outcome set for a `RoomOperationResult` echo.
    /// `id_matches` reports whether the embedded id equals the id of the
    /// client's currently pending operation.
    pub(crate) fn expectation_for_echo(
        &self,
        kind: EchoKind,
        id_matches: bool,
    ) -> Vec<AllowedOutcome> {
        if !self.correlation {
            // Enveloped results without negotiated correlation always violate.
            return violation_only();
        }
        // The one absorbed late reply for an overtaken voluntary leave.
        if self.absorbed_leave_id.is_some() && kind == EchoKind::SpectatorLeaveOk && id_matches {
            return one_of(vec![AllowedOutcome::empty()]);
        }
        let Some(fence) = self.fence else {
            return violation_only();
        };
        if !echo_kind_matches_fence(kind, fence) || !id_matches {
            return violation_only();
        }
        match kind {
            EchoKind::OperationFailed => exactly("RoomOperationFailed"),
            // The echo's Reconnected face historically carries no rotated
            // token, which the v3 lifecycle gate rejects; the fence retires
            // with the violation.
            EchoKind::ReconnectOk if self.v3 => violation_only(),
            EchoKind::JoinOk | EchoKind::SpectatorJoinOk | EchoKind::ReconnectOk => {
                event_or_violation(kind.name())
            }
            _ => exactly(kind.name()),
        }
    }

    /// Reactive state updates derived from observed events. The expectation
    /// oracle tracks fences, rosters, plans, and counters from what actually
    /// surfaced, so its predictions for later frames stay grounded.
    pub(crate) fn track_event(&mut self, ev: &SignalFishEvent) {
        match ev {
            SignalFishEvent::Authenticated { .. } => self.authenticated = true,
            SignalFishEvent::ProtocolInfo(payload) => {
                self.protocol_info_seen = true;
                self.v3 = payload.protocol_version.is_some_and(|v| v >= 3);
                self.correlation = self.v3
                    && self.config.requests_room_operation_ids()
                    && self.echo_room_ops
                    && payload
                        .capabilities
                        .iter()
                        .any(|capability| capability == "room_operation_ids");
            }
            SignalFishEvent::RoomJoined {
                player_id,
                room_id,
                room_code,
                current_players,
                lobby_state,
                ..
            } => {
                self.local_id = Some(*player_id);
                self.room_id = Some(*room_id);
                self.room_code = Some(room_code.clone());
                self.roster = current_players.iter().map(|p| p.id).collect();
                self.membership = Some(Role::Player);
                self.room_finalized = matches!(lobby_state, LobbyState::Finalized);
                self.batch_quarantined = false;
                self.quarantine_latched = false;
                self.absorbed_leave_armed = false;
                self.absorbed_leave_id = None;
                self.plan_seen = false;
                self.session_generation = None;
                self.session_transport = None;
                self.plan_peers.clear();
                if self.fence == Some(FenceKind::JoinPlayer) {
                    self.fence = None;
                }
            }
            SignalFishEvent::Reconnected {
                player_id,
                room_id,
                room_code,
                current_players,
                ..
            } => {
                self.local_id = Some(*player_id);
                self.room_id = Some(*room_id);
                self.room_code = Some(room_code.clone());
                self.roster = current_players.iter().map(|p| p.id).collect();
                self.membership = Some(Role::Player);
                self.room_finalized = true;
                self.batch_quarantined = false;
                self.quarantine_latched = false;
                self.absorbed_leave_armed = false;
                self.absorbed_leave_id = None;
                self.plan_seen = false;
                self.session_generation = None;
                self.session_transport = None;
                self.plan_peers.clear();
                self.pending_reconnects = self.pending_reconnects.saturating_sub(1);
                if self.fence == Some(FenceKind::ReconnectPlayer) {
                    self.fence = None;
                }
            }
            SignalFishEvent::SpectatorJoined {
                spectator_id,
                room_id,
                room_code,
                ..
            } => {
                self.local_id = Some(*spectator_id);
                self.room_id = Some(*room_id);
                self.room_code = Some(room_code.clone());
                self.membership = Some(Role::Spectator);
                self.batch_quarantined = false;
                self.quarantine_latched = false;
                self.absorbed_leave_armed = false;
                self.absorbed_leave_id = None;
                self.plan_seen = false;
                self.session_generation = None;
                self.session_transport = None;
                self.plan_peers.clear();
                if self.fence == Some(FenceKind::JoinSpectator) {
                    self.fence = None;
                }
            }
            SignalFishEvent::RoomLeft => {
                self.membership = None;
                self.room_finalized = false;
                self.roster.clear();
                self.local_id = None;
                self.room_id = None;
                self.room_code = None;
                self.room_id = None;
                self.room_code = None;
                self.quarantine_latched = false;
                self.plan_seen = false;
                self.session_generation = None;
                self.session_transport = None;
                self.plan_peers.clear();
                if self.fence == Some(FenceKind::LeavePlayer) {
                    self.fence = None;
                }
            }
            SignalFishEvent::SpectatorLeft {
                reason,
                room_id,
                room_code,
                ..
            } => {
                // Authoritative exits arm the absorbed-late-reply allowance
                // when they overtake a pending voluntary leave; the runner
                // fills in the captured operation id right after this event.
                if matches!(
                    reason,
                    Some(
                        SpectatorStateChangeReason::Disconnected
                            | SpectatorStateChangeReason::Removed
                            | SpectatorStateChangeReason::RoomClosed
                    )
                ) && self.fence == Some(FenceKind::LeaveSpectator)
                {
                    self.absorbed_leave_armed = true;
                }
                let _ = (room_id, room_code);
                self.membership = None;
                self.room_finalized = false;
                self.roster.clear();
                self.local_id = None;
                self.room_id = None;
                self.room_code = None;
                self.quarantine_latched = false;
                self.plan_seen = false;
                self.session_generation = None;
                self.session_transport = None;
                self.plan_peers.clear();
                if self.fence == Some(FenceKind::LeaveSpectator) {
                    self.fence = None;
                }
            }
            SignalFishEvent::RoomJoinFailed { .. } => {
                if self.fence == Some(FenceKind::JoinPlayer) {
                    self.fence = None;
                }
            }
            SignalFishEvent::SpectatorJoinFailed { .. } => {
                if self.fence == Some(FenceKind::JoinSpectator) {
                    self.fence = None;
                }
            }
            SignalFishEvent::ReconnectionFailed { .. } => {
                self.pending_reconnects = self.pending_reconnects.saturating_sub(1);
                if self.fence == Some(FenceKind::ReconnectPlayer) {
                    self.fence = None;
                }
            }
            SignalFishEvent::RoomOperationFailed { .. } => {
                if self.fence == Some(FenceKind::ReconnectPlayer) {
                    self.pending_reconnects = self.pending_reconnects.saturating_sub(1);
                }
                self.fence = None;
            }
            SignalFishEvent::PlayerJoined { player } => {
                self.roster.insert(player.id);
            }
            SignalFishEvent::PlayerLeft { player_id, .. } => {
                self.roster.remove(player_id);
            }
            SignalFishEvent::LobbyStateChanged { lobby_state, .. } => {
                self.room_finalized = matches!(lobby_state, LobbyState::Finalized);
            }
            SignalFishEvent::GameStarting { .. } => {
                self.room_finalized = true;
            }
            SignalFishEvent::SessionPlan {
                transport,
                generation,
                peers,
                ..
            } => {
                self.plan_seen = true;
                self.session_transport = Some(*transport);
                self.session_generation = *generation;
                self.plan_peers = peers.iter().map(|p| p.player_id).collect();
            }
            SignalFishEvent::RelayStats {
                interval_ms,
                sent_to_you,
                dropped_for_you,
                backpressure_events,
            } => {
                self.relay_interval = Some(*interval_ms);
                self.relay_counters = (*sent_to_you, *dropped_for_you, *backpressure_events);
            }
            SignalFishEvent::DeliveryReport(payload) => {
                if payload
                    .gaps
                    .iter()
                    .any(|gap| gap.reason == DeliveryGapReason::UnsupportedFormat)
                {
                    self.unsupported_format_advisory_armed = true;
                }
            }
            SignalFishEvent::Error { .. } => {
                // The causal advisory is one-shot per armed range.
                self.unsupported_format_advisory_armed = false;
            }
            SignalFishEvent::Disconnected { .. } => {
                self.fence = None;
                self.absorbed_leave_armed = false;
                self.absorbed_leave_id = None;
            }
            _ => {}
        }
    }

    pub(crate) fn arm_fence(&mut self, kind: FenceKind) {
        self.fence = Some(kind);
        if kind == FenceKind::ReconnectPlayer {
            self.pending_reconnects = self.pending_reconnects.saturating_add(1);
        }
    }

    /// Feed one observed event through the phase model and reconcile it
    /// against the pending expectation slot.
    pub(crate) fn observe(&mut self, ev: &SignalFishEvent) -> Result<(), String> {
        if oracle_strict() && self.terminal {
            return Err(format!(
                "event `{}` after terminal disconnect",
                event_name(ev)
            ));
        }
        // Pending teardown applies only to events *after* the violating one.
        let was_pending = self.pending_disconnect;
        let phase = |ok: bool| -> Result<(), String> {
            if ok || !oracle_strict() {
                Ok(())
            } else {
                Err("phase".to_string())
            }
        };
        match ev {
            SignalFishEvent::Connected => {
                if oracle_strict() && self.connected_event_seen {
                    return Err("duplicate Connected event".into());
                }
                if oracle_strict() && self.any_event_seen {
                    return Err("Connected was not the first event".into());
                }
                self.connected_event_seen = true;
            }
            SignalFishEvent::Disconnected {
                reason,
                last_server_error: _,
            } => {
                if oracle_strict() && self.disconnected_seen {
                    return Err("duplicate Disconnected event".into());
                }
                if oracle_strict()
                    && self.policy != ProtocolViolationPolicy::Disconnect
                    && !self.pending_disconnect
                    && !self.peer_close_armed
                    && !self.transport_error_armed
                {
                    return Err(
                        "Disconnected without a violation (Disconnect policy), peer close, \
                         or terminal transport error"
                            .into(),
                    );
                }
                self.terminal_cause = Some(if was_pending {
                    self.violation_teardown = true;
                    TerminalCause::Violation
                } else if self.peer_close_armed {
                    TerminalCause::PeerClose
                } else if self.transport_error_armed {
                    TerminalCause::TransportError
                } else {
                    TerminalCause::Violation
                });
                self.disconnect_reason = reason.clone();
                self.disconnected_seen = true;
                self.terminal = true;
                self.pending_disconnect = false;
                self.quarantine_latched = false;
            }
            SignalFishEvent::DecodeFailed { .. } => {}
            SignalFishEvent::ProtocolViolation { diagnostic, .. } => {
                self.violations = self.violations.saturating_add(1);
                if self.policy == ProtocolViolationPolicy::Quarantine {
                    self.batch_quarantined = true;
                    self.quarantine_latched = true;
                }
                if self.policy == ProtocolViolationPolicy::Disconnect {
                    self.pending_disconnect = true;
                }
                if oracle_strict() && diagnostic.is_empty() {
                    return Err("empty violation diagnostic".into());
                }
            }
            SignalFishEvent::Authenticated { .. } => {
                phase(!self.authenticated && self.membership.is_none())?;
            }
            SignalFishEvent::AuthenticationError { .. } => {
                phase(!self.authenticated && self.membership.is_none())?;
            }
            SignalFishEvent::ProtocolInfo(_) => {
                phase(self.authenticated && self.membership.is_none() && !self.protocol_info_seen)?;
            }
            SignalFishEvent::RoomJoined { .. } | SignalFishEvent::Reconnected { .. } => {
                phase(self.authenticated && self.membership.is_none())?;
            }
            SignalFishEvent::SpectatorJoined { .. } => {
                phase(self.authenticated && self.membership.is_none())?;
            }
            SignalFishEvent::RoomLeft => {
                phase(self.authenticated && self.membership == Some(Role::Player))?;
            }
            SignalFishEvent::SpectatorLeft { .. } => {
                phase(self.authenticated && self.membership == Some(Role::Spectator))?;
            }
            SignalFishEvent::RoomJoinFailed { .. }
            | SignalFishEvent::SpectatorJoinFailed { .. }
            | SignalFishEvent::ReconnectionFailed { .. } => {
                phase(self.authenticated)?;
            }
            SignalFishEvent::RoomOperationFailed { .. } => {}
            SignalFishEvent::PlayerJoined { .. }
            | SignalFishEvent::PlayerLeft { .. }
            | SignalFishEvent::GameData { .. }
            | SignalFishEvent::GameDataBinary { .. }
            | SignalFishEvent::AuthorityChanged { .. }
            | SignalFishEvent::LobbyStateChanged { .. }
            | SignalFishEvent::GameStarting { .. }
            | SignalFishEvent::PlayerReconnected { .. }
            | SignalFishEvent::NewSpectatorJoined { .. }
            | SignalFishEvent::SpectatorDisconnected { .. } => {
                phase(self.authenticated && self.membership.is_some())?;
            }
            SignalFishEvent::DeliveryReport(_) => {
                phase(self.authenticated && self.membership.is_some() && self.v3)?;
            }
            SignalFishEvent::SignalReceived { .. } => {
                phase(
                    self.authenticated
                        && self.membership == Some(Role::Player)
                        && self.plan_seen
                        && self.v3,
                )?;
            }
            SignalFishEvent::NewPeer { .. } | SignalFishEvent::PeerTransportStatus { .. } => {
                phase(self.authenticated && self.membership == Some(Role::Player) && self.v3)?;
            }
            SignalFishEvent::SessionPlan { .. } => {
                phase(self.authenticated && self.membership == Some(Role::Player) && self.v3)?;
                self.plan_seen = true;
            }
            SignalFishEvent::RelayStats { .. } | SignalFishEvent::GoingAway { .. } => {
                phase(self.authenticated && self.v3)?;
            }
            SignalFishEvent::AuthorityResponse { .. } => {
                phase(self.authenticated)?;
            }
            SignalFishEvent::Pong | SignalFishEvent::Error { .. } => {}
            // The campaign drives the polling client, which is caller-driven
            // by design and can never configure a reconnect policy.
            SignalFishEvent::Reconnecting { .. } | SignalFishEvent::ReconnectAbandoned { .. } => {
                return Err(format!(
                    "impossible reconnect-policy event `{}`",
                    event_name(ev)
                ));
            }
        }
        // Game-data suppression under quarantine.
        if oracle_strict()
            && matches!(
                ev,
                SignalFishEvent::GameData { .. } | SignalFishEvent::GameDataBinary { .. }
            )
            && self.policy == ProtocolViolationPolicy::Quarantine
            && self.batch_quarantined
        {
            return Err("game data delivered while quarantined".into());
        }
        // Non-violation events must not follow a pending Disconnect-policy teardown.
        if oracle_strict() && was_pending && !matches!(ev, SignalFishEvent::Disconnected { .. }) {
            return Err("non-terminal event after Disconnect-policy violation".into());
        }
        self.track_event(ev);
        self.any_event_seen = true;
        Ok(())
    }

    /// Reconcile one poll batch's observed outcome against the pending
    /// expectation slot (issue #219 oracle). `Disconnected`/`Connected` are
    /// excluded; they belong to the phase model.
    pub(crate) fn reconcile_batch(
        &mut self,
        events: &[&'static str],
        violations_in_batch: usize,
        step_index: usize,
        findings: &mut Vec<Finding>,
    ) {
        if !oracle_strict() {
            return;
        }
        if events.is_empty() && violations_in_batch == 0 {
            // Silence satisfies only the slots whose documented model permits
            // it (stale suppression, absorbed replies, quarantined game data).
            if let Some(front) = self.slots.front() {
                if front.alternatives.contains(&AllowedOutcome::empty()) {
                    self.slots.pop_front();
                    self.absorbed_leave_armed = false;
                    self.absorbed_leave_id = None;
                }
            }
            return;
        }
        let Some(slot_data) = self.slots.front().map(|slot| {
            (
                slot.frame,
                slot.fence_retired_on_violation,
                slot.alternatives.clone(),
            )
        }) else {
            findings.push(Finding {
                category: "expectation-fabricated".into(),
                detail: format!(
                    "events surfaced with no delivered frame to attribute them to: {events:?} \
                     (violations={violations_in_batch})"
                ),
                step_index,
            });
            return;
        };
        let mut observed_events = BTreeMap::new();
        for name in events {
            observed_events
                .entry(*name)
                .and_modify(|count: &mut usize| *count = count.saturating_add(1))
                .or_insert(1usize);
        }
        let observed = AllowedOutcome {
            events: observed_events,
            violations: violations_in_batch,
        };
        let (frame_name, fence_retired_on_violation, alternatives) = slot_data;
        if !alternatives.contains(&observed) {
            // Drift: a later frame's outcome surfaced while an earlier frame
            // produced nothing. Name the swallowed frame explicitly instead
            // of misattributing the outcome to the front slot.
            for (offset, slot) in self.slots.iter().enumerate().skip(1) {
                if slot.alternatives.contains(&observed) {
                    findings.push(Finding {
                        category: "expectation-swallowed".into(),
                        detail: format!(
                            "frame {frame_name} produced no documented outcome; the observed \
                             outcome belongs to frame {} (position {offset})",
                            slot.frame
                        ),
                        step_index,
                    });
                    return;
                }
            }
            let expected: Vec<String> = alternatives.iter().map(|alt| format!("{alt:?}")).collect();
            findings.push(Finding {
                category: "expectation-mismatch".into(),
                detail: format!(
                    "frame {frame_name} produced {observed:?} but the documented model permits only: {}",
                    expected.join(" | ")
                ),
                step_index,
            });
            return;
        }
        self.slots.pop_front();
        // A violating baseline retires its answered fence (the round-12
        // retire_answered_room_operation rule).
        if observed.violations > 0 {
            if let (Some(retired), Some(current)) = (fence_retired_on_violation, self.fence) {
                if retired == current {
                    self.fence = None;
                }
            }
        }
        // Consuming the silent face spends the absorbed-reply allowance.
        if observed.events.is_empty() && observed.violations == 0 {
            self.absorbed_leave_armed = false;
            self.absorbed_leave_id = None;
        }
    }

    /// Frame names still awaiting their documented outcome at teardown. With
    /// one-frame-per-poll scripts every delivered frame is polled before the
    /// next step, so a slot pending at teardown is a swallow by construction;
    /// there is no legitimately "queued but unsurfaced" delivery to forgive.
    pub(crate) fn pending_frames(&self) -> Vec<&'static str> {
        self.slots.iter().map(|slot| slot.frame).collect()
    }

    pub(crate) fn pending_slot_count(&self) -> usize {
        self.slots.len()
    }

    pub(crate) fn end_batch(&mut self) -> Result<(), String> {
        if oracle_strict() && self.pending_disconnect {
            return Err("Disconnect-policy violation without a same-batch Disconnected".into());
        }
        Ok(())
    }

    /// Close-info attribution: the `Disconnected` reason must agree with the
    /// armed transport face. `transport_peer_closed` mirrors
    /// `Transport::close_info()` at the end of the run.
    #[allow(clippy::collapsible_match)]
    pub(crate) fn verify_close_attribution(
        &self,
        transport_peer_closed: bool,
    ) -> Result<(), String> {
        if !oracle_strict() || !self.disconnected_seen {
            return Ok(());
        }
        let reason = self.disconnect_reason.clone();
        let reason_says_server = reason
            .as_deref()
            .is_some_and(|reason| reason.starts_with("closed by server"));
        if reason_says_server && !transport_peer_closed {
            return Err(format!(
                "close-info misattribution: reason reports a server close but \
                 close_info() reports no peer close: {reason:?}"
            ));
        }
        match self.terminal_cause {
            Some(TerminalCause::Violation) => {
                if reason.as_deref() != Some("protocol violation") {
                    return Err(format!(
                        "close-info misattribution: violation teardown reported reason \
                         {reason:?} instead of Some(\"protocol violation\")"
                    ));
                }
            }
            Some(TerminalCause::PeerClose) => {
                if !transport_peer_closed {
                    return Err(
                        "close-info misattribution: Disconnected classified as peer close \
                         but close_info() reports no peer-initiated close"
                            .into(),
                    );
                }
                let expected = "closed by server: code=Some(1001), \
                                reason=Some(\"server going away\")";
                if reason.as_deref() != Some(expected) {
                    return Err(format!(
                        "close-info misattribution: peer close reported reason {reason:?} \
                         instead of the armed TransportCloseInfo formatting"
                    ));
                }
            }
            Some(TerminalCause::TransportError) => {
                if !self
                    .expected_transport_reasons
                    .iter()
                    .any(|expected| Some(expected.as_str()) == reason.as_deref())
                {
                    return Err(format!(
                        "close-info misattribution: terminal transport error reported \
                         reason {reason:?} instead of the armed error {:?}",
                        self.expected_transport_reasons
                    ));
                }
            }
            None => {}
        }
        Ok(())
    }

    /// Snapshot-coherence invariants (checked after every poll batch).
    pub(crate) fn check_snapshot(
        &self,
        snap: &signal_fish_client::ClientSnapshot,
    ) -> Result<(), String> {
        if !oracle_strict() {
            return Ok(());
        }
        if !snap.connected {
            if snap.transport_ready {
                return Err("snapshot: disconnected but transport_ready".into());
            }
            if snap.room_role.is_some() || snap.room_id.is_some() || snap.player_id.is_some() {
                return Err("snapshot: disconnected session fields not cleared".into());
            }
        }
        match snap.room_role {
            None => {
                if snap.player_id.is_some() || snap.room_id.is_some() || snap.room_code.is_some() {
                    return Err("snapshot: role None but membership ids present".into());
                }
                if snap.session_generation.is_some()
                    || snap.session_topology.is_some()
                    || snap.session_transport.is_some()
                {
                    return Err("snapshot: role None but session plan fields present".into());
                }
            }
            Some(_) => {
                if snap.player_id.is_none() || snap.room_id.is_none() {
                    return Err("snapshot: in room but player/room id missing".into());
                }
            }
        }
        match self.policy {
            ProtocolViolationPolicy::Quarantine => {
                // Positive quarantine-latch invariant: every violation latches
                // under the Quarantine policy until an authoritative
                // membership transition clears it. A Quarantine policy that
                // behaves like Observe (flag never set) must be caught here.
                if self.quarantine_latched && snap.connected && !snap.quarantined {
                    return Err(
                        "snapshot: violation observed under Quarantine policy but the \
                         quarantined flag was never latched"
                            .into(),
                    );
                }
            }
            ProtocolViolationPolicy::Observe | ProtocolViolationPolicy::Disconnect => {
                if snap.quarantined {
                    return Err(format!(
                        "snapshot: quarantined flag set under {:?} policy",
                        self.policy
                    ));
                }
            }
        }
        Ok(())
    }
}

fn echo_kind_matches_fence(kind: EchoKind, fence: FenceKind) -> bool {
    matches!(
        (kind, fence),
        (
            EchoKind::JoinOk | EchoKind::JoinFailed,
            FenceKind::JoinPlayer
        ) | (EchoKind::LeaveOk, FenceKind::LeavePlayer)
            | (
                EchoKind::ReconnectOk | EchoKind::ReconnectFailed,
                FenceKind::ReconnectPlayer
            )
            | (
                EchoKind::SpectatorJoinOk | EchoKind::SpectatorJoinFailed,
                FenceKind::JoinSpectator
            )
            | (EchoKind::SpectatorLeaveOk, FenceKind::LeaveSpectator)
            | (EchoKind::OperationFailed, _)
    )
}

/// Baseline roster lifecycle shape (mirror of
/// `validate_local_player_snapshot`, which runs in the lifecycle validator):
/// the payload's player must appear exactly once with a consistent authority
/// flag and at most one authority overall. Stamp validity is a separate,
/// accountability-layer check.
fn baseline_roster_shape_valid(
    local: PlayerId,
    local_is_authority: bool,
    players: &[signal_fish_client::protocol::PlayerInfo],
) -> bool {
    if players.iter().filter(|p| p.id == local).count() != 1 {
        return false;
    }
    let Some(entry) = players.iter().find(|p| p.id == local) else {
        return false;
    };
    if entry.is_authority != local_is_authority {
        return false;
    }
    players.iter().filter(|p| p.is_authority).count() <= 1
}

/// Accountability-layer roster stamp validity for the negotiated dialect.
fn baseline_roster_stamps_valid(
    v3: bool,
    players: &[signal_fish_client::protocol::PlayerInfo],
) -> bool {
    if v3 {
        players.iter().all(|p| !v3_stamp_invalid(p.epoch, p.seq))
    } else {
        players.iter().all(|p| p.epoch.is_none() && p.seq.is_none())
    }
}

fn v3_stamp_invalid(epoch: Option<u32>, seq: Option<u64>) -> bool {
    match (epoch, seq) {
        (Some(e), Some(_)) => e == 0,
        _ => true,
    }
}

fn make_config(
    kind: ConfigKind,
    policy: ProtocolViolationPolicy,
    small_command_capacity: Option<usize>,
) -> SignalFishConfig {
    let base = SignalFishConfig::new("mb_app_round41_hostile")
        .with_protocol_violation_policy(policy)
        .with_event_channel_capacity(1024)
        .with_command_channel_capacity(small_command_capacity.unwrap_or(1024));
    match kind {
        ConfigKind::V3 => base.enable_v3(),
        ConfigKind::V3VersionOnly => base.with_protocol_version(3),
        ConfigKind::V2 => base,
        ConfigKind::V2Explicit => base.with_protocol_version(2),
    }
}

/// Look up the most recent outbound RoomOperation id recorded by the transport.
fn last_sent_operation_id(handles: &crate::transport::ScriptedHandles) -> Option<PlayerId> {
    let log = lock(&handles.outbound);
    for line in log.iter().rev() {
        if let Ok(ClientMessage::RoomOperation { operation_id, .. }) =
            serde_json::from_str::<ClientMessage>(line)
        {
            return Some(operation_id);
        }
    }
    None
}

/// Count game-data frames the transport accepted (issue #219 ledger): text
/// lines that deserialize to `ClientMessage::GameData` plus binary frames.
fn outbound_game_data_frames(handles: &crate::transport::ScriptedHandles) -> usize {
    let log = lock(&handles.outbound);
    let mut count = 0usize;
    for line in log.iter() {
        let is_game_data = line.starts_with("<binary ")
            || matches!(
                serde_json::from_str::<ClientMessage>(line),
                Ok(ClientMessage::GameData { .. })
            );
        if is_game_data {
            count = count.saturating_add(1);
        }
    }
    count
}

fn execute_cmd(
    client: &mut SignalFishPollingClient<ScriptedTransport>,
    cmd: &Cmd,
) -> Result<(), signal_fish_client::SignalFishError> {
    match cmd {
        Cmd::JoinRoom => client.join_room(JoinRoomParams::new("hostile-game", "hostile-player")),
        Cmd::JoinRoomMax(max) => client.join_room(
            JoinRoomParams::new("hostile-game", "hostile-player").with_max_players(*max),
        ),
        Cmd::LeaveRoom => client.leave_room(),
        Cmd::SendGameData(v) => client.send_game_data(v.clone()),
        Cmd::SendGameDataLatest(k) => client.send_game_data_with_delivery(
            serde_json::json!({ "tick": k }),
            GameDataDelivery::Latest { key: *k },
        ),
        Cmd::SendGameDataVolatile => client.send_game_data_with_delivery(
            serde_json::json!({ "volatile": true }),
            GameDataDelivery::Volatile,
        ),
        Cmd::SendBinaryGameData(n) => client.send_binary_game_data(vec![0xAB_u8; *n]),
        Cmd::SetReady => client.set_ready(),
        Cmd::StartGame => client.start_game(),
        Cmd::RequestAuthority(b) => client.request_authority(*b),
        Cmd::ProvideConnectionInfo => client.provide_connection_info(ConnectionInfo::Direct {
            host: "203.0.113.1".to_string(),
            port: 40000,
        }),
        Cmd::Reconnect(player, room) => {
            client.reconnect(*player, *room, "hostile-auth-token".to_string())
        }
        Cmd::JoinAsSpectator => {
            client.join_as_spectator("hostile-game".into(), "R1".into(), "spec".into())
        }
        Cmd::LeaveSpectator => client.leave_spectator(),
        Cmd::Ping => client.ping(),
        Cmd::SendSignal => client.send_signal(
            PlayerId::from_u128(42),
            PeerSignal::Offer("v=0 hostile".to_string()),
        ),
        Cmd::SendRawSignal => client.send_raw_signal(
            PlayerId::from_u128(42),
            serde_json::json!({ "IceCandidate": "x" }),
        ),
        Cmd::ReportTransportStatus => client.report_transport_status(TransportKind::Relay, true),
    }
}

/// Build a `RoomOperationResult` echo. Returns the message plus whether the
/// embedded id matches the id of the client's most recently sent operation.
fn build_echo_message(
    handles: &crate::transport::ScriptedHandles,
    kind: EchoKind,
    id_choice: EchoId,
) -> (ServerMessage, bool) {
    let pending_id = last_sent_operation_id(handles);
    let (operation_id, id_matches) = match id_choice {
        EchoId::Match => match pending_id {
            Some(id) => (id, true),
            None => (PlayerId::from_u128(0xE_C00), false),
        },
        EchoId::Wrong => (PlayerId::from_u128(0xBA_DD1D), false),
    };
    let result = match kind {
        EchoKind::JoinOk => {
            RoomOperationResult::RoomJoined(Box::new(RoomJoinedPayload {
                room_id: PlayerId::from_u128(0x1000),
                room_code: "ECHO".into(),
                player_id: PlayerId::from_u128(0x2000),
                game_name: "hostile-game".into(),
                max_players: 8,
                supports_authority: true,
                // The local player must appear exactly once with a consistent
                // authority flag for the baseline to be lifecycle-valid.
                current_players: vec![signal_fish_client::protocol::PlayerInfo {
                    id: PlayerId::from_u128(0x2000),
                    name: "echo-self".into(),
                    is_authority: false,
                    is_ready: false,
                    connected_at: "2026-09-02T00:00:00Z".into(),
                    connection_info: None,
                    epoch: Some(1),
                    seq: Some(0),
                }],
                is_authority: false,
                lobby_state: LobbyState::Waiting,
                ready_players: vec![],
                relay_type: "websocket".into(),
                current_spectators: vec![],
                ice_servers: vec![],
                reconnection_token: None,
            }))
        }
        EchoKind::JoinFailed => RoomOperationResult::RoomJoinFailed {
            reason: "echo-fail".into(),
            error_code: Some(signal_fish_client::ErrorCode::RoomFull),
        },
        EchoKind::LeaveOk => RoomOperationResult::RoomLeft,
        EchoKind::ReconnectOk => {
            RoomOperationResult::Reconnected(Box::new(ReconnectedPayload {
                room_id: PlayerId::from_u128(0x1000),
                room_code: "ECHO".into(),
                player_id: PlayerId::from_u128(0x2000),
                game_name: "hostile-game".into(),
                max_players: 8,
                supports_authority: false,
                current_players: vec![],
                is_authority: false,
                lobby_state: LobbyState::Finalized,
                ready_players: vec![],
                relay_type: "websocket".into(),
                current_spectators: vec![],
                ice_servers: vec![],
                missed_events: vec![],
                replay: Some(ReplayStatus::Complete),
                sender_watermarks: vec![],
                // v3 requires a rotated, nonempty token.
                reconnection_token: Some("rotated-echo-token".into()),
            }))
        }
        EchoKind::ReconnectFailed => RoomOperationResult::ReconnectionFailed {
            reason: "echo-fail".into(),
            error_code: signal_fish_client::ErrorCode::ReconnectionTokenInvalid,
        },
        EchoKind::SpectatorJoinOk => {
            RoomOperationResult::SpectatorJoined(Box::new(SpectatorJoinedPayload {
                room_id: PlayerId::from_u128(0x1000),
                room_code: "ECHO".into(),
                spectator_id: PlayerId::from_u128(0x3000),
                game_name: "hostile-game".into(),
                current_players: vec![],
                current_spectators: vec![signal_fish_client::protocol::SpectatorInfo {
                    id: PlayerId::from_u128(0x3000),
                    name: "echo-spec".into(),
                    connected_at: "2026-09-02T00:00:00Z".into(),
                }],
                lobby_state: LobbyState::Waiting,
                reason: None,
            }))
        }
        EchoKind::SpectatorJoinFailed => RoomOperationResult::SpectatorJoinFailed {
            reason: "echo-fail".into(),
            error_code: Some(signal_fish_client::ErrorCode::SpectatorNotAllowed),
        },
        EchoKind::SpectatorLeaveOk => RoomOperationResult::SpectatorLeft {
            room_id: Some(PlayerId::from_u128(0x1000)),
            room_code: Some("ECHO".into()),
            reason: Some(SpectatorStateChangeReason::VoluntaryLeave),
            current_spectators: vec![],
        },
        EchoKind::OperationFailed => RoomOperationResult::OperationFailed {
            reason: "echo-op-failed".into(),
            error_code: Some(signal_fish_client::ErrorCode::InvalidInput),
        },
    };
    (
        ServerMessage::RoomOperationResult {
            operation_id,
            result: Box::new(result),
        },
        id_matches,
    )
}

struct RunState {
    outcome: Outcome,
    oracle: Oracle,
    /// Accepted-send marker order for the send-pressure ledger.
    sent_markers: Vec<u64>,
    /// Count of accepted binary game-data sends.
    sent_binaries: usize,
    /// Predicted `game_data_received` (issue #219 ledger oracle).
    predicted_game_data_received: usize,
    /// Count of raw frames fed (each must surface exactly one DecodeFailed).
    raw_frames_fed: usize,
    /// DecodeFailed events observed (checked against `raw_frames_fed`).
    decode_failed_events: usize,
    /// `poll()` calls issued (for the `begin_poll_cycle` contract pin).
    polls_made: usize,
}

pub(crate) fn run_prefix(
    script: &Script,
    policy: ProtocolViolationPolicy,
    limit: usize,
) -> Outcome {
    let (transport, handles) = ScriptedTransport::new();
    let config = make_config(script.config_kind, policy, script.small_command_capacity);
    let mut client = SignalFishPollingClient::new(transport, config);
    let mut state = RunState {
        outcome: Outcome::new(),
        oracle: Oracle::new(policy, script.config_kind, script.echo_room_ops),
        sent_markers: Vec::new(),
        sent_binaries: 0,
        predicted_game_data_received: 0,
        raw_frames_fed: 0,
        decode_failed_events: 0,
        polls_made: 0,
    };

    for (idx, step) in script.steps.iter().take(limit).enumerate() {
        heartbeat();
        match step {
            Step::Deliver(msg, meta) => {
                note_delivered(crate::script::msg_variant_name(msg));
                if !state.oracle.terminal
                    && matches!(
                        msg,
                        ServerMessage::GameData { .. } | ServerMessage::GameDataBinary { .. }
                    )
                {
                    // Decoded game data counts before suppression (JSON text
                    // frames always decode here). Frames fed after a terminal
                    // teardown are never consumed.
                    state.predicted_game_data_received =
                        state.predicted_game_data_received.saturating_add(1);
                }
                let json = match serde_json::to_string(msg) {
                    Ok(json) => json,
                    Err(_) => "{}".to_string(),
                };
                ScriptedTransport::push_text(&handles, json);
                state.outcome.frames_fed = state.outcome.frames_fed.saturating_add(1);
                if state.oracle.terminal {
                    continue;
                }
                let expected = state.oracle.expectation_for(msg, meta);
                let fence_retired_on_violation = state.oracle.retire_on_violation(msg);
                state.oracle.slots.push_back(Slot {
                    alternatives: expected,
                    frame: crate::script::msg_variant_name(msg),
                    fence_retired_on_violation,
                });
                drive_polls(&mut client, &mut state, 1, idx);
                // Capture the absorbed-late-reply operation id (the pending
                // voluntary leave was the client's most recent operation).
                if state.oracle.absorbed_leave_armed && state.oracle.absorbed_leave_id.is_none() {
                    state.oracle.absorbed_leave_id = last_sent_operation_id(&handles);
                }
            }
            Step::DeliverRaw(raw) => {
                note_delivered("<raw schema-invalid frame>");
                ScriptedTransport::push_text(&handles, (*raw).to_string());
                state.outcome.frames_fed = state.outcome.frames_fed.saturating_add(1);
                if !state.oracle.terminal {
                    state.raw_frames_fed = state.raw_frames_fed.saturating_add(1);
                    state.oracle.slots.push_back(Slot {
                        alternatives: exactly("DecodeFailed"),
                        frame: "RawFrame",
                        fence_retired_on_violation: None,
                    });
                }
                drive_polls(&mut client, &mut state, 1, idx);
            }
            Step::DeliverBinary(bytes, meta) => {
                note_delivered("<physical binary frame>");
                ScriptedTransport::push_binary(&handles, bytes.clone());
                state.outcome.frames_fed = state.outcome.frames_fed.saturating_add(1);
                if state.oracle.terminal {
                    continue;
                }
                // Binary frames count only when the representation gate lets
                // them reach decode: the campaign's negotiated format is
                // always Json, so only the Observe policy's diagnostic
                // continue (on a v3 connection) reaches the decode counter.
                if !state.oracle.terminal
                    && state.oracle.in_room()
                    && state.oracle.protocol_info_seen
                    && state.oracle.v3
                    && policy == ProtocolViolationPolicy::Observe
                {
                    state.predicted_game_data_received =
                        state.predicted_game_data_received.saturating_add(1);
                }
                let expected = state.oracle.expectation_for_binary(meta);
                state.oracle.slots.push_back(Slot {
                    alternatives: expected,
                    frame: "BinaryFrame",
                    fence_retired_on_violation: None,
                });
                drive_polls(&mut client, &mut state, 1, idx);
            }
            Step::DeliverEcho(kind, id_choice) => {
                note_delivered("RoomOperationResult");
                note_delivered(&format!("RoomOperationResult::{}", kind.name()));
                let (msg, id_matches) = build_echo_message(&handles, *kind, *id_choice);
                let json = match serde_json::to_string(&msg) {
                    Ok(json) => json,
                    Err(_) => "{}".to_string(),
                };
                ScriptedTransport::push_text(&handles, json);
                state.outcome.frames_fed = state.outcome.frames_fed.saturating_add(1);
                if state.oracle.terminal {
                    // Frames fed to a dead transport are never consumed; they
                    // cannot carry a documented outcome.
                    continue;
                }
                let kind_matched_fence = state
                    .oracle
                    .fence
                    .is_some_and(|fence| echo_kind_matches_fence(*kind, fence));
                let expected = state.oracle.expectation_for_echo(*kind, id_matches);
                // A violating echo retires the answered fence only when the
                // expectation was accountability-ambiguous (the round-12
                // retire rule); definite lifecycle rejections keep it armed.
                // Event outcomes release the fence through event tracking.
                let accountability_ambiguous = expected
                    .iter()
                    .any(|alt| alt.violations > 0 && !alt.events.is_empty());
                let fence_retired_on_violation = if accountability_ambiguous && kind_matched_fence {
                    match kind {
                        EchoKind::JoinOk | EchoKind::JoinFailed => Some(FenceKind::JoinPlayer),
                        EchoKind::LeaveOk => Some(FenceKind::LeavePlayer),
                        EchoKind::ReconnectOk | EchoKind::ReconnectFailed => {
                            Some(FenceKind::ReconnectPlayer)
                        }
                        EchoKind::SpectatorJoinOk | EchoKind::SpectatorJoinFailed => {
                            Some(FenceKind::JoinSpectator)
                        }
                        EchoKind::SpectatorLeaveOk => Some(FenceKind::LeaveSpectator),
                        EchoKind::OperationFailed => state.oracle.fence,
                    }
                } else {
                    None
                };
                state.oracle.slots.push_back(Slot {
                    alternatives: expected,
                    frame: "RoomOperationResult",
                    fence_retired_on_violation,
                });
                drive_polls(&mut client, &mut state, 1, idx);
                if state.oracle.absorbed_leave_armed && state.oracle.absorbed_leave_id.is_none() {
                    state.oracle.absorbed_leave_id = last_sent_operation_id(&handles);
                }
            }
            Step::Cmd(cmd) => {
                heartbeat();
                let accepted = execute_cmd(&mut client, cmd).is_ok();
                if VERBOSE.load(Ordering::Relaxed) {
                    println!("       cmd: {} accepted={accepted}", cmd.name());
                }
                if accepted {
                    state.outcome.commands_accepted =
                        state.outcome.commands_accepted.saturating_add(1);
                    if let Some(fence) = cmd.fence() {
                        state.oracle.arm_fence(fence);
                    }
                    if let Cmd::SendGameData(payload) = cmd {
                        if let Some(marker) = payload.get("marker").and_then(|m| m.as_u64()) {
                            state.sent_markers.push(marker);
                        }
                    }
                    if matches!(cmd, Cmd::SendBinaryGameData(_)) {
                        state.sent_binaries = state.sent_binaries.saturating_add(1);
                    }
                    if state.oracle.terminal {
                        state.outcome.findings.push(Finding {
                            category: "command-after-terminal".into(),
                            detail: format!(
                                "command {} was accepted after terminal disconnect",
                                cmd.name()
                            ),
                            step_index: idx,
                        });
                    }
                } else {
                    state.outcome.commands_refused =
                        state.outcome.commands_refused.saturating_add(1);
                }
                drive_polls(&mut client, &mut state, 1, idx);
            }
            Step::Poll(n) => {
                drive_polls(&mut client, &mut state, *n, idx);
            }
            Step::SetSendDelay(delay) => {
                ScriptedTransport::set_send_delay(&handles, *delay);
            }
            Step::Close => {
                heartbeat();
                // Ledger + pressure assertions run at the close boundary while
                // the run state is still intact. The ledger covers every
                // archetype: game-data-delivering scripts end in Close, so
                // this is the one point their stats are still comparable.
                if state.outcome.findings.is_empty() {
                    ledger_checks(&client, &mut state, &handles, idx);
                    if script.archetype == "send_pressure" {
                        pressure_checks(&client, &mut state, &handles, idx);
                    }
                    // Transport-contract pin: `begin_poll_cycle` fires exactly
                    // once per `poll()` call (the documented scheduling-cycle
                    // contract for the polling driver).
                    let cycles = handles.begin_cycles.load(Ordering::Relaxed);
                    if cycles != state.polls_made {
                        state.outcome.findings.push(Finding {
                            category: "contract-begin-poll-cycle".into(),
                            detail: format!(
                                "begin_poll_cycle fired {cycles} times for {} poll() calls",
                                state.polls_made
                            ),
                            step_index: idx,
                        });
                    }
                }
                client.close();
                // Idempotence: a second close must be silent and cheap.
                client.close();
                check_silent_after_close(&mut client, &mut state, idx);
                state.oracle.terminal = true;
            }
            Step::PeerClose => {
                heartbeat();
                ScriptedTransport::arm_peer_close(&handles);
                state.oracle.peer_close_armed = true;
                drive_polls(&mut client, &mut state, 2, idx);
            }
            Step::TransportKill {
                fail_recv,
                fail_send,
            } => {
                heartbeat();
                if *fail_recv {
                    ScriptedTransport::arm_recv_error(&handles);
                }
                if *fail_send {
                    ScriptedTransport::arm_send_error(&handles);
                }
                state.oracle.arm_transport_error(*fail_recv, *fail_send);
                drive_polls(&mut client, &mut state, 3, idx);
            }
        }
        if !state.outcome.findings.is_empty() {
            break;
        }
    }

    // Terminal drain: whatever the script end state, close and verify stability.
    let tail_index = limit;
    if state.outcome.findings.is_empty()
        && !matches!(script.steps.get(limit.saturating_sub(1)), Some(Step::Close))
    {
        heartbeat();
        ledger_checks(&client, &mut state, &handles, tail_index);
        if state.outcome.findings.is_empty() {
            client.close();
            // Idempotence: a second close must be silent and cheap.
            client.close();
            check_silent_after_close(&mut client, &mut state, tail_index);
        }
    }

    // Terminal-state discipline.
    if state.outcome.findings.is_empty() {
        if state.oracle.disconnected_seen && client.snapshot().connected {
            state.outcome.findings.push(Finding {
                category: "terminal-snapshot".into(),
                detail: "snapshot.connected still true after terminal Disconnected".into(),
                step_index: limit,
            });
        }
        if state.oracle.transport_error_armed && !state.oracle.disconnected_seen {
            state.outcome.findings.push(Finding {
                category: "terminal-snapshot".into(),
                detail: "terminal transport error did not produce a Disconnected event".into(),
                step_index: limit,
            });
        }
        if let Err(detail) = state
            .oracle
            .verify_close_attribution(handles.peer_closed.load(Ordering::Relaxed))
        {
            state.outcome.findings.push(Finding {
                category: "close-attribution".into(),
                detail,
                step_index: limit,
            });
        }
        // Unresolved expectations: a non-suppressible frame produced no
        // event. This check runs at the end of EVERY run, including runs that
        // ended in a terminal teardown (a teardown cannot strand a delivered
        // frame's outcome in a one-frame-per-poll script).
        if state.oracle.pending_slot_count() > 0 {
            let pending = state.oracle.pending_frames();
            state.outcome.findings.push(Finding {
                category: "expectation-swallowed".into(),
                detail: format!(
                    "{} delivered frame(s) never produced their documented outcome: {pending:?}",
                    pending.len()
                ),
                step_index: limit,
            });
        }
    }

    state.outcome.violations = state.oracle.violations;
    state.outcome.terminal = state.oracle.disconnected_seen;
    state.outcome.violation_teardown = state.oracle.violation_teardown;
    state.outcome
}

/// Stats/ledger equivalence (issue #219): the client's cumulative counters
/// must equal the harness's independent count of what each surface documents.
fn ledger_checks(
    client: &SignalFishPollingClient<ScriptedTransport>,
    state: &mut RunState,
    handles: &crate::transport::ScriptedHandles,
    step_index: usize,
) {
    if !oracle_strict() || !state.outcome.findings.is_empty() {
        return;
    }
    let stats = client.stats();
    if stats.game_data_received
        != u64::try_from(state.predicted_game_data_received).unwrap_or(u64::MAX)
    {
        state.outcome.findings.push(Finding {
            category: "ledger-game-data-received".into(),
            detail: format!(
                "stats.game_data_received={} but the harness independently counted {} decoded \
                 game-data frames (over-suppression or phantom counting)",
                stats.game_data_received, state.predicted_game_data_received
            ),
            step_index,
        });
    }
    if stats.messages_undecodable != u64::try_from(state.raw_frames_fed).unwrap_or(u64::MAX)
        || u64::try_from(state.decode_failed_events).unwrap_or(u64::MAX)
            != stats.messages_undecodable
    {
        state.outcome.findings.push(Finding {
            category: "ledger-undecodable".into(),
            detail: format!(
                "raw frames fed={} but stats.messages_undecodable={} and DecodeFailed events={}",
                state.raw_frames_fed, stats.messages_undecodable, state.decode_failed_events
            ),
            step_index,
        });
    }
    let accepted = outbound_game_data_frames(handles);
    if stats.game_data_sent != u64::try_from(accepted).unwrap_or(u64::MAX) {
        state.outcome.findings.push(Finding {
            category: "ledger-game-data-sent".into(),
            detail: format!(
                "stats.game_data_sent={} but the transport accepted {} game-data frames",
                stats.game_data_sent, accepted
            ),
            step_index,
        });
    }
}

/// Send-pressure archetype assertions (issue #219): FIFO delivery under
/// `Pending`-refusing sends, capacity accounting, and full drain.
fn pressure_checks(
    client: &SignalFishPollingClient<ScriptedTransport>,
    state: &mut RunState,
    handles: &crate::transport::ScriptedHandles,
    step_index: usize,
) {
    if !oracle_strict() || !state.outcome.findings.is_empty() {
        return;
    }
    let log = lock(&handles.outbound);
    let mut delivered_markers: Vec<u64> = Vec::new();
    let mut binary_frames = 0usize;
    for line in log.iter() {
        if line.starts_with("<binary ") {
            binary_frames = binary_frames.saturating_add(1);
            continue;
        }
        if let Ok(ClientMessage::GameData { data, .. }) =
            serde_json::from_str::<ClientMessage>(line)
        {
            if let Some(marker) = data.get("marker").and_then(|marker| marker.as_u64()) {
                delivered_markers.push(marker);
            }
        }
    }
    drop(log);
    if delivered_markers != state.sent_markers {
        state.outcome.findings.push(Finding {
            category: "pressure-fifo".into(),
            detail: format!(
                "outbound marker order {delivered_markers:?} differs from accepted-send order {:?}",
                state.sent_markers
            ),
            step_index,
        });
    }
    if binary_frames != state.sent_binaries {
        state.outcome.findings.push(Finding {
            category: "pressure-binaries".into(),
            detail: format!(
                "accepted {} binary sends but the transport received {} binary frames",
                state.sent_binaries, binary_frames
            ),
            step_index,
        });
    }
    let poll_stats = client.polling_stats();
    if poll_stats.current_queue_depth != 0 {
        state.outcome.findings.push(Finding {
            category: "pressure-drain".into(),
            detail: format!(
                "queue depth {} after the full drain (accepted sends must all flush)",
                poll_stats.current_queue_depth
            ),
            step_index,
        });
    }
    if handles.send_pending_refusals.load(Ordering::Relaxed) == 0 {
        state.outcome.findings.push(Finding {
            category: "pressure-liveness".into(),
            detail: "the Pending-refusal send face never fired; pacing unexercised".into(),
            step_index,
        });
    }
    // The 96-send storm against a 64-slot queue with a six-poll drain must
    // overflow: SendBufferFull refusals are the archetype's own target.
    if state.outcome.commands_refused < 16 {
        state.outcome.findings.push(Finding {
            category: "pressure-capacity".into(),
            detail: format!(
                "only {} commands were refused; the queue never overflowed its capacity",
                state.outcome.commands_refused
            ),
            step_index,
        });
    }
}

fn check_silent_after_close(
    client: &mut SignalFishPollingClient<ScriptedTransport>,
    state: &mut RunState,
    step_index: usize,
) {
    let events = client.poll();
    if !events.is_empty() {
        state.outcome.findings.push(Finding {
            category: "events-after-close".into(),
            detail: format!("close() produced {} events", events.len()),
            step_index,
        });
        return;
    }
    let events2 = client.poll();
    if !events2.is_empty() {
        state.outcome.findings.push(Finding {
            category: "events-after-close".into(),
            detail: "second poll after close produced events".into(),
            step_index,
        });
        return;
    }
    let snap = client.snapshot();
    if snap.connected || snap.room_role.is_some() {
        state.outcome.findings.push(Finding {
            category: "snapshot-after-close".into(),
            detail: format!(
                "snapshot after close: connected={} role={:?}",
                snap.connected, snap.room_role
            ),
            step_index,
        });
    }
}

/// Poll up to `n` times, feeding every event through the oracle, reconciling
/// the per-frame expectation slots, and checking snapshot coherence.
fn drive_polls(
    client: &mut SignalFishPollingClient<ScriptedTransport>,
    state: &mut RunState,
    n: usize,
    step_index: usize,
) {
    for _ in 0..n {
        heartbeat();
        let quarantined = client.snapshot().quarantined;
        state.oracle.begin_batch(quarantined);
        if !state.oracle.terminal {
            // Post-terminal polls short-circuit before the driver begins a
            // scheduling cycle, so only live polls carry the contract.
            state.polls_made = state.polls_made.saturating_add(1);
        }
        let events = client.poll();
        state.outcome.events_seen = state.outcome.events_seen.saturating_add(events.len());
        let mut names: Vec<&'static str> = Vec::new();
        let mut violations_in_batch = 0usize;
        let mut terminal_in_batch = false;
        for ev in &events {
            if VERBOSE.load(Ordering::Relaxed) {
                if let SignalFishEvent::ProtocolViolation { kind, diagnostic } = ev {
                    println!("       event: ProtocolViolation {kind:?}: {diagnostic}");
                } else {
                    println!("       event: {}", event_name(ev));
                }
            }
            note_coverage(event_name(ev));
            match ev {
                SignalFishEvent::Connected => {}
                SignalFishEvent::Disconnected { .. } => {
                    terminal_in_batch = true;
                }
                SignalFishEvent::ProtocolViolation { .. } => {
                    violations_in_batch = violations_in_batch.saturating_add(1);
                }
                SignalFishEvent::DecodeFailed { .. } => {
                    state.decode_failed_events = state.decode_failed_events.saturating_add(1);
                    names.push(event_name(ev));
                }
                other => names.push(event_name(other)),
            }
            if let Err(detail) = state.oracle.observe(ev) {
                state.outcome.findings.push(Finding {
                    category: "oracle-event".into(),
                    detail: format!("event `{}`: {detail}", event_name(ev)),
                    step_index,
                });
                return;
            }
        }
        state.oracle.reconcile_batch(
            &names,
            violations_in_batch,
            step_index,
            &mut state.outcome.findings,
        );
        if !state.outcome.findings.is_empty() {
            return;
        }
        if terminal_in_batch && !state.outcome.findings.is_empty() {
            return;
        }
        if let Err(detail) = state.oracle.end_batch() {
            state.outcome.findings.push(Finding {
                category: "oracle-batch".into(),
                detail,
                step_index,
            });
            return;
        }
        if VERBOSE.load(Ordering::Relaxed) {
            let snap = client.snapshot();
            println!(
                "       snapshot: connected={} ready={} auth={} v3={:?} role={:?} room={:?} quarantined={}",
                snap.connected,
                snap.transport_ready,
                snap.authenticated,
                snap.negotiated_protocol_version,
                snap.room_role,
                snap.room_id,
                snap.quarantined
            );
        }
        heartbeat();
        if let Err(detail) = state.oracle.check_snapshot(&client.snapshot()) {
            state.outcome.findings.push(Finding {
                category: "oracle-snapshot".into(),
                detail,
                step_index,
            });
            return;
        }
        if state.oracle.terminal {
            heartbeat();
            if !client.poll().is_empty() {
                state.outcome.findings.push(Finding {
                    category: "events-after-terminal".into(),
                    detail: "poll after terminal disconnect produced events".into(),
                    step_index,
                });
                return;
            }
        }
    }
}

/// Verbose replay used by `--repro`: prints every step and event.
pub fn run_prefix_verbose(
    script: &Script,
    policy: ProtocolViolationPolicy,
    limit: usize,
) -> Outcome {
    VERBOSE.store(true, Ordering::Relaxed);
    let outcome = run_prefix(script, policy, limit.min(script.steps.len()));
    VERBOSE.store(false, Ordering::Relaxed);
    outcome
}

/// Run one script under one policy; on failure, reduce to a minimal prefix.
pub fn run_script(
    script: &Script,
    policy: ProtocolViolationPolicy,
) -> Result<Outcome, ReducedFailure> {
    *lock(&CURRENT_LABEL) = format!(
        "seed={} script={} archetype={} config={} policy={policy:?}",
        script.seed,
        script.index,
        script.archetype,
        script.config_kind.name(),
    );
    let total = script.steps.len();
    let outcome = match catch_unwind(AssertUnwindSafe(|| run_prefix(script, policy, total))) {
        Ok(outcome) => outcome,
        Err(panic) => {
            return Err(ReducedFailure {
                findings: vec![Finding {
                    category: "PANIC".into(),
                    detail: panic_message(&panic),
                    step_index: usize::MAX,
                }],
                prefix_len: total,
            });
        }
    };
    // Cross-policy differential: run the sibling policies once and compare the
    // documented differences (lazily cached per (seed,index) by the caller).
    if outcome.findings.is_empty()
        && !lock(&DIFFERENTIAL_DONE).contains(&(script.seed, script.index))
    {
        lock(&DIFFERENTIAL_DONE).insert((script.seed, script.index));
        let mut by_policy: [Option<Outcome>; 3] = [None, None, None];
        for p in [
            ProtocolViolationPolicy::Quarantine,
            ProtocolViolationPolicy::Observe,
            ProtocolViolationPolicy::Disconnect,
        ] {
            let index = match p {
                ProtocolViolationPolicy::Quarantine => 0,
                ProtocolViolationPolicy::Observe => 1,
                ProtocolViolationPolicy::Disconnect => 2,
            };
            let r = catch_unwind(AssertUnwindSafe(|| run_prefix(script, p, total)));
            let o = match r {
                Ok(o) => o,
                Err(panic) => {
                    return Err(ReducedFailure {
                        findings: vec![Finding {
                            category: "PANIC".into(),
                            detail: format!("policy {p:?}: {}", panic_message(&panic)),
                            step_index: usize::MAX,
                        }],
                        prefix_len: total,
                    });
                }
            };
            match by_policy.get_mut(index) {
                Some(slot) => *slot = Some(o),
                None => {
                    return Err(unreachable_policy_missing(p, total));
                }
            }
        }
        let Some(q) = by_policy[0].as_ref() else {
            return Err(unreachable_policy_missing(
                ProtocolViolationPolicy::Quarantine,
                total,
            ));
        };
        let Some(o) = by_policy[1].as_ref() else {
            return Err(unreachable_policy_missing(
                ProtocolViolationPolicy::Observe,
                total,
            ));
        };
        let Some(d) = by_policy[2].as_ref() else {
            return Err(unreachable_policy_missing(
                ProtocolViolationPolicy::Disconnect,
                total,
            ));
        };
        let mut diffs: Vec<String> = Vec::new();
        if (q.violations > 0) != (o.violations > 0) {
            diffs.push(format!(
                "violation presence differs: Quarantine={} Observe={}",
                q.violations, o.violations
            ));
        }
        if q.violations > 0 {
            if !d.terminal {
                diffs.push(format!(
                    "Disconnect policy did not tear down after violations \
                     (q_violations={} d_terminal={})",
                    q.violations, d.terminal
                ));
            }
            if d.violations == 0 {
                diffs.push("Disconnect run saw zero violations while Quarantine saw some".into());
            }
            if q.violation_teardown {
                diffs.push("Quarantine policy tore down the connection after violations".into());
            }
            if o.violation_teardown {
                diffs.push("Observe policy tore down the connection after violations".into());
            }
        } else {
            for (name, run) in [("Quarantine", q), ("Observe", o), ("Disconnect", d)] {
                if run.violation_teardown {
                    diffs.push(format!("{name} run tore down with zero violations"));
                }
            }
        }
        if !diffs.is_empty() {
            return Err(ReducedFailure {
                findings: vec![Finding {
                    category: "policy-differential".into(),
                    detail: diffs.join("; "),
                    step_index: total.saturating_sub(1),
                }],
                prefix_len: total,
            });
        }
    }
    if outcome.findings.is_empty() {
        // Storm expectations: dedicated hostile floods must surface diagnostics.
        let v3 = script.config_kind.is_v3();
        let expect_violation = match script.archetype {
            "roster_storm" => true,
            "accountability" => v3,
            _ => false,
        };
        if expect_violation && outcome.violations == 0 {
            return Err(ReducedFailure {
                findings: vec![Finding {
                    category: "bounds-unobserved".into(),
                    detail: "hostile bound-exceeding flood produced zero ProtocolViolation \
                             diagnostics"
                        .into(),
                    step_index: total.saturating_sub(1),
                }],
                prefix_len: total,
            });
        }
        return Ok(outcome);
    }
    // Reduce: binary search the minimal failing prefix.
    let mut lo = 1usize;
    let mut hi = total;
    while lo < hi {
        let mid = lo.saturating_add(hi.saturating_sub(lo).saturating_div(2));
        let failed = match catch_unwind(AssertUnwindSafe(|| run_prefix(script, policy, mid))) {
            Ok(o) => !o.findings.is_empty(),
            Err(_) => true,
        };
        if failed {
            hi = mid;
        } else {
            lo = mid.saturating_add(1);
        }
        heartbeat();
    }
    Err(ReducedFailure {
        findings: outcome.findings,
        prefix_len: lo,
    })
}

/// Fail-closed helper for a structurally impossible sibling-policy absence.
fn unreachable_policy_missing(p: ProtocolViolationPolicy, total: usize) -> ReducedFailure {
    ReducedFailure {
        findings: vec![Finding {
            category: "campaign-internal".into(),
            detail: format!("sibling policy {p:?} outcome missing from the differential"),
            step_index: total.saturating_sub(1),
        }],
        prefix_len: total,
    }
}

pub struct ReducedFailure {
    pub findings: Vec<Finding>,
    pub prefix_len: usize,
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

/// Render the minimal failing prefix for a report.
pub fn render_prefix(script: &Script, prefix_len: usize, max_steps: usize) -> String {
    let mut out = String::new();
    let shown = prefix_len.min(script.steps.len());
    let start = shown.saturating_sub(max_steps);
    if start > 0 {
        out.push_str(&format!("    … ({} earlier steps elided)\n", start));
    }
    for (offset, step) in script
        .steps
        .iter()
        .skip(start)
        .take(shown.saturating_sub(start))
        .enumerate()
    {
        out.push_str(&format!(
            "    [{:>4}] {}\n",
            start.saturating_add(offset),
            step.render()
        ));
    }
    out
}

/// Crate-visible constructors for canaries that drive the expectation oracle
/// directly (direct-feed sensitivity proofs).
pub(crate) mod test_support {
    use super::{AllowedOutcome, ConfigKind, Oracle, ProtocolViolationPolicy, Slot};

    pub(crate) fn slot_with(frame: &'static str, alternatives: Vec<AllowedOutcome>) -> Slot {
        Slot {
            alternatives,
            frame,
            fence_retired_on_violation: None,
        }
    }

    pub(crate) fn slot_for(frame: &'static str) -> Slot {
        Slot {
            alternatives: super::exactly(frame),
            frame,
            fence_retired_on_violation: None,
        }
    }

    pub(crate) fn fresh_oracle(
        policy: ProtocolViolationPolicy,
        config: ConfigKind,
        echo_room_ops: bool,
    ) -> Oracle {
        Oracle::new(policy, config, echo_room_ops)
    }
}
