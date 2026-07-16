//! Frame-capable polling transport contract.
//!
//! A transport owns any frame it accepts from `poll_send`. Polling makes the
//! same implementation usable by an async runtime driver and by a main-thread
//! game-loop driver without requiring `Send`.

use std::task::{Context, Poll};

use crate::error::SignalFishError;

/// One complete signaling transport frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportFrame {
    /// JSON protocol message.
    Text(String),
    /// Opaque protocol-v3 binary game-data frame.
    Binary(Vec<u8>),
}

/// Structured metadata for a terminal transport close.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransportCloseInfo {
    /// Protocol close code, when supplied by the peer.
    pub code: Option<u16>,
    /// Human-readable close reason, when supplied by the peer.
    pub reason: Option<String>,
    /// Whether the underlying transport reported a clean handshake.
    pub clean: Option<bool>,
    /// True when the peer initiated the close.
    pub initiated_by_peer: bool,
}

/// Scheduling and buffering diagnostics reported by a transport.
///
/// Counters are cumulative and saturating. Byte values describe backend-owned
/// buffering, not the polling client's command queue or peer delivery.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransportDiagnostics {
    /// Bytes currently buffered by the transport backend.
    pub current_buffered_bytes: u64,
    /// Highest observed backend-buffered byte count.
    pub peak_buffered_bytes: u64,
    /// Current admission watermark. `0` means the transport does not publish one.
    pub effective_watermark_bytes: u64,
    /// Frames accepted by the backend.
    pub accepted_frames: u64,
    /// Payload bytes accepted by the backend.
    pub accepted_bytes: u64,
    /// Sends deferred by the configured admission watermark.
    pub watermark_hits: u64,
    /// Sends deferred because the backend reported or approached native capacity.
    pub backend_capacity_hits: u64,
}

/// Bidirectional framed transport for the Signal Fish signaling protocol.
///
/// The trait itself deliberately has no `Send` bound. The async client applies
/// `Send + 'static` at its task-spawning boundary; the polling client can own a
/// main-thread-only transport.
///
/// # Outbound ownership
///
/// `poll_send` receives the caller's pending frame slot. An implementation may
/// take the frame only when it has accepted responsibility for preserving it.
/// Taking the frame is the ownership-transfer point: it means the backend has
/// accepted responsibility for the frame. A transport that needs more work may
/// return `Pending` after taking it, but must retain all required state and must
/// not accept a replacement until that operation returns `Ready`. Completion
/// does not imply peer delivery or that all socket-wide buffering reached zero.
///
/// # Close
///
/// `poll_close` is idempotent and may require multiple polls. Once it returns
/// `Ready(Ok(()))`, later calls must also succeed without sending another close.
pub trait Transport {
    /// Mark the start of one caller-driven polling cycle.
    ///
    /// Transports may use this to sample buffering once per rendered frame.
    fn begin_poll_cycle(&mut self) {}

    /// Advance one outbound frame.
    fn poll_send(
        &mut self,
        cx: &mut Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> Poll<Result<(), SignalFishError>>;

    /// Poll the next complete inbound frame.
    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<TransportFrame, SignalFishError>>>;

    /// Advance an idempotent graceful close.
    fn poll_close(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), SignalFishError>>;

    /// Immediately abandon transport work after a client-side close deadline.
    ///
    /// The default preserves source compatibility for existing implementors.
    fn abort(&mut self) {}

    /// Whether the connection handshake has completed.
    fn is_ready(&self) -> bool {
        true
    }

    /// Structured terminal close metadata, if available.
    fn close_info(&self) -> Option<TransportCloseInfo> {
        None
    }

    /// Return transport-owned buffering and admission diagnostics.
    fn diagnostics(&self) -> TransportDiagnostics {
        TransportDiagnostics::default()
    }
}

/// Drive one transport send to completion from an async runtime.
#[cfg(feature = "tokio-runtime")]
pub(crate) async fn send_frame<T: Transport + ?Sized>(
    transport: &mut T,
    frame: TransportFrame,
) -> Result<(), SignalFishError> {
    let mut pending = Some(frame);
    std::future::poll_fn(|cx| transport.poll_send(cx, &mut pending)).await
}

/// Await one inbound transport frame.
#[cfg(feature = "tokio-runtime")]
pub(crate) async fn recv_frame<T: Transport + ?Sized>(
    transport: &mut T,
) -> Option<Result<TransportFrame, SignalFishError>> {
    std::future::poll_fn(|cx| transport.poll_recv(cx)).await
}

/// Drive graceful transport close to completion.
#[cfg(feature = "tokio-runtime")]
pub(crate) async fn close_transport<T: Transport + ?Sized>(
    transport: &mut T,
) -> Result<(), SignalFishError> {
    std::future::poll_fn(|cx| transport.poll_close(cx)).await
}
