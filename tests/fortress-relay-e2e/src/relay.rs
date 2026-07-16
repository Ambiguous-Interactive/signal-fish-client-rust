use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use fortress_rollback::network::codec;
use fortress_rollback::{Message, NonBlockingSocket};
use signal_fish_client::protocol::GameDataEncoding;
use uuid::Uuid;

const DESTINATION_BYTES: usize = 16;
const MAX_OUTBOUND_FRAMES: usize = 256;
const MAX_INBOUND_FRAMES: usize = 256;
const MAX_INBOUND_PER_POLL: usize = 256;

#[derive(Debug, Clone, Copy, Default)]
pub struct RelayCounters {
    pub enqueued_outbound: u64,
    pub accepted_inbound: u64,
    pub malformed_inbound: u64,
    pub wrong_destination: u64,
    pub unknown_sender: u64,
    pub outbound_overflow: u64,
    pub inbound_overflow: u64,
    pub encode_failures: u64,
}

pub struct InboundRelayFrame<'a> {
    pub local: Uuid,
    pub known_remote: Uuid,
    pub from: Uuid,
    pub encoding: GameDataEncoding,
    pub seq: Option<u64>,
    pub epoch: Option<u32>,
    pub payload: &'a [u8],
}

#[derive(Debug, Default)]
struct Shared {
    outbound: VecDeque<Vec<u8>>,
    inbound: VecDeque<(Uuid, Message)>,
    counters: RelayCounters,
}

/// Fortress's UDP-like socket boundary backed by Signal Fish binary relay frames.
///
/// `send_to` only serializes and admits work to a local FIFO. The owner drains
/// that FIFO into `SignalFishPollingClient`, retaining the head when client-side
/// admission reports backpressure. This is the non-blocking ownership contract
/// required by Fortress and avoids the issue-242 stop-and-wait failure mode.
#[derive(Debug, Clone, Default)]
pub struct RelaySocket {
    shared: Arc<Mutex<Shared>>,
}

impl RelaySocket {
    pub fn take_outbound(&self) -> Option<Vec<u8>> {
        self.shared.lock().ok()?.outbound.pop_front()
    }

    pub fn return_outbound_front(&self, frame: Vec<u8>) {
        if let Ok(mut shared) = self.shared.lock() {
            if shared.outbound.len() >= MAX_OUTBOUND_FRAMES {
                shared.outbound.pop_back();
                shared.counters.outbound_overflow =
                    shared.counters.outbound_overflow.saturating_add(1);
            }
            shared.outbound.push_front(frame);
        }
    }

    pub fn outbound_depth(&self) -> usize {
        self.shared
            .lock()
            .map_or(usize::MAX, |shared| shared.outbound.len())
    }

    pub fn counters(&self) -> RelayCounters {
        self.shared
            .lock()
            .map_or_else(|_| RelayCounters::default(), |shared| shared.counters)
    }

    pub fn admit_inbound(&self, frame: InboundRelayFrame<'_>) {
        let InboundRelayFrame {
            local,
            known_remote,
            from,
            encoding,
            seq,
            epoch,
            payload,
        } = frame;
        let Ok(mut shared) = self.shared.lock() else {
            return;
        };
        if encoding != GameDataEncoding::MessagePack
            || seq.is_none_or(|value| value == 0)
            || epoch.is_none_or(|value| value == 0)
            || payload.len() <= DESTINATION_BYTES
        {
            shared.counters.malformed_inbound = shared.counters.malformed_inbound.saturating_add(1);
            return;
        }
        if from != known_remote {
            shared.counters.unknown_sender = shared.counters.unknown_sender.saturating_add(1);
            return;
        }

        let Some(destination_bytes) = payload.get(..DESTINATION_BYTES) else {
            shared.counters.malformed_inbound = shared.counters.malformed_inbound.saturating_add(1);
            return;
        };
        let Ok(destination) = Uuid::from_slice(destination_bytes) else {
            shared.counters.malformed_inbound = shared.counters.malformed_inbound.saturating_add(1);
            return;
        };
        if destination != local {
            shared.counters.wrong_destination = shared.counters.wrong_destination.saturating_add(1);
            return;
        }

        let Some(message_bytes) = payload.get(DESTINATION_BYTES..) else {
            shared.counters.malformed_inbound = shared.counters.malformed_inbound.saturating_add(1);
            return;
        };
        match codec::decode_message(message_bytes) {
            Ok((message, consumed)) if consumed == message_bytes.len() => {
                if shared.inbound.len() >= MAX_INBOUND_FRAMES {
                    shared.counters.inbound_overflow =
                        shared.counters.inbound_overflow.saturating_add(1);
                } else {
                    shared.inbound.push_back((from, message));
                    shared.counters.accepted_inbound =
                        shared.counters.accepted_inbound.saturating_add(1);
                }
            }
            Ok(_) | Err(_) => {
                shared.counters.malformed_inbound =
                    shared.counters.malformed_inbound.saturating_add(1);
            }
        }
    }
}

impl NonBlockingSocket<Uuid> for RelaySocket {
    fn send_to(&mut self, message: &Message, destination: &Uuid) {
        let encoded = match codec::encode(message) {
            Ok(encoded) => encoded,
            Err(_) => {
                if let Ok(mut shared) = self.shared.lock() {
                    shared.counters.encode_failures =
                        shared.counters.encode_failures.saturating_add(1);
                }
                return;
            }
        };
        let mut payload = Vec::with_capacity(DESTINATION_BYTES.saturating_add(encoded.len()));
        payload.extend_from_slice(destination.as_bytes());
        payload.extend_from_slice(&encoded);
        self.enqueue_outbound(payload);
    }

    fn receive_all_messages(&mut self) -> Vec<(Uuid, Message)> {
        let Ok(mut shared) = self.shared.lock() else {
            return Vec::new();
        };
        let count = shared.inbound.len().min(MAX_INBOUND_PER_POLL);
        shared.inbound.drain(..count).collect()
    }
}

impl RelaySocket {
    fn enqueue_outbound(&self, payload: Vec<u8>) {
        if let Ok(mut shared) = self.shared.lock() {
            if shared.outbound.len() >= MAX_OUTBOUND_FRAMES {
                shared.counters.outbound_overflow =
                    shared.counters.outbound_overflow.saturating_add(1);
                return;
            }
            shared.outbound.push_back(payload);
            shared.counters.enqueued_outbound = shared.counters.enqueued_outbound.saturating_add(1);
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_metadata_sender_destination_and_malformed_message() {
        let local = Uuid::new_v4();
        let remote = Uuid::new_v4();
        let mut local_payload = local.as_bytes().to_vec();
        local_payload.extend([0xAA, 0xBB]);
        let socket = RelaySocket::default();

        let frame = |from, encoding, seq, payload| InboundRelayFrame {
            local,
            known_remote: remote,
            from,
            encoding,
            seq,
            epoch: Some(1),
            payload,
        };
        socket.admit_inbound(frame(
            remote,
            GameDataEncoding::Json,
            Some(1),
            &local_payload,
        ));
        socket.admit_inbound(frame(
            remote,
            GameDataEncoding::MessagePack,
            Some(0),
            &local_payload,
        ));
        socket.admit_inbound(frame(
            Uuid::new_v4(),
            GameDataEncoding::MessagePack,
            Some(1),
            &local_payload,
        ));
        let mut wrong_destination = Uuid::new_v4().as_bytes().to_vec();
        wrong_destination.extend([0xAA, 0xBB]);
        socket.admit_inbound(frame(
            remote,
            GameDataEncoding::MessagePack,
            Some(1),
            &wrong_destination,
        ));
        socket.admit_inbound(frame(
            remote,
            GameDataEncoding::MessagePack,
            Some(1),
            &local_payload,
        ));

        let counters = socket.counters();
        assert_eq!(counters.malformed_inbound, 3);
        assert_eq!(counters.unknown_sender, 1);
        assert_eq!(counters.wrong_destination, 1);
        assert_eq!(counters.accepted_inbound, 0);
    }

    #[test]
    fn refused_outbound_can_be_restored_without_reordering() {
        let socket = RelaySocket::default();
        let first = vec![1];
        let second = vec![2];
        socket.return_outbound_front(second.clone());
        socket.return_outbound_front(first.clone());
        let refused = socket.take_outbound().expect("first");
        socket.return_outbound_front(refused);
        assert_eq!(socket.take_outbound(), Some(first));
        assert_eq!(socket.take_outbound(), Some(second));
        assert_eq!(socket.outbound_depth(), 0);
    }

    #[test]
    fn outbound_admission_is_bounded_and_overflow_is_observable() {
        let socket = RelaySocket::default();
        for byte in 0..MAX_OUTBOUND_FRAMES {
            socket.enqueue_outbound(vec![byte as u8]);
        }
        socket.enqueue_outbound(vec![0xFF]);
        assert_eq!(socket.outbound_depth(), MAX_OUTBOUND_FRAMES);
        assert_eq!(
            socket.counters().enqueued_outbound,
            MAX_OUTBOUND_FRAMES as u64
        );
        assert_eq!(socket.counters().outbound_overflow, 1);
    }
}
