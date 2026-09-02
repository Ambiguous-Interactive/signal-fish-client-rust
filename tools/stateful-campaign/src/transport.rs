//! Contract-faithful scripted in-memory `Transport` for the polling driver.
//!
//! The transport exercises the documented polling `Transport` contract:
//! immediate ownership transfer on `poll_send`, `Pending` with an empty queue,
//! idempotent nonblocking close, idempotent prompt abort, fused post-abort and
//! post-close polling, and structured peer-close metadata. A configurable
//! send-delay face returns `Pending` for the first N offers of each frame so
//! the send-side budget/pacing interactions are exercised (issue #219).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::task::{Context, Poll};

use signal_fish_client::error::SignalFishError;
use signal_fish_client::transport::{Transport, TransportFrame};

/// Shared inbound queue the harness pushes scripted server frames into.
pub type InboundQueue = Arc<StdMutex<VecDeque<TransportFrame>>>;

/// Number of `poll_send` offers each frame is refused with `Pending` before
/// acceptance, plus per-frame bookkeeping. Zero means immediate acceptance.
#[derive(Default)]
struct SendDelay {
    remaining: usize,
    /// Offers observed for the frame currently at the front of the driver's
    /// retained-send slot (`usize::MAX` once it has been accepted).
    current_frame_offers: usize,
}

pub struct ScriptedTransport {
    inbound: InboundQueue,
    outbound: Arc<StdMutex<Vec<String>>>,
    closed: Arc<AtomicBool>,
    aborted: Arc<AtomicBool>,
    pub close_calls: Arc<AtomicUsize>,
    pub abort_calls: Arc<AtomicUsize>,
    pub begin_cycles: Arc<AtomicUsize>,
    /// Server-initiated terminal face: poll_recv returns Ready(None) once set.
    pub peer_closed: Arc<AtomicBool>,
    /// Terminal receive-error face: poll_recv returns Ready(Some(Err(..))) once set.
    pub fail_recv_error: Arc<AtomicBool>,
    /// Terminal send-error face: poll_send returns Ready(Err(..)) once set.
    pub fail_send_error: Arc<AtomicBool>,
    /// Pending-refusal face for `poll_send` (send-side pacing).
    send_delay: Arc<StdMutex<SendDelay>>,
    /// Number of `Pending` refusals `poll_send` has returned in total.
    pub send_pending_refusals: Arc<AtomicUsize>,
}

pub struct ScriptedHandles {
    pub inbound: InboundQueue,
    pub outbound: Arc<StdMutex<Vec<String>>>,
    pub closed: Arc<AtomicBool>,
    pub close_calls: Arc<AtomicUsize>,
    pub abort_calls: Arc<AtomicUsize>,
    pub begin_cycles: Arc<AtomicUsize>,
    /// Server-initiated terminal face: poll_recv returns Ready(None) once set.
    pub peer_closed: Arc<AtomicBool>,
    /// Terminal receive-error face: poll_recv returns Ready(Some(Err(..))) once set.
    pub fail_recv_error: Arc<AtomicBool>,
    /// Terminal send-error face: poll_send returns Ready(Err(..)) once set.
    pub fail_send_error: Arc<AtomicBool>,
    pub send_pending_refusals: Arc<AtomicUsize>,
    /// Shared send-delay state (armed through the handles because the
    /// transport itself is owned by the client under test).
    send_delay: Arc<StdMutex<SendDelay>>,
}

/// Exact `Display` strings of the scripted terminal errors. The client copies
/// the terminal error's `to_string()` into the `Disconnected` reason, so the
/// oracle asserts against these constants verbatim.
pub const RECV_ERROR_DISPLAY: &str = "transport receive error: scripted terminal receive failure";
pub const SEND_ERROR_DISPLAY: &str = "transport send error: scripted terminal send failure";

/// Poison-resume lock helper: a poisoned lock can only mean a previous finding
/// (a panicking transport method is a contract violation the harness wants to
/// report), so resume instead of panicking again.
pub fn lock<'a, T>(mutex: &'a StdMutex<T>) -> std::sync::MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

impl ScriptedTransport {
    pub fn new() -> (Self, ScriptedHandles) {
        let inbound: InboundQueue = Arc::new(StdMutex::new(VecDeque::new()));
        let outbound = Arc::new(StdMutex::new(Vec::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let aborted = Arc::new(AtomicBool::new(false));
        let close_calls = Arc::new(AtomicUsize::new(0));
        let abort_calls = Arc::new(AtomicUsize::new(0));
        let begin_cycles = Arc::new(AtomicUsize::new(0));
        let peer_closed = Arc::new(AtomicBool::new(false));
        let fail_recv_error = Arc::new(AtomicBool::new(false));
        let fail_send_error = Arc::new(AtomicBool::new(false));
        let send_delay = Arc::new(StdMutex::new(SendDelay::default()));
        let send_pending_refusals = Arc::new(AtomicUsize::new(0));
        (
            Self {
                inbound: Arc::clone(&inbound),
                outbound: Arc::clone(&outbound),
                closed: Arc::clone(&closed),
                aborted: Arc::clone(&aborted),
                close_calls: Arc::clone(&close_calls),
                abort_calls: Arc::clone(&abort_calls),
                begin_cycles: Arc::clone(&begin_cycles),
                peer_closed: Arc::clone(&peer_closed),
                fail_recv_error: Arc::clone(&fail_recv_error),
                fail_send_error: Arc::clone(&fail_send_error),
                send_delay: Arc::clone(&send_delay),
                send_pending_refusals: Arc::clone(&send_pending_refusals),
            },
            ScriptedHandles {
                inbound,
                outbound,
                closed,
                close_calls,
                abort_calls,
                begin_cycles,
                peer_closed,
                fail_recv_error,
                fail_send_error,
                send_pending_refusals,
                send_delay: Arc::clone(&send_delay),
            },
        )
    }

    /// Arm the server-initiated close face (transport contract: capture
    /// structured close metadata before returning Ready(None)).
    pub fn arm_peer_close(handles: &ScriptedHandles) {
        handles.peer_closed.store(true, Ordering::Relaxed);
    }

    /// Arm the terminal receive-error face (contract: poll_recv ->
    /// Ready(Some(Err(error))) is outbound-terminal for the client).
    pub fn arm_recv_error(handles: &ScriptedHandles) {
        handles.fail_recv_error.store(true, Ordering::Relaxed);
    }

    /// Arm the terminal send-error face (contract: poll_send ->
    /// Ready(Err(error)) without taking the caller-owned frame).
    pub fn arm_send_error(handles: &ScriptedHandles) {
        handles.fail_send_error.store(true, Ordering::Relaxed);
    }

    /// Refuse each frame's first `delay` offers with `Pending`. A smaller
    /// value replaces a larger one (including back to zero).
    pub fn set_send_delay(handles: &ScriptedHandles, delay: usize) {
        let mut state = lock(&handles.send_delay);
        state.remaining = delay;
    }

    pub fn push_text(handles: &ScriptedHandles, json: String) {
        lock(&handles.inbound).push_back(TransportFrame::Text(json));
    }

    pub fn push_binary(handles: &ScriptedHandles, bytes: Vec<u8>) {
        lock(&handles.inbound).push_back(TransportFrame::Binary(bytes));
    }

    pub fn outbound_len(handles: &ScriptedHandles) -> usize {
        lock(&handles.outbound).len()
    }
}

impl Transport for ScriptedTransport {
    fn poll_send(
        &mut self,
        _cx: &mut Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> Poll<Result<(), SignalFishError>> {
        if self.aborted.load(Ordering::Relaxed) || self.closed.load(Ordering::Relaxed) {
            // Fused post-abort/post-close terminal convenience (contract-legal).
            return Poll::Ready(Err(SignalFishError::TransportClosed));
        }
        if self.fail_send_error.load(Ordering::Relaxed) {
            // Contract-faithful terminal send failure: the error is
            // outbound-terminal and must NOT take the caller-owned frame.
            return Poll::Ready(Err(SignalFishError::TransportSend(
                "scripted terminal send failure".into(),
            )));
        }
        if frame.is_none() {
            return Poll::Ready(Ok(()));
        }
        let delay = {
            let mut state = lock(&self.send_delay);
            if state.remaining > 0 && state.current_frame_offers < state.remaining {
                state.current_frame_offers = state.current_frame_offers.saturating_add(1);
                self.send_pending_refusals.fetch_add(1, Ordering::Relaxed);
                true
            } else {
                // Frame accepted: reset the per-frame offer counter so the
                // next frame receives its own delay budget.
                state.current_frame_offers = 0;
                false
            }
        };
        if delay {
            // Contract: Pending before acceptance leaves the frame intact.
            return Poll::Pending;
        }
        match frame.take() {
            Some(TransportFrame::Text(text)) => {
                lock(&self.outbound).push(text);
                Poll::Ready(Ok(()))
            }
            Some(TransportFrame::Binary(bytes)) => {
                lock(&self.outbound).push(format!("<binary {} bytes>", bytes.len()));
                Poll::Ready(Ok(()))
            }
            None => Poll::Ready(Ok(())),
        }
    }

    fn poll_recv(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<TransportFrame, SignalFishError>>> {
        if self.aborted.load(Ordering::Relaxed) || self.peer_closed.load(Ordering::Relaxed) {
            return Poll::Ready(None);
        }
        if self.fail_recv_error.load(Ordering::Relaxed) {
            // Contract-faithful terminal receive failure (fused, like the
            // built-in transports fusing EOF and terminal socket errors).
            return Poll::Ready(Some(Err(SignalFishError::TransportReceive(
                "scripted terminal receive failure".into(),
            ))));
        }
        match lock(&self.inbound).pop_front() {
            Some(frame) => Poll::Ready(Some(Ok(frame))),
            None => Poll::Pending,
        }
    }

    fn close_info(&self) -> Option<signal_fish_client::transport::TransportCloseInfo> {
        if self.peer_closed.load(Ordering::Relaxed) {
            Some(signal_fish_client::transport::TransportCloseInfo {
                code: Some(1001),
                reason: Some("server going away".into()),
                clean: Some(true),
                initiated_by_peer: true,
            })
        } else {
            None
        }
    }

    fn poll_close(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), SignalFishError>> {
        self.close_calls.fetch_add(1, Ordering::Relaxed);
        if self.aborted.load(Ordering::Relaxed) {
            return Poll::Ready(Ok(()));
        }
        self.closed.store(true, Ordering::Relaxed);
        Poll::Ready(Ok(()))
    }

    fn begin_poll_cycle(&mut self) {
        self.begin_cycles.fetch_add(1, Ordering::Relaxed);
    }

    fn abort(&mut self) {
        self.abort_calls.fetch_add(1, Ordering::Relaxed);
        lock(&self.inbound).clear();
        self.aborted.store(true, Ordering::Relaxed);
    }

    fn is_ready(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{ScriptedTransport, RECV_ERROR_DISPLAY, SEND_ERROR_DISPLAY};
    use signal_fish_client::Transport;
    use std::task::Poll;

    #[test]
    fn terminal_error_displays_are_stable() {
        // The oracle asserts the Disconnected reason against these constants;
        // the client copies `error.to_string()` verbatim.
        assert!(SEND_ERROR_DISPLAY.starts_with("transport send error:"));
        assert!(RECV_ERROR_DISPLAY.starts_with("transport receive error:"));
    }

    #[test]
    fn send_delay_refuses_then_accepts_per_frame() {
        let (mut transport, handles) = ScriptedTransport::new();
        ScriptedTransport::set_send_delay(&handles, 2);
        let mut frame = Some(signal_fish_client::transport::TransportFrame::Text(
            "a".into(),
        ));
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        for _ in 0..2 {
            assert!(matches!(
                transport.poll_send(&mut cx, &mut frame),
                Poll::Pending
            ));
            assert!(frame.is_some(), "Pending must leave the frame intact");
        }
        assert!(matches!(
            transport.poll_send(&mut cx, &mut frame),
            Poll::Ready(Ok(()))
        ));
        assert!(frame.is_none(), "acceptance takes ownership");
        // The next frame gets its own delay budget.
        let mut second = Some(signal_fish_client::transport::TransportFrame::Text(
            "b".into(),
        ));
        assert!(matches!(
            transport.poll_send(&mut cx, &mut second),
            Poll::Pending
        ));
        assert!(second.is_some());
    }
}
