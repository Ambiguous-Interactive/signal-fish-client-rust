//! Frame-capable polling transport contract.
//!
//! A transport owns any frame it accepts from `poll_send`. Polling makes the
//! same implementation usable by an async runtime driver and by a main-thread
//! game-loop driver without requiring `Send`.
//!
//! The contract begins after a backend has produced one complete, ordered
//! text/binary frame stream for one intended server. It does not define server
//! authentication, raw-stream framing, or a datagram envelope. Backends over
//! streams or datagrams own message sizing, delimiting, server trust/source
//! binding, fragmentation, loss/duplicate/reorder handling, and terminal/error
//! policy before yielding a [`TransportFrame`].

use std::task::{Context, Poll};

use crate::error::SignalFishError;

/// One complete signaling transport frame.
#[derive(Clone, PartialEq, Eq)]
pub enum TransportFrame {
    /// JSON protocol message.
    Text(String),
    /// Opaque binary game-data frame; protocol decoding happens above the transport.
    Binary(Vec<u8>),
}

impl std::fmt::Debug for TransportFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Frames can contain every credential and application payload in the
        // protocol. Length is safe and still useful for transport diagnostics.
        match self {
            Self::Text(text) => f.debug_struct("Text").field("bytes", &text.len()).finish(),
            Self::Binary(bytes) => f
                .debug_struct("Binary")
                .field("bytes", &bytes.len())
                .finish(),
        }
    }
}

/// Structured metadata for a terminal transport close.
#[derive(Clone, Default, PartialEq, Eq)]
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

impl std::fmt::Debug for TransportCloseInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Close reasons are peer-controlled and can contain arbitrary text.
        f.debug_struct("TransportCloseInfo")
            .field("code", &self.code)
            .field("has_reason", &self.reason.is_some())
            .field("clean", &self.clean)
            .field("initiated_by_peer", &self.initiated_by_peer)
            .finish()
    }
}

/// Scheduling and buffering diagnostics reported by a transport.
///
/// Counters are cumulative and saturating. Byte values describe backend-owned
/// buffering, not the polling client's command queue or peer delivery.
///
/// Publishing is optional per backend: the built-in native WebSocket and
/// Emscripten transports report the all-default view (zero buffered bytes and
/// counters), while the Godot adapter populates every field. A zero
/// `effective_watermark_bytes` means "no watermark published" for backends
/// that do not track one; the Godot adapter can also publish a genuine
/// configured zero (a fixed 0-byte watermark is strict stop-and-wait).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransportDiagnostics {
    /// Bytes currently buffered by the transport backend.
    ///
    /// Always `0` for backends that do not publish diagnostics (the built-in
    /// native WebSocket and Emscripten transports).
    pub current_buffered_bytes: u64,
    /// Highest observed backend-buffered byte count.
    ///
    /// Always `0` for backends that do not publish diagnostics (the built-in
    /// native WebSocket and Emscripten transports).
    pub peak_buffered_bytes: u64,
    /// Current admission watermark. Backends that publish no watermark report
    /// `0`; so does a Godot adapter configured with a fixed 0-byte (strict
    /// stop-and-wait) watermark.
    pub effective_watermark_bytes: u64,
    /// Frames accepted by the backend.
    pub accepted_frames: u64,
    /// Payload bytes accepted by the backend.
    pub accepted_bytes: u64,
    /// Sends deferred by the configured admission watermark. One hit per
    /// deferred send: repeated polls that re-observe the same parked frame do
    /// not count again, while a later independent send does. A send deferred
    /// by multiple mechanisms records only its first mechanism.
    pub watermark_hits: u64,
    /// Sends deferred because the backend reported or approached native
    /// capacity. One hit per deferred send: repeated polls that re-observe
    /// the same parked frame do not count again, while a later independent
    /// send does. A send deferred by multiple mechanisms records only its
    /// first mechanism.
    pub backend_capacity_hits: u64,
}

/// Bidirectional framed transport for the Signal Fish signaling protocol.
///
/// Each successful receive yields exactly one complete text or binary frame in
/// protocol order from the one intended server connection. `TransportFrame`
/// carries no source address or peer identity, so the client attributes every
/// yielded frame to that server. This trait does not authenticate the server or
/// turn arbitrary stream/datagram bytes into frames; the backend must apply an
/// appropriate trust/source-binding policy and report unrecoverable framing,
/// corruption, or loss as a transport error instead of silently omitting or
/// fabricating frames. That policy need not provide cryptographic identity,
/// but the SDK then provides no spoof protection. The crate includes no raw
/// UDP backend, and
/// [`RelayTransport::Udp`](crate::protocol::RelayTransport::Udp) is legacy wire
/// metadata rather than a switch for this transport.
///
/// The trait itself deliberately has no `Send` bound. The async client applies
/// `Send + 'static` at its task-spawning boundary; the polling client can own a
/// main-thread-only transport.
///
/// # Panic boundary
///
/// Only [`abort`](Self::abort) is required not to panic (it runs from `Drop`,
/// including during unwinding — a panic inside it aborts the whole process).
/// The other methods have no panic constraint, but a panic in any of them is a
/// backend contract violation with driver-specific consequences: the async
/// driver's loop task dies, so the event channel closes **without** a
/// `Disconnected` event, queued and parked reliable sends resolve with
/// [`SignalFishError::NotConnected`], and `abort` still runs exactly once from
/// the loop's drop guard; the polling driver propagates the panic into the
/// caller's thread, and `close()` afterwards heals — through the close-deadline
/// `abort` only if the violating method keeps panicking. The core keeps its last pre-death state in both cases until a
/// `shutdown`/`close` reconciles it.
///
/// # Outbound ownership
///
/// `poll_send` receives the caller's pending frame slot. An implementation may
/// take the frame only when it has accepted responsibility for preserving it.
/// Taking the frame is the ownership-transfer point: it means the backend has
/// accepted responsibility for the frame. A transport that needs more work may
/// return `Pending` after taking it, but must retain all required state and must
/// not accept a replacement until that operation returns `Ready`. Completion
/// must leave the caller's slot empty, including on continuation polls.
/// Completion does not imply peer delivery or that all socket-wide buffering
/// reached zero.
///
/// # Close
///
/// `poll_close` is idempotent and may require multiple polls. Once it returns
/// `Ready(Ok(()))`, later calls must also succeed without sending another close.
/// If it returns an error, logical I/O must terminate and the client immediately
/// calls `abort`; fallible backend cleanup may remain safely retryable.
///
/// `abort` is the fallback when graceful close fails, its deadline expires, or
/// the transport owner is cancelled or dropped. It is required so every
/// backend has an explicit abandonment path: a call must promptly release or
/// safely detach backend-owned work and discard any retained send. It must be
/// idempotent: completed cleanup is not repeated, while failed cleanup may be
/// retried safely. After it returns, drivers make no further polling calls;
/// only repeated `abort`, `is_ready`, `close_info`, `diagnostics`, and drop are
/// allowed.
pub trait Transport {
    /// Mark the start of one driver scheduling cycle.
    ///
    /// The polling client calls this once per `SignalFishPollingClient::poll`
    /// invocation; the async client calls it once per outer transport-loop
    /// iteration. Transports may use the hook to sample buffering once per
    /// scheduling cycle, but must not assume it represents a rendered frame or
    /// a fixed wall-clock interval.
    fn begin_poll_cycle(&mut self) {}

    /// Advance one outbound frame.
    ///
    /// The clients treat an error as outbound-terminal. It must not take a
    /// caller-owned frame unless the backend had already accepted it. They may
    /// still call [`poll_recv`](Self::poll_recv) to process a bounded sequence
    /// of complete frames that are immediately ready, stopping at its first
    /// `Pending`, error, or EOF, before they close or abort the transport.
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

    /// Immediately abandon transport work after failed or preempted close.
    ///
    /// This operation must return promptly without blocking or panicking,
    /// release or safely detach backend resources, discard any retained send,
    /// and be idempotent. Completed cleanup must not be repeated, but a failed
    /// cleanup attempt may be retried safely; when an external API cannot
    /// unregister callbacks, preserving their backing allocation is required
    /// to avoid use-after-free. After it returns, the client will not
    /// call `begin_poll_cycle`, `poll_send`, `poll_recv`, or `poll_close` again.
    /// Only repeated `abort`, `is_ready`, `close_info`, `diagnostics`, and drop
    /// are allowed. This method can run from `Drop`, including during
    /// unwinding, so it must never panic.
    fn abort(&mut self);

    /// Whether the connection handshake has completed.
    ///
    /// This must be cheap and monotonic for one physical connection. A
    /// transport that changes from `false` to `true` while the async client is
    /// blocked must wake a waker previously registered by `poll_send` or
    /// `poll_recv`, because this accessor does not receive a waker itself.
    /// While this returns `false`, `poll_send` must return `Pending` without
    /// taking a caller-owned frame.
    /// Complete protocol frames must not be returned by `poll_recv` before
    /// readiness.
    fn is_ready(&self) -> bool {
        true
    }

    /// Structured terminal close metadata, if available.
    fn close_info(&self) -> Option<TransportCloseInfo> {
        None
    }

    /// Return transport-owned buffering and admission diagnostics.
    ///
    /// The default returns the all-default view; backends that do not track
    /// buffering (the built-in native WebSocket and Emscripten transports)
    /// keep it. See [`TransportDiagnostics`] for field semantics.
    fn diagnostics(&self) -> TransportDiagnostics {
        TransportDiagnostics::default()
    }

    /// Advisory maximum size in bytes for one complete inbound frame.
    ///
    /// Backends that enforce an inbound frame or assembled-message bound (as
    /// the built-in WebSocket transport does by default) return `Some(bound)`.
    /// The drivers then enforce that bound themselves before any frame reaches
    /// protocol decoding: a larger frame is treated as a terminal receive
    /// error and tears the session down, mirroring how the built-in transport's
    /// own over-limit delivery ends the connection (RFC 6455 close 1009
    /// semantics). This keeps a hostile or misbehaving peer from amplifying
    /// one delivered frame into unbounded client-side parse memory when a
    /// backend's enforcement cannot be structurally assumed.
    ///
    /// Return `None` (the default) when the backend enforces no bound; the
    /// drivers then accept every delivered frame. A declared hint is a
    /// contract: the backend must never deliver a larger frame, and the value
    /// must stay stable for the lifetime of one connection. A backend that
    /// declares a hint but does not enforce it still gets driver-side
    /// enforcement, so oversized frames fail fast instead of being decoded.
    fn max_frame_hint(&self) -> Option<usize> {
        None
    }
}

/// Gate a synchronous backend send without transferring caller ownership until
/// the backend accepts the borrowed frame.
#[cfg(any(test, target_os = "emscripten"))]
pub(crate) fn poll_accept_frame<F>(
    ready: bool,
    frame: &mut Option<TransportFrame>,
    accept: F,
) -> Poll<Result<(), SignalFishError>>
where
    F: FnOnce(&TransportFrame) -> Result<(), SignalFishError>,
{
    let Some(candidate) = frame.as_ref() else {
        return Poll::Ready(Ok(()));
    };
    if !ready {
        return Poll::Pending;
    }
    match accept(candidate) {
        Ok(()) => {
            let _ = frame.take();
            Poll::Ready(Ok(()))
        }
        Err(error) => Poll::Ready(Err(error)),
    }
}

/// Decode a text-frame payload, surfacing corruption as a terminal receive
/// error instead of silently omitting the frame.
///
/// The [`Transport`] contract requires corrupt inbound input to be surfaced
/// rather than dropped. Engine-backed transports surface the corruption for
/// the driver to treat as terminal (tungstenite reports protocol and UTF-8
/// errors, which the WebSocket transport treats as terminal; the Godot
/// adapter reports invalid UTF-8 text packets as a receive error, which
/// likewise ends the stream at the driver); byte-oriented backends decode
/// raw text payloads themselves and must use this helper for the same
/// guarantee. The error message carries only the UTF-8 diagnostic, never
/// the payload bytes.
#[cfg(any(test, target_os = "emscripten"))]
pub(crate) fn text_frame_from_utf8(payload: &[u8]) -> Result<String, SignalFishError> {
    std::str::from_utf8(payload)
        .map(str::to_owned)
        .map_err(|error| {
            SignalFishError::TransportReceive(
                format!("received a text frame that is not valid UTF-8: {error}").into(),
            )
        })
}

/// Drive one transport send to completion from an async runtime.
///
/// Test helper for transports with async-runtime tests; gated to those
/// transports so sparse-feature builds never carry an unused helper.
#[cfg(all(test, feature = "transport-websocket"))]
pub(crate) async fn send_frame<T: Transport + ?Sized>(
    transport: &mut T,
    frame: TransportFrame,
) -> Result<(), SignalFishError> {
    let mut pending = Some(frame);
    std::future::poll_fn(|cx| transport.poll_send(cx, &mut pending)).await
}

/// Await one inbound transport frame.
///
/// Test helper for transports with async-runtime tests; gated to those
/// transports so sparse-feature builds never carry an unused helper.
#[cfg(all(test, feature = "transport-websocket"))]
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::task::Poll;

    use super::{poll_accept_frame, text_frame_from_utf8, TransportFrame};
    use crate::SignalFishError;

    #[test]
    fn synchronous_acceptance_retains_the_exact_frame_until_success() {
        let original = TransportFrame::Binary(vec![1, 2, 3]);
        let mut pending = Some(original.clone());
        let called = Cell::new(false);

        assert!(matches!(
            poll_accept_frame(false, &mut pending, |_| {
                called.set(true);
                Ok(())
            }),
            Poll::Pending
        ));
        assert!(!called.get());
        assert_eq!(pending, Some(original.clone()));

        let refused = poll_accept_frame(true, &mut pending, |_| {
            Err(SignalFishError::TransportSend("refused".into()))
        });
        assert!(matches!(refused, Poll::Ready(Err(_))));
        assert_eq!(pending, Some(original));

        assert!(matches!(
            poll_accept_frame(true, &mut pending, |_| Ok(())),
            Poll::Ready(Ok(()))
        ));
        assert_eq!(pending, None);
    }

    #[test]
    fn text_frame_from_utf8_surfaces_corruption_without_echoing_payload() {
        assert!(matches!(
            text_frame_from_utf8(b"hello"),
            Ok(text) if text == "hello"
        ));

        let error = text_frame_from_utf8(&[0xFF, 0xFE, 0x53, 0x45, 0x43, 0x52, 0x45, 0x54]);
        assert!(
            matches!(&error, Err(SignalFishError::TransportReceive(boxed)) if {
                let rendered = boxed.to_string();
                rendered.contains("not valid UTF-8")
                    && !rendered.contains("SECRET")
                    && !rendered.contains("ff")
                    && !rendered.contains("FF")
            }),
            "error must be a terminal receive error describing the corruption \
             without echoing payload bytes: {error:?}"
        );
    }
}
