//! Adversarial regression tests for protocol-version negotiation across
//! reconnects (no silent downgrade of an active v3 session) and for transport
//! robustness (send errors mid-flight, sends after disconnect, out-of-order
//! server messages). These pin behaviors that are easy to regress and would
//! silently break mesh sessions after a network blip.
#![cfg(feature = "tokio-runtime")]
#![allow(clippy::arithmetic_side_effects)]
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

use signal_fish_client::protocol::{
    ClientMessage, LobbyState, PlayerInfo, ReconnectedPayload, ReplayStatus, SenderWatermark,
    ServerMessage, SessionPeer, SessionPlanPayload, Topology, TransportKind,
};
use signal_fish_client::transport::TransportFrame;
use signal_fish_client::{
    JoinRoomParams, SignalFishClient, SignalFishConfig, SignalFishError, SignalFishEvent, Transport,
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

async fn drain_until_violation(rx: &mut tokio::sync::mpsc::Receiver<SignalFishEvent>) {
    loop {
        if matches!(
            rx.recv().await.expect("event"),
            SignalFishEvent::ProtocolViolation { .. }
        ) {
            break;
        }
    }
}

/// Build a `Reconnected` JSON whose `missed_events` is an arbitrary list.
fn reconnected_with_missed(missed: Vec<ServerMessage>) -> String {
    reconnected_with(missed, ReplayStatus::Complete)
}

/// Build a `Reconnected` JSON with an arbitrary `missed_events` list and
/// caller-chosen `replay` status.
fn reconnected_with(missed: Vec<ServerMessage>, replay: ReplayStatus) -> String {
    let payload = ReconnectedPayload {
        room_id: uuid::Uuid::from_u128(100),
        room_code: "RECON1".into(),
        player_id: uuid::Uuid::from_u128(200),
        game_name: "recon-game".into(),
        max_players: 6,
        supports_authority: false,
        current_players: [200, 9]
            .into_iter()
            .map(|id| PlayerInfo {
                id: uuid::Uuid::from_u128(id),
                name: format!("player-{id}"),
                is_authority: id == 200,
                is_ready: true,
                connected_at: "2026-01-01T00:00:00Z".into(),
                connection_info: None,
                epoch: Some(1),
                seq: Some(0),
            })
            .collect(),
        is_authority: true,
        lobby_state: LobbyState::Finalized,
        ready_players: vec![],
        relay_type: "tcp".into(),
        current_spectators: vec![],
        ice_servers: vec![],
        missed_events: missed,
        replay: Some(replay),
        sender_watermarks: [200, 9]
            .into_iter()
            .map(|id| SenderWatermark {
                player_id: uuid::Uuid::from_u128(id),
                epoch: 1,
                seq: 0,
            })
            .collect(),
        reconnection_token: Some("rotated".into()),
    };
    serde_json::to_string(&ServerMessage::Reconnected(Box::new(payload))).unwrap()
}

fn protocol_info_msg(version: Option<u16>) -> ServerMessage {
    ServerMessage::ProtocolInfo(protocol_info_payload(version))
}

fn session_plan_msg() -> ServerMessage {
    ServerMessage::SessionPlan(Box::new(SessionPlanPayload {
        generation: Some(uuid::Uuid::from_u128(12)),
        topology: Topology::Mesh,
        transport: TransportKind::WebRtc,
        host: None,
        direct_endpoint: None,
        peers: vec![SessionPeer {
            player_id: uuid::Uuid::from_u128(9),
            player_name: "peer".into(),
            is_authority: false,
            initiate: true,
        }],
        ice_servers: vec![],
        fallback: TransportKind::Relay,
    }))
}

// ════════════════════════════════════════════════════════════════════
// Reconnect must never silently downgrade an active v3 negotiation
// ════════════════════════════════════════════════════════════════════

/// A reconnect cannot carry `ProtocolInfo`; rejecting it must not downgrade
/// the already-negotiated connection.
#[tokio::test]
async fn reconnect_rejects_protocol_info_without_downgrading_active_v3() {
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(protocol_info_json(Some(3)))), // negotiate v3
        Some(Ok(reconnected_with_missed(vec![protocol_info_msg(None)]))),
    ]);
    drain_until_authenticated(&mut events).await;
    drain_until_protocol_info(&mut events).await;
    client
        .reconnect(
            uuid::Uuid::from_u128(200),
            uuid::Uuid::from_u128(100),
            "submitted-token".into(),
        )
        .expect("authenticated reconnect must queue");
    assert_eq!(client.negotiated_protocol_version(), Some(3));

    drain_until_violation(&mut events).await;

    assert_eq!(
        client.negotiated_protocol_version(),
        Some(3),
        "v2 ProtocolInfo in missed_events silently downgraded an active v3 session"
    );
    assert!(client.supports_mesh());
    client.shutdown().await;
}

/// The vendored AsyncAPI pins exactly three `ReplayStatus` wire tokens
/// (`complete` / `truncated` / `unavailable`). Every value must decode and
/// surface verbatim on the `Reconnected` event; previously only `complete`
/// was ever exercised anywhere.
#[tokio::test]
async fn reconnected_replay_status_decodes_and_surfaces_every_wire_value() {
    for (status, token) in [
        (ReplayStatus::Complete, "complete"),
        (ReplayStatus::Truncated, "truncated"),
        (ReplayStatus::Unavailable, "unavailable"),
    ] {
        // Wire token, both directions.
        assert_eq!(
            serde_json::to_string(&status)
                .expect("ReplayStatus always serializes")
                .as_str(),
            format!("\"{token}\""),
            "serialize direction for {token}"
        );
        assert_eq!(
            serde_json::from_str::<ReplayStatus>(&format!("\"{token}\"")).ok(),
            Some(status),
            "decode direction for {token}"
        );

        let (mut client, mut events, _sent, _closed) = start_client(vec![
            Some(Ok(authenticated_json())),
            Some(Ok(protocol_info_json(Some(3)))),
            Some(Ok(reconnected_with(vec![], status))),
        ]);
        drain_until_authenticated(&mut events).await;
        drain_until_protocol_info(&mut events).await;
        client
            .reconnect(
                uuid::Uuid::from_u128(200),
                uuid::Uuid::from_u128(100),
                "submitted-token".into(),
            )
            .expect("authenticated reconnect must queue");
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
            .await
            .expect("timed out waiting for the Reconnected event")
            .expect("event channel closed");
        assert!(
            matches!(&event, SignalFishEvent::Reconnected { replay: Some(replay), .. } if *replay == status),
            "expected replay {status:?} ({token}), got {event:?}"
        );
        client.shutdown().await;
    }
}

/// Multiple replayed negotiation messages are rejected as one invalid reconnect.
#[tokio::test]
async fn reconnect_multiple_protocol_info_is_rejected() {
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(protocol_info_json(Some(3)))),
        Some(Ok(reconnected_with_missed(vec![
            protocol_info_msg(Some(3)),
            protocol_info_msg(Some(4)),
        ]))),
    ]);
    drain_until_authenticated(&mut events).await;
    drain_until_protocol_info(&mut events).await;
    client
        .reconnect(
            uuid::Uuid::from_u128(200),
            uuid::Uuid::from_u128(100),
            "submitted-token".into(),
        )
        .expect("authenticated reconnect must queue");
    drain_until_violation(&mut events).await;

    assert_eq!(
        client.negotiated_protocol_version(),
        Some(3),
        "invalid replayed ProtocolInfo must not replace outer negotiation"
    );
    client.shutdown().await;
}

/// A versioned `ProtocolInfo` followed by a v2 (`None`) one: the trailing v2
/// must NOT clobber the earlier version (a `None` is skipped, not stored as 0).
#[tokio::test]
async fn reconnect_versioned_then_v2_keeps_version() {
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(protocol_info_json(Some(3)))),
        Some(Ok(reconnected_with_missed(vec![
            protocol_info_msg(Some(3)),
            protocol_info_msg(None),
        ]))),
    ]);
    drain_until_authenticated(&mut events).await;
    drain_until_protocol_info(&mut events).await;
    client
        .reconnect(
            uuid::Uuid::from_u128(200),
            uuid::Uuid::from_u128(100),
            "submitted-token".into(),
        )
        .expect("authenticated reconnect must queue");
    drain_until_violation(&mut events).await;

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
        Some(Ok(reconnected_with_missed(vec![]))),
        Some(Ok(serde_json::to_string(&session_plan_msg()).unwrap())),
    ]);
    drain_until_authenticated(&mut events).await;
    drain_until_protocol_info(&mut events).await;
    client
        .reconnect(
            uuid::Uuid::from_u128(200),
            uuid::Uuid::from_u128(100),
            "submitted-token".into(),
        )
        .expect("authenticated reconnect must queue");
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

    wait_for_sent_len(&sent, 3).await;
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
    sent_join_room: bool,
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
            sent_join_room: false,
        };
        (t, sent, closed)
    }
}

impl Transport for SendErrorTransport {
    fn abort(&mut self) {}

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
            if serde_json::from_str::<ClientMessage>(&message)
                .is_ok_and(|message| matches!(message, ClientMessage::JoinRoom { .. }))
            {
                self.sent_join_room = true;
            }
            self.sent.lock().unwrap().push(message);
        }
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_recv(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<TransportFrame, SignalFishError>>> {
        // Room responses follow admitted joins on this fixture.
        if !self.sent_join_room {
            let front_is_room_joined = match self.incoming.front() {
                Some(Some(Ok(json))) => serde_json::from_str::<ServerMessage>(json)
                    .is_ok_and(|message| matches!(message, ServerMessage::RoomJoined(_))),
                _ => false,
            };
            if front_is_room_joined {
                return std::task::Poll::Pending;
            }
        }
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
    // send #1 = Authenticate, send #2 = JoinRoom, send #3 = our ping (errors).
    let (transport, _sent, _closed) = SendErrorTransport::new(
        vec![
            Some(Ok(authenticated_json())),
            Some(Ok(protocol_info_json(None))),
            Some(Ok(room_joined_json())),
        ],
        3,
    );
    let config = SignalFishConfig::new("mb_audit").enable_mesh();
    let (mut client, mut events) = SignalFishClient::start(transport, config);
    common::wait_for_authentication(&client).await;
    client
        .join_room(JoinRoomParams::new("test-game", "Alice"))
        .expect("room fixture must follow an admitted join");

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
    let (transport, _sent, _closed) = common::MockTransport::new_ungated(vec![
        Some(Ok(game_data_json(
            uuid::Uuid::from_u128(7),
            serde_json::json!({"k": "v"}),
        ))),
        Some(Ok(authenticated_json())),
        Some(Ok(common::room_left_json())),
        None, // clean close
    ]);
    let config = SignalFishConfig::new("mb_audit").enable_mesh();
    let (mut client, mut events) = SignalFishClient::start(transport, config);

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
