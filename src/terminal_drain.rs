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
mod tests {
    use super::*;
    use crate::transport::TransportCloseInfo;

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
}
