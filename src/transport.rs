//! Frame-capable polling transport contract.
//!
//! A transport owns any frame it accepts from `poll_send` until that send
//! completes. Polling makes the same implementation usable by an async runtime
//! driver and by a main-thread game-loop driver without requiring `Send`.

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
/// Once taken, it must retain the frame internally until the method returns
/// `Ready`. The caller must keep polling with the same slot and must not replace
/// it while the operation is pending.
///
/// # Close
///
/// `poll_close` is idempotent and may require multiple polls. Once it returns
/// `Ready(Ok(()))`, later calls must also succeed without sending another close.
pub trait Transport {
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

    /// Whether the connection handshake has completed.
    fn is_ready(&self) -> bool {
        true
    }

    /// Structured terminal close metadata, if available.
    fn close_info(&self) -> Option<TransportCloseInfo> {
        None
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
