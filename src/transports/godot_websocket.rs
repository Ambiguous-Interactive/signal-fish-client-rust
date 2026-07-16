//! Godot 4.5 `WebSocketPeer` transport for native and web exports.
//!
//! Godot owns the platform WebSocket implementation. This transport advances
//! it from [`Transport::poll_send`], [`Transport::poll_recv`], and
//! [`Transport::poll_close`], making it suitable for
//! [`SignalFishPollingClient`](crate::SignalFishPollingClient) in a Node's
//! `_process` callback. It contains no GDScript or Emscripten WebSocket FFI.

use std::fmt;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use godot::builtin::PackedByteArray;
use godot::classes::{web_socket_peer, WebSocketPeer};
use godot::global::Error;
use godot::obj::{Gd, NewGd};

use crate::error::SignalFishError;
use crate::transport::{Transport, TransportCloseInfo, TransportDiagnostics, TransportFrame};

const DEFAULT_ADAPTIVE_FLOOR: usize = 4 * 1024;
const DEFAULT_ADAPTIVE_CEILING: usize = 32 * 1024;
const DEFAULT_ADAPTIVE_LATENCY: Duration = Duration::from_millis(50);

/// Godot outbound admission strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GodotBackpressurePolicy {
    /// Refuse a frame when it would exceed a fixed buffered-byte watermark.
    Fixed {
        /// Maximum normally admitted backend-buffered payload bytes.
        high_water_mark_bytes: usize,
    },
    /// Adapt the watermark to the observed accepted burst and drain rate.
    Adaptive {
        /// Target time for draining backend-owned buffered bytes.
        latency_target: Duration,
        /// Minimum adaptive watermark.
        floor_bytes: usize,
        /// Maximum adaptive watermark.
        ceiling_bytes: usize,
    },
    /// Use only Godot's native outbound capacity boundary.
    NativeCapacity,
}

impl GodotBackpressurePolicy {
    /// Construct the recommended adaptive policy (50 ms, 4 KiB–32 KiB).
    #[must_use]
    pub const fn adaptive() -> Self {
        Self::Adaptive {
            latency_target: DEFAULT_ADAPTIVE_LATENCY,
            floor_bytes: DEFAULT_ADAPTIVE_FLOOR,
            ceiling_bytes: DEFAULT_ADAPTIVE_CEILING,
        }
    }
}

impl Default for GodotBackpressurePolicy {
    fn default() -> Self {
        Self::adaptive()
    }
}

/// Construction options for [`GodotWebSocketTransport`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GodotWebSocketOptions {
    /// Outbound buffering policy. Defaults to the recommended adaptive policy.
    pub backpressure_policy: GodotBackpressurePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeerState {
    Connecting,
    Open,
    Closing,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BackendSendResult {
    Accepted,
    Capacity,
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeCapacityBoundary {
    /// Godot web rejects `buffered + next >= capacity`.
    GreaterThanOrEqual,
    /// Godot native rejects `buffered + next > capacity`.
    GreaterThan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdmissionDecision {
    Admit,
    NativeCapacity,
    Watermark,
}

fn admission_decision(
    current: usize,
    next: usize,
    native_capacity: usize,
    boundary: NativeCapacityBoundary,
    watermark: usize,
) -> AdmissionDecision {
    let Some(total) = current.checked_add(next) else {
        return AdmissionDecision::NativeCapacity;
    };
    if native_capacity != 0
        && match boundary {
            NativeCapacityBoundary::GreaterThanOrEqual => total >= native_capacity,
            NativeCapacityBoundary::GreaterThan => total > native_capacity,
        }
    {
        return AdmissionDecision::NativeCapacity;
    }
    if current != 0 && total > watermark {
        return AdmissionDecision::Watermark;
    }
    AdmissionDecision::Admit
}

trait GodotWebSocketBackend {
    fn poll(&mut self);
    fn state(&self) -> PeerState;
    fn outbound_buffered_amount(&self) -> i32;
    fn outbound_capacity(&self) -> i32;
    fn capacity_boundary(&self) -> NativeCapacityBoundary;
    fn send_text(&mut self, text: &str) -> BackendSendResult;
    fn send_binary(&mut self, bytes: &[u8]) -> BackendSendResult;
    fn available_packet_count(&self) -> i32;
    fn receive_packet(&mut self) -> Result<(Vec<u8>, bool), String>;
    fn close(&mut self);
    fn abort(&mut self);
    fn close_code(&self) -> i32;
    fn close_reason(&self) -> String;
}

impl GodotWebSocketBackend for Gd<WebSocketPeer> {
    fn poll(&mut self) {
        std::ops::DerefMut::deref_mut(self).poll();
    }

    fn state(&self) -> PeerState {
        match std::ops::Deref::deref(self).get_ready_state() {
            web_socket_peer::State::CONNECTING => PeerState::Connecting,
            web_socket_peer::State::OPEN => PeerState::Open,
            web_socket_peer::State::CLOSING => PeerState::Closing,
            web_socket_peer::State::CLOSED => PeerState::Closed,
            state => {
                tracing::warn!(?state, "Godot returned an unknown WebSocketPeer state");
                PeerState::Closed
            }
        }
    }

    fn outbound_buffered_amount(&self) -> i32 {
        std::ops::Deref::deref(self).get_current_outbound_buffered_amount()
    }

    fn outbound_capacity(&self) -> i32 {
        std::ops::Deref::deref(self).get_outbound_buffer_size()
    }

    fn capacity_boundary(&self) -> NativeCapacityBoundary {
        if cfg!(target_os = "emscripten") {
            NativeCapacityBoundary::GreaterThanOrEqual
        } else {
            NativeCapacityBoundary::GreaterThan
        }
    }

    fn send_text(&mut self, text: &str) -> BackendSendResult {
        let result = std::ops::DerefMut::deref_mut(self).send_text(text);
        godot_send_result(result, "send_text")
    }

    fn send_binary(&mut self, bytes: &[u8]) -> BackendSendResult {
        let result = std::ops::DerefMut::deref_mut(self)
            .send_ex(&PackedByteArray::from(bytes.to_vec()))
            .write_mode(web_socket_peer::WriteMode::BINARY)
            .done();
        godot_send_result(result, "send binary")
    }

    fn available_packet_count(&self) -> i32 {
        std::ops::Deref::deref(self).get_available_packet_count()
    }

    fn receive_packet(&mut self) -> Result<(Vec<u8>, bool), String> {
        let packet = std::ops::DerefMut::deref_mut(self).get_packet();
        let result = std::ops::Deref::deref(self).get_packet_error();
        if result != Error::OK {
            return Err(format!("get_packet failed with Godot error {result:?}"));
        }
        let is_text = std::ops::Deref::deref(self).was_string_packet();
        Ok((packet.to_vec(), is_text))
    }

    fn close(&mut self) {
        std::ops::DerefMut::deref_mut(self).close();
    }

    fn abort(&mut self) {
        std::ops::DerefMut::deref_mut(self)
            .close_ex()
            .code(-1)
            .done();
    }

    fn close_code(&self) -> i32 {
        std::ops::Deref::deref(self).get_close_code()
    }

    fn close_reason(&self) -> String {
        std::ops::Deref::deref(self).get_close_reason().to_string()
    }
}

fn godot_send_result(result: Error, operation: &str) -> BackendSendResult {
    if result == Error::OK {
        BackendSendResult::Accepted
    } else if result == Error::ERR_OUT_OF_MEMORY {
        BackendSendResult::Capacity
    } else {
        BackendSendResult::Error(format!(
            "Godot WebSocketPeer {operation} failed with {result:?}"
        ))
    }
}

/// A main-thread [`Transport`] backed by Godot 4.5's `WebSocketPeer`.
///
/// Enable the `transport-godot` feature and drive this transport through
/// [`SignalFishPollingClient`](crate::SignalFishPollingClient). The contained
/// Godot object is intentionally not required to be `Send`; call the polling
/// client's `poll()` method from the same Godot thread on every frame.
pub struct GodotWebSocketTransport {
    backend: Box<dyn GodotWebSocketBackend>,
    options: GodotWebSocketOptions,
    diagnostics: TransportDiagnostics,
    adaptive: AdaptiveState,
    ever_ready: bool,
    close_deferred_to_receive: bool,
    close_started: bool,
    terminal: bool,
    close_info: Option<TransportCloseInfo>,
}

#[derive(Debug, Default)]
struct AdaptiveState {
    last_sample: Option<Instant>,
    previous_buffered: usize,
    accepted_since_sample: usize,
    accepted_burst_ewma: u64,
    drain_bytes_per_second_ewma: u64,
}

impl fmt::Debug for GodotWebSocketTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GodotWebSocketTransport")
            .field("options", &self.options)
            .field("diagnostics", &self.diagnostics)
            .field("ever_ready", &self.ever_ready)
            .field("close_deferred_to_receive", &self.close_deferred_to_receive)
            .field("close_started", &self.close_started)
            .field("terminal", &self.terminal)
            .field("close_info", &self.close_info)
            .finish_non_exhaustive()
    }
}

impl GodotWebSocketTransport {
    /// Create a Godot `WebSocketPeer` and begin a non-blocking connection.
    ///
    /// The connection handshake advances when the transport is polled. For web
    /// exports, use `wss://` when the exported page is served over HTTPS.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::Io`] when Godot rejects the URL or cannot
    /// start the connection attempt.
    pub fn connect(url: &str) -> Result<Self, SignalFishError> {
        Self::connect_with_options(url, GodotWebSocketOptions::default())
    }

    /// Create a Godot `WebSocketPeer` with explicit backpressure options.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::Io`] when Godot rejects the URL or cannot
    /// start the connection attempt.
    pub fn connect_with_options(
        url: &str,
        options: GodotWebSocketOptions,
    ) -> Result<Self, SignalFishError> {
        let mut peer = WebSocketPeer::new_gd();
        let result = peer.connect_to_url(url);
        if result != Error::OK {
            return Err(SignalFishError::Io(std::io::Error::other(format!(
                "Godot WebSocketPeer connect_to_url failed with {result:?}"
            ))));
        }
        Ok(Self::from_peer_with_options(peer, options))
    }

    /// Wrap a Godot `WebSocketPeer` whose connection attempt has already begun.
    ///
    /// This supports callers that need to configure handshake headers,
    /// subprotocols, buffer sizes, or TLS options before `connect_to_url`.
    pub fn from_peer(peer: Gd<WebSocketPeer>) -> Self {
        Self::from_peer_with_options(peer, GodotWebSocketOptions::default())
    }

    /// Wrap a connected/configured peer with explicit backpressure options.
    pub fn from_peer_with_options(peer: Gd<WebSocketPeer>, options: GodotWebSocketOptions) -> Self {
        Self::from_backend_with_options(Box::new(peer), options)
    }

    #[cfg(test)]
    fn from_backend(backend: Box<dyn GodotWebSocketBackend>) -> Self {
        Self::from_backend_with_options(backend, GodotWebSocketOptions::default())
    }

    fn from_backend_with_options(
        backend: Box<dyn GodotWebSocketBackend>,
        options: GodotWebSocketOptions,
    ) -> Self {
        let ever_ready = backend.state() == PeerState::Open;
        let mut transport = Self {
            backend,
            options,
            diagnostics: TransportDiagnostics::default(),
            adaptive: AdaptiveState::default(),
            ever_ready,
            close_deferred_to_receive: false,
            close_started: false,
            terminal: false,
            close_info: None,
        };
        transport.sample_cycle_at(Instant::now());
        transport
    }

    fn advance(&mut self) -> PeerState {
        if !self.terminal {
            self.backend.poll();
        }
        let state = self.backend.state();
        if state == PeerState::Open {
            self.ever_ready = true;
        }
        state
    }

    fn record_close(&mut self) {
        self.diagnostics.current_buffered_bytes =
            u64::try_from(self.buffered_bytes()).unwrap_or(u64::MAX);
        if self.close_info.is_some() {
            return;
        }
        let raw_code = self.backend.close_code();
        let clean = raw_code != -1;
        let code = u16::try_from(raw_code).ok();
        let reason = self.backend.close_reason();
        self.close_info = Some(TransportCloseInfo {
            code,
            reason: (!reason.is_empty()).then_some(reason),
            clean: Some(clean),
            initiated_by_peer: !self.close_started,
        });
    }

    fn closed_receive(&mut self) -> Poll<Option<Result<TransportFrame, SignalFishError>>> {
        self.record_close();
        self.terminal = true;
        if self.ever_ready || self.close_started {
            Poll::Ready(None)
        } else {
            Poll::Ready(Some(Err(SignalFishError::TransportReceive(
                "Godot WebSocket connection closed before opening".to_string(),
            ))))
        }
    }

    fn closed_send_error(&self) -> SignalFishError {
        if self.ever_ready || self.close_started {
            SignalFishError::TransportClosed
        } else {
            SignalFishError::TransportReceive(
                "Godot WebSocket connection closed before opening".to_string(),
            )
        }
    }

    fn buffered_bytes(&self) -> usize {
        usize::try_from(self.backend.outbound_buffered_amount()).unwrap_or(0)
    }

    fn native_capacity(&self) -> usize {
        usize::try_from(self.backend.outbound_capacity()).unwrap_or(0)
    }

    fn safe_native_watermark(&self) -> usize {
        let capacity = self.native_capacity();
        if capacity == 0 {
            return usize::MAX;
        }
        match self.backend.capacity_boundary() {
            NativeCapacityBoundary::GreaterThanOrEqual => capacity.saturating_sub(1),
            NativeCapacityBoundary::GreaterThan => capacity,
        }
    }

    fn configured_watermark(&self) -> usize {
        let safe_native = self.safe_native_watermark();
        match self.options.backpressure_policy {
            GodotBackpressurePolicy::Fixed {
                high_water_mark_bytes,
            } => high_water_mark_bytes.min(safe_native),
            GodotBackpressurePolicy::Adaptive { .. } => {
                usize::try_from(self.diagnostics.effective_watermark_bytes)
                    .unwrap_or(usize::MAX)
                    .min(safe_native)
            }
            GodotBackpressurePolicy::NativeCapacity => safe_native,
        }
    }

    fn sample_cycle_at(&mut self, now: Instant) {
        let previous_effective = self.diagnostics.effective_watermark_bytes;
        let current = self.buffered_bytes();
        self.diagnostics.current_buffered_bytes = u64::try_from(current).unwrap_or(u64::MAX);
        self.diagnostics.peak_buffered_bytes = self
            .diagnostics
            .peak_buffered_bytes
            .max(self.diagnostics.current_buffered_bytes);

        let safe_native = self.safe_native_watermark();
        let effective = match self.options.backpressure_policy {
            GodotBackpressurePolicy::Fixed {
                high_water_mark_bytes,
            } => high_water_mark_bytes.min(safe_native),
            GodotBackpressurePolicy::NativeCapacity => safe_native,
            GodotBackpressurePolicy::Adaptive {
                latency_target,
                floor_bytes,
                ceiling_bytes,
            } => {
                if let Some(last_sample) = self.adaptive.last_sample {
                    let accepted =
                        u64::try_from(self.adaptive.accepted_since_sample).unwrap_or(u64::MAX);
                    self.adaptive.accepted_burst_ewma =
                        ewma_one_eighth(self.adaptive.accepted_burst_ewma, accepted);

                    let available = self
                        .adaptive
                        .previous_buffered
                        .saturating_add(self.adaptive.accepted_since_sample);
                    let drained = available.saturating_sub(current);
                    let elapsed_nanos = now.saturating_duration_since(last_sample).as_nanos();
                    if elapsed_nanos > 0 {
                        let rate = (drained as u128)
                            .saturating_mul(1_000_000_000)
                            .checked_div(elapsed_nanos)
                            .unwrap_or(0)
                            .min(u128::from(u64::MAX)) as u64;
                        self.adaptive.drain_bytes_per_second_ewma =
                            ewma_one_eighth(self.adaptive.drain_bytes_per_second_ewma, rate);
                    }
                }

                let latency_bytes = u128::from(self.adaptive.drain_bytes_per_second_ewma)
                    .saturating_mul(latency_target.as_nanos())
                    .checked_div(1_000_000_000)
                    .unwrap_or(0)
                    .min(u128::from(u64::MAX)) as u64;
                let desired = self.adaptive.accepted_burst_ewma.max(latency_bytes);
                let low = floor_bytes.min(ceiling_bytes);
                let high = ceiling_bytes;
                usize::try_from(desired)
                    .unwrap_or(usize::MAX)
                    .clamp(low, high)
                    .min(safe_native)
            }
        };
        self.diagnostics.effective_watermark_bytes = u64::try_from(effective).unwrap_or(u64::MAX);
        if matches!(
            self.options.backpressure_policy,
            GodotBackpressurePolicy::Adaptive { .. }
        ) && previous_effective != self.diagnostics.effective_watermark_bytes
        {
            tracing::debug!(
                previous_watermark_bytes = previous_effective,
                effective_watermark_bytes = self.diagnostics.effective_watermark_bytes,
                accepted_burst_ewma_bytes = self.adaptive.accepted_burst_ewma,
                drain_bytes_per_second_ewma = self.adaptive.drain_bytes_per_second_ewma,
                "Godot adaptive outbound watermark changed"
            );
        }
        self.adaptive.last_sample = Some(now);
        self.adaptive.previous_buffered = current;
        self.adaptive.accepted_since_sample = 0;
    }
}

fn ewma_one_eighth(previous: u64, sample: u64) -> u64 {
    u64::try_from((u128::from(previous) * 7 + u128::from(sample)) / 8).unwrap_or(u64::MAX)
}

impl Transport for GodotWebSocketTransport {
    fn begin_poll_cycle(&mut self) {
        self.sample_cycle_at(Instant::now());
    }

    fn poll_send(
        &mut self,
        _cx: &mut Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> Poll<Result<(), SignalFishError>> {
        if self.terminal {
            return Poll::Ready(Err(self.closed_send_error()));
        }
        match self.advance() {
            PeerState::Connecting => return Poll::Pending,
            PeerState::Closing => return Poll::Pending,
            PeerState::Closed => {
                self.record_close();
                if !self.close_deferred_to_receive {
                    self.close_deferred_to_receive = true;
                    return Poll::Pending;
                }
                self.terminal = true;
                return Poll::Ready(Err(self.closed_send_error()));
            }
            PeerState::Open => {}
        }

        let Some(next_frame) = frame.as_ref() else {
            return Poll::Ready(Ok(()));
        };
        let next_bytes = match next_frame {
            TransportFrame::Text(text) => text.len(),
            TransportFrame::Binary(bytes) => bytes.len(),
        };
        let current = self.buffered_bytes();
        self.diagnostics.current_buffered_bytes = u64::try_from(current).unwrap_or(u64::MAX);
        self.diagnostics.peak_buffered_bytes = self
            .diagnostics
            .peak_buffered_bytes
            .max(self.diagnostics.current_buffered_bytes);
        match admission_decision(
            current,
            next_bytes,
            self.native_capacity(),
            self.backend.capacity_boundary(),
            self.configured_watermark(),
        ) {
            AdmissionDecision::Admit => {}
            AdmissionDecision::NativeCapacity => {
                self.diagnostics.backend_capacity_hits =
                    self.diagnostics.backend_capacity_hits.saturating_add(1);
                return Poll::Pending;
            }
            AdmissionDecision::Watermark => {
                self.diagnostics.watermark_hits = self.diagnostics.watermark_hits.saturating_add(1);
                return Poll::Pending;
            }
        }

        let result = match next_frame {
            TransportFrame::Text(text) => self.backend.send_text(text),
            TransportFrame::Binary(bytes) => self.backend.send_binary(bytes),
        };
        match result {
            BackendSendResult::Accepted => {
                let _ = frame.take();
                self.adaptive.accepted_since_sample = self
                    .adaptive
                    .accepted_since_sample
                    .saturating_add(next_bytes);
                self.diagnostics.accepted_frames =
                    self.diagnostics.accepted_frames.saturating_add(1);
                self.diagnostics.accepted_bytes = self
                    .diagnostics
                    .accepted_bytes
                    .saturating_add(u64::try_from(next_bytes).unwrap_or(u64::MAX));
                let observed = self.buffered_bytes();
                self.diagnostics.current_buffered_bytes =
                    u64::try_from(observed).unwrap_or(u64::MAX);
                self.diagnostics.peak_buffered_bytes = self
                    .diagnostics
                    .peak_buffered_bytes
                    .max(self.diagnostics.current_buffered_bytes);
                Poll::Ready(Ok(()))
            }
            BackendSendResult::Capacity => {
                self.diagnostics.backend_capacity_hits =
                    self.diagnostics.backend_capacity_hits.saturating_add(1);
                Poll::Pending
            }
            BackendSendResult::Error(error) => {
                Poll::Ready(Err(SignalFishError::TransportSend(error)))
            }
        }
    }

    fn poll_recv(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<TransportFrame, SignalFishError>>> {
        if self.terminal {
            return Poll::Ready(None);
        }
        let state = self.advance();
        if state == PeerState::Connecting {
            return Poll::Pending;
        }

        if self.backend.available_packet_count() <= 0 {
            return if state == PeerState::Closed {
                self.closed_receive()
            } else {
                Poll::Pending
            };
        }
        let (bytes, is_text) = match self.backend.receive_packet() {
            Ok(packet) => packet,
            Err(error) => return Poll::Ready(Some(Err(SignalFishError::TransportReceive(error)))),
        };
        if is_text {
            match String::from_utf8(bytes) {
                Ok(text) => Poll::Ready(Some(Ok(TransportFrame::Text(text)))),
                Err(error) => Poll::Ready(Some(Err(SignalFishError::TransportReceive(format!(
                    "Godot WebSocket text packet was not valid UTF-8: {error}"
                ))))),
            }
        } else {
            Poll::Ready(Some(Ok(TransportFrame::Binary(bytes))))
        }
    }

    fn poll_close(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), SignalFishError>> {
        if self.terminal {
            return Poll::Ready(Ok(()));
        }

        let state = self.advance();
        if state == PeerState::Closed {
            if self.backend.available_packet_count() > 0 {
                return Poll::Pending;
            }
            self.record_close();
            self.terminal = true;
            return Poll::Ready(Ok(()));
        }
        if !self.close_started && state != PeerState::Closing {
            self.backend.close();
            self.close_started = true;
        }
        if self.advance() == PeerState::Closed {
            if self.backend.available_packet_count() > 0 {
                return Poll::Pending;
            }
            self.record_close();
            self.terminal = true;
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }

    fn is_ready(&self) -> bool {
        self.ever_ready
    }

    fn close_info(&self) -> Option<TransportCloseInfo> {
        self.close_info.clone()
    }

    fn abort(&mut self) {
        if !self.terminal {
            self.backend.abort();
            self.close_started = true;
            self.terminal = true;
            self.diagnostics.current_buffered_bytes = 0;
        }
    }

    fn diagnostics(&self) -> TransportDiagnostics {
        self.diagnostics
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing
)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    use super::*;

    #[derive(Debug)]
    struct FakeBackend {
        states: VecDeque<PeerState>,
        state: PeerState,
        buffered: i32,
        capacity: i32,
        capacity_boundary: NativeCapacityBoundary,
        buffered_after_poll: VecDeque<i32>,
        packets: VecDeque<Result<(Vec<u8>, bool), String>>,
        sent: Rc<RefCell<Vec<TransportFrame>>>,
        send_result: Option<BackendSendResult>,
        close_calls: usize,
        abort_calls: Rc<Cell<usize>>,
        drain_immediately_on_send: bool,
        close_code: i32,
        close_codes_after_poll: VecDeque<i32>,
        close_reason: String,
    }

    impl FakeBackend {
        fn new(state: PeerState) -> Self {
            Self {
                states: VecDeque::new(),
                state,
                buffered: 0,
                capacity: 64 * 1024,
                capacity_boundary: NativeCapacityBoundary::GreaterThan,
                buffered_after_poll: VecDeque::new(),
                packets: VecDeque::new(),
                sent: Rc::new(RefCell::new(Vec::new())),
                send_result: None,
                close_calls: 0,
                abort_calls: Rc::new(Cell::new(0)),
                drain_immediately_on_send: false,
                close_code: -1,
                close_codes_after_poll: VecDeque::new(),
                close_reason: String::new(),
            }
        }
    }

    impl GodotWebSocketBackend for FakeBackend {
        fn poll(&mut self) {
            if let Some(state) = self.states.pop_front() {
                self.state = state;
            }
            if let Some(buffered) = self.buffered_after_poll.pop_front() {
                self.buffered = buffered;
            }
            if let Some(close_code) = self.close_codes_after_poll.pop_front() {
                self.close_code = close_code;
            }
        }

        fn state(&self) -> PeerState {
            self.state
        }

        fn outbound_buffered_amount(&self) -> i32 {
            self.buffered
        }

        fn outbound_capacity(&self) -> i32 {
            self.capacity
        }

        fn capacity_boundary(&self) -> NativeCapacityBoundary {
            self.capacity_boundary
        }

        fn send_text(&mut self, text: &str) -> BackendSendResult {
            if let Some(result) = self.send_result.take() {
                return result;
            }
            self.sent
                .borrow_mut()
                .push(TransportFrame::Text(text.to_string()));
            self.buffered = self
                .buffered
                .saturating_add(i32::try_from(text.len()).unwrap_or(i32::MAX));
            if self.drain_immediately_on_send {
                self.buffered = 0;
            }
            BackendSendResult::Accepted
        }

        fn send_binary(&mut self, bytes: &[u8]) -> BackendSendResult {
            if let Some(result) = self.send_result.take() {
                return result;
            }
            self.sent
                .borrow_mut()
                .push(TransportFrame::Binary(bytes.to_vec()));
            self.buffered = self
                .buffered
                .saturating_add(i32::try_from(bytes.len()).unwrap_or(i32::MAX));
            if self.drain_immediately_on_send {
                self.buffered = 0;
            }
            BackendSendResult::Accepted
        }

        fn available_packet_count(&self) -> i32 {
            i32::try_from(self.packets.len()).unwrap_or(i32::MAX)
        }

        fn receive_packet(&mut self) -> Result<(Vec<u8>, bool), String> {
            self.packets
                .pop_front()
                .unwrap_or_else(|| Err("missing fake packet".to_string()))
        }

        fn close(&mut self) {
            self.close_calls += 1;
        }

        fn abort(&mut self) {
            self.abort_calls
                .set(self.abort_calls.get().saturating_add(1));
            self.state = PeerState::Closed;
        }

        fn close_code(&self) -> i32 {
            self.close_code
        }

        fn close_reason(&self) -> String {
            self.close_reason.clone()
        }
    }

    fn context() -> Context<'static> {
        Context::from_waker(std::task::Waker::noop())
    }

    #[test]
    fn connecting_does_not_take_outbound_frame() {
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(FakeBackend::new(
            PeerState::Connecting,
        )));
        let mut frame = Some(TransportFrame::Text("hello".to_string()));

        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Pending
        ));
        assert_eq!(frame, Some(TransportFrame::Text("hello".to_string())));
        assert!(!transport.is_ready());
    }

    #[test]
    fn sticky_nonzero_buffer_accepts_multiple_frames_without_waiting_for_zero() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.buffered = 7;
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));
        transport.begin_poll_cycle();

        for expected in [
            TransportFrame::Text("one".to_string()),
            TransportFrame::Binary(vec![1, 2, 3]),
            TransportFrame::Text("three".to_string()),
            TransportFrame::Binary(vec![4, 5]),
        ] {
            let mut frame = Some(expected);
            assert!(matches!(
                transport.poll_send(&mut context(), &mut frame),
                Poll::Ready(Ok(()))
            ));
            assert!(frame.is_none());
        }

        assert_eq!(transport.diagnostics().accepted_frames, 4);
        assert!(transport.diagnostics().current_buffered_bytes > 7);
    }

    #[test]
    fn accepted_send_diagnostics_use_backend_observation_not_an_estimate() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.drain_immediately_on_send = true;
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));
        let mut frame = Some(TransportFrame::Text("native drain".to_string()));

        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Ready(Ok(()))
        ));
        assert_eq!(transport.diagnostics().current_buffered_bytes, 0);
        assert_eq!(transport.diagnostics().peak_buffered_bytes, 0);
        assert_eq!(transport.diagnostics().accepted_frames, 1);
    }

    #[test]
    fn watermark_refusal_retains_exact_frame_then_resumes_once() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.buffered = 32 * 1024 - 4;
        backend.buffered_after_poll.extend([32 * 1024 - 4, 0]);
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));
        let expected = TransportFrame::Binary(vec![9; 8]);
        let mut frame = Some(expected.clone());

        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Pending
        ));
        assert_eq!(frame, Some(expected));
        assert_eq!(transport.diagnostics().watermark_hits, 1);

        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Ready(Ok(()))
        ));
        assert!(frame.is_none());
        assert_eq!(transport.diagnostics().accepted_frames, 1);
    }

    #[test]
    fn backend_capacity_result_is_retryable_and_retains_frame() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.send_result = Some(BackendSendResult::Capacity);
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));
        let expected = TransportFrame::Text("retry me".to_string());
        let mut frame = Some(expected.clone());

        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Pending
        ));
        assert_eq!(frame, Some(expected));
        assert_eq!(transport.diagnostics().backend_capacity_hits, 1);
        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Ready(Ok(()))
        ));
        assert!(frame.is_none());
    }

    #[test]
    fn terminal_backend_error_does_not_take_caller_frame() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.send_result = Some(BackendSendResult::Error("ERR_BUG".to_string()));
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));
        let expected = TransportFrame::Binary(vec![1, 2, 3]);
        let mut frame = Some(expected.clone());

        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Ready(Err(SignalFishError::TransportSend(error))) if error.contains("ERR_BUG")
        ));
        assert_eq!(frame, Some(expected));
    }

    #[test]
    fn web_and_native_capacity_boundaries_match_godot() {
        let mut web = FakeBackend::new(PeerState::Open);
        web.buffered = 7;
        web.capacity = 10;
        web.capacity_boundary = NativeCapacityBoundary::GreaterThanOrEqual;
        let mut web_transport = GodotWebSocketTransport::from_backend_with_options(
            Box::new(web),
            GodotWebSocketOptions {
                backpressure_policy: GodotBackpressurePolicy::NativeCapacity,
            },
        );
        let mut web_frame = Some(TransportFrame::Binary(vec![1, 2, 3]));
        assert!(matches!(
            web_transport.poll_send(&mut context(), &mut web_frame),
            Poll::Pending
        ));
        assert!(web_frame.is_some());

        let mut native = FakeBackend::new(PeerState::Open);
        native.buffered = 7;
        native.capacity = 10;
        native.capacity_boundary = NativeCapacityBoundary::GreaterThan;
        let mut native_transport = GodotWebSocketTransport::from_backend_with_options(
            Box::new(native),
            GodotWebSocketOptions {
                backpressure_policy: GodotBackpressurePolicy::NativeCapacity,
            },
        );
        let mut native_frame = Some(TransportFrame::Binary(vec![1, 2, 3]));
        assert!(matches!(
            native_transport.poll_send(&mut context(), &mut native_frame),
            Poll::Ready(Ok(()))
        ));
        assert!(native_frame.is_none());
    }

    #[test]
    fn one_oversized_frame_escapes_watermark_only_when_buffer_is_empty() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.capacity = 16 * 1024;
        let options = GodotWebSocketOptions {
            backpressure_policy: GodotBackpressurePolicy::Fixed {
                high_water_mark_bytes: 4 * 1024,
            },
        };
        let mut transport =
            GodotWebSocketTransport::from_backend_with_options(Box::new(backend), options);
        let mut oversized = Some(TransportFrame::Binary(vec![0; 8 * 1024]));

        assert!(matches!(
            transport.poll_send(&mut context(), &mut oversized),
            Poll::Ready(Ok(()))
        ));
        let mut second = Some(TransportFrame::Binary(vec![1]));
        assert!(matches!(
            transport.poll_send(&mut context(), &mut second),
            Poll::Pending
        ));
        assert!(second.is_some());
    }

    #[test]
    fn zero_native_capacity_means_unlimited() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.capacity = 0;
        backend.capacity_boundary = NativeCapacityBoundary::GreaterThanOrEqual;
        let mut transport = GodotWebSocketTransport::from_backend_with_options(
            Box::new(backend),
            GodotWebSocketOptions {
                backpressure_policy: GodotBackpressurePolicy::NativeCapacity,
            },
        );
        let mut frame = Some(TransportFrame::Binary(vec![3; 64 * 1024]));

        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Ready(Ok(()))
        ));
        assert!(frame.is_none());
        assert_eq!(transport.diagnostics().effective_watermark_bytes, u64::MAX);
    }

    #[test]
    fn native_greater_than_capacity_refusal_retains_exact_frame() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.buffered = 8;
        backend.capacity = 10;
        backend.capacity_boundary = NativeCapacityBoundary::GreaterThan;
        let mut transport = GodotWebSocketTransport::from_backend_with_options(
            Box::new(backend),
            GodotWebSocketOptions {
                backpressure_policy: GodotBackpressurePolicy::NativeCapacity,
            },
        );
        let expected = TransportFrame::Text("abc".to_string());
        let mut frame = Some(expected.clone());

        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Pending
        ));
        assert_eq!(frame, Some(expected));
        assert_eq!(transport.diagnostics().backend_capacity_hits, 1);
    }

    #[test]
    fn capacity_recovery_preserves_fifo_without_duplication() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.send_result = Some(BackendSendResult::Capacity);
        let sent = Rc::clone(&backend.sent);
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));
        let first = TransportFrame::Text("first".to_string());
        let second = TransportFrame::Binary(vec![2]);
        let mut pending = Some(first.clone());

        assert!(matches!(
            transport.poll_send(&mut context(), &mut pending),
            Poll::Pending
        ));
        assert_eq!(pending, Some(first.clone()));
        assert!(matches!(
            transport.poll_send(&mut context(), &mut pending),
            Poll::Ready(Ok(()))
        ));
        pending = Some(second.clone());
        assert!(matches!(
            transport.poll_send(&mut context(), &mut pending),
            Poll::Ready(Ok(()))
        ));

        assert_eq!(&*sent.borrow(), &[first, second]);
    }

    #[test]
    fn godot_out_of_memory_is_retryable_capacity() {
        assert_eq!(
            godot_send_result(Error::ERR_OUT_OF_MEMORY, "send"),
            BackendSendResult::Capacity
        );
        assert!(matches!(
            godot_send_result(Error::ERR_BUG, "send"),
            BackendSendResult::Error(_)
        ));
    }

    #[test]
    fn policy_watermarks_obey_default_and_configured_bounds() {
        assert_eq!(
            GodotBackpressurePolicy::adaptive(),
            GodotBackpressurePolicy::Adaptive {
                latency_target: Duration::from_millis(50),
                floor_bytes: 4 * 1024,
                ceiling_bytes: 32 * 1024,
            }
        );
        assert_eq!(
            GodotWebSocketOptions::default().backpressure_policy,
            GodotBackpressurePolicy::adaptive()
        );
        let adaptive =
            GodotWebSocketTransport::from_backend(Box::new(FakeBackend::new(PeerState::Open)));
        assert_eq!(
            adaptive.diagnostics().effective_watermark_bytes,
            DEFAULT_ADAPTIVE_FLOOR as u64
        );

        let mut web = FakeBackend::new(PeerState::Open);
        web.capacity = 10;
        web.capacity_boundary = NativeCapacityBoundary::GreaterThanOrEqual;
        let native = GodotWebSocketTransport::from_backend_with_options(
            Box::new(web),
            GodotWebSocketOptions {
                backpressure_policy: GodotBackpressurePolicy::NativeCapacity,
            },
        );
        assert_eq!(native.diagnostics().effective_watermark_bytes, 9);

        let reversed = GodotWebSocketTransport::from_backend_with_options(
            Box::new(FakeBackend::new(PeerState::Open)),
            GodotWebSocketOptions {
                backpressure_policy: GodotBackpressurePolicy::Adaptive {
                    latency_target: Duration::from_millis(50),
                    floor_bytes: 100,
                    ceiling_bytes: 10,
                },
            },
        );
        assert_eq!(reversed.diagnostics().effective_watermark_bytes, 10);
    }

    #[test]
    fn admission_decision_matches_exhaustive_spec() {
        let values = [0, 1, 2, 3, 7, 8, 9, 10, 31, 32, usize::MAX];
        for current in values {
            for next in values {
                for capacity in values {
                    for watermark in values {
                        for boundary in [
                            NativeCapacityBoundary::GreaterThanOrEqual,
                            NativeCapacityBoundary::GreaterThan,
                        ] {
                            let expected = match current.checked_add(next) {
                                None => AdmissionDecision::NativeCapacity,
                                Some(total)
                                    if capacity != 0
                                        && ((boundary
                                            == NativeCapacityBoundary::GreaterThanOrEqual
                                            && total >= capacity)
                                            || (boundary
                                                == NativeCapacityBoundary::GreaterThan
                                                && total > capacity)) =>
                                {
                                    AdmissionDecision::NativeCapacity
                                }
                                Some(total) if current > 0 && total > watermark => {
                                    AdmissionDecision::Watermark
                                }
                                Some(_) => AdmissionDecision::Admit,
                            };
                            assert_eq!(
                                admission_decision(current, next, capacity, boundary, watermark),
                                expected,
                                "current={current} next={next} capacity={capacity} watermark={watermark} boundary={boundary:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn adaptive_watermark_uses_one_eighth_ewma_and_native_clamp() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.capacity = 90;
        let mut transport = GodotWebSocketTransport::from_backend_with_options(
            Box::new(backend),
            GodotWebSocketOptions {
                backpressure_policy: GodotBackpressurePolicy::Adaptive {
                    latency_target: Duration::from_millis(50),
                    floor_bytes: 0,
                    ceiling_bytes: 1_000,
                },
            },
        );
        let base = Instant::now();
        transport.adaptive = AdaptiveState::default();
        transport.sample_cycle_at(base);
        transport.adaptive.accepted_since_sample = 800;
        transport.sample_cycle_at(base + Duration::from_secs(1));

        assert_eq!(transport.adaptive.accepted_burst_ewma, 100);
        assert_eq!(transport.adaptive.drain_bytes_per_second_ewma, 100);
        assert_eq!(transport.diagnostics().effective_watermark_bytes, 90);
    }

    #[test]
    fn adaptive_formula_tracks_burst_and_latency_across_cycles() {
        let mut transport = GodotWebSocketTransport::from_backend_with_options(
            Box::new(FakeBackend::new(PeerState::Open)),
            GodotWebSocketOptions {
                backpressure_policy: GodotBackpressurePolicy::Adaptive {
                    latency_target: Duration::from_secs(2),
                    floor_bytes: 0,
                    ceiling_bytes: 10_000,
                },
            },
        );
        let base = Instant::now();
        transport.adaptive = AdaptiveState::default();
        transport.sample_cycle_at(base);
        transport.adaptive.accepted_since_sample = 800;
        transport.sample_cycle_at(base + Duration::from_secs(1));
        assert_eq!(transport.adaptive.accepted_burst_ewma, 100);
        assert_eq!(transport.adaptive.drain_bytes_per_second_ewma, 100);
        assert_eq!(transport.diagnostics().effective_watermark_bytes, 200);

        transport.sample_cycle_at(base + Duration::from_secs(2));
        assert_eq!(transport.adaptive.accepted_burst_ewma, 87);
        assert_eq!(transport.adaptive.drain_bytes_per_second_ewma, 87);
        assert_eq!(transport.diagnostics().effective_watermark_bytes, 174);
        assert_eq!(ewma_one_eighth(u64::MAX, u64::MAX), u64::MAX);
    }

    #[test]
    fn client_poll_accepts_multiple_frames_with_sticky_godot_buffer() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.buffered = 7;
        let transport = GodotWebSocketTransport::from_backend(Box::new(backend));
        let mut client = crate::SignalFishPollingClient::new(
            transport,
            crate::SignalFishConfig::new("mb_app_test"),
        );
        client.ping().expect("first ping should queue");
        client.ping().expect("second ping should queue");
        client.ping().expect("third ping should queue");

        let _ = client.poll();

        assert_eq!(client.transport_diagnostics().accepted_frames, 4);
        assert_eq!(client.polling_stats().current_queue_depth, 0);
        assert!(client.transport_diagnostics().current_buffered_bytes > 7);
    }

    #[test]
    fn abort_force_closes_and_clears_current_buffer_diagnostic() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.buffered = 99;
        let abort_calls = Rc::clone(&backend.abort_calls);
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));
        transport.begin_poll_cycle();

        transport.abort();

        assert_eq!(abort_calls.get(), 1);
        assert_eq!(transport.diagnostics().current_buffered_bytes, 0);
        assert!(matches!(
            transport.poll_close(&mut context()),
            Poll::Ready(Ok(()))
        ));
    }

    #[test]
    fn receives_text_and_binary_packets_without_conflating_them() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.packets.push_back(Ok((b"hello".to_vec(), true)));
        backend.packets.push_back(Ok((vec![0, 255], false)));
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));

        assert!(matches!(
            transport.poll_recv(&mut context()),
            Poll::Ready(Some(Ok(TransportFrame::Text(text)))) if text == "hello"
        ));
        assert!(matches!(
            transport.poll_recv(&mut context()),
            Poll::Ready(Some(Ok(TransportFrame::Binary(bytes)))) if bytes == vec![0, 255]
        ));
    }

    #[test]
    fn invalid_text_utf8_is_a_receive_error() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.packets.push_back(Ok((vec![255], true)));
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));

        let Poll::Ready(Some(Err(SignalFishError::TransportReceive(error)))) =
            transport.poll_recv(&mut context())
        else {
            panic!("expected a transport receive error");
        };
        assert!(error.contains("UTF-8"));
    }

    #[test]
    fn godot_packet_error_is_preserved_as_receive_error() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend
            .packets
            .push_back(Err("get_packet failed with ERR_BUSY".to_string()));
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));

        let Poll::Ready(Some(Err(SignalFishError::TransportReceive(error)))) =
            transport.poll_recv(&mut context())
        else {
            panic!("expected a transport receive error");
        };
        assert!(error.contains("ERR_BUSY"));
    }

    #[test]
    fn peer_close_preserves_metadata_and_reports_terminal_once_ready() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.states.push_back(PeerState::Closed);
        backend.close_code = 4000;
        backend.close_reason = "server draining".to_string();
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));
        let mut frame = Some(TransportFrame::Text("after close".to_string()));

        assert!(matches!(
            transport.poll_recv(&mut context()),
            Poll::Ready(None)
        ));
        assert_eq!(
            transport.close_info(),
            Some(TransportCloseInfo {
                code: Some(4000),
                reason: Some("server draining".to_string()),
                clean: Some(true),
                initiated_by_peer: true,
            })
        );
        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Ready(Err(SignalFishError::TransportClosed))
        ));
        assert!(frame.is_some());
    }

    #[test]
    fn closing_and_closed_states_drain_already_buffered_packets() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend
            .states
            .extend([PeerState::Closing, PeerState::Closed]);
        backend.packets.push_back(Ok((b"last text".to_vec(), true)));
        backend.packets.push_back(Ok((vec![1, 2, 3], false)));
        backend.close_code = 4000;
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));

        assert!(matches!(
            transport.poll_recv(&mut context()),
            Poll::Ready(Some(Ok(TransportFrame::Text(text)))) if text == "last text"
        ));
        assert_eq!(transport.close_info(), None);
        assert!(matches!(
            transport.poll_recv(&mut context()),
            Poll::Ready(Some(Ok(TransportFrame::Binary(bytes)))) if bytes == vec![1, 2, 3]
        ));
        assert_eq!(transport.close_info(), None);
        assert!(matches!(
            transport.poll_recv(&mut context()),
            Poll::Ready(None)
        ));
        assert_eq!(
            transport.close_info().and_then(|info| info.code),
            Some(4000)
        );
    }

    #[test]
    fn close_waits_for_already_buffered_inbound_packets() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.states.push_back(PeerState::Closed);
        backend
            .packets
            .push_back(Ok((b"last packet".to_vec(), true)));
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));

        assert!(matches!(
            transport.poll_close(&mut context()),
            Poll::Pending
        ));
        assert!(matches!(
            transport.poll_recv(&mut context()),
            Poll::Ready(Some(Ok(TransportFrame::Text(text)))) if text == "last packet"
        ));
        assert!(matches!(
            transport.poll_close(&mut context()),
            Poll::Ready(Ok(()))
        ));
    }

    #[test]
    fn locally_started_close_waits_for_buffered_inbound_packets() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.states.extend([PeerState::Open, PeerState::Closed]);
        backend
            .packets
            .push_back(Ok((b"last packet".to_vec(), true)));
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));

        assert!(matches!(
            transport.poll_close(&mut context()),
            Poll::Pending
        ));
        assert!(matches!(
            transport.poll_recv(&mut context()),
            Poll::Ready(Some(Ok(TransportFrame::Text(text)))) if text == "last packet"
        ));
        assert!(matches!(
            transport.poll_close(&mut context()),
            Poll::Ready(Ok(()))
        ));
    }

    #[test]
    fn handshake_failure_is_reported_once_then_becomes_terminal() {
        let mut backend = FakeBackend::new(PeerState::Connecting);
        backend.states.push_back(PeerState::Closed);
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));

        assert!(matches!(
            transport.poll_recv(&mut context()),
            Poll::Ready(Some(Err(SignalFishError::TransportReceive(_))))
        ));
        assert_eq!(
            transport.close_info().and_then(|info| info.clean),
            Some(false)
        );
        assert!(matches!(
            transport.poll_recv(&mut context()),
            Poll::Ready(None)
        ));
    }

    #[test]
    fn send_defers_a_handshake_failure_to_receive() {
        let mut backend = FakeBackend::new(PeerState::Connecting);
        backend.states.push_back(PeerState::Closed);
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));
        let mut frame = Some(TransportFrame::Text("authenticate".to_string()));

        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Pending
        ));
        assert!(frame.is_some());
        assert!(matches!(
            transport.poll_recv(&mut context()),
            Poll::Ready(Some(Err(SignalFishError::TransportReceive(error))))
                if error.contains("closed before opening")
        ));
        assert!(matches!(
            transport.poll_recv(&mut context()),
            Poll::Ready(None)
        ));
    }

    #[test]
    fn send_only_driver_gets_the_handshake_failure_after_deferral() {
        let mut backend = FakeBackend::new(PeerState::Connecting);
        backend.states.push_back(PeerState::Closed);
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));
        let mut frame = Some(TransportFrame::Text("authenticate".to_string()));

        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Pending
        ));
        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Ready(Err(SignalFishError::TransportReceive(error)))
                if error.contains("closed before opening")
        ));
        assert!(frame.is_some());
    }

    #[test]
    fn send_defers_a_peer_close_to_receive_with_metadata() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.states.push_back(PeerState::Closed);
        backend.close_code = 4000;
        backend.close_reason = "server draining".to_string();
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));
        let mut frame = Some(TransportFrame::Text("pending".to_string()));

        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Pending
        ));
        assert!(frame.is_some());
        assert!(matches!(
            transport.poll_recv(&mut context()),
            Poll::Ready(None)
        ));
        assert_eq!(
            transport.close_info(),
            Some(TransportCloseInfo {
                code: Some(4000),
                reason: Some("server draining".to_string()),
                clean: Some(true),
                initiated_by_peer: true,
            })
        );
    }

    #[test]
    fn closing_does_not_freeze_incomplete_close_metadata() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend
            .states
            .extend([PeerState::Closing, PeerState::Closed]);
        backend.close_codes_after_poll.extend([-1, 4000]);
        backend.close_reason = "server draining".to_string();
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));
        let mut frame = Some(TransportFrame::Text("unsent".to_string()));

        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Pending
        ));
        assert!(frame.is_some());
        assert_eq!(transport.close_info(), None);

        assert!(matches!(
            transport.poll_close(&mut context()),
            Poll::Ready(Ok(()))
        ));
        assert_eq!(
            transport.close_info(),
            Some(TransportCloseInfo {
                code: Some(4000),
                reason: Some("server draining".to_string()),
                clean: Some(true),
                initiated_by_peer: true,
            })
        );
    }

    #[test]
    fn close_is_polled_to_completion_and_idempotent() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.states.extend([
            PeerState::Open,
            PeerState::Open,
            PeerState::Closing,
            PeerState::Closed,
        ]);
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));

        assert!(matches!(
            transport.poll_close(&mut context()),
            Poll::Pending
        ));
        assert!(matches!(
            transport.poll_close(&mut context()),
            Poll::Ready(Ok(()))
        ));
        assert!(matches!(
            transport.poll_close(&mut context()),
            Poll::Ready(Ok(()))
        ));
        assert_eq!(
            transport.close_info().map(|info| info.initiated_by_peer),
            Some(false)
        );
    }

    #[test]
    fn close_does_not_claim_an_already_peer_initiated_handshake() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend
            .states
            .extend([PeerState::Closing, PeerState::Closed]);
        backend.close_code = 1000;
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));

        assert!(matches!(
            transport.poll_close(&mut context()),
            Poll::Ready(Ok(()))
        ));
        assert!(!transport.close_started);
        assert_eq!(
            transport.close_info().map(|info| info.initiated_by_peer),
            Some(true)
        );
    }

    #[test]
    fn close_starts_immediately_after_an_accepted_frame() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.states.extend([
            PeerState::Open,
            PeerState::Open,
            PeerState::Closing,
            PeerState::Closed,
        ]);
        backend.buffered = 7;
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));
        let mut frame = Some(TransportFrame::Text("final frame".to_string()));

        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Ready(Ok(()))
        ));
        assert!(frame.is_none());

        assert!(matches!(
            transport.poll_close(&mut context()),
            Poll::Pending
        ));
        assert!(transport.close_started);

        assert!(matches!(
            transport.poll_close(&mut context()),
            Poll::Ready(Ok(()))
        ));
    }
}
