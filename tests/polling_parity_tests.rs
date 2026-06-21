//! Parity regression tests: `SignalFishPollingClient` (sync) must mirror
//! `SignalFishClient` (async) v3 behaviour exactly.
//!
//! These drive BOTH clients through equivalent scenarios with the same scripted
//! server messages and assert identical observable behaviour: negotiated-version
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

// ── PARITY 1: relay-floor Authenticate byte-identity ─────────────────

#[tokio::test]
async fn parity_relay_floor_authenticate_is_byte_identical() {
    let async_mock = SharedMock::new(vec![]);
    let (_client, _events) =
        SignalFishClient::start(async_mock.clone(), SignalFishConfig::new("app"));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
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
async fn parity_ensure_v3_relay_only_mode_after_auth_no_v3() {
    let peer: PlayerId = PEER_UUID.parse().unwrap();

    let async_mock = SharedMock::new(vec![AUTH]);
    let (client, mut events) = SignalFishClient::start(async_mock, SignalFishConfig::new("app"));
    let _ = events.recv().await; // Connected
    let _ = events.recv().await; // Authenticated
    let async_err = client.send_offer(peer, "sdp").unwrap_err();

    let poll_mock = SharedMock::new(vec![AUTH]);
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
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
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
