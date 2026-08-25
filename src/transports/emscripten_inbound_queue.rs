//! Target-independent inbound-byte accounting for the Emscripten callback
//! queue.
//!
//! Emscripten WebSocket callbacks deliver complete frames into an internal
//! channel between polling ticks. Without a bound, a hostile or buggy server
//! can grow that buffer without limit while the game loop is busy. This
//! module keeps the admission, fusion, and drain accounting free of FFI so
//! the ordinary workspace test suite exercises it when the Emscripten target
//! is absent.

/// Default inclusive bound on buffered inbound WebSocket input, matching the
/// native transport's 8 MiB client resource policy.
pub(super) const DEFAULT_MAX_INBOUND_QUEUE_BYTES: usize = 8 * 1024 * 1024;

/// Conservative per-frame minimum charge against the byte ledger.
///
/// Every admitted frame occupies queue-node and event-header storage
/// regardless of payload length. Charging and releasing this minimum
/// symmetrically keeps zero-length flood frames from bypassing the byte
/// bound while never drifting from the drained amount.
const MIN_FRAME_CHARGE_BYTES: usize = 64;

/// Ledger units charged for one frame with `payload_len` content bytes,
/// admitted and released symmetrically.
pub(super) const fn charged_frame_bytes(payload_len: usize) -> usize {
    if payload_len > MIN_FRAME_CHARGE_BYTES {
        payload_len
    } else {
        MIN_FRAME_CHARGE_BYTES
    }
}

/// Why [`InboundQueueBound::admit`] refused inbound input.
///
/// Every hard refusal permanently fuses the queue;
/// [`QueueRefusal::AlreadyFused`] reports an earlier fusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QueueRefusal {
    /// The transport already fused after an earlier refusal. Callers must
    /// drop further input silently so a flood cannot enqueue unbounded error
    /// events.
    AlreadyFused,
    /// One frame alone exceeded the configured bound.
    FrameExceedsLimit { frame_bytes: usize, limit: usize },
    /// Bytes already buffered plus this frame would exceed the configured
    /// bound before the next drain.
    QueueWouldExceedLimit {
        queued_bytes: usize,
        frame_bytes: usize,
        limit: usize,
    },
}

impl QueueRefusal {
    /// Terminal-error text for the first refusal. Exposes byte counts only;
    /// never payload data.
    pub(super) fn message(self) -> String {
        match self {
            Self::AlreadyFused => "inbound WebSocket input refused after the \
                 callback queue was already fused"
                .to_owned(),
            Self::FrameExceedsLimit { frame_bytes, limit } => format!(
                "inbound WebSocket frame of {frame_bytes} bytes exceeds the \
                 {limit}-byte client resource limit; connection fused"
            ),
            Self::QueueWouldExceedLimit {
                queued_bytes,
                frame_bytes,
                limit,
            } => format!(
                "buffered inbound WebSocket input ({queued_bytes} bytes) plus a \
                 {frame_bytes}-byte frame would exceed the {limit}-byte client \
                 resource limit; connection fused"
            ),
        }
    }
}

/// Admission ledger for buffered inbound frames.
///
/// Shared between the Emscripten message callback (admission) and
/// `poll_recv` (drain) through interior mutability; both sides run on the
/// single emscripten main thread and never interleave.
#[derive(Clone, Copy)]
pub(super) struct InboundQueueBound {
    /// Inclusive byte ceiling; `None` disables every bound.
    limit: Option<usize>,
    queued_bytes: usize,
    fused: bool,
}

impl InboundQueueBound {
    pub(super) const fn new(limit: Option<usize>) -> Self {
        Self {
            limit,
            queued_bytes: 0,
            fused: false,
        }
    }

    /// Admits one frame of `frame_bytes` charged units, recording them
    /// against the buffered total on success. Callers must pass
    /// [`charged_frame_bytes`] so empty payloads still reserve their queuing
    /// overhead.
    ///
    /// The first over-limit input permanently fuses the queue; later calls
    /// report [`QueueRefusal::AlreadyFused`] regardless of size so callers
    /// can drop flood traffic without allocating.
    ///
    /// Capacity returns only through [`Self::record_drained`], keeping the
    /// admitted total at most `limit` bytes between drains.
    pub(super) fn admit(&mut self, frame_bytes: usize) -> Result<(), QueueRefusal> {
        if self.fused {
            return Err(QueueRefusal::AlreadyFused);
        }
        if let Some(limit) = self.limit {
            if frame_bytes > limit {
                self.fused = true;
                return Err(QueueRefusal::FrameExceedsLimit { frame_bytes, limit });
            }
            match self.queued_bytes.checked_add(frame_bytes) {
                Some(total) if total <= limit => self.queued_bytes = total,
                _ => {
                    self.fused = true;
                    return Err(QueueRefusal::QueueWouldExceedLimit {
                        queued_bytes: self.queued_bytes,
                        frame_bytes,
                        limit,
                    });
                }
            }
        }
        Ok(())
    }

    /// Releases the charged units of a drained frame. Saturates at zero so
    /// an accounting bug fails closed toward refusals instead of unbounded
    /// buffering.
    pub(super) fn record_drained(&mut self, drained_bytes: usize) {
        self.queued_bytes = self.queued_bytes.saturating_sub(drained_bytes);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
mod tests {
    use super::*;

    /// Comfortably above [`MIN_FRAME_CHARGE_BYTES`] so tests exercise
    /// realistic payload sizes end to end.
    const LIMIT: usize = 1024;

    #[test]
    fn default_limit_matches_native_eight_mib_policy() {
        assert_eq!(DEFAULT_MAX_INBOUND_QUEUE_BYTES, 8 * 1024 * 1024);
    }

    #[test]
    fn zero_length_frames_carry_the_minimum_charge() {
        assert_eq!(charged_frame_bytes(0), MIN_FRAME_CHARGE_BYTES);
        assert_eq!(
            charged_frame_bytes(MIN_FRAME_CHARGE_BYTES - 1),
            MIN_FRAME_CHARGE_BYTES
        );
        assert_eq!(
            charged_frame_bytes(MIN_FRAME_CHARGE_BYTES),
            MIN_FRAME_CHARGE_BYTES
        );
        assert_eq!(
            charged_frame_bytes(MIN_FRAME_CHARGE_BYTES + 1),
            MIN_FRAME_CHARGE_BYTES + 1
        );
        assert_eq!(charged_frame_bytes(usize::MAX), usize::MAX);
    }

    #[test]
    fn admits_frames_up_to_the_inclusive_limit_and_released_capacity_is_reusable() {
        let mut bound = InboundQueueBound::new(Some(LIMIT));
        bound.admit(charged_frame_bytes(300)).unwrap();
        // Exactly-at-limit totals are admitted, mirroring the native codec's
        // inclusive maximum.
        bound.admit(charged_frame_bytes(LIMIT - 300)).unwrap();

        // Draining releases exactly the charged bytes...
        bound.record_drained(charged_frame_bytes(300));
        bound.record_drained(charged_frame_bytes(LIMIT - 300));

        // ...and released capacity admits new work again.
        bound.admit(charged_frame_bytes(LIMIT)).unwrap();
    }

    #[test]
    fn oversized_frames_refuse_and_fuse_permanently() {
        let mut bound = InboundQueueBound::new(Some(LIMIT));
        assert_eq!(
            bound.admit(charged_frame_bytes(LIMIT + 1)),
            Err(QueueRefusal::FrameExceedsLimit {
                frame_bytes: LIMIT + 1,
                limit: LIMIT,
            })
        );

        // Even zero-byte input is refused once fused so callers drop flood
        // traffic uniformly without allocating.
        assert_eq!(
            bound.admit(charged_frame_bytes(0)),
            Err(QueueRefusal::AlreadyFused)
        );

        // Draining does not unfuse.
        bound.record_drained(usize::MAX);
        assert_eq!(
            bound.admit(1),
            Err(QueueRefusal::AlreadyFused),
            "draining must not clear fusion"
        );
    }

    #[test]
    fn aggregate_backlog_refuses_before_exceeding_the_limit() {
        let mut bound = InboundQueueBound::new(Some(LIMIT));
        bound.admit(charged_frame_bytes(600)).unwrap();
        // 600 queued + 500 incoming > 1024 refuses even though either alone
        // fits.
        assert_eq!(
            bound.admit(charged_frame_bytes(500)),
            Err(QueueRefusal::QueueWouldExceedLimit {
                queued_bytes: charged_frame_bytes(600),
                frame_bytes: charged_frame_bytes(500),
                limit: LIMIT,
            })
        );
        assert_eq!(bound.admit(1), Err(QueueRefusal::AlreadyFused));
    }

    #[test]
    fn zero_length_flood_frames_cannot_bypass_the_bound() {
        let mut bound = InboundQueueBound::new(Some(2 * MIN_FRAME_CHARGE_BYTES));
        // Each empty frame reserves its queuing overhead...
        bound.admit(charged_frame_bytes(0)).unwrap();
        bound.admit(charged_frame_bytes(0)).unwrap();
        // ...so the count flood hits the same aggregate refusal as payloads.
        assert_eq!(
            bound.admit(charged_frame_bytes(0)),
            Err(QueueRefusal::QueueWouldExceedLimit {
                queued_bytes: 2 * MIN_FRAME_CHARGE_BYTES,
                frame_bytes: MIN_FRAME_CHARGE_BYTES,
                limit: 2 * MIN_FRAME_CHARGE_BYTES,
            })
        );

        // Releasing a drained empty frame restores exactly its charge —
        // pinned by the reusable-capacity test — while fusion here remains
        // permanent even after a drain.
        bound.record_drained(charged_frame_bytes(0));
        assert_eq!(
            bound.admit(charged_frame_bytes(0)),
            Err(QueueRefusal::AlreadyFused)
        );
    }

    #[test]
    fn disabled_limit_never_refuses_or_fuses() {
        let mut bound = InboundQueueBound::new(None);
        bound.admit(charged_frame_bytes(0)).unwrap();
        bound.admit(usize::MAX).unwrap();
        bound.record_drained(7);
    }

    #[test]
    fn refusal_messages_carry_byte_counts_only() {
        let frame = QueueRefusal::FrameExceedsLimit {
            frame_bytes: 9,
            limit: 8,
        }
        .message();
        assert!(frame.contains("9"));
        assert!(frame.contains("8"));

        let backlog = QueueRefusal::QueueWouldExceedLimit {
            queued_bytes: 6,
            frame_bytes: 3,
            limit: 8,
        }
        .message();
        assert!(backlog.contains("6"));
        assert!(backlog.contains("3"));
        assert!(backlog.contains("8"));
    }
}
