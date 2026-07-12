//! Godot 4.5 `WebSocketPeer` transport for native and web exports.
//!
//! Godot owns the platform WebSocket implementation. This transport advances
//! it from [`Transport::poll_send`], [`Transport::poll_recv`], and
//! [`Transport::poll_close`], making it suitable for
//! [`SignalFishPollingClient`](crate::SignalFishPollingClient) in a Node's
//! `_process` callback. It contains no GDScript or Emscripten WebSocket FFI.

use std::fmt;
use std::task::{Context, Poll};

use godot::builtin::PackedByteArray;
use godot::classes::{web_socket_peer, WebSocketPeer};
use godot::global::Error;
use godot::obj::{Gd, NewGd};

use crate::error::SignalFishError;
use crate::transport::{Transport, TransportCloseInfo, TransportFrame};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeerState {
    Connecting,
    Open,
    Closing,
    Closed,
}

trait GodotWebSocketBackend {
    fn poll(&mut self);
    fn state(&self) -> PeerState;
    fn outbound_buffered_amount(&self) -> i32;
    fn send_text(&mut self, text: &str) -> Result<(), String>;
    fn send_binary(&mut self, bytes: Vec<u8>) -> Result<(), String>;
    fn available_packet_count(&self) -> i32;
    fn receive_packet(&mut self) -> Result<(Vec<u8>, bool), String>;
    fn close(&mut self);
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

    fn send_text(&mut self, text: &str) -> Result<(), String> {
        let result = std::ops::DerefMut::deref_mut(self).send_text(text);
        godot_result(result, "send_text")
    }

    fn send_binary(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        let result = std::ops::DerefMut::deref_mut(self).send(&PackedByteArray::from(bytes));
        godot_result(result, "send binary")
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

    fn close_code(&self) -> i32 {
        std::ops::Deref::deref(self).get_close_code()
    }

    fn close_reason(&self) -> String {
        std::ops::Deref::deref(self).get_close_reason().to_string()
    }
}

fn godot_result(result: Error, operation: &str) -> Result<(), String> {
    if result == Error::OK {
        Ok(())
    } else {
        Err(format!(
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
    ever_ready: bool,
    send_in_flight: bool,
    close_started: bool,
    terminal: bool,
    close_info: Option<TransportCloseInfo>,
}

impl fmt::Debug for GodotWebSocketTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GodotWebSocketTransport")
            .field("ever_ready", &self.ever_ready)
            .field("send_in_flight", &self.send_in_flight)
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
        let mut peer = WebSocketPeer::new_gd();
        let result = peer.connect_to_url(url);
        if result != Error::OK {
            return Err(SignalFishError::Io(std::io::Error::other(format!(
                "Godot WebSocketPeer connect_to_url failed with {result:?}"
            ))));
        }
        Ok(Self::from_peer(peer))
    }

    /// Wrap a Godot `WebSocketPeer` whose connection attempt has already begun.
    ///
    /// This supports callers that need to configure handshake headers,
    /// subprotocols, buffer sizes, or TLS options before `connect_to_url`.
    pub fn from_peer(peer: Gd<WebSocketPeer>) -> Self {
        Self::from_backend(Box::new(peer))
    }

    fn from_backend(backend: Box<dyn GodotWebSocketBackend>) -> Self {
        let ever_ready = backend.state() == PeerState::Open;
        Self {
            backend,
            ever_ready,
            send_in_flight: false,
            close_started: false,
            terminal: false,
            close_info: None,
        }
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
        self.send_in_flight = false;
        if self.ever_ready || self.close_started {
            Poll::Ready(None)
        } else {
            Poll::Ready(Some(Err(SignalFishError::TransportReceive(
                "Godot WebSocket connection closed before opening".to_string(),
            ))))
        }
    }
}

impl Transport for GodotWebSocketTransport {
    fn poll_send(
        &mut self,
        _cx: &mut Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> Poll<Result<(), SignalFishError>> {
        match self.advance() {
            PeerState::Connecting => return Poll::Pending,
            PeerState::Closing | PeerState::Closed => {
                self.record_close();
                self.terminal = true;
                self.send_in_flight = false;
                return Poll::Ready(Err(SignalFishError::TransportClosed));
            }
            PeerState::Open => {}
        }

        if self.send_in_flight {
            if self.backend.outbound_buffered_amount() == 0 {
                self.send_in_flight = false;
                return Poll::Ready(Ok(()));
            }
            return Poll::Pending;
        }

        let Some(frame) = frame.take() else {
            return Poll::Ready(Ok(()));
        };
        let result = match frame {
            TransportFrame::Text(text) => self.backend.send_text(&text),
            TransportFrame::Binary(bytes) => self.backend.send_binary(bytes),
        };
        if let Err(error) = result {
            return Poll::Ready(Err(SignalFishError::TransportSend(error)));
        }

        if self.backend.outbound_buffered_amount() == 0 {
            Poll::Ready(Ok(()))
        } else {
            self.send_in_flight = true;
            Poll::Pending
        }
    }

    fn poll_recv(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<TransportFrame, SignalFishError>>> {
        match self.advance() {
            PeerState::Connecting | PeerState::Closing => return Poll::Pending,
            PeerState::Closed => return self.closed_receive(),
            PeerState::Open => {}
        }

        if self.backend.available_packet_count() <= 0 {
            return Poll::Pending;
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
            self.record_close();
            self.terminal = true;
            self.send_in_flight = false;
            return Poll::Ready(Ok(()));
        }
        if !self.close_started {
            self.backend.close();
            self.close_started = true;
        }
        if self.advance() == PeerState::Closed {
            self.record_close();
            self.terminal = true;
            self.send_in_flight = false;
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
    use std::collections::VecDeque;

    use super::*;

    #[derive(Debug)]
    struct FakeBackend {
        states: VecDeque<PeerState>,
        state: PeerState,
        buffered: i32,
        buffered_after_poll: VecDeque<i32>,
        packets: VecDeque<Result<(Vec<u8>, bool), String>>,
        sent: Vec<TransportFrame>,
        send_error: Option<String>,
        close_calls: usize,
        close_code: i32,
        close_reason: String,
    }

    impl FakeBackend {
        fn new(state: PeerState) -> Self {
            Self {
                states: VecDeque::new(),
                state,
                buffered: 0,
                buffered_after_poll: VecDeque::new(),
                packets: VecDeque::new(),
                sent: Vec::new(),
                send_error: None,
                close_calls: 0,
                close_code: -1,
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
        }

        fn state(&self) -> PeerState {
            self.state
        }

        fn outbound_buffered_amount(&self) -> i32 {
            self.buffered
        }

        fn send_text(&mut self, text: &str) -> Result<(), String> {
            if let Some(error) = self.send_error.take() {
                return Err(error);
            }
            self.sent.push(TransportFrame::Text(text.to_string()));
            Ok(())
        }

        fn send_binary(&mut self, bytes: Vec<u8>) -> Result<(), String> {
            if let Some(error) = self.send_error.take() {
                return Err(error);
            }
            self.sent.push(TransportFrame::Binary(bytes));
            Ok(())
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
    fn accepted_frame_stays_in_flight_until_godot_buffer_drains() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend.buffered = 7;
        backend.buffered_after_poll.extend([7, 0]);
        let mut transport = GodotWebSocketTransport::from_backend(Box::new(backend));
        let mut frame = Some(TransportFrame::Binary(vec![1, 2, 3]));

        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Pending
        ));
        assert!(frame.is_none());
        assert!(transport.send_in_flight);
        assert!(matches!(
            transport.poll_send(&mut context(), &mut frame),
            Poll::Ready(Ok(()))
        ));
        assert!(frame.is_none());
        assert!(!transport.send_in_flight);
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
    fn handshake_failure_is_not_misreported_as_clean_peer_close() {
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
    }

    #[test]
    fn close_is_polled_to_completion_and_idempotent() {
        let mut backend = FakeBackend::new(PeerState::Open);
        backend
            .states
            .extend([PeerState::Open, PeerState::Closing, PeerState::Closed]);
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
}
