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
use std::task::{Context, Poll, Waker};

const RESULT_RUNNING: i32 = 0;
const RESULT_PASS: i32 = 1;
const RESULT_FAIL: i32 = 2;

/// Ledger bound for the `ledger-bound` scenario: two 50-byte frames charge
/// 64 units each (the 64-byte minimum charge); the third 64-unit charge
/// would exceed 160 and fuse.
const LEDGER_BOUND_BYTES: usize = 160;
const LEDGER_FLOOD_FRAME_LEN: usize = 50;

/// Upper bound on scheduling steps for one scenario so a wedged callback
/// bridge fails the harness instead of hanging CI.
const MAX_STEPS: u32 = 4096;

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
        let Some(transport) = self.transport.as_mut() else {
            return self.fail("step called before sfh_begin".into());
        };
        transport.begin_poll_cycle();
        let outcome = match &mut self.scenario {
            Scenario::Roundtrip(scenario) => scenario.step(transport),
            Scenario::SendAfterClose(scenario) => scenario.step(transport),
            Scenario::LedgerBound(scenario) => scenario.step(transport),
            Scenario::AbruptError(scenario) => scenario.step(transport),
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

enum Scenario {
    Roundtrip(RoundtripScenario),
    SendAfterClose(SendAfterCloseScenario),
    LedgerBound(LedgerBoundScenario),
    AbruptError(AbruptErrorScenario),
}

// ── Exported entry points ───────────────────────────────────────────────────

/// Construct the transport for `mode` and begin the scenario.
///
/// Returns [`RESULT_FAIL`] (with the reason readable through
/// `sfh_fail_reason`) when setup fails, otherwise [`RESULT_RUNNING`].
#[no_mangle]
pub extern "C" fn sfh_begin(url: *const c_char, mode: i32) -> i32 {
    // SAFETY: the driver passes a NUL-terminated string allocated by the
    // Emscripten runtime (via `ccall`), valid for this call.
    let url = unsafe { std::ffi::CStr::from_ptr(url) };
    let url = match url.to_str() {
        Ok(url) => url.to_owned(),
        Err(error) => return begin_fail(format!("url was not valid UTF-8: {error}")),
    };

    let options = if mode == 2 {
        EmscriptenWebSocketConnectOptions::new()
            .with_max_inbound_queue_bytes(Some(LEDGER_BOUND_BYTES))
    } else {
        EmscriptenWebSocketConnectOptions::new()
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

fn main() {}
