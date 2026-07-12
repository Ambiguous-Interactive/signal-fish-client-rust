//! Adversarial regression tests for protocol-version negotiation across
//! reconnects (no silent downgrade of an active v3 session) and for transport
//! robustness (send errors mid-flight, sends after disconnect, out-of-order
//! server messages). These pin behaviors that are easy to regress and would
//! silently break mesh sessions after a network blip.
#![cfg(feature = "tokio-runtime")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::type_complexity,
    dead_code
)]

#[allow(dead_code)]
mod common;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use signal_fish_client::protocol::{LobbyState, ReconnectedPayload, ServerMessage};
use signal_fish_client::transport::TransportFrame;
use signal_fish_client::{
    SignalFishClient, SignalFishConfig, SignalFishError, SignalFishEvent, Transport,
};

use common::{
    authenticated_json, game_data_json, protocol_info_json, protocol_info_payload,
    room_joined_json, wait_for_sent_len,
};

fn start_client(
    incoming: Vec<Option<Result<String, SignalFishError>>>,
) -> (
    SignalFishClient,
    tokio::sync::mpsc::Receiver<SignalFishEvent>,
    Arc<StdMutex<Vec<String>>>,
    Arc<AtomicBool>,
) {
    let (transport, sent, closed) = common::MockTransport::new(incoming);
    let config = SignalFishConfig::new("mb_audit").enable_mesh();
    let (client, events) = SignalFishClient::start(transport, config);
    (client, events, sent, closed)
}

async fn drain_until_authenticated(rx: &mut tokio::sync::mpsc::Receiver<SignalFishEvent>) {
    loop {
        if matches!(
            rx.recv().await.expect("event"),
            SignalFishEvent::Authenticated { .. }
        ) {
            break;
        }
    }
}

async fn drain_until_reconnected(rx: &mut tokio::sync::mpsc::Receiver<SignalFishEvent>) {
    loop {
        if matches!(
            rx.recv().await.expect("event"),
            SignalFishEvent::Reconnected { .. }
        ) {
            break;
        }
    }
}

async fn drain_until_protocol_info(rx: &mut tokio::sync::mpsc::Receiver<SignalFishEvent>) {
    loop {
        if matches!(
            rx.recv().await.expect("event"),
            SignalFishEvent::ProtocolInfo { .. }
        ) {
            break;
        }
    }
}

/// Build a `Reconnected` JSON whose `missed_events` is an arbitrary list.
fn reconnected_with_missed(missed: Vec<ServerMessage>) -> String {
    let payload = ReconnectedPayload {
        room_id: uuid::Uuid::from_u128(100),
        room_code: "RECON1".into(),
        player_id: uuid::Uuid::from_u128(200),
        game_name: "recon-game".into(),
        max_players: 6,
        supports_authority: false,
        current_players: vec![],
        is_authority: true,
        lobby_state: LobbyState::Waiting,
        ready_players: vec![],
        relay_type: "tcp".into(),
        current_spectators: vec![],
        ice_servers: vec![],
        missed_events: missed,
        replay: None,
        sender_watermarks: vec![],
        reconnection_token: None,
    };
    serde_json::to_string(&ServerMessage::Reconnected(Box::new(payload))).unwrap()
}

fn protocol_info_msg(version: Option<u16>) -> ServerMessage {
    ServerMessage::ProtocolInfo(protocol_info_payload(version))
}

// ════════════════════════════════════════════════════════════════════
// Reconnect must never silently downgrade an active v3 negotiation
// ════════════════════════════════════════════════════════════════════

/// A reconnect whose `missed_events` contain a v2 `ProtocolInfo` (no version)
/// must NOT downgrade an already-negotiated v3 session.
#[tokio::test]
async fn reconnect_v2_protocol_info_does_not_downgrade_active_v3() {
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(protocol_info_json(Some(3)))), // negotiate v3
        Some(Ok(reconnected_with_missed(vec![protocol_info_msg(None)]))),
    ]);
    drain_until_authenticated(&mut events).await;
    drain_until_protocol_info(&mut events).await;
    assert_eq!(client.negotiated_protocol_version(), Some(3));

    drain_until_reconnected(&mut events).await;

    assert_eq!(
        client.negotiated_protocol_version(),
        Some(3),
        "v2 ProtocolInfo in missed_events silently downgraded an active v3 session"
    );
    assert!(client.supports_mesh());
    client
        .send_offer(uuid::Uuid::from_u128(9), "sdp")
        .expect("send_offer must still work after reconnect blip");
    client.shutdown().await;
}

/// Multiple `ProtocolInfo` in `missed_events` — the last versioned one wins.
#[tokio::test]
async fn reconnect_multiple_protocol_info_last_wins() {
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(reconnected_with_missed(vec![
            protocol_info_msg(Some(3)),
            protocol_info_msg(Some(4)),
        ]))),
    ]);
    drain_until_authenticated(&mut events).await;
    drain_until_reconnected(&mut events).await;

    assert_eq!(
        client.negotiated_protocol_version(),
        Some(4),
        "expected last ProtocolInfo (v4) to win"
    );
    client.shutdown().await;
}

/// A versioned `ProtocolInfo` followed by a v2 (`None`) one: the trailing v2
/// must NOT clobber the earlier version (a `None` is skipped, not stored as 0).
#[tokio::test]
async fn reconnect_versioned_then_v2_keeps_version() {
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(reconnected_with_missed(vec![
            protocol_info_msg(Some(3)),
            protocol_info_msg(None),
        ]))),
    ]);
    drain_until_authenticated(&mut events).await;
    drain_until_reconnected(&mut events).await;

    assert_eq!(
        client.negotiated_protocol_version(),
        Some(3),
        "trailing v2 ProtocolInfo clobbered an earlier versioned one"
    );
    client.shutdown().await;
}

/// The most important case: a `Reconnected` carrying NO `ProtocolInfo` at all
/// must PRESERVE the prior v3 negotiation. A downgrade here would silently
/// break the mesh after a network blip.
#[tokio::test]
async fn reconnect_without_protocol_info_preserves_prior_v3() {
    let (mut client, mut events, sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(protocol_info_json(Some(3)))), // negotiate v3 first
        Some(Ok(reconnected_with_missed(vec![]))), // reconnect, NO ProtocolInfo
    ]);
    drain_until_authenticated(&mut events).await;
    drain_until_protocol_info(&mut events).await;
    assert_eq!(client.negotiated_protocol_version(), Some(3));

    drain_until_reconnected(&mut events).await;

    assert_eq!(
        client.negotiated_protocol_version(),
        Some(3),
        "reconnect with no ProtocolInfo must not wipe the active v3 negotiation"
    );
    assert!(client.supports_mesh());
    client
        .send_offer(uuid::Uuid::from_u128(9), "sdp")
        .expect("mesh must survive a reconnect that omits ProtocolInfo");

    wait_for_sent_len(&sent, 2).await;
    let signal_sent = sent.lock().unwrap().iter().any(|m| m.contains("Signal"));
    assert!(signal_sent, "Signal should have reached the wire");
    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// Transport robustness
// ════════════════════════════════════════════════════════════════════

/// A transport that errors on the Nth `send()` call (1-indexed).
struct SendErrorTransport {
    incoming: VecDeque<Option<Result<String, SignalFishError>>>,
    sent: Arc<StdMutex<Vec<String>>>,
    closed: Arc<AtomicBool>,
    send_count: usize,
    error_on_send: usize,
}

impl SendErrorTransport {
    fn new(
        incoming: Vec<Option<Result<String, SignalFishError>>>,
        error_on_send: usize,
    ) -> (Self, Arc<StdMutex<Vec<String>>>, Arc<AtomicBool>) {
        let sent = Arc::new(StdMutex::new(Vec::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let t = Self {
            incoming: VecDeque::from(incoming),
            sent: Arc::clone(&sent),
            closed: Arc::clone(&closed),
            send_count: 0,
            error_on_send,
        };
        (t, sent, closed)
    }
}

impl Transport for SendErrorTransport {
    fn poll_send(
        &mut self,
        _cx: &mut std::task::Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        self.send_count += 1;
        if self.send_count == self.error_on_send {
            return std::task::Poll::Ready(Err(SignalFishError::TransportSend("send boom".into())));
        }
        if let Some(frame) = frame.take() {
            let TransportFrame::Text(message) = frame else {
                panic!("test mock expected an outbound text frame");
            };
            self.sent.lock().unwrap().push(message);
        }
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_recv(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<TransportFrame, SignalFishError>>> {
        if let Some(item) = self.incoming.pop_front() {
            std::task::Poll::Ready(item.map(|result| result.map(TransportFrame::Text)))
        } else {
            std::task::Poll::Pending
        }
    }

    fn poll_close(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), SignalFishError>> {
        self.closed.store(true, Ordering::Relaxed);
        std::task::Poll::Ready(Ok(()))
    }
}

/// A `send()` error mid-flight must emit `Disconnected` AND clear session state.
#[tokio::test]
async fn send_error_midflight_disconnects_and_clears_state() {
    // send #1 = Authenticate (ok), send #2 = our ping (errors).
    let (transport, _sent, _closed) = SendErrorTransport::new(
        vec![Some(Ok(authenticated_json())), Some(Ok(room_joined_json()))],
        2,
    );
    let config = SignalFishConfig::new("mb_audit").enable_mesh();
    let (mut client, mut events) = SignalFishClient::start(transport, config);

    drain_until_authenticated(&mut events).await;
    loop {
        if matches!(
            events.recv().await.expect("event"),
            SignalFishEvent::RoomJoined { .. }
        ) {
            break;
        }
    }
    assert!(client.is_authenticated());
    assert!(client.current_room_id().await.is_some());

    client.ping().expect("ping queued");

    let mut saw_disconnect = false;
    loop {
        match events.recv().await {
            Some(SignalFishEvent::Disconnected { reason, .. }) => {
                assert!(
                    reason.as_deref().unwrap_or("").contains("send"),
                    "reason should mention the send error: {reason:?}"
                );
                saw_disconnect = true;
                break;
            }
            Some(_) => {}
            None => break,
        }
    }
    assert!(saw_disconnect, "send error must emit Disconnected");

    assert!(!client.is_connected(), "is_connected must be false");
    assert!(!client.is_authenticated(), "authenticated must be cleared");
    assert!(client.current_room_id().await.is_none(), "room cleared");
    assert!(client.current_player_id().await.is_none(), "player cleared");

    assert!(matches!(client.ping(), Err(SignalFishError::NotConnected)));
    client.shutdown().await;
}

/// v3 sends after a disconnect return a clean error, never panic.
#[tokio::test]
async fn v3_send_after_disconnect_does_not_panic() {
    let (transport, _sent, _closed) = SendErrorTransport::new(
        vec![
            Some(Ok(authenticated_json())),
            Some(Ok(protocol_info_json(Some(3)))),
        ],
        2,
    );
    let config = SignalFishConfig::new("mb_audit").enable_mesh();
    let (mut client, mut events) = SignalFishClient::start(transport, config);

    drain_until_authenticated(&mut events).await;
    drain_until_protocol_info(&mut events).await;
    assert!(client.supports_mesh());

    client.ping().expect("queued");
    loop {
        match events.recv().await {
            Some(SignalFishEvent::Disconnected { .. }) | None => break,
            _ => {}
        }
    }

    let r = client.send_offer(uuid::Uuid::from_u128(2), "sdp");
    assert!(
        matches!(
            r,
            Err(SignalFishError::NotConnected) | Err(SignalFishError::ProtocolUnsupported { .. })
        ),
        "expected clean error, got {r:?}"
    );
    client.shutdown().await;
}

/// Out-of-order / post-close server messages must not panic.
#[tokio::test]
async fn out_of_order_and_post_close_messages_do_not_panic() {
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(game_data_json(
            uuid::Uuid::from_u128(7),
            serde_json::json!({"k": "v"}),
        ))),
        Some(Ok(authenticated_json())),
        Some(Ok(common::room_left_json())),
        None, // clean close
    ]);

    let mut saw_disconnect = false;
    while let Some(ev) = events.recv().await {
        if matches!(ev, SignalFishEvent::Disconnected { .. }) {
            saw_disconnect = true;
        }
    }
    assert!(saw_disconnect || !client.is_connected());
    assert!(!client.is_connected());
    client.shutdown().await;
}
