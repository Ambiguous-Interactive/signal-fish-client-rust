#![cfg(feature = "tokio-runtime")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing
)]
//! Integration-style client tests for the Signal Fish Client.
//!
//! Uses the shared `MockTransport` from `tests/common` to script server
//! responses and verify that `SignalFishClient` processes them correctly,
//! including state transitions, API message generation, and event delivery.

mod common;

use std::collections::VecDeque;

use async_trait::async_trait;
use signal_fish_client::protocol::{
    ClientMessage, ConnectionInfo, GameDataEncoding, RelayTransport, ServerMessage, TransportKind,
};
use signal_fish_client::{
    ErrorCode, JoinRoomParams, PeerSignal, SignalFishClient, SignalFishConfig, SignalFishError,
    SignalFishEvent, Transport,
};

use common::{
    authenticated_json, authority_response_json, error_json, game_data_binary_json, game_data_json,
    new_peer_json, peer_transport_status_json, player_left_json, pong_json, protocol_info_json,
    reconnected_json, reconnected_with_protocol_info_json, room_joined_json, room_left_json,
    session_plan_json, signal_json, spectator_joined_json, spectator_left_json, MockTransport,
};

// ════════════════════════════════════════════════════════════════════
// Helper: start a mock client with scripted responses
// ════════════════════════════════════════════════════════════════════

/// Start a client with the given scripted server responses. The first item
/// is typically `authenticated_json()` so the auth handshake succeeds.
#[allow(clippy::type_complexity)]
fn start_client(
    incoming: Vec<Option<Result<String, SignalFishError>>>,
) -> (
    SignalFishClient,
    tokio::sync::mpsc::Receiver<SignalFishEvent>,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let (transport, sent, closed) = MockTransport::new(incoming);
    let config = SignalFishConfig::new("mb_test_integration");
    let (client, events) = SignalFishClient::start(transport, config);
    (client, events, sent, closed)
}

/// Consume events up to and including the first `Authenticated` event.
/// Panics if the Connected or Authenticated events are not received.
async fn drain_until_authenticated(rx: &mut tokio::sync::mpsc::Receiver<SignalFishEvent>) {
    let ev = rx.recv().await.expect("expected Connected event");
    assert!(
        matches!(ev, SignalFishEvent::Connected),
        "first event should be Connected, got {ev:?}"
    );
    let ev = rx.recv().await.expect("expected Authenticated event");
    assert!(
        matches!(ev, SignalFishEvent::Authenticated { .. }),
        "second event should be Authenticated, got {ev:?}"
    );
}

/// Transport that can script incoming messages but hangs forever in `close()`.
struct HangingCloseTransport {
    incoming: VecDeque<Option<Result<String, SignalFishError>>>,
}

impl HangingCloseTransport {
    fn new(incoming: Vec<Option<Result<String, SignalFishError>>>) -> Self {
        Self {
            incoming: VecDeque::from(incoming),
        }
    }
}

#[async_trait]
impl Transport for HangingCloseTransport {
    async fn send(&mut self, _message: String) -> Result<(), SignalFishError> {
        Ok(())
    }

    async fn recv(&mut self) -> Option<Result<String, SignalFishError>> {
        if let Some(item) = self.incoming.pop_front() {
            item
        } else {
            // No more scripted messages — pending() never completes,
            // keeping the task alive until shutdown aborts it.
            std::future::pending().await
        }
    }

    async fn close(&mut self) -> Result<(), SignalFishError> {
        // Simulate a transport that never completes close().
        std::future::pending().await
    }
}

// ════════════════════════════════════════════════════════════════════
// Auth flow lifecycle
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn auth_flow_connected_then_authenticated() {
    let (mut client, mut events, sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json()))]);

    // First event: Connected (synthetic).
    let ev = events.recv().await.expect("event");
    assert!(matches!(ev, SignalFishEvent::Connected));

    // Second event: Authenticated (from server response).
    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::Authenticated {
        app_name,
        rate_limits,
        ..
    } = ev
    {
        assert_eq!(app_name, "test-app");
        assert_eq!(rate_limits.per_minute, 60);
    } else {
        panic!("expected Authenticated, got {ev:?}");
    }

    assert!(client.is_connected());
    assert!(client.is_authenticated());

    // Verify the Authenticate message was sent.
    {
        let messages = sent.lock().unwrap();
        assert!(!messages.is_empty());
        let first: ClientMessage = serde_json::from_str(&messages[0]).expect("parse auth message");
        if let ClientMessage::Authenticate { app_id, .. } = first {
            assert_eq!(app_id, "mb_test_integration");
        } else {
            panic!("expected Authenticate, got {first:?}");
        }
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// Room join → leave → rejoin flow
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn room_join_leave_rejoin_flow() {
    // NOTE: Scripted messages are consumed immediately, so we cannot
    // assert intermediate state between RoomLeft and the next RoomJoined.
    // Instead we test each transition in sequence.
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(room_joined_json())),
        Some(Ok(room_left_json())),
        Some(Ok(room_joined_json())),
    ]);

    drain_until_authenticated(&mut events).await;

    // Join room.
    client
        .join_room(JoinRoomParams::new("test-game", "Alice"))
        .expect("join_room");
    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::RoomJoined {
        room_code,
        game_name,
        ..
    } = ev
    {
        assert_eq!(room_code, "ABC123");
        assert_eq!(game_name, "test-game");
    } else {
        panic!("expected RoomJoined, got {ev:?}");
    }

    // Leave room (the RoomLeft event is received).
    client.leave_room().expect("leave_room");
    let ev = events.recv().await.expect("event");
    assert!(matches!(ev, SignalFishEvent::RoomLeft));

    // Rejoin (the second RoomJoined event arrives immediately).
    client
        .join_room(JoinRoomParams::new("test-game", "Alice"))
        .expect("rejoin");
    let ev = events.recv().await.expect("event");
    assert!(matches!(ev, SignalFishEvent::RoomJoined { .. }));

    // After the final RoomJoined, state should reflect the room.
    assert_eq!(client.current_room_code().await.as_deref(), Some("ABC123"));

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// Reconnection flow
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn reconnection_flow_updates_state() {
    let (mut client, mut events, sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(reconnected_json())),
    ]);

    drain_until_authenticated(&mut events).await;

    // Issue a reconnect.
    let pid = uuid::Uuid::from_u128(200);
    let rid = uuid::Uuid::from_u128(100);
    client
        .reconnect(pid, rid, "auth-tok".into())
        .expect("reconnect");

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::Reconnected {
        room_code,
        player_id,
        is_authority,
        ..
    } = ev
    {
        assert_eq!(room_code, "RECON1");
        assert_eq!(player_id, pid);
        assert!(is_authority);
    } else {
        panic!("expected Reconnected, got {ev:?}");
    }

    // State should be updated.
    assert_eq!(client.current_room_code().await.as_deref(), Some("RECON1"));
    assert_eq!(client.current_player_id().await, Some(pid));

    // Verify the Reconnect message was sent.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    {
        let messages = sent.lock().unwrap();
        // The reconnect message might be any position after authenticate.
        // We just check at least one Reconnect was sent.
        let found = messages.iter().any(|m| {
            serde_json::from_str::<ClientMessage>(m)
                .map(|cm| matches!(cm, ClientMessage::Reconnect { .. }))
                .unwrap_or(false)
        });
        assert!(
            found,
            "expected a Reconnect message to be sent, but messages were: {messages:?}"
        );
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// Spectator flow
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn spectator_join_and_leave_flow() {
    // NOTE: Scripted messages are consumed immediately, so we avoid
    // asserting intermediate state between SpectatorJoined and SpectatorLeft.
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(spectator_joined_json())),
        Some(Ok(spectator_left_json())),
    ]);

    drain_until_authenticated(&mut events).await;

    // Join as spectator.
    client
        .join_as_spectator("spec-game".into(), "SPEC1".into(), "Watcher".into())
        .expect("join_as_spectator");

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::SpectatorJoined {
        room_code,
        spectator_id,
        game_name,
        ..
    } = ev
    {
        assert_eq!(room_code, "SPEC1");
        assert_eq!(spectator_id, uuid::Uuid::from_u128(400));
        assert_eq!(game_name, "spec-game");
    } else {
        panic!("expected SpectatorJoined, got {ev:?}");
    }

    // Leave spectator.
    client.leave_spectator().expect("leave_spectator");
    let ev = events.recv().await.expect("event");
    assert!(matches!(ev, SignalFishEvent::SpectatorLeft { .. }));

    // After SpectatorLeft, room state should be cleared.
    assert!(client.current_room_id().await.is_none());
    assert!(client.current_room_code().await.is_none());

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// Authority request/response flow
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn authority_request_granted() {
    let (mut client, mut events, sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(authority_response_json(true, None))),
    ]);

    drain_until_authenticated(&mut events).await;

    client.request_authority(true).expect("request_authority");

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::AuthorityResponse {
        granted,
        reason,
        error_code,
    } = ev
    {
        assert!(granted);
        assert!(reason.is_none());
        assert!(error_code.is_none());
    } else {
        panic!("expected AuthorityResponse, got {ev:?}");
    }

    // Verify the AuthorityRequest message was sent.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    {
        let messages = sent.lock().unwrap();
        let found = messages.iter().any(|m| {
            serde_json::from_str::<ClientMessage>(m)
                .map(|cm| {
                    matches!(
                        cm,
                        ClientMessage::AuthorityRequest {
                            become_authority: true
                        }
                    )
                })
                .unwrap_or(false)
        });
        assert!(found, "expected AuthorityRequest message");
    }

    client.shutdown().await;
}

#[tokio::test]
async fn authority_request_denied() {
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(authority_response_json(false, Some("not allowed")))),
    ]);

    drain_until_authenticated(&mut events).await;

    client.request_authority(true).expect("request_authority");

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::AuthorityResponse {
        granted, reason, ..
    } = ev
    {
        assert!(!granted);
        assert_eq!(reason.as_deref(), Some("not allowed"));
    } else {
        panic!("expected AuthorityResponse, got {ev:?}");
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// ProvideConnectionInfo flow
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn provide_connection_info_sends_correct_message() {
    let (mut client, mut events, sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json()))]);

    drain_until_authenticated(&mut events).await;

    let conn_info = ConnectionInfo::Direct {
        host: "192.168.0.1".into(),
        port: 7777,
    };
    client
        .provide_connection_info(conn_info)
        .expect("provide_connection_info");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    {
        let messages = sent.lock().unwrap();
        let found = messages.iter().any(|m| {
            serde_json::from_str::<ClientMessage>(m)
                .map(|cm| matches!(cm, ClientMessage::ProvideConnectionInfo { .. }))
                .unwrap_or(false)
        });
        assert!(found, "expected ProvideConnectionInfo message");

        // Verify the actual content.
        let pci_msg = messages.iter().find_map(|m| {
            let cm: ClientMessage = serde_json::from_str(m).ok()?;
            if let ClientMessage::ProvideConnectionInfo { connection_info } = cm {
                Some(connection_info)
            } else {
                None
            }
        });
        let ci = pci_msg.expect("ProvideConnectionInfo not found");
        if let ConnectionInfo::Direct { host, port } = ci {
            assert_eq!(host, "192.168.0.1");
            assert_eq!(port, 7777);
        } else {
            panic!("expected Direct connection info");
        }
    }

    client.shutdown().await;
}

#[tokio::test]
async fn provide_relay_connection_info() {
    let (mut client, mut events, sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json()))]);

    drain_until_authenticated(&mut events).await;

    let conn_info = ConnectionInfo::Relay {
        host: "relay.example.com".into(),
        port: 3000,
        transport: RelayTransport::Tcp,
        allocation_id: "room-abc".into(),
        token: "tok-xyz".into(),
        client_id: Some(5),
    };
    client
        .provide_connection_info(conn_info)
        .expect("provide_connection_info");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    {
        let messages = sent.lock().unwrap();
        let pci_msg = messages.iter().find_map(|m| {
            let cm: ClientMessage = serde_json::from_str(m).ok()?;
            if let ClientMessage::ProvideConnectionInfo { connection_info } = cm {
                Some(connection_info)
            } else {
                None
            }
        });
        let ci = pci_msg.expect("ProvideConnectionInfo not found");
        if let ConnectionInfo::Relay {
            host,
            port,
            transport,
            token,
            client_id,
            ..
        } = ci
        {
            assert_eq!(host, "relay.example.com");
            assert_eq!(port, 3000);
            assert!(matches!(transport, RelayTransport::Tcp));
            assert_eq!(token, "tok-xyz");
            assert_eq!(client_id, Some(5));
        } else {
            panic!("expected Relay connection info");
        }
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// JoinAsSpectator + LeaveSpectator message verification
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn join_as_spectator_sends_correct_message() {
    let (mut client, mut events, sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json()))]);

    drain_until_authenticated(&mut events).await;

    client
        .join_as_spectator("game1".into(), "CODE1".into(), "Viewer".into())
        .expect("join_as_spectator");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    {
        let messages = sent.lock().unwrap();
        let found = messages.iter().find_map(|m| {
            let cm: ClientMessage = serde_json::from_str(m).ok()?;
            if let ClientMessage::JoinAsSpectator {
                game_name,
                room_code,
                spectator_name,
            } = cm
            {
                Some((game_name, room_code, spectator_name))
            } else {
                None
            }
        });
        let (gn, rc, sn) = found.expect("JoinAsSpectator not found");
        assert_eq!(gn, "game1");
        assert_eq!(rc, "CODE1");
        assert_eq!(sn, "Viewer");
    }

    client.shutdown().await;
}

#[tokio::test]
async fn leave_spectator_sends_correct_message() {
    let (mut client, mut events, sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json()))]);

    drain_until_authenticated(&mut events).await;

    client.leave_spectator().expect("leave_spectator");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    {
        let messages = sent.lock().unwrap();
        let found = messages.iter().any(|m| {
            serde_json::from_str::<ClientMessage>(m)
                .map(|cm| matches!(cm, ClientMessage::LeaveSpectator))
                .unwrap_or(false)
        });
        assert!(found, "expected LeaveSpectator message");
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// Error event handling
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn server_error_event_received() {
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(error_json(
            "something went wrong",
            Some(ErrorCode::InternalError),
        ))),
    ]);

    drain_until_authenticated(&mut events).await;

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::Error {
        message,
        error_code,
    } = ev
    {
        assert_eq!(message, "something went wrong");
        assert_eq!(error_code, Some(ErrorCode::InternalError));
    } else {
        panic!("expected Error event, got {ev:?}");
    }

    client.shutdown().await;
}

#[tokio::test]
async fn server_error_event_without_error_code() {
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(error_json("generic error", None))),
    ]);

    drain_until_authenticated(&mut events).await;

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::Error {
        message,
        error_code,
    } = ev
    {
        assert_eq!(message, "generic error");
        assert!(error_code.is_none());
    } else {
        panic!("expected Error event, got {ev:?}");
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// Disconnect handling
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn disconnect_on_transport_close() {
    let (mut client, mut events, _sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json())), None]);

    drain_until_authenticated(&mut events).await;

    let ev = events.recv().await.expect("event");
    assert!(matches!(ev, SignalFishEvent::Disconnected { .. }));
    assert!(!client.is_connected());

    client.shutdown().await;
}

#[tokio::test]
async fn disconnect_on_transport_error() {
    let (mut client, mut events, _sent, _closed) = start_client(vec![Some(Err(
        SignalFishError::TransportReceive("network failure".into()),
    ))]);

    // Connected might still be emitted before the error is processed.
    let ev = events.recv().await.expect("event");
    assert!(matches!(ev, SignalFishEvent::Connected));

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::Disconnected { reason } = ev {
        let r = reason.expect("reason should be present");
        assert!(r.contains("network failure"), "reason was: {r}");
    } else {
        panic!("expected Disconnected, got {ev:?}");
    }

    assert!(!client.is_connected());
    client.shutdown().await;
}

#[tokio::test]
async fn operations_fail_after_disconnect() {
    let (mut client, mut events, _sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json())), None]);

    drain_until_authenticated(&mut events).await;

    // Wait for Disconnected.
    let _ev = events.recv().await;

    // Give the loop time to update the connected flag.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let result = client.ping();
    assert!(
        matches!(result, Err(SignalFishError::NotConnected)),
        "expected NotConnected error, got {result:?}"
    );

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// GameData and GameDataBinary event handling
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn game_data_event_received() {
    let player = uuid::Uuid::from_u128(42);
    let data = serde_json::json!({"score": 100, "level": 5});
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(game_data_json(player, data.clone()))),
    ]);

    drain_until_authenticated(&mut events).await;

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::GameData {
        from_player,
        data: d,
    } = ev
    {
        assert_eq!(from_player, player);
        assert_eq!(d["score"], 100);
        assert_eq!(d["level"], 5);
    } else {
        panic!("expected GameData event, got {ev:?}");
    }

    client.shutdown().await;
}

#[tokio::test]
async fn game_data_binary_event_received() {
    let player = uuid::Uuid::from_u128(99);
    let payload = vec![0xCA, 0xFE, 0xBA, 0xBE];
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(game_data_binary_json(
            player,
            GameDataEncoding::MessagePack,
            payload.clone(),
        ))),
    ]);

    drain_until_authenticated(&mut events).await;

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::GameDataBinary {
        from_player,
        encoding,
        payload: p,
    } = ev
    {
        assert_eq!(from_player, player);
        assert!(matches!(encoding, GameDataEncoding::MessagePack));
        assert_eq!(p, payload);
    } else {
        panic!("expected GameDataBinary event, got {ev:?}");
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// send_game_data API method
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn send_game_data_produces_correct_json() {
    let (mut client, mut events, sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json()))]);

    drain_until_authenticated(&mut events).await;

    let data = serde_json::json!({"type": "chat", "msg": "hello"});
    client.send_game_data(data.clone()).expect("send_game_data");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    {
        let messages = sent.lock().unwrap();
        let gd_msg = messages.iter().find_map(|m| {
            let cm: ClientMessage = serde_json::from_str(m).ok()?;
            if let ClientMessage::GameData { data: d } = cm {
                Some(d)
            } else {
                None
            }
        });
        let d = gd_msg.expect("GameData message not found");
        assert_eq!(d["type"], "chat");
        assert_eq!(d["msg"], "hello");
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// set_ready and ping API methods
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn set_ready_sends_player_ready_message() {
    let (mut client, mut events, sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json()))]);

    drain_until_authenticated(&mut events).await;

    client.set_ready().expect("set_ready");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    {
        let messages = sent.lock().unwrap();
        let found = messages.iter().any(|m| {
            serde_json::from_str::<ClientMessage>(m)
                .map(|cm| matches!(cm, ClientMessage::PlayerReady))
                .unwrap_or(false)
        });
        assert!(found, "expected PlayerReady message");
    }

    client.shutdown().await;
}

#[tokio::test]
async fn ping_and_pong_flow() {
    let (mut client, mut events, sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json())), Some(Ok(pong_json()))]);

    drain_until_authenticated(&mut events).await;

    client.ping().expect("ping");

    let ev = events.recv().await.expect("event");
    assert!(matches!(ev, SignalFishEvent::Pong));

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    {
        let messages = sent.lock().unwrap();
        let found = messages.iter().any(|m| {
            serde_json::from_str::<ClientMessage>(m)
                .map(|cm| matches!(cm, ClientMessage::Ping))
                .unwrap_or(false)
        });
        assert!(found, "expected Ping message");
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// JoinRoom with builder options
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn join_room_with_all_options_sends_correct_message() {
    let (mut client, mut events, sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json()))]);

    drain_until_authenticated(&mut events).await;

    let params = JoinRoomParams::new("arena", "Alice")
        .with_room_code("ROOM42")
        .with_max_players(8)
        .with_supports_authority(true)
        .with_relay_transport(RelayTransport::Udp);

    client.join_room(params).expect("join_room");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    {
        let messages = sent.lock().unwrap();
        let jr_msg = messages.iter().find_map(|m| {
            let cm: ClientMessage = serde_json::from_str(m).ok()?;
            if let ClientMessage::JoinRoom {
                game_name,
                room_code,
                player_name,
                max_players,
                supports_authority,
                relay_transport,
            } = cm
            {
                Some((
                    game_name,
                    room_code,
                    player_name,
                    max_players,
                    supports_authority,
                    relay_transport,
                ))
            } else {
                None
            }
        });
        let (gn, rc, pn, mp, sa, rt) = jr_msg.expect("JoinRoom message not found");
        assert_eq!(gn, "arena");
        assert_eq!(rc.as_deref(), Some("ROOM42"));
        assert_eq!(pn, "Alice");
        assert_eq!(mp, Some(8));
        assert_eq!(sa, Some(true));
        assert!(matches!(rt, Some(RelayTransport::Udp)));
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// Multiple sequential operations
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn multiple_sequential_operations() {
    let player = uuid::Uuid::from_u128(42);
    let data_msg = game_data_json(player, serde_json::json!({"tick": 1}));
    let (mut client, mut events, sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(room_joined_json())),
        Some(Ok(data_msg)),
        Some(Ok(pong_json())),
        Some(Ok(room_left_json())),
    ]);

    drain_until_authenticated(&mut events).await;

    // Join room.
    client
        .join_room(JoinRoomParams::new("game", "Player1"))
        .expect("join");
    let ev = events.recv().await.expect("event");
    assert!(matches!(ev, SignalFishEvent::RoomJoined { .. }));

    // Send game data.
    client
        .send_game_data(serde_json::json!({"action": "jump"}))
        .expect("send_game_data");

    // Receive server game data.
    let ev = events.recv().await.expect("event");
    assert!(matches!(ev, SignalFishEvent::GameData { .. }));

    // Ping.
    client.ping().expect("ping");
    let ev = events.recv().await.expect("event");
    assert!(matches!(ev, SignalFishEvent::Pong));

    // Leave room.
    client.leave_room().expect("leave");
    let ev = events.recv().await.expect("event");
    assert!(matches!(ev, SignalFishEvent::RoomLeft));

    // Verify all expected messages were sent.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    {
        let messages = sent.lock().unwrap();
        // Should have: Authenticate, JoinRoom, GameData, Ping, LeaveRoom
        assert!(
            messages.len() >= 5,
            "expected at least 5 messages, got {}",
            messages.len()
        );
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// PlayerJoined / PlayerLeft events
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn player_joined_event_received() {
    let new_player = uuid::Uuid::from_u128(555);
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(room_joined_json())),
        Some(Ok(common::player_joined_json("Bob", new_player))),
    ]);

    drain_until_authenticated(&mut events).await;
    let _rj = events.recv().await; // RoomJoined

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::PlayerJoined { player } = ev {
        assert_eq!(player.name, "Bob");
        assert_eq!(player.id, new_player);
    } else {
        panic!("expected PlayerJoined event, got {ev:?}");
    }

    client.shutdown().await;
}

#[tokio::test]
async fn player_left_event_received() {
    let left_player = uuid::Uuid::from_u128(666);

    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(room_joined_json())),
        Some(Ok(player_left_json(left_player))),
    ]);

    drain_until_authenticated(&mut events).await;
    let _rj = events.recv().await; // RoomJoined

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::PlayerLeft { player_id } = ev {
        assert_eq!(player_id, left_player);
    } else {
        panic!("expected PlayerLeft event, got {ev:?}");
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// LobbyStateChanged and GameStarting events
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn lobby_state_changed_event() {
    let p1 = uuid::Uuid::from_u128(1);
    let lobby_json = serde_json::to_string(&ServerMessage::LobbyStateChanged {
        lobby_state: signal_fish_client::protocol::LobbyState::Finalized,
        ready_players: vec![p1],
        all_ready: true,
    })
    .expect("serialize");

    let (mut client, mut events, _sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json())), Some(Ok(lobby_json))]);

    drain_until_authenticated(&mut events).await;

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::LobbyStateChanged {
        lobby_state,
        ready_players,
        all_ready,
    } = ev
    {
        assert!(matches!(
            lobby_state,
            signal_fish_client::protocol::LobbyState::Finalized
        ));
        assert_eq!(ready_players.len(), 1);
        assert!(all_ready);
    } else {
        panic!("expected LobbyStateChanged event, got {ev:?}");
    }

    client.shutdown().await;
}

#[tokio::test]
async fn game_starting_event() {
    use signal_fish_client::protocol::PeerConnectionInfo;

    let gs_json = serde_json::to_string(&ServerMessage::GameStarting {
        peer_connections: vec![PeerConnectionInfo {
            player_id: uuid::Uuid::from_u128(10),
            player_name: "Peer".into(),
            is_authority: true,
            relay_type: "auto".into(),
            connection_info: None,
        }],
    })
    .expect("serialize");

    let (mut client, mut events, _sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json())), Some(Ok(gs_json))]);

    drain_until_authenticated(&mut events).await;

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::GameStarting { peer_connections } = ev {
        assert_eq!(peer_connections.len(), 1);
        assert_eq!(peer_connections[0].player_name, "Peer");
    } else {
        panic!("expected GameStarting event, got {ev:?}");
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// AuthorityChanged event
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn authority_changed_event() {
    let auth_player = uuid::Uuid::from_u128(77);
    let ac_json = serde_json::to_string(&ServerMessage::AuthorityChanged {
        authority_player: Some(auth_player),
        you_are_authority: true,
    })
    .expect("serialize");

    let (mut client, mut events, _sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json())), Some(Ok(ac_json))]);

    drain_until_authenticated(&mut events).await;

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::AuthorityChanged {
        authority_player,
        you_are_authority,
    } = ev
    {
        assert_eq!(authority_player, Some(auth_player));
        assert!(you_are_authority);
    } else {
        panic!("expected AuthorityChanged event, got {ev:?}");
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// NewSpectatorJoined and SpectatorDisconnected events
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn new_spectator_joined_event() {
    let nsj_json = serde_json::to_string(&ServerMessage::NewSpectatorJoined {
        spectator: signal_fish_client::protocol::SpectatorInfo {
            id: uuid::Uuid::from_u128(500),
            name: "NewViewer".into(),
            connected_at: "2026-01-01T00:00:00Z".into(),
        },
        current_spectators: vec![],
        reason: Some(signal_fish_client::protocol::SpectatorStateChangeReason::Joined),
    })
    .expect("serialize");

    let (mut client, mut events, _sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json())), Some(Ok(nsj_json))]);

    drain_until_authenticated(&mut events).await;

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::NewSpectatorJoined {
        spectator, reason, ..
    } = ev
    {
        assert_eq!(spectator.name, "NewViewer");
        assert!(matches!(
            reason,
            Some(signal_fish_client::protocol::SpectatorStateChangeReason::Joined)
        ));
    } else {
        panic!("expected NewSpectatorJoined event, got {ev:?}");
    }

    client.shutdown().await;
}

#[tokio::test]
async fn spectator_disconnected_event() {
    let sd_json = serde_json::to_string(&ServerMessage::SpectatorDisconnected {
        spectator_id: uuid::Uuid::from_u128(600),
        reason: Some(signal_fish_client::protocol::SpectatorStateChangeReason::Disconnected),
        current_spectators: vec![],
    })
    .expect("serialize");

    let (mut client, mut events, _sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json())), Some(Ok(sd_json))]);

    drain_until_authenticated(&mut events).await;

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::SpectatorDisconnected {
        spectator_id,
        reason,
        ..
    } = ev
    {
        assert_eq!(spectator_id, uuid::Uuid::from_u128(600));
        assert!(matches!(
            reason,
            Some(signal_fish_client::protocol::SpectatorStateChangeReason::Disconnected)
        ));
    } else {
        panic!("expected SpectatorDisconnected event, got {ev:?}");
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// ReconnectionFailed and PlayerReconnected events
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn reconnection_failed_event() {
    let rf_json = serde_json::to_string(&ServerMessage::ReconnectionFailed {
        reason: "expired".into(),
        error_code: ErrorCode::ReconnectionExpired,
    })
    .expect("serialize");

    let (mut client, mut events, _sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json())), Some(Ok(rf_json))]);

    drain_until_authenticated(&mut events).await;

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::ReconnectionFailed { reason, error_code } = ev {
        assert_eq!(reason, "expired");
        assert_eq!(error_code, ErrorCode::ReconnectionExpired);
    } else {
        panic!("expected ReconnectionFailed event, got {ev:?}");
    }

    client.shutdown().await;
}

#[tokio::test]
async fn player_reconnected_event() {
    let pr_json = serde_json::to_string(&ServerMessage::PlayerReconnected {
        player_id: uuid::Uuid::from_u128(700),
    })
    .expect("serialize");

    let (mut client, mut events, _sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json())), Some(Ok(pr_json))]);

    drain_until_authenticated(&mut events).await;

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::PlayerReconnected { player_id } = ev {
        assert_eq!(player_id, uuid::Uuid::from_u128(700));
    } else {
        panic!("expected PlayerReconnected event, got {ev:?}");
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// AuthenticationError event
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn authentication_error_event() {
    let ae_json = serde_json::to_string(&ServerMessage::AuthenticationError {
        error: "bad credentials".into(),
        error_code: ErrorCode::InvalidAppId,
    })
    .expect("serialize");

    let (mut client, mut events, _sent, _closed) = start_client(vec![Some(Ok(ae_json))]);

    // Connected is first.
    let ev = events.recv().await.expect("event");
    assert!(matches!(ev, SignalFishEvent::Connected));

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::AuthenticationError { error, error_code } = ev {
        assert_eq!(error, "bad credentials");
        assert_eq!(error_code, ErrorCode::InvalidAppId);
    } else {
        panic!("expected AuthenticationError event, got {ev:?}");
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// RoomJoinFailed and SpectatorJoinFailed events
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn room_join_failed_event() {
    let rjf_json = serde_json::to_string(&ServerMessage::RoomJoinFailed {
        reason: "room full".into(),
        error_code: Some(ErrorCode::RoomFull),
    })
    .expect("serialize");

    let (mut client, mut events, _sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json())), Some(Ok(rjf_json))]);

    drain_until_authenticated(&mut events).await;

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::RoomJoinFailed { reason, error_code } = ev {
        assert_eq!(reason, "room full");
        assert_eq!(error_code, Some(ErrorCode::RoomFull));
    } else {
        panic!("expected RoomJoinFailed event, got {ev:?}");
    }

    client.shutdown().await;
}

#[tokio::test]
async fn spectator_join_failed_event() {
    let sjf_json = serde_json::to_string(&ServerMessage::SpectatorJoinFailed {
        reason: "spectators disabled".into(),
        error_code: Some(ErrorCode::SpectatorNotAllowed),
    })
    .expect("serialize");

    let (mut client, mut events, _sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json())), Some(Ok(sjf_json))]);

    drain_until_authenticated(&mut events).await;

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::SpectatorJoinFailed { reason, error_code } = ev {
        assert_eq!(reason, "spectators disabled");
        assert_eq!(error_code, Some(ErrorCode::SpectatorNotAllowed));
    } else {
        panic!("expected SpectatorJoinFailed event, got {ev:?}");
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// ProtocolInfo event
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn protocol_info_event() {
    let pi_json = serde_json::to_string(&ServerMessage::ProtocolInfo(
        signal_fish_client::protocol::ProtocolInfoPayload {
            platform: Some("unity".into()),
            sdk_version: Some("1.0.0".into()),
            minimum_version: None,
            recommended_version: None,
            capabilities: vec!["spectator".into()],
            notes: None,
            game_data_formats: vec![GameDataEncoding::Json],
            player_name_rules: None,
            protocol_version: None,
            min_protocol_version: None,
            max_protocol_version: None,
        },
    ))
    .expect("serialize");

    let (mut client, mut events, _sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json())), Some(Ok(pi_json))]);

    drain_until_authenticated(&mut events).await;

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::ProtocolInfo(payload) = ev {
        assert_eq!(payload.platform.as_deref(), Some("unity"));
        assert_eq!(payload.capabilities, vec!["spectator"]);
    } else {
        panic!("expected ProtocolInfo event, got {ev:?}");
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// Shutdown behavior
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn shutdown_closes_transport() {
    let (mut client, mut events, _sent, closed) =
        start_client(vec![Some(Ok(authenticated_json()))]);

    drain_until_authenticated(&mut events).await;

    client.shutdown().await;

    // The Disconnected event should be emitted.
    let ev = events.recv().await.expect("event");
    assert!(matches!(ev, SignalFishEvent::Disconnected { .. }));

    assert!(closed.load(std::sync::atomic::Ordering::Relaxed));
}

#[tokio::test]
async fn shutdown_timeout_clears_state_even_when_disconnected_event_is_skipped() {
    let transport = HangingCloseTransport::new(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(room_joined_json())),
    ]);
    let config = SignalFishConfig::new("mb_test_integration")
        .with_shutdown_timeout(std::time::Duration::from_millis(1));
    let (mut client, mut events) = SignalFishClient::start(transport, config);

    drain_until_authenticated(&mut events).await;
    let ev = events.recv().await.expect("expected RoomJoined event");
    assert!(matches!(ev, SignalFishEvent::RoomJoined { .. }));

    assert!(client.is_authenticated());
    assert!(client.current_player_id().await.is_some());
    assert!(client.current_room_id().await.is_some());
    assert_eq!(client.current_room_code().await.as_deref(), Some("ABC123"));

    client.shutdown().await;

    assert!(!client.is_connected());
    assert!(!client.is_authenticated());
    assert!(client.current_player_id().await.is_none());
    assert!(client.current_room_id().await.is_none());
    assert!(client.current_room_code().await.is_none());
}

#[tokio::test]
async fn leave_room_sends_leave_room_message() {
    let (mut client, mut events, sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json()))]);

    drain_until_authenticated(&mut events).await;

    client.leave_room().expect("leave_room");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    {
        let messages = sent.lock().unwrap();
        let found = messages.iter().any(|m| {
            serde_json::from_str::<ClientMessage>(m)
                .map(|cm| matches!(cm, ClientMessage::LeaveRoom))
                .unwrap_or(false)
        });
        assert!(found, "expected LeaveRoom message");
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// Malformed JSON resilience
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn malformed_json_does_not_crash_and_next_message_arrives() {
    // Send garbled text followed by a valid message.
    // The transport loop should warn on the invalid JSON and continue.
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok("{{not valid json at all!!!".into())),
        Some(Ok(pong_json())),
    ]);

    drain_until_authenticated(&mut events).await;

    // The invalid JSON is silently dropped. The next valid message should arrive.
    let ev = events
        .recv()
        .await
        .expect("expected Pong after malformed JSON");
    assert!(
        matches!(ev, SignalFishEvent::Pong),
        "expected Pong event after malformed JSON, got {ev:?}"
    );

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// PlayerJoined with ConnectionInfo::Direct
// ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn player_joined_with_connection_info_direct() {
    use signal_fish_client::protocol::PlayerInfo;

    let new_player = uuid::Uuid::from_u128(777);
    let pj_json = serde_json::to_string(&ServerMessage::PlayerJoined {
        player: PlayerInfo {
            id: new_player,
            name: "ConnectedPlayer".into(),
            is_authority: true,
            is_ready: true,
            connected_at: "2026-02-20T12:00:00Z".into(),
            connection_info: Some(ConnectionInfo::Direct {
                host: "10.0.0.5".into(),
                port: 5555,
            }),
        },
    })
    .expect("serialize");

    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(room_joined_json())),
        Some(Ok(pj_json)),
    ]);

    drain_until_authenticated(&mut events).await;
    let _rj = events.recv().await; // RoomJoined

    let ev = events.recv().await.expect("event");
    if let SignalFishEvent::PlayerJoined { player } = ev {
        assert_eq!(player.name, "ConnectedPlayer");
        assert_eq!(player.id, new_player);
        assert!(player.is_authority);
        assert!(player.is_ready);
        if let Some(ConnectionInfo::Direct { host, port }) = player.connection_info {
            assert_eq!(host, "10.0.0.5");
            assert_eq!(port, 5555);
        } else {
            panic!(
                "expected Direct connection_info, got {:?}",
                player.connection_info
            );
        }
    } else {
        panic!("expected PlayerJoined event, got {ev:?}");
    }

    client.shutdown().await;
}

// ════════════════════════════════════════════════════════════════════
// Protocol v2/v3: start_game, mesh signaling, and the negotiation guard
// ════════════════════════════════════════════════════════════════════

/// Consume events until a `ProtocolInfo` event is observed (which proves the
/// client processed the negotiation and updated its state).
async fn drain_until_protocol_info(rx: &mut tokio::sync::mpsc::Receiver<SignalFishEvent>) {
    loop {
        let ev = rx.recv().await.expect("expected ProtocolInfo event");
        if matches!(ev, SignalFishEvent::ProtocolInfo(_)) {
            return;
        }
    }
}

/// Parse all currently-recorded outgoing messages into `ClientMessage`s.
///
/// Every captured frame MUST deserialize cleanly: silently dropping
/// unparseable frames would let a malformed or unexpected wire shape pass
/// assertions like "no v3 message reached the wire" that depend on *seeing*
/// every outgoing message. A parse failure here is a real bug in the client,
/// so we surface it loudly instead of hiding it.
fn sent_messages(sent: &std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> Vec<ClientMessage> {
    sent.lock()
        .unwrap()
        .iter()
        .map(|m| {
            serde_json::from_str::<ClientMessage>(m)
                .unwrap_or_else(|e| panic!("outgoing client message must deserialize: {e}\n{m}"))
        })
        .collect()
}

#[tokio::test]
async fn start_game_sends_start_game_message() {
    let (mut client, mut events, sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json()))]);
    drain_until_authenticated(&mut events).await;

    client.start_game().expect("start_game");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert!(sent_messages(&sent)
        .iter()
        .any(|m| matches!(m, ClientMessage::StartGame)));
    client.shutdown().await;
}

#[tokio::test]
async fn start_game_available_on_relay_floor() {
    // start_game is the universal v2 change — NOT gated behind v3 negotiation.
    let (mut client, mut events, sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json()))]);
    drain_until_authenticated(&mut events).await;
    assert!(client.negotiated_protocol_version().is_none());

    client
        .start_game()
        .expect("start_game must work on the relay floor");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(sent_messages(&sent)
        .iter()
        .any(|m| matches!(m, ClientMessage::StartGame)));
    client.shutdown().await;
}

#[tokio::test]
async fn send_signal_before_v3_returns_protocol_unsupported() {
    let (mut client, mut events, sent, _closed) =
        start_client(vec![Some(Ok(authenticated_json()))]);
    drain_until_authenticated(&mut events).await;

    // Authenticated but no `ProtocolInfo` yet → negotiation is still in flight,
    // so the guard reports "pre-negotiation" (NOT "relay-only", which is
    // reserved for a `ProtocolInfo` that resolved at the v2 floor — see
    // `v2_protocol_info_keeps_relay_floor_guard`).
    let err = client
        .send_signal(uuid::Uuid::from_u128(2), PeerSignal::Offer("sdp".into()))
        .expect_err("send_signal must fail before negotiation completes");
    assert!(matches!(
        err,
        SignalFishError::ProtocolUnsupported {
            mode: "pre-negotiation"
        }
    ));
    // report_transport_status fails fast too.
    assert!(matches!(
        client.report_transport_status(TransportKind::WebRtc, true),
        Err(SignalFishError::ProtocolUnsupported { .. })
    ));

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    // No v3 message ever reached the wire.
    assert!(!sent_messages(&sent).iter().any(|m| matches!(
        m,
        ClientMessage::Signal { .. } | ClientMessage::TransportStatus { .. }
    )));
    client.shutdown().await;
}

#[tokio::test]
async fn send_signal_after_v3_negotiation_is_sent() {
    let peer = uuid::Uuid::from_u128(2);
    let (mut client, mut events, sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(protocol_info_json(Some(3)))),
    ]);
    drain_until_authenticated(&mut events).await;
    drain_until_protocol_info(&mut events).await;
    assert_eq!(client.negotiated_protocol_version(), Some(3));

    client.send_offer(peer, "the-sdp").expect("send_offer");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let signal = sent_messages(&sent).into_iter().find_map(|m| match m {
        ClientMessage::Signal { to, signal } if to == peer => Some(signal),
        _ => None,
    });
    assert_eq!(signal, Some(serde_json::json!({ "Offer": "the-sdp" })));
    client.shutdown().await;
}

#[tokio::test]
async fn report_transport_status_after_v3_is_sent() {
    let (mut client, mut events, sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(protocol_info_json(Some(3)))),
    ]);
    drain_until_authenticated(&mut events).await;
    drain_until_protocol_info(&mut events).await;

    client
        .report_transport_status(TransportKind::WebRtc, true)
        .expect("report_transport_status");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(sent_messages(&sent).iter().any(|m| matches!(
        m,
        ClientMessage::TransportStatus {
            transport: TransportKind::WebRtc,
            connected: true
        }
    )));
    client.shutdown().await;
}

#[tokio::test]
async fn v2_protocol_info_keeps_relay_floor_guard() {
    // A v2 ProtocolInfo (no version field) leaves negotiated version None, so v3
    // sends still fail fast — and because a `ProtocolInfo` *did* arrive, the
    // guard reports the terminal "relay-only" mode (contrast
    // `send_signal_before_v3_returns_protocol_unsupported`, which is
    // "pre-negotiation" because no `ProtocolInfo` has arrived).
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(protocol_info_json(None))),
    ]);
    drain_until_authenticated(&mut events).await;
    drain_until_protocol_info(&mut events).await;
    assert!(client.negotiated_protocol_version().is_none());
    assert!(matches!(
        client.send_offer(uuid::Uuid::from_u128(2), "x"),
        Err(SignalFishError::ProtocolUnsupported { mode: "relay-only" })
    ));
    client.shutdown().await;
}

#[tokio::test]
async fn v3_session_plan_and_signal_events_are_emitted() {
    let peer = uuid::Uuid::from_u128(7);
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(protocol_info_json(Some(3)))),
        Some(Ok(session_plan_json(peer, true))),
        Some(Ok(signal_json(
            peer,
            serde_json::json!({ "Offer": "remote-sdp" }),
        ))),
    ]);
    drain_until_authenticated(&mut events).await;

    let mut saw_plan = false;
    let mut saw_signal = false;
    while !(saw_plan && saw_signal) {
        match events.recv().await.expect("event") {
            SignalFishEvent::SessionPlan {
                topology,
                transport,
                peers,
                fallback,
                ..
            } => {
                assert!(matches!(topology, signal_fish_client::Topology::Mesh));
                assert!(matches!(transport, TransportKind::WebRtc));
                assert!(matches!(fallback, TransportKind::Relay));
                assert_eq!(peers.len(), 1);
                assert_eq!(peers[0].player_id, peer);
                assert!(peers[0].initiate);
                saw_plan = true;
            }
            SignalFishEvent::SignalReceived { from, signal } => {
                assert_eq!(from, peer);
                assert_eq!(
                    PeerSignal::try_from(&signal).expect("typed signal"),
                    PeerSignal::Offer("remote-sdp".into())
                );
                saw_signal = true;
            }
            _ => {}
        }
    }
    client.shutdown().await;
}

#[tokio::test]
async fn new_peer_and_peer_transport_status_events_are_emitted() {
    let peer = uuid::Uuid::from_u128(8);
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(new_peer_json(peer, true))),
        Some(Ok(peer_transport_status_json(peer, true))),
    ]);
    drain_until_authenticated(&mut events).await;

    let mut saw_new_peer = false;
    let mut saw_status = false;
    while !(saw_new_peer && saw_status) {
        match events.recv().await.expect("event") {
            SignalFishEvent::NewPeer {
                peer_id,
                you_initiate,
            } => {
                assert_eq!(peer_id, peer);
                assert!(you_initiate);
                saw_new_peer = true;
            }
            SignalFishEvent::PeerTransportStatus {
                peer_id,
                transport,
                connected,
            } => {
                assert_eq!(peer_id, peer);
                assert!(matches!(transport, TransportKind::WebRtc));
                assert!(connected);
                saw_status = true;
            }
            _ => {}
        }
    }
    client.shutdown().await;
}

#[tokio::test]
async fn unknown_server_message_type_is_skipped_then_next_arrives() {
    // Forward-compat: a well-formed but unknown `type` is logged+skipped, and the
    // following valid message still surfaces.
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(r#"{"type":"SomeFutureV4Message","data":{}}"#.to_string())),
        Some(Ok(pong_json())),
    ]);
    drain_until_authenticated(&mut events).await;
    let ev = events.recv().await.expect("event after unknown type");
    assert!(matches!(ev, SignalFishEvent::Pong));
    assert!(client.is_connected());
    client.shutdown().await;
}

#[tokio::test]
async fn send_signal_before_authentication_is_pre_negotiation() {
    // The `mode: "pre-negotiation"` branch of the guard: no auth scripted, so the
    // client is connected but has not authenticated/negotiated.
    let (mut client, _events, sent, _closed) = start_client(vec![]);

    let err = client
        .send_offer(uuid::Uuid::from_u128(2), "sdp")
        .expect_err("send before negotiation must fail");
    assert!(matches!(
        err,
        SignalFishError::ProtocolUnsupported {
            mode: "pre-negotiation"
        }
    ));
    assert!(!client.supports_mesh());
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(sent_messages(&sent)
        .iter()
        .all(|m| !matches!(m, ClientMessage::Signal { .. })));
    client.shutdown().await;
}

#[tokio::test]
async fn negotiated_version_resets_on_disconnect() {
    // Note: the scripted clean-close races the mid-stream state, so we assert the
    // post-disconnect state only. The full negotiate-then-reset cycle is proven
    // deterministically by the polling client's synchronous equivalent.
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(protocol_info_json(Some(3)))),
        None, // clean close
    ]);
    drain_until_authenticated(&mut events).await;

    // Drain until the transport closes (clear_session_state runs before the
    // Disconnected event is delivered).
    loop {
        match events.recv().await {
            Some(SignalFishEvent::Disconnected { .. }) | None => break,
            _ => {}
        }
    }
    assert_eq!(client.negotiated_protocol_version(), None);
    assert!(!client.supports_mesh());
    assert!(client.send_offer(uuid::Uuid::from_u128(2), "x").is_err());
    client.shutdown().await;
}

#[tokio::test]
async fn reconnect_restores_negotiated_version_from_missed_events() {
    let peer = uuid::Uuid::from_u128(2);
    let (mut client, mut events, sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(reconnected_with_protocol_info_json(Some(3)))),
    ]);
    drain_until_authenticated(&mut events).await;
    // Drain until the Reconnected event (state updated by then).
    loop {
        if matches!(
            events.recv().await.expect("event"),
            SignalFishEvent::Reconnected { .. }
        ) {
            break;
        }
    }
    assert_eq!(client.negotiated_protocol_version(), Some(3));
    assert!(client.supports_mesh());

    client
        .send_offer(peer, "sdp")
        .expect("send_offer after reconnect");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(sent_messages(&sent)
        .iter()
        .any(|m| matches!(m, ClientMessage::Signal { .. })));
    client.shutdown().await;
}

#[tokio::test]
async fn v4_negotiation_still_enables_mesh() {
    // `>= 3` (not `== 3`) semantics: a future v4 negotiation must still enable mesh.
    let (mut client, mut events, _sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(protocol_info_json(Some(4)))),
    ]);
    drain_until_authenticated(&mut events).await;
    drain_until_protocol_info(&mut events).await;
    assert_eq!(client.negotiated_protocol_version(), Some(4));
    assert!(client.supports_mesh());
    client
        .send_offer(uuid::Uuid::from_u128(2), "sdp")
        .expect("v4 must enable mesh");
    client.shutdown().await;
}

#[tokio::test]
async fn send_answer_ice_and_raw_signal_wire_shapes() {
    let peer = uuid::Uuid::from_u128(5);
    let (mut client, mut events, sent, _closed) = start_client(vec![
        Some(Ok(authenticated_json())),
        Some(Ok(protocol_info_json(Some(3)))),
    ]);
    drain_until_authenticated(&mut events).await;
    drain_until_protocol_info(&mut events).await;

    client.send_answer(peer, "ans").expect("send_answer");
    client
        .send_ice_candidate(peer, "cand")
        .expect("send_ice_candidate");
    client
        .send_raw_signal(peer, serde_json::json!({ "Renegotiate": true }))
        .expect("send_raw_signal");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let signals: Vec<serde_json::Value> = sent_messages(&sent)
        .into_iter()
        .filter_map(|m| match m {
            ClientMessage::Signal { to, signal } if to == peer => Some(signal),
            _ => None,
        })
        .collect();
    assert!(signals.contains(&serde_json::json!({ "Answer": "ans" })));
    assert!(signals.contains(&serde_json::json!({ "IceCandidate": "cand" })));
    // The raw escape hatch forwards an opaque value verbatim.
    assert!(signals.contains(&serde_json::json!({ "Renegotiate": true })));
    client.shutdown().await;
}
