//! Parity regression tests: `SignalFishPollingClient` (sync) must mirror
//! `SignalFishClient` (async) v3 behavior exactly.
//!
//! These drive BOTH clients through equivalent scenarios with the same scripted
//! server messages and assert identical observable behavior: negotiated-version
//! tracking, `ensure_v3` guard modes, reconnect replay restoration (no v2
//! downgrade), accessors, and relay-floor byte-identity. The polling client is a
//! primary WASM/Godot path, so any divergence is a silent bug for those users.
// Compares the async and polling clients side by side, so it needs both.
#![cfg(all(feature = "polling-client", feature = "tokio-runtime"))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::type_complexity
)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use signal_fish_client::client::{SignalFishClient, SignalFishConfig};
use signal_fish_client::error::SignalFishError;
use signal_fish_client::polling_client::SignalFishPollingClient;
use signal_fish_client::protocol::{
    LobbyState, PlayerId, ProtocolInfoPayload, ReconnectedPayload, ServerMessage,
};
use signal_fish_client::{SignalFishEvent, Transport};

const PEER_UUID: &str = "00000000-0000-0000-0000-000000000007";
const AUTH: &str = r#"{"type":"Authenticated","data":{"app_name":"test","rate_limits":{"per_minute":60,"per_hour":1000,"per_day":10000}}}"#;
const PI_V3: &str = r#"{"type":"ProtocolInfo","data":{"capabilities":[],"game_data_formats":[],"protocol_version":3,"min_protocol_version":2,"max_protocol_version":3}}"#;
// A v2 negotiation omits the version fields, so it deserializes to
// `protocol_version: None` — a terminal relay floor.
const PI_V2: &str = r#"{"type":"ProtocolInfo","data":{"capabilities":[],"game_data_formats":[]}}"#;

// ── Shared mock transport (works for both async + polling drivers) ────

#[derive(Clone)]
struct SharedMock {
    incoming: Arc<Mutex<VecDeque<Option<Result<String, SignalFishError>>>>>,
    sent: Arc<Mutex<Vec<String>>>,
}

impl SharedMock {
    fn new(msgs: Vec<&str>) -> Self {
        Self {
            incoming: Arc::new(Mutex::new(
                msgs.into_iter().map(|m| Some(Ok(m.to_string()))).collect(),
            )),
            sent: Arc::new(Mutex::new(Vec::new())),
        }
    }
    fn from_msgs(msgs: Vec<Option<Result<String, SignalFishError>>>) -> Self {
        Self {
            incoming: Arc::new(Mutex::new(msgs.into())),
            sent: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl Transport for SharedMock {
    async fn send(&mut self, message: String) -> Result<(), SignalFishError> {
        self.sent.lock().unwrap().push(message);
        Ok(())
    }
    async fn recv(&mut self) -> Option<Result<String, SignalFishError>> {
        let item = self.incoming.lock().unwrap().pop_front();
        match item {
            Some(inner) => inner,
            // No scripted messages remain — this future never completes, keeping
            // the loop alive (noop waker for polling, awaited for async).
            None => std::future::pending().await,
        }
    }
    async fn close(&mut self) -> Result<(), SignalFishError> {
        Ok(())
    }
}

fn pi_v3_payload() -> ProtocolInfoPayload {
    ProtocolInfoPayload {
        platform: None,
        sdk_version: None,
        minimum_version: None,
        recommended_version: None,
        capabilities: vec![],
        notes: None,
        game_data_formats: vec![],
        player_name_rules: None,
        protocol_version: Some(3),
        min_protocol_version: Some(2),
        max_protocol_version: Some(3),
    }
}

fn pi_v2_payload() -> ProtocolInfoPayload {
    let mut p = pi_v3_payload();
    p.protocol_version = None;
    p.min_protocol_version = None;
    p.max_protocol_version = None;
    p
}

fn reconnected_with_missed(missed: Vec<ServerMessage>) -> String {
    let payload = ReconnectedPayload {
        room_id: uuid::Uuid::from_u128(100),
        room_code: "R".into(),
        player_id: uuid::Uuid::from_u128(200),
        game_name: "g".into(),
        max_players: 4,
        supports_authority: false,
        current_players: vec![],
        is_authority: false,
        lobby_state: LobbyState::Waiting,
        ready_players: vec![],
        relay_type: "tcp".into(),
        current_spectators: vec![],
        ice_servers: vec![],
        missed_events: missed,
    };
    serde_json::to_string(&ServerMessage::Reconnected(Box::new(payload))).unwrap()
}

async fn wait_for_sent_len(mock: &SharedMock, expected_len: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if mock.sent.lock().unwrap().len() >= expected_len {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "timed out waiting for {expected_len} sent message(s); got {}",
            mock.sent.lock().unwrap().len()
        )
    });
}

// ── PARITY 1: relay-floor Authenticate byte-identity ─────────────────

#[tokio::test]
async fn parity_relay_floor_authenticate_is_byte_identical() {
    let async_mock = SharedMock::new(vec![]);
    let (_client, _events) =
        SignalFishClient::start(async_mock.clone(), SignalFishConfig::new("app"));
    wait_for_sent_len(&async_mock, 1).await;
    let async_sent = async_mock.sent.lock().unwrap().clone();

    let poll_mock = SharedMock::new(vec![]);
    let mut poll_client =
        SignalFishPollingClient::new(poll_mock.clone(), SignalFishConfig::new("app"));
    poll_client.poll();
    let poll_sent = poll_mock.sent.lock().unwrap().clone();

    assert!(!async_sent.is_empty());
    assert!(!poll_sent.is_empty());
    assert_eq!(
        async_sent[0], poll_sent[0],
        "Authenticate bytes must be byte-identical between clients"
    );
    let v: serde_json::Value = serde_json::from_str(&poll_sent[0]).unwrap();
    assert!(v["data"].get("protocol_version").is_none());
    assert!(v["data"].get("supported_transports").is_none());
    assert!(v["data"].get("supported_topologies").is_none());
}

// ── PARITY 2: ensure_v3 guard modes ──────────────────────────────────

#[tokio::test]
async fn parity_ensure_v3_pre_negotiation_mode() {
    let peer: PlayerId = PEER_UUID.parse().unwrap();

    let async_mock = SharedMock::new(vec![]);
    let (client, _events) = SignalFishClient::start(async_mock, SignalFishConfig::new("app"));
    let async_err = client.send_offer(peer, "sdp").unwrap_err();

    let poll_mock = SharedMock::new(vec![]);
    let mut poll_client = SignalFishPollingClient::new(poll_mock, SignalFishConfig::new("app"));
    let poll_err = poll_client.send_offer(peer, "sdp").unwrap_err();

    assert!(matches!(
        async_err,
        SignalFishError::ProtocolUnsupported {
            mode: "pre-negotiation"
        }
    ));
    assert!(matches!(
        poll_err,
        SignalFishError::ProtocolUnsupported {
            mode: "pre-negotiation"
        }
    ));
}

#[tokio::test]
async fn parity_ensure_v3_relay_only_mode_after_v2_negotiation() {
    // Both clients must report the terminal "relay-only" mode once a v2
    // `ProtocolInfo` has been observed — distinct from the "pre-negotiation"
    // state before any `ProtocolInfo` arrives (see the parity test above).
    let peer: PlayerId = PEER_UUID.parse().unwrap();

    let async_mock = SharedMock::new(vec![AUTH, PI_V2]);
    let (client, mut events) = SignalFishClient::start(async_mock, SignalFishConfig::new("app"));
    // Drain until the v2 ProtocolInfo has been processed into client state.
    loop {
        match events.recv().await {
            Some(SignalFishEvent::ProtocolInfo(_)) | None => break,
            _ => {}
        }
    }
    let async_err = client.send_offer(peer, "sdp").unwrap_err();

    let poll_mock = SharedMock::new(vec![AUTH, PI_V2]);
    let mut poll_client = SignalFishPollingClient::new(poll_mock, SignalFishConfig::new("app"));
    poll_client.poll();
    let poll_err = poll_client.send_offer(peer, "sdp").unwrap_err();

    assert!(matches!(
        async_err,
        SignalFishError::ProtocolUnsupported { mode: "relay-only" }
    ));
    assert!(matches!(
        poll_err,
        SignalFishError::ProtocolUnsupported { mode: "relay-only" }
    ));
}

// ── PARITY 3: negotiated version + supports_mesh after v3 ─────────────

#[tokio::test]
async fn parity_negotiated_version_after_v3() {
    let async_mock = SharedMock::new(vec![AUTH, PI_V3]);
    let (client, mut events) = SignalFishClient::start(async_mock, SignalFishConfig::new("app"));
    for _ in 0..3 {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), events.recv()).await;
    }
    assert_eq!(client.negotiated_protocol_version(), Some(3));
    assert!(client.supports_mesh());

    let poll_mock = SharedMock::new(vec![AUTH, PI_V3]);
    let mut poll_client = SignalFishPollingClient::new(poll_mock, SignalFishConfig::new("app"));
    poll_client.poll();
    assert_eq!(poll_client.negotiated_protocol_version(), Some(3));
    assert!(poll_client.supports_mesh());
}

// ── PARITY 4: reconnect replay restores v3 (downgrade-risk hunt) ──────

#[tokio::test]
async fn parity_reconnect_replay_restores_v3_from_missed_events() {
    let recon = reconnected_with_missed(vec![ServerMessage::ProtocolInfo(pi_v3_payload())]);

    let async_mock = SharedMock::new(vec![AUTH, &recon]);
    let (client, mut events) = SignalFishClient::start(async_mock, SignalFishConfig::new("app"));
    for _ in 0..4 {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), events.recv()).await;
    }
    assert_eq!(
        client.negotiated_protocol_version(),
        Some(3),
        "async: reconnect must restore v3 from missed_events"
    );

    let poll_mock = SharedMock::new(vec![AUTH, &recon]);
    let mut poll_client = SignalFishPollingClient::new(poll_mock, SignalFishConfig::new("app"));
    poll_client.poll();
    assert_eq!(
        poll_client.negotiated_protocol_version(),
        Some(3),
        "polling: reconnect must restore v3 from missed_events"
    );
}

// ── PARITY 5: reconnect with v2 missed_events must NOT downgrade ──────

#[tokio::test]
async fn parity_reconnect_v2_missed_events_does_not_downgrade_active_v3() {
    let recon_v2 = reconnected_with_missed(vec![ServerMessage::ProtocolInfo(pi_v2_payload())]);

    let async_mock = SharedMock::new(vec![AUTH, PI_V3, &recon_v2]);
    let (client, mut events) = SignalFishClient::start(async_mock, SignalFishConfig::new("app"));
    for _ in 0..5 {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), events.recv()).await;
    }
    assert_eq!(
        client.negotiated_protocol_version(),
        Some(3),
        "async: a replayed v2 ProtocolInfo must not downgrade active v3"
    );

    let poll_mock = SharedMock::new(vec![AUTH, PI_V3, &recon_v2]);
    let mut poll_client = SignalFishPollingClient::new(poll_mock, SignalFishConfig::new("app"));
    poll_client.poll();
    assert_eq!(
        poll_client.negotiated_protocol_version(),
        Some(3),
        "polling: a replayed v2 ProtocolInfo must not downgrade active v3"
    );
}

// ── PARITY 6: enable_mesh advertises v3 identically ──────────────────

#[tokio::test]
async fn parity_enable_mesh_authenticate_is_byte_identical() {
    let async_mock = SharedMock::new(vec![]);
    let (_client, _events) = SignalFishClient::start(
        async_mock.clone(),
        SignalFishConfig::new("app").enable_mesh(),
    );
    wait_for_sent_len(&async_mock, 1).await;
    let async_sent = async_mock.sent.lock().unwrap().clone();

    let poll_mock = SharedMock::new(vec![]);
    let mut poll_client = SignalFishPollingClient::new(
        poll_mock.clone(),
        SignalFishConfig::new("app").enable_mesh(),
    );
    poll_client.poll();
    let poll_sent = poll_mock.sent.lock().unwrap().clone();

    assert_eq!(
        async_sent[0], poll_sent[0],
        "enable_mesh Authenticate must be byte-identical between clients"
    );
}

// ── PARITY 7: disconnect resets negotiated version in both ─────────────

#[tokio::test]
async fn parity_disconnect_resets_negotiated_version() {
    let poll_mock = SharedMock::new(vec![AUTH, PI_V3]);
    let mut poll_client = SignalFishPollingClient::new(poll_mock, SignalFishConfig::new("app"));
    poll_client.poll();
    assert!(poll_client.supports_mesh());
    poll_client.close();
    assert_eq!(poll_client.negotiated_protocol_version(), None);

    let async_mock = SharedMock::from_msgs(vec![
        Some(Ok(AUTH.to_string())),
        Some(Ok(PI_V3.to_string())),
        Some(Err(SignalFishError::TransportReceive("reset".into()))),
    ]);
    let (client, mut events) = SignalFishClient::start(async_mock, SignalFishConfig::new("app"));
    let mut saw_disconnect = false;
    for _ in 0..6 {
        match tokio::time::timeout(std::time::Duration::from_millis(150), events.recv()).await {
            Ok(Some(SignalFishEvent::Disconnected { .. })) => {
                saw_disconnect = true;
                break;
            }
            Ok(Some(_)) => {}
            _ => break,
        }
    }
    assert!(saw_disconnect, "async should have disconnected");
    assert_eq!(client.negotiated_protocol_version(), None);
}

// ── PARITY 9: DecodeFailed surfacing is identical ─────────────────────

#[tokio::test]
async fn parity_decode_failed_async_vs_polling() {
    const BAD_FRAME: &str =
        r#"{"type":"Error","data":{"message":"x","error_code":"FUTURE_CODE_XYZ"}}"#;

    // Async client.
    let async_mock = SharedMock::new(vec![AUTH, BAD_FRAME]);
    let (async_client, mut events) =
        SignalFishClient::start(async_mock, SignalFishConfig::new("app"));
    let mut async_decode_failed = None;
    for _ in 0..6 {
        match tokio::time::timeout(std::time::Duration::from_millis(150), events.recv()).await {
            Ok(Some(ev @ SignalFishEvent::DecodeFailed { .. })) => {
                async_decode_failed = Some(ev);
                break;
            }
            Ok(Some(_)) => {}
            _ => break,
        }
    }

    // Polling client.
    let poll_mock = SharedMock::new(vec![AUTH, BAD_FRAME]);
    let mut poll_client = SignalFishPollingClient::new(poll_mock, SignalFishConfig::new("app"));
    let poll_events = poll_client.poll();
    let poll_decode_failed = poll_events
        .into_iter()
        .find(|e| matches!(e, SignalFishEvent::DecodeFailed { .. }));

    // Both must surface the event, with identical fields.
    let (
        Some(SignalFishEvent::DecodeFailed {
            message_type: a_type,
            error: a_err,
            raw_prefix: a_raw,
        }),
        Some(SignalFishEvent::DecodeFailed {
            message_type: p_type,
            error: p_err,
            raw_prefix: p_raw,
        }),
    ) = (async_decode_failed, poll_decode_failed)
    else {
        panic!("both clients must surface DecodeFailed for the same frame");
    };
    assert_eq!(a_type, p_type);
    assert_eq!(a_err, p_err);
    assert_eq!(a_raw, p_raw);
    assert_eq!(a_type.as_deref(), Some("Error"));

    // And identical stats accounting.
    assert_eq!(async_client.stats().messages_undecodable, 1);
    assert_eq!(poll_client.stats().messages_undecodable, 1);
}

// ── PARITY 10: Disconnected carries last_server_error identically ─────

#[tokio::test]
async fn parity_disconnected_carries_last_server_error() {
    const FAREWELL: &str = r#"{"type":"Error","data":{"message":"Disconnected as a slow consumer","error_code":"SLOW_CONSUMER"}}"#;

    // Async: farewell then clean close.
    let async_mock = SharedMock::from_msgs(vec![
        Some(Ok(AUTH.to_string())),
        Some(Ok(FAREWELL.to_string())),
        None,
    ]);
    let (_client, mut events) = SignalFishClient::start(async_mock, SignalFishConfig::new("app"));
    let mut async_info = None;
    for _ in 0..8 {
        match tokio::time::timeout(std::time::Duration::from_millis(150), events.recv()).await {
            Ok(Some(SignalFishEvent::Disconnected {
                last_server_error, ..
            })) => {
                async_info = last_server_error;
                break;
            }
            Ok(Some(_)) => {}
            _ => break,
        }
    }

    // Polling: same script.
    let poll_mock = SharedMock::from_msgs(vec![
        Some(Ok(AUTH.to_string())),
        Some(Ok(FAREWELL.to_string())),
        None,
    ]);
    let mut poll_client = SignalFishPollingClient::new(poll_mock, SignalFishConfig::new("app"));
    let poll_events = poll_client.poll();
    let poll_info = poll_events.into_iter().find_map(|e| match e {
        SignalFishEvent::Disconnected {
            last_server_error, ..
        } => last_server_error,
        _ => None,
    });

    let async_info = async_info.expect("async Disconnected must carry the farewell");
    let poll_info = poll_info.expect("polling Disconnected must carry the farewell");
    assert_eq!(async_info, poll_info);
    assert_eq!(
        async_info.error_code,
        Some(signal_fish_client::ErrorCode::SlowConsumer)
    );
}
