use std::task::{Context, Poll};

use crate::error::SignalFishError;
use crate::transport::{Transport, TransportFrame};

pub(crate) const DEFAULT_DRIVER_WORK_FRAMES: usize = 64;
pub(crate) const DEFAULT_DRIVER_WORK_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy)]
pub(crate) struct ReadyFrameDrainBudget {
    pub(crate) frames: usize,
    pub(crate) bytes: usize,
}

impl ReadyFrameDrainBudget {
    pub(crate) const fn new(frames: usize, bytes: usize) -> Self {
        Self { frames, bytes }
    }

    pub(crate) const fn standard() -> Self {
        Self::new(DEFAULT_DRIVER_WORK_FRAMES, DEFAULT_DRIVER_WORK_BYTES)
    }
}

pub(crate) enum ReadyFrameDrainPoll {
    Frame {
        frame: TransportFrame,
        budget_reached: bool,
    },
    Pending,
    Closed,
    ReceiveFailed(SignalFishError),
    DeadlineReached,
}

pub(crate) struct ReadyFrameDrain {
    first: Option<TransportFrame>,
    budget: ReadyFrameDrainBudget,
    frames: usize,
    bytes: usize,
}

impl ReadyFrameDrain {
    pub(crate) fn new(first: Option<TransportFrame>, budget: ReadyFrameDrainBudget) -> Self {
        Self {
            first,
            budget,
            frames: 0,
            bytes: 0,
        }
    }

    pub(crate) fn poll_next<T: Transport + ?Sized>(
        &mut self,
        transport: &mut T,
        cx: &mut Context<'_>,
        deadline_reached: bool,
    ) -> ReadyFrameDrainPoll {
        if deadline_reached {
            return ReadyFrameDrainPoll::DeadlineReached;
        }
        let frame = if let Some(frame) = self.first.take() {
            frame
        } else {
            match transport.poll_recv(cx) {
                Poll::Ready(Some(Ok(frame))) => frame,
                Poll::Ready(Some(Err(error))) => {
                    return ReadyFrameDrainPoll::ReceiveFailed(error);
                }
                Poll::Ready(None) => return ReadyFrameDrainPoll::Closed,
                Poll::Pending => return ReadyFrameDrainPoll::Pending,
            }
        };
        self.frames = self.frames.saturating_add(1);
        self.bytes = self.bytes.saturating_add(frame_payload_len(&frame));
        ReadyFrameDrainPoll::Frame {
            frame,
            budget_reached: self.frames >= self.budget.frames || self.bytes >= self.budget.bytes,
        }
    }
}

pub(crate) fn frame_payload_len(frame: &TransportFrame) -> usize {
    match frame {
        TransportFrame::Text(text) => text.len(),
        TransportFrame::Binary(bytes) => bytes.len(),
    }
}

fn format_close_reason(info: crate::transport::TransportCloseInfo) -> String {
    let initiator = if info.initiated_by_peer {
        "closed by server"
    } else {
        "closed by transport"
    };
    format!(
        "{initiator}: code={:?}, reason={:?}",
        info.code, info.reason
    )
}

pub(crate) fn close_reason<T: Transport + ?Sized>(transport: &T) -> Option<String> {
    transport.close_info().map(format_close_reason)
}

pub(crate) fn peer_close_reason<T: Transport + ?Sized>(transport: &T) -> Option<String> {
    transport
        .close_info()
        .filter(|info| info.initiated_by_peer)
        .map(format_close_reason)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::transport::TransportCloseInfo;
    use std::task::{Context, Poll};

    #[test]
    fn close_reason_labels_match_the_reported_initiator() {
        let cases = [
            (
                "peer-initiated close metadata keeps the established label",
                TransportCloseInfo {
                    code: Some(1000),
                    reason: Some("normal closure".into()),
                    clean: Some(true),
                    initiated_by_peer: true,
                },
                "closed by server: code=Some(1000), reason=Some(\"normal closure\")",
            ),
            (
                "locally-initiated metadata from a third-party transport is not mislabeled",
                TransportCloseInfo {
                    code: None,
                    reason: None,
                    clean: Some(false),
                    initiated_by_peer: false,
                },
                "closed by transport: code=None, reason=None",
            ),
        ];
        for (context, info, expected) in cases {
            assert_eq!(format_close_reason(info), expected, "{context}");
        }
    }

    #[test]
    fn standard_driver_budget_is_the_documented_64_frames_64_kib() {
        assert_eq!(DEFAULT_DRIVER_WORK_FRAMES, 64);
        assert_eq!(DEFAULT_DRIVER_WORK_BYTES, 65_536);
        let standard = ReadyFrameDrainBudget::standard();
        assert_eq!(standard.frames, 64);
        assert_eq!(standard.bytes, 65_536);
        let custom = ReadyFrameDrainBudget::new(3, 9);
        assert_eq!(custom.frames, 3);
        assert_eq!(custom.bytes, 9);
    }

    /// Minimal transport: a scripted receive queue plus close metadata.
    struct StubDrainTransport {
        frames: std::vec::Vec<Option<Result<TransportFrame, SignalFishError>>>,
        close: Option<TransportCloseInfo>,
    }

    impl StubDrainTransport {
        fn with_frames(frames: Vec<TransportFrame>) -> Self {
            Self {
                frames: frames.into_iter().map(|frame| Some(Ok(frame))).collect(),
                close: None,
            }
        }
    }

    impl Transport for StubDrainTransport {
        fn poll_send(
            &mut self,
            _cx: &mut Context<'_>,
            _frame: &mut Option<TransportFrame>,
        ) -> Poll<Result<(), SignalFishError>> {
            Poll::Ready(Ok(()))
        }

        fn poll_recv(
            &mut self,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<TransportFrame, SignalFishError>>> {
            Poll::Ready(self.frames.pop().flatten())
        }

        fn poll_close(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), SignalFishError>> {
            Poll::Ready(Ok(()))
        }

        fn abort(&mut self) {}

        fn close_info(&self) -> Option<TransportCloseInfo> {
            self.close.clone()
        }
    }

    fn noop_context() -> Context<'static> {
        let waker = std::task::Waker::noop();
        Context::from_waker(waker)
    }

    #[test]
    fn frame_budget_flags_reached_on_the_exact_last_admitted_frame() {
        let mut transport = StubDrainTransport::with_frames(vec![
            TransportFrame::Text("1".into()),
            TransportFrame::Text("2".into()),
            TransportFrame::Text("3".into()),
        ]);
        let mut drain = ReadyFrameDrain::new(None, ReadyFrameDrainBudget::new(3, 1_000));
        let mut cx = noop_context();
        let mut drained = 0;
        for expected_reached in [false, false, true] {
            let budget_reached = match drain.poll_next(&mut transport, &mut cx, false) {
                ReadyFrameDrainPoll::Frame { budget_reached, .. } => budget_reached,
                ReadyFrameDrainPoll::Pending => panic!("transport unexpectedly pending"),
                ReadyFrameDrainPoll::Closed => panic!("transport unexpectedly closed"),
                ReadyFrameDrainPoll::ReceiveFailed(_) => panic!("unexpected receive failure"),
                ReadyFrameDrainPoll::DeadlineReached => panic!("unexpected deadline"),
            };
            assert_eq!(budget_reached, expected_reached);
            drained += 1;
        }
        assert_eq!(drained, 3);
    }

    #[test]
    fn byte_budget_counts_exact_frame_payload_lengths() {
        let mut transport = StubDrainTransport::with_frames(vec![
            TransportFrame::Text("abcd".into()),
            TransportFrame::Binary(vec![9, 9, 9, 9]),
        ]);
        // Byte budget 7: the 4-byte text frame stays under it, the second
        // 4-byte frame crosses it (8 >= 7) and must be flagged.
        let mut drain = ReadyFrameDrain::new(None, ReadyFrameDrainBudget::new(1_000, 7));
        let mut cx = noop_context();
        let first_reached = match drain.poll_next(&mut transport, &mut cx, false) {
            ReadyFrameDrainPoll::Frame { budget_reached, .. } => budget_reached,
            _ => panic!("expected the first frame"),
        };
        assert!(!first_reached, "4 bytes must stay under a 7-byte budget");
        let second_reached = match drain.poll_next(&mut transport, &mut cx, false) {
            ReadyFrameDrainPoll::Frame { budget_reached, .. } => budget_reached,
            _ => panic!("expected the second frame"),
        };
        assert!(second_reached, "8 total bytes must cross the 7-byte budget");
    }

    #[test]
    fn close_reason_and_peer_filter_read_transport_metadata() {
        let peer_info = TransportCloseInfo {
            code: Some(1000),
            reason: Some("done".into()),
            clean: Some(true),
            initiated_by_peer: true,
        };
        let local_info = TransportCloseInfo {
            initiated_by_peer: false,
            ..peer_info.clone()
        };

        let mut peer_closed = StubDrainTransport::with_frames(Vec::new());
        peer_closed.close = Some(peer_info);
        assert!(close_reason(&peer_closed).is_some());
        assert!(peer_close_reason(&peer_closed).is_some());

        let mut locally_closed = StubDrainTransport::with_frames(Vec::new());
        locally_closed.close = Some(local_info);
        assert!(close_reason(&locally_closed).is_some());
        assert_eq!(peer_close_reason(&locally_closed), None);

        let mut open = StubDrainTransport::with_frames(Vec::new());
        assert_eq!(close_reason(&open), None);
        assert_eq!(peer_close_reason(&open), None);
    }
}
