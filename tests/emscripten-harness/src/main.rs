//! On-target runtime harness for the Emscripten WebSocket transport.
//!
//! This binary is built for `wasm32-unknown-emscripten` and executed under
//! Node.js by `driver/run-harness.cjs`, which owns the loopback WebSocket
//! server and drives one exported scheduling step per ~1 ms timer tick so
//! the JavaScript event loop can deliver browser events between steps at a
//! cadence resembling a polling game loop.
//!
//! Scenarios (selected by the `mode` argument of `sfh_begin`):
//!
//! | mode | scenario         | On-target contract pinned                              |
//! | ---- | ---------------- | ------------------------------------------------------ |
//! | 0    | roundtrip        | pre-open `Pending` retention, text/binary/empty-frame echo, client-initiated close→delete reclaim |
//! | 1    | send-after-close | sends on a socket the browser already closed fail terminally with the frame retained (round-24 fix), then peer close metadata |
//! | 2    | ledger-bound     | inbound-byte bound admits two 50-byte frames, fuses on the third, drops flood frames afterwards |
//! | 3    | abrupt-error     | `onerror` surfaces a terminal receive error and the queued `onclose` (1006, unclean) still reaches `close_info()` |
//! | 4    | ledger-drain     | draining admitted frames releases ledger credit: a second admission wave against the same bound succeeds (issue #212 M2) |
//! | 5    | pre-open-failure | a rejected handshake surfaces the terminal error before `is_ready()`, consumes the close tail, refuses post-terminal sends with the frame retained (issue #212 M3) |
//! | 6    | abort-extended   | extended-length (126/127) frames round-trip byte-exact, `abort()` is idempotent, and post-abort sends/receives close cleanly (issue #212) |
//!
//! Every scenario runs under an error-latching tracing subscriber: a
//! `tracing::error!` between scenario boundaries — the Drop path's
//! cleanup-failure signal — fails the next scheduling step instead of
//! passing silently (issue #212 M4); `sfh_finish` drops the final transport
//! under the same latch so the last cleanup is observed too.
//!
//! Invalid-UTF-8 text fusion is deliberately absent: Emscripten's WebSocket
//! shim only delivers text frames as JavaScript strings (which are always
//! valid UTF-8 once re-encoded), so that path stays covered by the
//! host-target unit tests of the shared decode helper.

#![allow(deprecated)]

use signal_fish_client::{
    EmscriptenWebSocketConnectOptions, EmscriptenWebSocketTransport, SignalFishError, Transport,
    TransportFrame,
};
use std::ffi::{c_char, CString};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll, Waker};

const RESULT_RUNNING: i32 = 0;
const RESULT_PASS: i32 = 1;
const RESULT_FAIL: i32 = 2;

/// Ledger bound for the `ledger-bound` and `ledger-drain` scenarios: two
/// 50-byte frames charge 64 units each (the 64-byte minimum charge); the
/// third 64-unit charge would exceed 160 and fuse.
const LEDGER_BOUND_BYTES: usize = 160;
const LEDGER_FLOOD_FRAME_LEN: usize = 50;

/// Text-frame length that forces the 2-byte extended-length encoding
/// (payload lengths from 126 through 65535).
const EXTENDED_TEXT_LEN: usize = 126;
/// Binary-frame length well inside the 2-byte extended-length encoding.
const EXTENDED_BINARY_LEN: usize = 300;
/// Binary probe length that forces the 8-byte extended-length encoding
/// (payload lengths above 65535).
const LARGE_PROBE_LEN: usize = 70_000;

/// Upper bound on scheduling steps for one scenario so a wedged callback
/// bridge fails the harness instead of hanging CI.
const MAX_STEPS: u32 = 4096;

// ── Error-latch subscriber (issue #212 M4) ──────────────────────────────────

/// Latches the first `tracing::error!` event.
///
/// The transport's `Drop` path logs an error-level event when cleanup fails
/// and its callback state is intentionally leaked. `store_harness` drops the
/// previous scenario's transport inside the next `sfh_begin`, so without this
/// latch a delete failure between scenarios would pass silently.
struct ErrorLatch {
    latched: AtomicBool,
    detail: std::sync::Mutex<Option<String>>,
}

impl tracing::Subscriber for ErrorLatch {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _attributes: &tracing::span::Attributes<'_>) -> tracing::Id {
        // No spans are recorded by the transport; any id would do, and zero
        // is the one value `Id::from_u64` rejects.
        tracing::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::Id, _follows: &tracing::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        if event.metadata().level() == &tracing::Level::ERROR {
            self.latched.store(true, Ordering::Relaxed);
            if let Ok(mut detail) = self.detail.lock() {
                *detail = Some(event.metadata().name().to_owned());
            }
        }
    }

    fn enter(&self, _span: &tracing::Id) {}

    fn exit(&self, _span: &tracing::Id) {}
}

static ERROR_LATCH: OnceLock<Arc<ErrorLatch>> = OnceLock::new();

fn error_latch() -> &'static Arc<ErrorLatch> {
    ERROR_LATCH.get_or_init(|| ErrorLatch {
        latched: AtomicBool::new(false),
        detail: std::sync::Mutex::new(None),
    }.into())
}

fn reset_error_latch() {
    error_latch().latched.store(false, Ordering::Relaxed);
}

fn error_latched() -> bool {
    error_latch().latched.load(Ordering::Relaxed)
}

fn ctx() -> Context<'static> {
    Context::from_waker(Waker::noop())
}

fn cstring(reason: &str) -> CString {
    CString::new(reason).unwrap_or_else(|_| CString::default())
}

// Single-threaded wasm32-unknown-emscripten execution model: every exported
// entry point runs on the main JavaScript thread (the transport also forces
// `create_on_main_thread = 1`), so there is no concurrent access to this
// static and the `!Send` transport never has to satisfy `Send`/`Sync`.
static mut HARNESS: Option<Harness> = None;

struct Harness {
    transport: Option<EmscriptenWebSocketTransport>,
    scenario: Scenario,
    fail_reason: Option<CString>,
    steps: u32,
}

impl Harness {
    fn fail(&mut self, reason: String) -> i32 {
        if self.fail_reason.is_none() {
            self.fail_reason = Some(cstring(&reason));
        }
        RESULT_FAIL
    }

    fn step(&mut self) -> i32 {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            return self.fail(format!("scenario did not finish within {MAX_STEPS} steps"));
        }
        if error_latched() {
            let detail = error_latch()
                .detail
                .lock()
                .ok()
                .and_then(|detail| detail.clone())
                .unwrap_or_default();
            return self.fail(format!(
                "a tracing error-level event was observed ({detail}); the Drop \
                 path's cleanup-failure signal must never fire between scenarios"
            ));
        }
        let Some(transport) = self.transport.as_mut() else {
            return self.fail("step called before sfh_begin".into());
        };
        transport.begin_poll_cycle();
        let outcome = match &mut self.scenario {
            Scenario::Roundtrip(scenario) => scenario.step(transport),
            Scenario::SendAfterClose(scenario) => scenario.step(transport),
            Scenario::LedgerBound(scenario) => scenario.step(transport),
            Scenario::AbruptError(scenario) => scenario.step(transport),
            Scenario::LedgerDrain(scenario) => scenario.step(transport),
            Scenario::PreOpenFailure(scenario) => scenario.step(transport),
            Scenario::AbortExtended(scenario) => scenario.step(transport),
        };
        match outcome {
            StepOutcome::Running => RESULT_RUNNING,
            StepOutcome::Pass => RESULT_PASS,
            StepOutcome::Fail(reason) => self.fail(reason),
        }
    }
}

enum StepOutcome {
    Running,
    Pass,
    Fail(String),
}

// ── Shared drain helper ─────────────────────────────────────────────────────

/// Outcome of draining one `poll_recv` observation.
enum Recv {
    Pending,
    Frame(TransportFrame),
    TerminalError(SignalFishError),
    Ended,
}

fn poll_once(transport: &mut EmscriptenWebSocketTransport) -> Recv {
    match transport.poll_recv(&mut ctx()) {
        Poll::Pending => Recv::Pending,
        Poll::Ready(Some(Ok(frame))) => Recv::Frame(frame),
        Poll::Ready(Some(Err(error))) => Recv::TerminalError(error),
        Poll::Ready(None) => Recv::Ended,
    }
}

// ── Scenario 0: roundtrip ───────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum Echo {
    Text(String),
    Binary(Vec<u8>),
}

struct RoundtripScenario {
    phase: RoundtripPhase,
    /// Outbound frames still to send, in order. The head is installed into
    /// the slot before `onopen` so the pre-open retention contract is
    /// exercised on the very first frame.
    pending_sends: Vec<Echo>,
    /// Echoes expected back, in order.
    expected: Vec<Echo>,
    /// Frame slot for `poll_send`; must be retained across `Pending`.
    slot: Option<TransportFrame>,
    /// How many `poll_send` calls observed the pre-open `Pending` retention;
    /// the scenario refuses to pass if the CONNECTING window was never
    /// observed (the driver delays the handshake to guarantee it).
    pre_open_pending_polls: u32,
}

enum RoundtripPhase {
    /// Before `onopen`: a queued send must stay `Pending` and retain the
    /// frame; `is_ready()` is false until the open event is drained.
    Connecting,
    /// Readiness observed; echoes in flight.
    Echoing,
    /// All echoes verified; close and require success.
    Closing,
}

impl RoundtripScenario {
    fn new() -> Self {
        let script = vec![
            Echo::Text("retained-before-open".into()),
            Echo::Binary(b"\x00\x01\x02 binary \xff payload".to_vec()),
            Echo::Text(String::new()),
            Echo::Binary(Vec::new()),
        ];
        Self {
            phase: RoundtripPhase::Connecting,
            expected: script.clone(),
            pending_sends: script,
            slot: None,
            pre_open_pending_polls: 0,
        }
    }

    fn install_next_send(&mut self) {
        if self.slot.is_none() {
            match self.pending_sends.first() {
                Some(Echo::Text(text)) => {
                    self.slot = Some(TransportFrame::Text(text.clone()));
                }
                Some(Echo::Binary(bytes)) => {
                    self.slot = Some(TransportFrame::Binary(bytes.clone()));
                }
                None => {}
            }
        }
    }
}

impl RoundtripScenario {
    fn step(&mut self, transport: &mut EmscriptenWebSocketTransport) -> StepOutcome {
        match self.phase {
            RoundtripPhase::Connecting => {
                // Drain observations first: the open event is what flips
                // `is_ready()`.
                match poll_once(transport) {
                    Recv::Pending => {}
                    Recv::Frame(frame) => {
                        return StepOutcome::Fail(format!(
                            "received frame {frame:?} before completing any send"
                        ));
                    }
                    Recv::TerminalError(error) => {
                        return StepOutcome::Fail(format!("error before open: {error}"));
                    }
                    Recv::Ended => return StepOutcome::Fail("peer closed before open".into()),
                }
                if transport.is_ready() {
                    self.phase = RoundtripPhase::Echoing;
                    return StepOutcome::Running;
                }
                // Queue the first frame before the handshake completes; the
                // transport must keep it pending and caller-owned.
                self.install_next_send();
                match transport.poll_send(&mut ctx(), &mut self.slot) {
                    Poll::Pending => {
                        if self.slot.is_none() {
                            return StepOutcome::Fail(
                                "pre-open Pending poll_send consumed the caller frame".into(),
                            );
                        }
                        self.pre_open_pending_polls += 1;
                        StepOutcome::Running
                    }
                    Poll::Ready(Ok(())) => {
                        StepOutcome::Fail("pre-open poll_send accepted a frame before onopen".into())
                    }
                    Poll::Ready(Err(error)) => {
                        StepOutcome::Fail(format!("pre-open poll_send failed: {error}"))
                    }
                }
            }
            RoundtripPhase::Echoing => {
                if !transport.is_ready() {
                    return StepOutcome::Fail("is_ready() regressed after onopen".into());
                }
                self.install_next_send();
                if self.slot.is_some() {
                    match transport.poll_send(&mut ctx(), &mut self.slot) {
                        Poll::Ready(Ok(())) => {
                            self.pending_sends.remove(0);
                        }
                        Poll::Ready(Err(error)) => {
                            return StepOutcome::Fail(format!("echo poll_send failed: {error}"));
                        }
                        Poll::Pending => {}
                    }
                }
                // Drain every frame that is ready and verify the echo order.
                loop {
                    match poll_once(transport) {
                        Recv::Pending => break,
                        Recv::Frame(frame) => {
                            let Some(expected) = self.expected.first() else {
                                return StepOutcome::Fail(format!(
                                    "received unexpected extra frame: {frame:?}"
                                ));
                            };
                            let received = match frame {
                                TransportFrame::Text(text) => Echo::Text(text),
                                TransportFrame::Binary(bytes) => Echo::Binary(bytes),
                            };
                            if &received != expected {
                                return StepOutcome::Fail(format!(
                                    "echo mismatch: expected {expected:?}, got {received:?}"
                                ));
                            }
                            self.expected.remove(0);
                        }
                        Recv::TerminalError(error) => {
                            return StepOutcome::Fail(format!("echo poll_recv error: {error}"));
                        }
                        Recv::Ended => {
                            return StepOutcome::Fail("peer closed during echo roundtrip".into())
                        }
                    }
                }
                if self.expected.is_empty() {
                    self.phase = RoundtripPhase::Closing;
                }
                StepOutcome::Running
            }
            RoundtripPhase::Closing => {
                if self.pre_open_pending_polls == 0 {
                    return StepOutcome::Fail(
                        "the CONNECTING window was never observed; the pre-open \
                         retention pin did not run"
                            .into(),
                    );
                }
                match transport.poll_close(&mut ctx()) {
                    Poll::Ready(Ok(())) => StepOutcome::Pass,
                    Poll::Ready(Err(error)) => {
                        StepOutcome::Fail(format!("client-initiated poll_close failed: {error}"))
                    }
                    Poll::Pending => StepOutcome::Running,
                }
            }
        }
    }
}

// ── Scenario 1: send-after-close ────────────────────────────────────────────

struct SendAfterCloseScenario {
    phase: SendAfterClosePhase,
    slot: Option<TransportFrame>,
}

enum SendAfterClosePhase {
    Connecting,
    /// Open; announce readiness with one normal send so the server closes
    /// only after the probe phase is definitely reached.
    Announce,
    /// Open and announced; wait until the browser socket stops being open
    /// for sends.
    WaitingForDeadSocket,
    /// A send refused on the dead socket was observed; drain the close.
    DrainPeerClose,
}

impl SendAfterCloseScenario {
    fn step(&mut self, transport: &mut EmscriptenWebSocketTransport) -> StepOutcome {
        match self.phase {
            SendAfterClosePhase::Connecting => match poll_once(transport) {
                Recv::Pending => {
                    if transport.is_ready() {
                        self.phase = SendAfterClosePhase::Announce;
                    }
                    StepOutcome::Running
                }
                Recv::Frame(frame) => {
                    StepOutcome::Fail(format!("unexpected frame before server close: {frame:?}"))
                }
                Recv::TerminalError(error) => {
                    StepOutcome::Fail(format!("error before server close: {error}"))
                }
                Recv::Ended => StepOutcome::Fail("peer closed before the probe".into()),
            },
            SendAfterClosePhase::Announce => {
                if self.slot.is_none() {
                    self.slot = Some(TransportFrame::Text("ready".into()));
                }
                match transport.poll_send(&mut ctx(), &mut self.slot) {
                    Poll::Ready(Ok(())) => {
                        self.phase = SendAfterClosePhase::WaitingForDeadSocket;
                        StepOutcome::Running
                    }
                    Poll::Pending => StepOutcome::Running,
                    Poll::Ready(Err(error)) => {
                        StepOutcome::Fail(format!("announce send failed: {error}"))
                    }
                }
            }
            SendAfterClosePhase::WaitingForDeadSocket => {
                // While the close frame is still in flight the socket is
                // genuinely open, so accepted probes are correct; keep
                // probing until the browser has processed the peer close and
                // the live ready state turns non-open.
                if self.slot.is_none() {
                    self.slot = Some(TransportFrame::Text("probe-after-close".into()));
                }
                match transport.poll_send(&mut ctx(), &mut self.slot) {
                    Poll::Pending => {
                        // Still open for sends; the server has not closed yet.
                        if self.slot.is_none() {
                            return StepOutcome::Fail(
                                "Pending poll_send consumed the caller frame".into(),
                            );
                        }
                        StepOutcome::Running
                    }
                    Poll::Ready(Ok(())) => StepOutcome::Running,
                    Poll::Ready(Err(error)) => {
                        // Round-24 contract: the send fails terminally instead
                        // of being silently discarded, and the frame stays
                        // caller-owned.
                        if self.slot.is_none() {
                            return StepOutcome::Fail(
                                "refused send destroyed the caller-owned frame".into(),
                            );
                        }
                        if !matches!(error, SignalFishError::TransportSend(_)) {
                            return StepOutcome::Fail(format!(
                                "expected TransportSend on dead socket, got: {error}"
                            ));
                        }
                        self.phase = SendAfterClosePhase::DrainPeerClose;
                        StepOutcome::Running
                    }
                }
            }
            SendAfterClosePhase::DrainPeerClose => match poll_once(transport) {
                Recv::Pending => StepOutcome::Running,
                Recv::Frame(frame) => {
                    StepOutcome::Fail(format!("unexpected frame after server close: {frame:?}"))
                }
                Recv::TerminalError(error) => StepOutcome::Fail(format!(
                    "expected a clean peer close, got terminal error: {error}"
                )),
                Recv::Ended => {
                    let Some(info) = transport.close_info() else {
                        return StepOutcome::Fail("close_info missing after peer close".into());
                    };
                    if info.code != Some(4000)
                        || info.reason.as_deref() != Some("draining")
                        || info.clean != Some(true)
                        || !info.initiated_by_peer
                    {
                        return StepOutcome::Fail(format!("close metadata mismatch: {info:?}"));
                    }
                    StepOutcome::Pass
                }
            },
        }
    }
}

// ── Scenario 2: ledger-bound ────────────────────────────────────────────────

struct LedgerBoundScenario {
    phase: LedgerPhase,
    drained_frames: usize,
}

enum LedgerPhase {
    /// Two flood frames are admitted against the 160-byte ledger, then the
    /// third refusal fuses; drain and verify the admitted pair.
    Draining,
    ExpectTerminalError,
    ExpectClosed,
}

impl LedgerBoundScenario {
    fn step(&mut self, transport: &mut EmscriptenWebSocketTransport) -> StepOutcome {
        match self.phase {
            LedgerPhase::Draining => match poll_once(transport) {
                Recv::Pending => StepOutcome::Running,
                Recv::Frame(TransportFrame::Text(text)) => {
                    self.drained_frames += 1;
                    if text.len() != LEDGER_FLOOD_FRAME_LEN {
                        return StepOutcome::Fail(format!(
                            "flood frame {} had {} bytes, expected {LEDGER_FLOOD_FRAME_LEN}",
                            self.drained_frames,
                            text.len()
                        ));
                    }
                    if self.drained_frames > 2 {
                        return StepOutcome::Fail(format!(
                            "ledger admitted {} frames; expected exactly 2 before fusion",
                            self.drained_frames
                        ));
                    }
                    if self.drained_frames == 2 {
                        self.phase = LedgerPhase::ExpectTerminalError;
                    }
                    StepOutcome::Running
                }
                Recv::Frame(frame) => {
                    StepOutcome::Fail(format!("unexpected flood frame kind: {frame:?}"))
                }
                Recv::TerminalError(error) => StepOutcome::Fail(format!(
                    "expected two admitted frames first, got early error: {error}"
                )),
                Recv::Ended => StepOutcome::Fail("peer closed before ledger fusion".into()),
            },
            LedgerPhase::ExpectTerminalError => match poll_once(transport) {
                Recv::Pending => StepOutcome::Running,
                Recv::Frame(frame) => StepOutcome::Fail(format!(
                    "post-fusion frame should have been dropped at the callback: {frame:?}"
                )),
                Recv::TerminalError(SignalFishError::TransportReceive(_)) => {
                    self.phase = LedgerPhase::ExpectClosed;
                    StepOutcome::Running
                }
                Recv::TerminalError(error) => StepOutcome::Fail(format!(
                    "expected TransportReceive fusion error, got: {error}"
                )),
                Recv::Ended => StepOutcome::Fail("transport ended without the fusion error".into()),
            },
            LedgerPhase::ExpectClosed => match poll_once(transport) {
                Recv::Ended => {
                    // Remaining flood frames were dropped at the callback
                    // (AlreadyFused); nothing else may surface.
                    StepOutcome::Pass
                }
                Recv::Pending => StepOutcome::Fail(
                    "terminal fusion error must end the transport immediately".into(),
                ),
                Recv::Frame(frame) => {
                    StepOutcome::Fail(format!("unexpected frame after fusion: {frame:?}"))
                }
                Recv::TerminalError(error) => StepOutcome::Fail(format!(
                    "unexpected second error after fusion: {error}"
                )),
            },
        }
    }
}

// ── Scenario 3: abrupt-error ────────────────────────────────────────────────

struct AbruptErrorScenario {
    phase: AbruptPhase,
}

enum AbruptPhase {
    Connecting,
    ExpectTerminalError,
    VerifyCloseMetadata,
}

impl AbruptErrorScenario {
    fn step(&mut self, transport: &mut EmscriptenWebSocketTransport) -> StepOutcome {
        match self.phase {
            AbruptPhase::Connecting => match poll_once(transport) {
                Recv::Pending => {
                    if transport.is_ready() {
                        self.phase = AbruptPhase::ExpectTerminalError;
                    }
                    StepOutcome::Running
                }
                Recv::Frame(frame) => {
                    StepOutcome::Fail(format!("unexpected frame before abrupt close: {frame:?}"))
                }
                Recv::TerminalError(error) => {
                    StepOutcome::Fail(format!("error before abrupt close: {error}"))
                }
                Recv::Ended => StepOutcome::Fail("peer closed before the abrupt test".into()),
            },
            AbruptPhase::ExpectTerminalError => match poll_once(transport) {
                Recv::Pending => StepOutcome::Running,
                Recv::TerminalError(SignalFishError::TransportReceive(_)) => {
                    self.phase = AbruptPhase::VerifyCloseMetadata;
                    StepOutcome::Running
                }
                Recv::TerminalError(error) => {
                    StepOutcome::Fail(format!("expected TransportReceive, got: {error}"))
                }
                Recv::Frame(frame) => {
                    StepOutcome::Fail(format!("unexpected frame during abrupt close: {frame:?}"))
                }
                Recv::Ended => StepOutcome::Fail("transport ended without the onerror event".into()),
            },
            AbruptPhase::VerifyCloseMetadata => {
                // The onclose queued behind onerror must reach close_info
                // (round-24 tail-consume contract). Browsers and faithful
                // hosts report abnormal termination as 1006/unclean.
                let Some(info) = transport.close_info() else {
                    return StepOutcome::Fail(
                        "close_info missing after onerror+onclose drain".into(),
                    );
                };
                if info.code != Some(1006) || info.clean != Some(false) || !info.initiated_by_peer
                {
                    return StepOutcome::Fail(format!("abrupt-close metadata mismatch: {info:?}"));
                }
                StepOutcome::Pass
            }
        }
    }
}

// ── Scenario 4: ledger drain-credit ─────────────────────────────────────────
//
// Mode 2 proves admission/fusion on one admission wave. It cannot detect a
// regression where draining frames never returns their ledger credit,
// because the single wave fuses before any drain matters. This scenario
// admits wave 1 against the same 160-byte bound, drains it, then proves a
// second wave is admitted: if `poll_recv` stopped releasing credit, wave 2's
// first frame would fuse at the callback and surface a terminal error.

struct LedgerDrainScenario {
    phase: LedgerDrainPhase,
    drained_frames: usize,
    /// Frame slot for the wave-2 request send.
    slot: Option<TransportFrame>,
    /// The wave-2 request is sent exactly once; the server answers every
    /// request with a fresh two-frame wave, so a re-send would stack waves
    /// past the ledger bound and spuriously report an M2 regression.
    sent_next: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LedgerDrainPhase {
    /// Drain wave 1 (two 50-byte frames; the open event is consumed inside
    /// the same first drain) — drains release ledger credit.
    Draining,
    /// All four frames drained; close cleanly.
    Closing,
}

impl LedgerDrainScenario {
    fn step(&mut self, transport: &mut EmscriptenWebSocketTransport) -> StepOutcome {
        match self.phase {
            LedgerDrainPhase::Draining => match poll_once(transport) {
                Recv::Pending => {
                    // Once wave 1 is fully drained, request wave 2 exactly
                    // once. Pending polls retain the caller-owned frame
                    // until accepted.
                    if self.drained_frames == 2 && !self.sent_next {
                        if self.slot.is_none() {
                            self.slot = Some(TransportFrame::Text("next".into()));
                        }
                        match transport.poll_send(&mut ctx(), &mut self.slot) {
                            Poll::Ready(Ok(())) => {
                                self.slot = None;
                                self.sent_next = true;
                            }
                            Poll::Ready(Err(error)) => {
                                return StepOutcome::Fail(format!("wave-2 request failed: {error}"))
                            }
                            Poll::Pending => {}
                        }
                    }
                    StepOutcome::Running
                }
                Recv::Frame(TransportFrame::Text(text)) => {
                    if text.len() != LEDGER_FLOOD_FRAME_LEN {
                        return StepOutcome::Fail(format!(
                            "wave frame {} had {} bytes, expected {LEDGER_FLOOD_FRAME_LEN}",
                            self.drained_frames + 1,
                            text.len()
                        ));
                    }
                    self.drained_frames += 1;
                    if self.drained_frames == 4 {
                        self.phase = LedgerDrainPhase::Closing;
                    }
                    StepOutcome::Running
                }
                Recv::Frame(frame) => {
                    StepOutcome::Fail(format!("unexpected wave frame kind: {frame:?}"))
                }
                Recv::TerminalError(error) => {
                    if self.drained_frames >= 2 {
                        return StepOutcome::Fail(format!(
                            "wave 2 fused after {} drained frames; draining did not \
                             release ledger credit (issue #212 M2 regression): {error}",
                            self.drained_frames
                        ));
                    }
                    StepOutcome::Fail(format!("error during wave 1: {error}"))
                }
                Recv::Ended => {
                    StepOutcome::Fail("peer closed before both waves drained".into())
                }
            },
            LedgerDrainPhase::Closing => {
                if self.drained_frames != 4 {
                    return StepOutcome::Fail(format!(
                        "closing with {} drained frames; expected 4 (two full waves)",
                        self.drained_frames
                    ));
                }
                match transport.poll_close(&mut ctx()) {
                    Poll::Ready(Ok(())) => StepOutcome::Pass,
                    Poll::Ready(Err(error)) => {
                        StepOutcome::Fail(format!("post-drain poll_close failed: {error}"))
                    }
                    Poll::Pending => StepOutcome::Running,
                }
            }
        }
    }
}

// ── Scenario 5: pre-open failure ────────────────────────────────────────────
//
// No other scenario starts from a failing handshake, so the transport's
// error-before-open path never executed on-target: a terminal receive error
// while `is_ready()` was never true, the close-tail consume behind it, and
// the post-terminal send refusal. Host unit tests cannot reach this
// target-gated module.

struct PreOpenFailureScenario {
    phase: PreOpenFailurePhase,
    /// Probe frame installed during CONNECTING; must survive both the
    /// pre-open `Pending` window and the post-terminal refusal.
    slot: Option<TransportFrame>,
    /// How many `poll_send` calls observed the pre-open `Pending` retention;
    /// the scenario refuses to pass if the window was never observed.
    pre_open_pending_polls: u32,
}

enum PreOpenFailurePhase {
    /// CONNECTING against a server that rejects the handshake.
    Connecting,
    /// Terminal error drained while never open; verify close metadata.
    VerifyCloseMetadata,
    /// Post-terminal sends refuse with the frame retained.
    ExpectSendRefusal,
    /// The receive stream ends after the terminal error.
    ExpectEnded,
}

impl PreOpenFailureScenario {
    fn step(&mut self, transport: &mut EmscriptenWebSocketTransport) -> StepOutcome {
        match self.phase {
            PreOpenFailurePhase::Connecting => match poll_once(transport) {
                Recv::Pending => {
                    if transport.is_ready() {
                        return StepOutcome::Fail(
                            "the handshake opened before the server rejected it".into(),
                        );
                    }
                    if self.slot.is_none() {
                        self.slot = Some(TransportFrame::Text("retained-into-failure".into()));
                    }
                    match transport.poll_send(&mut ctx(), &mut self.slot) {
                        Poll::Pending => {
                            if self.slot.is_none() {
                                return StepOutcome::Fail(
                                    "pre-open Pending poll_send consumed the caller frame".into(),
                                );
                            }
                            self.pre_open_pending_polls += 1;
                            StepOutcome::Running
                        }
                        Poll::Ready(Ok(())) => StepOutcome::Fail(
                            "pre-open poll_send accepted a frame during a rejected handshake".into(),
                        ),
                        Poll::Ready(Err(error)) => {
                            StepOutcome::Fail(format!("pre-open poll_send failed: {error}"))
                        }
                    }
                }
                Recv::TerminalError(error) => {
                    if transport.is_ready() {
                        return StepOutcome::Fail(
                            "is_ready() was observed before the terminal error".into(),
                        );
                    }
                    if !matches!(error, SignalFishError::TransportReceive(_)) {
                        return StepOutcome::Fail(format!(
                            "expected TransportReceive for the rejected handshake, got: {error}"
                        ));
                    }
                    if self.pre_open_pending_polls == 0 {
                        return StepOutcome::Fail(
                            "the CONNECTING window was never observed; the pre-open \
                             retention pin did not run"
                                .into(),
                        );
                    }
                    self.phase = PreOpenFailurePhase::VerifyCloseMetadata;
                    StepOutcome::Running
                }
                Recv::Frame(frame) => {
                    StepOutcome::Fail(format!("unexpected frame before rejection: {frame:?}"))
                }
                Recv::Ended => {
                    StepOutcome::Fail("transport ended without the pre-open terminal error".into())
                }
            },
            PreOpenFailurePhase::VerifyCloseMetadata => {
                // The browser polyfill reports an abnormal closure (1006,
                // unclean) for a rejected handshake: no close frame was ever
                // received. The close-tail consume must have captured it.
                let Some(info) = transport.close_info() else {
                    return StepOutcome::Fail(
                        "close_info missing after the pre-open terminal error".into(),
                    );
                };
                if info.code != Some(1006) || info.clean != Some(false) || !info.initiated_by_peer
                {
                    return StepOutcome::Fail(format!(
                        "rejected-handshake close metadata mismatch: {info:?}"
                    ));
                }
                self.phase = PreOpenFailurePhase::ExpectSendRefusal;
                StepOutcome::Running
            }
            PreOpenFailurePhase::ExpectSendRefusal => {
                let Some(slot) = self.slot.as_ref() else {
                    return StepOutcome::Fail("probe frame vanished before the refusal".into());
                };
                let _ = slot;
                match transport.poll_send(&mut ctx(), &mut self.slot) {
                    Poll::Ready(Err(SignalFishError::TransportClosed)) => {
                        if self.slot.is_none() {
                            return StepOutcome::Fail(
                                "post-terminal refusal destroyed the caller-owned frame".into(),
                            );
                        }
                        self.phase = PreOpenFailurePhase::ExpectEnded;
                        StepOutcome::Running
                    }
                    Poll::Ready(Err(error)) => {
                        StepOutcome::Fail(format!(
                            "expected TransportClosed after the terminal error, got: {error}"
                        ))
                    }
                    Poll::Ready(Ok(())) => StepOutcome::Fail(
                        "poll_send accepted a frame on a terminally closed transport".into(),
                    ),
                    Poll::Pending => StepOutcome::Fail(
                        "poll_send deferred on a terminally closed transport".into(),
                    ),
                }
            }
            PreOpenFailurePhase::ExpectEnded => match poll_once(transport) {
                Recv::Ended => StepOutcome::Pass,
                Recv::Pending => StepOutcome::Fail(
                    "the receive stream must end after the terminal error".into(),
                ),
                Recv::Frame(frame) => {
                    StepOutcome::Fail(format!("unexpected frame after the terminal error: {frame:?}"))
                }
                Recv::TerminalError(error) => StepOutcome::Fail(format!(
                    "unexpected second terminal error: {error}"
                )),
            },
        }
    }
}

// ── Scenario 6: abort + extended-length frames ──────────────────────────────
//
// Server→client frames of 126/127-class lengths never executed on-target,
// and neither did `Transport::abort()`. The codec's extended-length encodings
// are probe-verified standalone; this scenario pins the whole on-target path:
// exact byte content through the shim for both extended encodings, in both
// directions, then abort idempotence and its post-abort refusals.

struct AbortExtendedScenario {
    phase: AbortExtendedPhase,
    /// Frame slot for `poll_send`; retained across `Pending` and refusals.
    slot: Option<TransportFrame>,
    /// The exact 70 KiB probe payload; the echo must match byte-for-byte.
    probe: Vec<u8>,
    server_text_seen: bool,
    server_binary_seen: bool,
    /// The 70 KiB probe is installed and sent exactly once; a re-send would
    /// put a second echo in flight whose bytes nothing compares.
    probe_sent: bool,
}

enum AbortExtendedPhase {
    /// Drain the server's 126-byte text and 300-byte binary frames.
    ExpectServerFrames,
    /// Send the 70 KiB binary probe and verify the exact echo.
    RoundTripLarge,
    /// `abort()` twice, then pin post-abort refusals and clean close.
    AbortAndVerify,
}

fn extended_probe(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|offset| seed.wrapping_add(offset as u8)).collect()
}

impl AbortExtendedScenario {
    fn step(&mut self, transport: &mut EmscriptenWebSocketTransport) -> StepOutcome {
        match self.phase {
            AbortExtendedPhase::ExpectServerFrames => match poll_once(transport) {
                Recv::Pending => StepOutcome::Running,
                Recv::Frame(TransportFrame::Text(text)) => {
                    if self.server_text_seen {
                        return StepOutcome::Fail("duplicate server text frame".into());
                    }
                    let expected = "x".repeat(EXTENDED_TEXT_LEN);
                    if text != expected {
                        return StepOutcome::Fail(format!(
                            "126-byte text frame mismatched (got {} bytes)",
                            text.len()
                        ));
                    }
                    self.server_text_seen = true;
                    StepOutcome::Running
                }
                Recv::Frame(TransportFrame::Binary(bytes)) => {
                    if !self.server_text_seen || self.server_binary_seen {
                        return StepOutcome::Fail("server frames arrived out of order".into());
                    }
                    if bytes != extended_probe(EXTENDED_BINARY_LEN, 0) {
                        return StepOutcome::Fail(format!(
                            "300-byte binary frame mismatched (got {} bytes)",
                            bytes.len()
                        ));
                    }
                    self.server_binary_seen = true;
                    self.phase = AbortExtendedPhase::RoundTripLarge;
                    StepOutcome::Running
                }
                Recv::TerminalError(error) => {
                    StepOutcome::Fail(format!("error during server frames: {error}"))
                }
                Recv::Ended => StepOutcome::Fail("peer closed during server frames".into()),
            },
            AbortExtendedPhase::RoundTripLarge => {
                if !self.probe_sent {
                    if self.slot.is_none() {
                        self.slot = Some(TransportFrame::Binary(extended_probe(LARGE_PROBE_LEN, 7)));
                    }
                    match transport.poll_send(&mut ctx(), &mut self.slot) {
                        Poll::Ready(Ok(())) => self.probe_sent = true,
                        Poll::Ready(Err(error)) => {
                            return StepOutcome::Fail(format!("probe send failed: {error}"))
                        }
                        Poll::Pending => {}
                    }
                }
                match poll_once(transport) {
                    Recv::Pending => StepOutcome::Running,
                    Recv::Frame(TransportFrame::Binary(bytes)) => {
                        if bytes != self.probe {
                            return StepOutcome::Fail(format!(
                                "70 KiB echo mismatched (got {} bytes, expected {})",
                                bytes.len(),
                                self.probe.len()
                            ));
                        }
                        self.phase = AbortExtendedPhase::AbortAndVerify;
                        StepOutcome::Running
                    }
                    Recv::Frame(frame) => {
                        StepOutcome::Fail(format!("unexpected frame during probe echo: {frame:?}"))
                    }
                    Recv::TerminalError(error) => {
                        StepOutcome::Fail(format!("error during probe echo: {error}"))
                    }
                    Recv::Ended => StepOutcome::Fail("peer closed during probe echo".into()),
                }
            }
            AbortExtendedPhase::AbortAndVerify => {
                // Required, prompt, nonblocking, idempotent: the second call
                // must be a no-op rather than a panic or repeated cleanup.
                transport.abort();
                transport.abort();
                if self.slot.is_none() {
                    self.slot = Some(TransportFrame::Text("post-abort".into()));
                }
                match transport.poll_send(&mut ctx(), &mut self.slot) {
                    Poll::Ready(Err(SignalFishError::TransportClosed)) => {
                        if self.slot.is_none() {
                            return StepOutcome::Fail(
                                "post-abort refusal destroyed the caller-owned frame".into(),
                            );
                        }
                    }
                    Poll::Ready(Err(error)) => {
                        return StepOutcome::Fail(format!(
                            "expected TransportClosed after abort, got: {error}"
                        ))
                    }
                    Poll::Ready(Ok(())) => {
                        return StepOutcome::Fail("poll_send accepted a frame after abort".into())
                    }
                    Poll::Pending => {
                        return StepOutcome::Fail("poll_send deferred after abort".into())
                    }
                }
                match poll_once(transport) {
                    Recv::Ended => {}
                    Recv::Pending => {
                        return StepOutcome::Fail("the receive stream must end after abort".into())
                    }
                    Recv::Frame(frame) => {
                        return StepOutcome::Fail(format!(
                            "unexpected frame after abort: {frame:?}"
                        ))
                    }
                    Recv::TerminalError(error) => {
                        return StepOutcome::Fail(format!("unexpected error after abort: {error}"))
                    }
                }
                // Abort completed the cleanup; a later poll_close stays safe.
                match transport.poll_close(&mut ctx()) {
                    Poll::Ready(Ok(())) => StepOutcome::Pass,
                    Poll::Ready(Err(error)) => {
                        StepOutcome::Fail(format!("poll_close after abort failed: {error}"))
                    }
                    Poll::Pending => {
                        StepOutcome::Fail("poll_close deferred after a completed abort".into())
                    }
                }
            }
        }
    }
}

enum Scenario {
    Roundtrip(RoundtripScenario),
    SendAfterClose(SendAfterCloseScenario),
    LedgerBound(LedgerBoundScenario),
    AbruptError(AbruptErrorScenario),
    LedgerDrain(LedgerDrainScenario),
    PreOpenFailure(PreOpenFailureScenario),
    AbortExtended(AbortExtendedScenario),
}

// ── Exported entry points ───────────────────────────────────────────────────

/// Construct the transport for `mode` and begin the scenario.
///
/// Returns [`RESULT_FAIL`] (with the reason readable through
/// `sfh_fail_reason`) when setup fails, otherwise [`RESULT_RUNNING`].
// The `url` pointer is only dereferenced through `CStr::from_ptr`, whose
// validity the caller contract guarantees (a NUL-terminated string allocated
// by the driver's `ccall`, valid for this call); clippy cannot see that
// cross-language guarantee.
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn sfh_begin(url: *const c_char, mode: i32) -> i32 {
    // Fresh attribution: an error latched by dropping the previous scenario's
    // transport must fail the next scheduling step, not the previous scenario.
    reset_error_latch();

    // SAFETY: the driver passes a NUL-terminated string allocated by the
    // Emscripten runtime (via `ccall`), valid for this call.
    let url = unsafe { std::ffi::CStr::from_ptr(url) };
    let url = match url.to_str() {
        Ok(url) => url.to_owned(),
        Err(error) => return begin_fail(format!("url was not valid UTF-8: {error}")),
    };

    let options = match mode {
        // The bounded scenarios share the 160-byte bound: two 64-unit charges
        // fit, a third would fuse (mode 2) or require released drain credit
        // (mode 4).
        2 | 4 => EmscriptenWebSocketConnectOptions::new()
            .with_max_inbound_queue_bytes(Some(LEDGER_BOUND_BYTES)),
        _ => EmscriptenWebSocketConnectOptions::new(),
    };

    let transport = match EmscriptenWebSocketTransport::connect_with_options(&url, options) {
        Ok(transport) => transport,
        Err(error) => return begin_fail(format!("connect_with_options failed: {error}")),
    };

    let scenario = match mode {
        0 => Scenario::Roundtrip(RoundtripScenario::new()),
        1 => Scenario::SendAfterClose(SendAfterCloseScenario {
            phase: SendAfterClosePhase::Connecting,
            slot: None,
        }),
        2 => Scenario::LedgerBound(LedgerBoundScenario {
            phase: LedgerPhase::Draining,
            drained_frames: 0,
        }),
        3 => Scenario::AbruptError(AbruptErrorScenario {
            phase: AbruptPhase::Connecting,
        }),
        4 => Scenario::LedgerDrain(LedgerDrainScenario {
            phase: LedgerDrainPhase::Draining,
            drained_frames: 0,
            slot: None,
            sent_next: false,
        }),
        5 => Scenario::PreOpenFailure(PreOpenFailureScenario {
            phase: PreOpenFailurePhase::Connecting,
            slot: None,
            pre_open_pending_polls: 0,
        }),
        6 => Scenario::AbortExtended(AbortExtendedScenario {
            phase: AbortExtendedPhase::ExpectServerFrames,
            slot: None,
            probe: extended_probe(LARGE_PROBE_LEN, 7),
            server_text_seen: false,
            server_binary_seen: false,
            probe_sent: false,
        }),
        other => return begin_fail(format!("unknown scenario mode {other}")),
    };

    store_harness(Harness {
        transport: Some(transport),
        scenario,
        fail_reason: None,
        steps: 0,
    });
    RESULT_RUNNING
}

/// Store a fresh harness state, dropping any previous one so the prior
/// scenario's transport runs its `Drop` (close → delete → callback-state
/// reclaim) instead of leaking a registered socket.
fn store_harness(harness: Harness) {
    // SAFETY: main-thread-only access; the previous value, if any, was fully
    // observed via sfh_step/sfh_fail_reason before the driver starts the
    // next scenario.
    let previous = unsafe { std::ptr::replace(ptr::addr_of_mut!(HARNESS), Some(harness)) };
    drop(previous);
}

fn begin_fail(reason: String) -> i32 {
    store_harness(Harness {
        transport: None,
        scenario: Scenario::Roundtrip(RoundtripScenario {
            phase: RoundtripPhase::Connecting,
            pending_sends: Vec::new(),
            expected: Vec::new(),
            slot: None,
            pre_open_pending_polls: 0,
        }),
        fail_reason: Some(cstring(&reason)),
        steps: 0,
    });
    RESULT_FAIL
}

/// Drive one scheduling step. Returns 0 running, 1 pass, 2 fail.
#[no_mangle]
pub extern "C" fn sfh_step() -> i32 {
    // SAFETY: see the `HARNESS` static comment.
    let harness = unsafe { &mut *ptr::addr_of_mut!(HARNESS) };
    match harness {
        Some(harness) => harness.step(),
        None => RESULT_FAIL,
    }
}

/// NUL-terminated failure reason, valid until the next `sfh_begin`.
#[no_mangle]
pub extern "C" fn sfh_fail_reason() -> *const c_char {
    const NO_FAILURE: &[u8] = b"no failure recorded\0";
    // SAFETY: see the `HARNESS` static comment.
    let harness = unsafe { &*ptr::addr_of_mut!(HARNESS) };
    harness
        .as_ref()
        .and_then(|harness| harness.fail_reason.as_ref())
        .map_or(NO_FAILURE.as_ptr().cast(), |reason| reason.as_ptr())
}

/// Tear the harness down: drop the last transport explicitly so its `Drop`
/// cleanup runs while the error latch can still observe it (the driver stops
/// stepping once the final scenario passes, so `store_harness` would never
/// reclaim it).
///
/// Returns [`RESULT_FAIL`] when a `tracing::error!` fired during that drop —
/// the transport's intentional-leak signal — otherwise [`RESULT_PASS`].
#[no_mangle]
pub extern "C" fn sfh_finish() -> i32 {
    reset_error_latch();
    // SAFETY: see the `HARNESS` static comment; the driver calls this exactly
    // once, after every scenario completed.
    let harness = unsafe { std::ptr::replace(ptr::addr_of_mut!(HARNESS), None) };
    drop(harness);
    if error_latched() {
        RESULT_FAIL
    } else {
        RESULT_PASS
    }
}

fn main() {
    // Install the error latch before any scenario can run: `main` executes
    // during module initialization, before the driver's first `sfh_begin`.
    if tracing::subscriber::set_global_default(Arc::clone(error_latch())).is_err() {
        // A second install would mean a prior subscriber exists; the latch
        // contract only holds under our own subscriber, so fail loudly.
        eprintln!("harness FAIL: could not install the error-latch subscriber");
        std::process::exit(1);
    }
}
