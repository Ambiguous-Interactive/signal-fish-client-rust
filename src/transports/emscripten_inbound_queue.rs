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

    /// Admits `frame_bytes` of inbound input, recording them against the
    /// buffered total on success.
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

    /// Releases capacity held by drained frames, saturating at zero so an
    /// accounting bug fails closed toward refusals instead of unbounded
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

    const LIMIT: usize = 8;

    #[test]
    fn default_limit_matches_native_eight_mib_policy() {
        assert_eq!(DEFAULT_MAX_INBOUND_QUEUE_BYTES, 8 * 1024 * 1024);
    }

    #[test]
    fn admits_frames_up_to_the_inclusive_limit_and_released_capacity_is_reusable() {
        let mut bound = InboundQueueBound::new(Some(LIMIT));
        bound.admit(3).unwrap();
        // Exactly-at-limit totals are admitted, mirroring the native codec's
        // inclusive maximum.
        bound.admit(LIMIT - 3).unwrap();

        // Draining releases exactly the drained bytes...
        bound.record_drained(5);
        bound.record_drained(3);

        // ...and released capacity admits new work again.
        bound.admit(LIMIT).unwrap();
    }

    #[test]
    fn oversized_frames_refuse_and_fuse_permanently() {
        let mut bound = InboundQueueBound::new(Some(LIMIT));
        assert_eq!(
            bound.admit(LIMIT + 1),
            Err(QueueRefusal::FrameExceedsLimit {
                frame_bytes: LIMIT + 1,
                limit: LIMIT,
            })
        );

        // Even zero-byte input is refused once fused so callers drop flood
        // traffic uniformly without allocating.
        assert_eq!(bound.admit(0), Err(QueueRefusal::AlreadyFused));

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
        bound.admit(6).unwrap();
        // 6 queued + 3 incoming > 8 refuses even though either alone fits.
        assert_eq!(
            bound.admit(3),
            Err(QueueRefusal::QueueWouldExceedLimit {
                queued_bytes: 6,
                frame_bytes: 3,
                limit: LIMIT,
            })
        );
        assert_eq!(bound.admit(1), Err(QueueRefusal::AlreadyFused));
    }

    #[test]
    fn disabled_limit_never_refuses_or_fuses() {
        let mut bound = InboundQueueBound::new(None);
        bound.admit(usize::MAX).unwrap();
        bound.admit(usize::MAX).unwrap();
        bound.record_drained(7);
    }

    #[test]
    fn empty_frames_stay_free_even_at_full_capacity() {
        let mut bound = InboundQueueBound::new(Some(LIMIT));
        bound.admit(LIMIT).unwrap();
        // Zero-byte frames add nothing to the ledger, so they neither trip
        // the aggregate bound nor disturb later admissions.
        bound.admit(0).unwrap();
        bound.record_drained(LIMIT);
        bound.admit(LIMIT).unwrap();
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
