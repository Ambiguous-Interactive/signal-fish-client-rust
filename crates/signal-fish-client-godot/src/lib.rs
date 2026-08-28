//! Godot 4.5 `WebSocketPeer` transport for native and web exports.
//!
//! Godot owns the platform WebSocket implementation. This transport advances
//! it from [`Transport::poll_send`], [`Transport::poll_recv`], and
//! [`Transport::poll_close`], making it suitable for
//! [`SignalFishPollingClient`](signal_fish_client::SignalFishPollingClient) in a Node's
//! `_process` callback. It contains no GDScript or Emscripten WebSocket FFI.
//!
//! This crate is versioned in lockstep with `signal-fish-client`, supports
//! godot-rust 0.4.5 through 0.5.x, and requires Rust 1.94. Applications should
//! select one exact `godot` version in that range so their `Gd<WebSocketPeer>`
//! has the same type identity as the adapter's public API.
//!
//! # Outbound frame bound
//!
//! Godot refuses to buffer an outbound message once the peer's outbound
//! buffer would overflow, so a single frame larger than `outbound_buffer_size`
//! (65,535 bytes by default) is never admitted on native builds — and one at
//! or above that size on web exports. Such a frame parks as `Pending`,
//! growing only the capacity diagnostics. SDK-created peers keep that legacy
//! outbound default and raise only the inbound buffer; keep game payloads
//! under ~64 KiB, or construct your own peer with a raised outbound buffer
//! and wrap it with [`GodotWebSocketTransport::from_peer`].
//!
//! # Inbound queue bounds and platform divergence
//!
//! Godot bounds its inbound queue by both bytes (`inbound_buffer_size`) and
//! packet count (`max_queued_packets`, engine default 4,096). Godot's native
//! and web backends can **silently drop** newly arriving frames once either
//! bound is hit; the adapter receives no error it could surface. SDK-created
//! peers therefore raise the packet cap from 4,096 to 65,536 alongside the
//! 8 MiB inbound byte buffer. The limits remain independent: enough very small
//! frames can still reach the packet cap first.
//! [`GodotWebSocketTransport::from_peer`] preserves whatever the application
//! configured — raise `max_queued_packets` yourself if a caller-owned peer
//! must absorb large inbound bursts on web.
//!
//! Two further engine-imposed limits cannot be worked around adapter-side:
//!
//! - **Native tail frames:** Godot's native build makes buffered packets
//!   inaccessible the moment the peer leaves the `OPEN` state, so final
//!   application frames that arrived just before the peer's CLOSE are
//!   delivered by web exports but discarded natively.
//! - **Synthesized close codes:** when the engine terminates the connection
//!   itself (protocol error, oversized message), it reports a locally
//!   synthesized status code (for example 1002/1007/1009) as if it were a
//!   wire close frame; the JS `wasClean` flag is discarded on web. Close
//!   metadata in [`Transport::close_info`] therefore cannot perfectly
//!   distinguish a genuine peer close from a locally synthesized one.

use std::fmt;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use godot::builtin::PackedByteArray;
use godot::classes::{web_socket_peer, WebSocketPeer};
use godot::global::Error;
use godot::obj::{Gd, NewGd};

use signal_fish_client::{
    SignalFishError, Transport, TransportCloseInfo, TransportDiagnostics, TransportFrame,
};

const DEFAULT_ADAPTIVE_FLOOR: usize = 4 * 1024;
const DEFAULT_ADAPTIVE_CEILING: usize = 32 * 1024;
const DEFAULT_ADAPTIVE_LATENCY: Duration = Duration::from_millis(50);
const DEFAULT_INBOUND_BUFFER_SIZE: i32 = 8 * 1024 * 1024;
// Godot bounds its inbound queue by packet count as well as bytes, and its
// native and web backends can silently drop frames once either bound is hit. The engine
// default of 4,096 packets would silently discard legitimate traffic long
// before an 8 MiB byte buffer filled (frames averaging under ~2 KiB), so
// SDK-created peers raise the cap while retaining a finite metadata bound.
const DEFAULT_MAX_QUEUED_PACKETS: i32 = 65_536;

/// Godot outbound admission strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GodotBackpressurePolicy {
    /// Refuse a frame when it would exceed a fixed buffered-byte watermark.
    ///
    /// A frame within Godot's native outbound capacity is always admitted
    /// while the backend buffer is empty (so a watermark of **0** degenerates
    /// to strict stop-and-wait: at most one such frame in flight until Godot
    /// drains it), and refused once any bytes are buffered beyond the
    /// watermark. Native-capacity refusal takes precedence over the
    /// watermark in every state.
    Fixed {
        /// Maximum normally admitted backend-buffered payload bytes.
        high_water_mark_bytes: usize,
    },
    /// Adapt the watermark to the observed accepted burst and drain rate.
    ///
    /// Edge values are safe by construction: a zero [`latency_target`](Self::Adaptive::latency_target)
    /// drops the latency term (the watermark then tracks the burst estimate
    /// alone), a [`floor_bytes`](Self::Adaptive::floor_bytes) above
    /// [`ceiling_bytes`](Self::Adaptive::ceiling_bytes) pins the watermark to
    /// the ceiling, and huge targets saturate without overflow.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcceptedAdmissionAudit {
    WithinWatermark,
    EmptyBufferEscape(usize),
    Violation,
}

fn accepted_admission_audit(
    current: usize,
    next: usize,
    watermark: usize,
) -> AcceptedAdmissionAudit {
    let Some(total) = current.checked_add(next) else {
        return AcceptedAdmissionAudit::Violation;
    };
    if total <= watermark {
        AcceptedAdmissionAudit::WithinWatermark
    } else if current == 0 {
        AcceptedAdmissionAudit::EmptyBufferEscape(next)
    } else {
        AcceptedAdmissionAudit::Violation
    }
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
    fn set_inbound_buffer_size(&mut self, bytes: i32);
    fn set_max_queued_packets(&mut self, packets: i32);
    fn connect_to_url(&mut self, url: &str) -> Result<(), Error>;
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
    fn set_inbound_buffer_size(&mut self, bytes: i32) {
        std::ops::DerefMut::deref_mut(self).set_inbound_buffer_size(bytes);
    }

    fn set_max_queued_packets(&mut self, packets: i32) {
        std::ops::DerefMut::deref_mut(self).set_max_queued_packets(packets);
    }

    fn connect_to_url(&mut self, url: &str) -> Result<(), Error> {
        let result = std::ops::DerefMut::deref_mut(self).connect_to_url(url);
        if result == Error::OK {
            Ok(())
        } else {
            Err(result)
        }
    }

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
/// Add this adapter crate and drive the transport through
/// [`SignalFishPollingClient`](signal_fish_client::SignalFishPollingClient). The contained
/// Godot object is intentionally not required to be `Send`; call the polling
/// client's `poll()` method from the same Godot thread on every frame.
///
/// A connection that closes before opening surfaces as
/// [`SignalFishError::TransportReceive`] even when observed first through
/// [`Transport::poll_send`]: the handshake result is a receive-path event, so
/// the send defers it by one poll and the classification is identical on both
/// sides.
pub struct GodotWebSocketTransport {
    backend: Box<dyn GodotWebSocketBackend>,
    backpressure_policy: GodotBackpressurePolicy,
    diagnostics: TransportDiagnostics,
    adaptive: AdaptiveState,
    ever_ready: bool,
    close_deferred_to_receive: bool,
    close_started: bool,
    terminal: bool,
    close_info: Option<TransportCloseInfo>,
    admission_watermark_violations: u64,
    one_frame_escape_frames: u64,
    one_frame_escape_bytes: u64,
    /// The current caller frame's deferral is already counted, so polls that
    /// re-observe the same parked state do not inflate the deferred-send
    /// counters. Cleared when the frame is accepted (or the slot is empty).
    deferred_counted: bool,
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
            .field("backpressure_policy", &self.backpressure_policy)
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
    /// Accepted sends that exceeded the contemporaneous watermark while the
    /// backend already owned buffered bytes.
    ///
    /// This is a defensive invariant counter and should remain zero. It does
    /// not count the documented single-frame escape from an empty buffer.
    #[must_use]
    pub const fn admission_watermark_violations(&self) -> u64 {
        self.admission_watermark_violations
    }

    /// Payload bytes accepted through the single-frame watermark escape while
    /// the backend buffer was empty.
    #[must_use]
    pub const fn one_frame_escape_bytes(&self) -> u64 {
        self.one_frame_escape_bytes
    }

    /// Frames accepted through the single-frame watermark escape while the
    /// backend buffer was empty.
    ///
    /// Pair this with [`Self::one_frame_escape_bytes`] to distinguish one
    /// individually oversized frame from the same cumulative byte total
    /// spread across multiple escapes.
    #[must_use]
    pub const fn one_frame_escape_frames(&self) -> u64 {
        self.one_frame_escape_frames
    }

    /// Create a Godot `WebSocketPeer` and begin a non-blocking connection.
    ///
    /// The connection handshake advances when the transport is polled. For web
    /// exports, use `wss://` when the exported page is served over HTTPS. The
    /// SDK-created peer uses an 8 MiB Godot inbound buffer and raises the
    /// queued-packet cap from 4,096 to 65,536, because Godot can silently drop
    /// inbound frames when either independent bound fills.
    /// The outbound buffer keeps Godot's own default — see the crate docs for
    /// how that bounds a single admitted frame. Use [`Self::from_peer`] when
    /// the application must choose any of these settings.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::InvalidConfig`] when Godot rejects the URL.
    /// Returns [`SignalFishError::Io`] when Godot cannot start the connection
    /// attempt.
    pub fn connect(url: &str) -> Result<Self, SignalFishError> {
        Self::connect_with_options(url, GodotWebSocketOptions::default())
    }

    /// Create a Godot `WebSocketPeer` with explicit backpressure options.
    ///
    /// These options control outbound admission. The SDK-created peer uses the
    /// same 8 MiB inbound buffer and 65,536-packet cap as [`Self::connect`];
    /// use [`Self::from_peer_with_options`] to preserve a caller-configured
    /// peer.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::InvalidConfig`] when Godot rejects the URL.
    /// Returns [`SignalFishError::Io`] when Godot cannot start the connection
    /// attempt.
    pub fn connect_with_options(
        url: &str,
        options: GodotWebSocketOptions,
    ) -> Result<Self, SignalFishError> {
        Self::connect_backend_with_options(
            Box::new(WebSocketPeer::new_gd()),
            url,
            DEFAULT_INBOUND_BUFFER_SIZE,
            options,
        )
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

    fn connect_backend_with_options(
        mut backend: Box<dyn GodotWebSocketBackend>,
        url: &str,
        inbound_buffer_size: i32,
        options: GodotWebSocketOptions,
    ) -> Result<Self, SignalFishError> {
        backend.set_inbound_buffer_size(inbound_buffer_size);
        backend.set_max_queued_packets(DEFAULT_MAX_QUEUED_PACKETS);
        backend.connect_to_url(url).map_err(|error| match error {
            // Godot 4.5 returns ERR_INVALID_PARAMETER exactly for URL faults
            // (empty URL, an unparsable URL, or a non-ws(s):// scheme)
            // before any connection work starts; see `WSLPeer::connect_to_url`
            // in the engine source `modules/websocket/wsl_peer.cpp` (4.5).
            Error::ERR_INVALID_PARAMETER => SignalFishError::InvalidConfig {
                field: "url",
                problem: format!("Godot rejected the URL with {error:?}"),
            },
            // Engine- or resource-level failures (for example WSS on a build
            // without the TLS module, or socket exhaustion) are platform
            // conditions, not URL faults.
            other => SignalFishError::Io(std::io::Error::other(format!(
                "Godot WebSocketPeer connect_to_url failed with {other:?}"
            ))),
        })?;
        Ok(Self::from_backend_with_options(backend, options))
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
            backpressure_policy: options.backpressure_policy,
            diagnostics: TransportDiagnostics::default(),
            adaptive: AdaptiveState::default(),
            ever_ready,
            close_deferred_to_receive: false,
            close_started: false,
            terminal: false,
            close_info: None,
            admission_watermark_violations: 0,
            one_frame_escape_frames: 0,
            one_frame_escape_bytes: 0,
            deferred_counted: false,
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
        // -1 is Godot native's "no wire CLOSE frame" value. 1006 and 1015 are
        // the codes engines synthesize for abnormal termination and TLS
        // handshake failure; both are forbidden on the wire by RFC 6455
        // section 7.4.1, so observing one means no clean CLOSE handshake
        // occurred. 1005 stays clean: it reports a real CLOSE frame that
        // merely carried no status code.
        let clean = raw_code != -1 && raw_code != 1006 && raw_code != 1015;
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
                "Godot WebSocket connection closed before opening".into(),
            ))))
        }
    }

    fn closed_send_error(&self) -> SignalFishError {
        if self.ever_ready || self.close_started {
            SignalFishError::TransportClosed
        } else {
            SignalFishError::TransportReceive(
                "Godot WebSocket connection closed before opening".into(),
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
        match self.backpressure_policy {
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

    /// Count one deferred send for the current caller frame. Repeated polls
    /// of the same parked frame re-enter `poll_send`, so the hit is recorded
    /// only on the first deferral of the episode and cleared when the frame
    /// is accepted or leaves the slot.
    fn count_deferred_send(&mut self, select: impl FnOnce(&mut TransportDiagnostics) -> &mut u64) {
        if !self.deferred_counted {
            let counter = select(&mut self.diagnostics);
            *counter = counter.saturating_add(1);
            self.deferred_counted = true;
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
        let effective = match self.backpressure_policy {
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
            self.backpressure_policy,
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

/// Exponential moving average of one eighth of each new sample.
///
/// The arithmetic runs in `u128`: for `u64` inputs, `7 * previous + sample`
/// is bounded by `8 * (u64::MAX - 1) + u64::MAX`, far below `u128::MAX`, so
/// neither the multiply-add nor the division can overflow and the only
/// fallible step is the narrowing conversion handled by `unwrap_or`.
#[allow(clippy::arithmetic_side_effects)]
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
            self.deferred_counted = false;
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
        let watermark = self.configured_watermark();
        match admission_decision(
            current,
            next_bytes,
            self.native_capacity(),
            self.backend.capacity_boundary(),
            watermark,
        ) {
            AdmissionDecision::Admit => {}
            AdmissionDecision::NativeCapacity => {
                self.count_deferred_send(|diagnostics| &mut diagnostics.backend_capacity_hits);
                return Poll::Pending;
            }
            AdmissionDecision::Watermark => {
                self.count_deferred_send(|diagnostics| &mut diagnostics.watermark_hits);
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
                self.deferred_counted = false;
                match accepted_admission_audit(current, next_bytes, watermark) {
                    AcceptedAdmissionAudit::WithinWatermark => {}
                    AcceptedAdmissionAudit::EmptyBufferEscape(bytes) => {
                        self.one_frame_escape_frames =
                            self.one_frame_escape_frames.saturating_add(1);
                        self.one_frame_escape_bytes = self
                            .one_frame_escape_bytes
                            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
                    }
                    AcceptedAdmissionAudit::Violation => {
                        self.admission_watermark_violations =
                            self.admission_watermark_violations.saturating_add(1);
                    }
                }
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
                self.count_deferred_send(|diagnostics| &mut diagnostics.backend_capacity_hits);
                Poll::Pending
            }
            BackendSendResult::Error(error) => {
                Poll::Ready(Err(SignalFishError::TransportSend(error.into())))
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
            Err(error) => {
                return Poll::Ready(Some(Err(SignalFishError::TransportReceive(error.into()))))
            }
        };
        if is_text {
            match String::from_utf8(bytes) {
                Ok(text) => Poll::Ready(Some(Ok(TransportFrame::Text(text)))),
                Err(error) => Poll::Ready(Some(Err(SignalFishError::TransportReceive(
                    format!("Godot WebSocket text packet was not valid UTF-8: {error}").into(),
                )))),
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
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
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
        configured_inbound_buffer_size: Rc<Cell<i32>>,
        configured_max_queued_packets: Rc<Cell<i32>>,
        connection_steps: Rc<RefCell<Vec<&'static str>>>,
        connect_error: Option<Error>,
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
                configured_inbound_buffer_size: Rc::new(Cell::new(0)),
                configured_max_queued_packets: Rc::new(Cell::new(0)),
                connection_steps: Rc::new(RefCell::new(Vec::new())),
                connect_error: None,
            }
        }
    }

    impl GodotWebSocketBackend for FakeBackend {
        fn set_inbound_buffer_size(&mut self, bytes: i32) {
            self.configured_inbound_buffer_size.set(bytes);
            self.connection_steps.borrow_mut().push("configure_inbound");
        }

        fn set_max_queued_packets(&mut self, packets: i32) {
            self.configured_max_queued_packets.set(packets);
            self.connection_steps.borrow_mut().push("configure_packets");
        }

        fn connect_to_url(&mut self, _url: &str) -> Result<(), Error> {
            self.connection_steps.borrow_mut().push("connect");
            self.connect_error.take().map_or(Ok(()), Err)
        }

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
        assert_eq!(transport.admission_watermark_violations(), 0);
        assert_eq!(transport.one_frame_escape_bytes(), 0);
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
    fn watermark_park_across_repeated_polls_counts_one_deferred_send() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.buffered = 32 * 1024 - 4;
        // Two consecutive polls observe the same parked watermark state, then
        // the buffer drains, the third poll admits the frame, and a fourth
        // parks again for an independent send.
        backend
            .buffered_after_poll
            .extend([32 * 1024 - 4, 32 * 1024 - 4, 0, 32 * 1024 - 7]);
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));
        let expected = TransportFrame::Binary(vec![9; 8]);
        let mut frame = Some(expected.clone());

        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Pending
        ));
        assert_eq!(frame, Some(expected.clone()));
        assert_eq!(transport.diagnostics().watermark_hits, 1);

        // The same frame still parked: the deferral must not count again.
        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Pending
        ));
        assert_eq!(frame, Some(expected.clone()));
        assert_eq!(transport.diagnostics().watermark_hits, 1);

        // Once the frame is admitted, the episode ends.
        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Ready(Ok(()))
        ));
        assert!(frame.is_none());
        assert_eq!(transport.diagnostics().watermark_hits, 1);

        // A later, independent send counts as its own deferral (the accepted
        // 8-byte frame left 8 buffered bytes; the sampled entry re-parks the
        // buffer above the watermark for this 8-byte frame).
        let mut second = Some(TransportFrame::Binary(vec![7; 8]));
        assert!(matches!(
            transport.poll_send(&mut context(), &mut second),
            Poll::Pending
        ));
        assert_eq!(transport.diagnostics().watermark_hits, 2);
    }

    #[test]
    fn native_capacity_park_across_repeated_polls_counts_one_deferred_send() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.buffered = 8;
        backend.capacity = 10;
        backend.capacity_boundary = NativeCapacityBoundary::GreaterThan;
        backend.buffered_after_poll.extend([8, 8, 0]);
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
        assert_eq!(transport.diagnostics().backend_capacity_hits, 1);

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
        assert_eq!(transport.diagnostics().backend_capacity_hits, 1);
    }

    #[test]
    fn backend_capacity_result_park_counts_one_deferred_send_per_frame() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.send_result = Some(BackendSendResult::Capacity);
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));
        let expected = TransportFrame::Text("retry me".to_string());
        let mut frame = Some(expected.clone());

        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Pending
        ));
        assert_eq!(transport.diagnostics().backend_capacity_hits, 1);
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
            Poll::Ready(Err(SignalFishError::TransportSend(error)))
                if error.to_string().contains("ERR_BUG")
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
        assert_eq!(transport.admission_watermark_violations(), 0);
        assert_eq!(transport.one_frame_escape_frames(), 1);
        assert_eq!(transport.one_frame_escape_bytes(), 8 * 1024);
        let mut second = Some(TransportFrame::Binary(vec![1]));
        assert!(matches!(
            transport.poll_send(&mut context(), &mut second),
            Poll::Pending
        ));
        assert!(second.is_some());
        assert_eq!(transport.admission_watermark_violations(), 0);
        assert_eq!(transport.one_frame_escape_frames(), 1);
        assert_eq!(transport.one_frame_escape_bytes(), 8 * 1024);
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
    fn sdk_created_peer_configures_inbound_limits_before_connect() {
        let backend = FakeBackend::new(PeerState::Connecting);
        let observed_size = Rc::clone(&backend.configured_inbound_buffer_size);
        let observed_packets = Rc::clone(&backend.configured_max_queued_packets);
        let observed_steps = Rc::clone(&backend.connection_steps);

        let transport = GodotWebSocketTransport::connect_backend_with_options(
            Box::new(backend),
            "ws://example.invalid/v2/ws",
            DEFAULT_INBOUND_BUFFER_SIZE,
            GodotWebSocketOptions::default(),
        )
        .expect("fake connection setup should succeed");

        assert_eq!(observed_size.get(), DEFAULT_INBOUND_BUFFER_SIZE);
        // The queued-packet cap must be raised with the byte buffer: Godot's
        // native and web backends can silently drop inbound frames once the
        // engine default of 4,096 packets fills, even with bytes to spare.
        assert_eq!(
            observed_packets.get(),
            DEFAULT_MAX_QUEUED_PACKETS,
            "SDK-created peers must keep the packet-count bound from becoming the effective inbound limit"
        );
        assert_eq!(
            &*observed_steps.borrow(),
            &["configure_inbound", "configure_packets", "connect"]
        );
        assert!(!transport.is_ready());
    }

    #[test]
    fn connection_failure_follows_buffer_configuration() {
        let mut backend = FakeBackend::new(PeerState::Connecting);
        backend.connect_error = Some(Error::FAILED);
        let observed_steps = Rc::clone(&backend.connection_steps);

        let error = GodotWebSocketTransport::connect_backend_with_options(
            Box::new(backend),
            "ws://example.invalid/v2/ws",
            DEFAULT_INBOUND_BUFFER_SIZE,
            GodotWebSocketOptions::default(),
        )
        .expect_err("scripted setup failure must surface");

        assert!(matches!(error, SignalFishError::Io(_)));
        assert_eq!(
            &*observed_steps.borrow(),
            &["configure_inbound", "configure_packets", "connect"]
        );
    }

    #[test]
    fn url_fault_error_code_is_invalid_config() {
        let mut backend = FakeBackend::new(PeerState::Connecting);
        backend.connect_error = Some(Error::ERR_INVALID_PARAMETER);

        let error = GodotWebSocketTransport::connect_backend_with_options(
            Box::new(backend),
            "not a url",
            DEFAULT_INBOUND_BUFFER_SIZE,
            GodotWebSocketOptions::default(),
        )
        .expect_err("a URL-fault error code must surface");

        assert!(matches!(
            error,
            SignalFishError::InvalidConfig { field: "url", .. }
        ));
    }

    #[test]
    fn wrapping_existing_backend_preserves_connection_configuration() {
        const CALLER_INBOUND_BUFFER_SIZE: i32 = 2 * 1024 * 1024 + 17;
        const CALLER_MAX_QUEUED_PACKETS: i32 = 12_345;
        let backend = FakeBackend::new(PeerState::Open);
        backend
            .configured_inbound_buffer_size
            .set(CALLER_INBOUND_BUFFER_SIZE);
        backend
            .configured_max_queued_packets
            .set(CALLER_MAX_QUEUED_PACKETS);
        let observed_size = Rc::clone(&backend.configured_inbound_buffer_size);
        let observed_packets = Rc::clone(&backend.configured_max_queued_packets);
        let observed_steps = Rc::clone(&backend.connection_steps);

        let transport = GodotWebSocketTransport::from_backend(Box::new(backend));

        assert_eq!(observed_size.get(), CALLER_INBOUND_BUFFER_SIZE);
        assert_eq!(
            observed_packets.get(),
            CALLER_MAX_QUEUED_PACKETS,
            "from_peer must preserve caller configuration untouched"
        );
        assert!(observed_steps.borrow().is_empty());
        assert!(transport.is_ready());
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
    fn accepted_admission_audit_classifies_all_invariant_outcomes() {
        assert_eq!(
            accepted_admission_audit(1, 2, 3),
            AcceptedAdmissionAudit::WithinWatermark
        );
        assert_eq!(
            accepted_admission_audit(0, 4, 3),
            AcceptedAdmissionAudit::EmptyBufferEscape(4)
        );
        assert_eq!(
            accepted_admission_audit(2, 2, 3),
            AcceptedAdmissionAudit::Violation
        );
        assert_eq!(
            accepted_admission_audit(usize::MAX, 1, usize::MAX),
            AcceptedAdmissionAudit::Violation
        );
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
        let mut client = signal_fish_client::SignalFishPollingClient::new(
            transport,
            signal_fish_client::SignalFishConfig::new("mb_app_test"),
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
        transport.abort();

        assert_eq!(abort_calls.get(), 1);
        assert_eq!(transport.diagnostics().current_buffered_bytes, 0);
        let expected = TransportFrame::Text("caller still owns this".into());
        let mut offered = Some(expected.clone());
        assert!(matches!(
            transport.poll_send(&mut context(), &mut offered),
            Poll::Ready(Err(SignalFishError::TransportClosed))
        ));
        assert_eq!(offered, Some(expected));
        assert!(matches!(
            transport.poll_recv(&mut context()),
            Poll::Ready(None)
        ));
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
        assert!(error.to_string().contains("UTF-8"));
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
        assert!(error.to_string().contains("ERR_BUSY"));
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
    fn observed_close_codes_classify_cleanliness_data_driven() {
        let cases = [
            // (-1): native reports no code for every abnormal termination.
            (-1, None, Some(false)),
            // 1006: engines synthesize it for abnormal termination and it is
            // forbidden on the wire (RFC 6455 section 7.4.1).
            (1006, Some(1006), Some(false)),
            // 1015: reserved for local TLS-handshake failure reports; also
            // never transmitted on the wire.
            (1015, Some(1015), Some(false)),
            (1000, Some(1000), Some(true)),
            (4000, Some(4000), Some(true)),
        ];
        for (raw_code, expected_code, expected_clean) in cases {
            let mut backend = FakeBackend::new(PeerState::Open);
            backend.states.push_back(PeerState::Closed);
            backend.close_code = raw_code;
            let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));

            assert!(matches!(
                transport.poll_recv(&mut context()),
                Poll::Ready(None)
            ));
            let info = transport.close_info().expect("close metadata recorded");
            assert_eq!(info.code, expected_code, "code for raw {raw_code}");
            assert_eq!(info.clean, expected_clean, "clean for raw {raw_code}");
        }
    }

    #[test]
    fn debug_redacts_peer_close_reason_transitively() {
        let secret = "godot-close-secret";
        let mut transport =
            GodotWebSocketTransport::from_backend(Box::new(FakeBackend::new(PeerState::Closed)));
        transport.close_info = Some(TransportCloseInfo {
            code: Some(4000),
            reason: Some(secret.into()),
            clean: Some(true),
            initiated_by_peer: true,
        });

        let output = format!("{transport:?}");
        assert!(!output.contains(secret), "debug output leaked: {output}");
        assert!(output.contains("has_reason: true"));
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
                if error.to_string().contains("closed before opening")
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
                if error.to_string().contains("closed before opening")
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
