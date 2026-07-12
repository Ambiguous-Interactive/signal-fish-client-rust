#![cfg(feature = "transport-websocket")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing
)]
//! End-to-end tests against a **real** Signal Fish server.
//!
//! Ignored by default so `cargo test --all-features` stays green offline.
//! Two ways to provide a server:
//!
//! 1. **Spawn mode (preferred — tests control server config):**
//!    `SIGNAL_FISH_SERVER_BIN=/path/to/signal-fish-server \
//!     cargo test --test real_server_e2e -- --ignored --test-threads=1`
//!    Each test spawns its own server on an ephemeral port with the
//!    `SIGNAL_FISH__*` env overrides it needs.
//! 2. **External mode:** `SIGNAL_FISH_E2E_URL=ws://host:port/v2/ws` — only
//!    the tests that work with default server config run; tests that need
//!    custom queue/timeout config skip with a message.
//!
//! `SIGNAL_FISH_E2E_APP_ID` overrides the app id (default `e2e-test-app`;
//! the server accepts any app id unless `require_websocket_auth` is on).

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use signal_fish_client::{
    ErrorCode, JoinRoomParams, SignalFishClient, SignalFishConfig, SignalFishEvent,
    WebSocketTransport,
};

// ── Harness ─────────────────────────────────────────────────────────

struct ServerGuard {
    child: Child,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn app_id() -> String {
    std::env::var("SIGNAL_FISH_E2E_APP_ID").unwrap_or_else(|_| "e2e-test-app".to_string())
}

/// Reserve an ephemeral port by binding to :0 and dropping the listener.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

/// Spawn a server binary (from `SIGNAL_FISH_SERVER_BIN`) with the given
/// `SIGNAL_FISH__*` overrides. Returns `None` when the env var is unset.
async fn spawn_server(overrides: &[(&str, &str)]) -> Option<(ServerGuard, String)> {
    let bin = std::env::var("SIGNAL_FISH_SERVER_BIN").ok()?;
    let port = free_port();

    let mut cmd = Command::new(&bin);
    cmd.env("SIGNAL_FISH__PORT", port.to_string())
        .env("SIGNAL_FISH__LOGGING__LEVEL", "warn")
        // The server refuses to start without metrics-auth configuration;
        // this throwaway localhost instance does not expose metrics.
        .env("SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH", "false")
        // Secure-by-default builds require registered app ids; the tests use
        // an arbitrary one, so run the throwaway server in open mode.
        .env("SIGNAL_FISH__SECURITY__REQUIRE_WEBSOCKET_AUTH", "false")
        // The default SDK-compatibility registry recognizes only
        // unity/godot/godot-rust/test — and none with a minimum this crate's
        // version satisfies — so an honest Rust client cannot authenticate
        // against a default-config server at all (tracked upstream).
        .env("SIGNAL_FISH__PROTOCOL__SDK_COMPATIBILITY__ENFORCE", "false")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, value) in overrides {
        cmd.env(key, value);
    }
    let child = cmd.spawn().expect("spawn signal-fish-server");
    let guard = ServerGuard { child };

    // Wait for readiness: TCP connect retry with a deadline.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
            Ok(_) => break,
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("server did not become ready on port {port}: {e}"),
        }
    }

    Some((guard, format!("ws://127.0.0.1:{port}/v2/ws")))
}

/// External server URL, if configured.
fn external_url() -> Option<String> {
    std::env::var("SIGNAL_FISH_E2E_URL").ok()
}

/// Connect a client and drain events until `Authenticated`.
async fn connect_authenticated(
    url: &str,
    config: SignalFishConfig,
) -> (
    SignalFishClient,
    tokio::sync::mpsc::Receiver<SignalFishEvent>,
) {
    let transport = WebSocketTransport::connect(url)
        .await
        .expect("connect to real server");
    let (client, mut events) = SignalFishClient::start(transport, config);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Some(SignalFishEvent::Authenticated { .. })) => break,
            Ok(Some(SignalFishEvent::AuthenticationError { error, error_code })) => {
                panic!("authentication failed: {error} ({error_code:?})")
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("event stream ended before Authenticated"),
            Err(_) => panic!("timed out waiting for Authenticated"),
        }
    }
    (client, events)
}

/// Drain events until the predicate matches, with a deadline. Returns the
/// matching event; panics on timeout or stream end.
async fn wait_for_event(
    events: &mut tokio::sync::mpsc::Receiver<SignalFishEvent>,
    what: &str,
    deadline: Duration,
    mut predicate: impl FnMut(&SignalFishEvent) -> bool,
) -> SignalFishEvent {
    let end = Instant::now() + deadline;
    loop {
        let remaining = end.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Some(ev)) if predicate(&ev) => return ev,
            Ok(Some(_)) => {}
            Ok(None) => panic!("event stream ended while waiting for {what}"),
            Err(_) => panic!("timed out waiting for {what}"),
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────

/// A fully stalled consumer is evicted loudly: the room observes
/// `PlayerLeft`, and the victim — once it resumes draining — observes what
/// the eviction actually looked like from the client side (recorded as
/// experiment data: whether the best-effort `SLOW_CONSUMER` farewell
/// arrived, and what `Disconnected` carried).
#[tokio::test]
#[ignore = "requires a live signal-fish server; set SIGNAL_FISH_SERVER_BIN (spawn mode)"]
async fn e2e_slow_consumer_eviction_is_observable() {
    // Needs custom config → spawn mode only.
    let Some((_guard, url)) = spawn_server(&[
        ("SIGNAL_FISH__WEBSOCKET__SEND_QUEUE_CAPACITY", "8"),
        ("SIGNAL_FISH__WEBSOCKET__SLOW_CONSUMER_TIMEOUT_MS", "500"),
    ])
    .await
    else {
        eprintln!("skipping: SIGNAL_FISH_SERVER_BIN not set");
        return;
    };

    // Sender A: normal config, joins and creates the room.
    let (a, mut a_events) = connect_authenticated(&url, SignalFishConfig::new(app_id())).await;
    a.join_room(JoinRoomParams::new("e2e-evict", "sender"))
        .expect("A join_room");
    let joined = wait_for_event(&mut a_events, "A RoomJoined", Duration::from_secs(5), |e| {
        matches!(e, SignalFishEvent::RoomJoined { .. })
    })
    .await;
    let SignalFishEvent::RoomJoined { room_code, .. } = joined else {
        unreachable!()
    };

    // Victim B: tiny event channel so it wedges as soon as it stops draining.
    let (_b, mut b_events) = connect_authenticated(
        &url,
        SignalFishConfig::new(app_id()).with_event_channel_capacity(1),
    )
    .await;
    // B joins the room, draining just enough to complete the join.
    _b.join_room(JoinRoomParams::new("e2e-evict", "victim").with_room_code(&room_code))
        .expect("B join_room");
    wait_for_event(&mut b_events, "B RoomJoined", Duration::from_secs(5), |e| {
        matches!(e, SignalFishEvent::RoomJoined { .. })
    })
    .await;
    // A sees B join.
    wait_for_event(
        &mut a_events,
        "A PlayerJoined",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::PlayerJoined { .. }),
    )
    .await;

    // B now stops draining entirely (wedged consumer). A floods with large
    // payloads: B's kernel receive buffer must fill before the server's
    // 8-slot outbound queue can exert backpressure, so small payloads would
    // take far longer to trip the eviction.
    let payload = serde_json::json!({ "pad": "x".repeat(8 * 1024) });
    let flood_until = Instant::now() + Duration::from_secs(15);
    let mut a_saw_player_left = false;
    while Instant::now() < flood_until {
        // Keep the flood going; ignore transient SendBufferFull.
        for _ in 0..64 {
            let _ = a.send_game_data(payload.clone());
        }
        // Drain A's own events opportunistically, watching for PlayerLeft.
        while let Ok(ev) = a_events.try_recv() {
            if matches!(ev, SignalFishEvent::PlayerLeft { .. }) {
                a_saw_player_left = true;
            }
        }
        if a_saw_player_left {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        a_saw_player_left,
        "the room must observe the slow consumer's eviction as PlayerLeft \
         within the flood window (queue=8, timeout=500ms, 8KiB payloads)"
    );

    // B resumes draining: record what the eviction looked like client-side.
    let mut saw_farewell_error = None;
    let disconnected = wait_for_event(
        &mut b_events,
        "B Disconnected",
        Duration::from_secs(10),
        |e| {
            if let SignalFishEvent::Error {
                message,
                error_code,
            } = e
            {
                saw_farewell_error = Some((message.clone(), error_code.clone()));
            }
            matches!(e, SignalFishEvent::Disconnected { .. })
        },
    )
    .await;
    let SignalFishEvent::Disconnected {
        reason,
        last_server_error,
    } = disconnected
    else {
        unreachable!()
    };

    // Experiment record (E3): what actually arrived.
    println!("E3 DATA: farewell Error event pre-disconnect: {saw_farewell_error:?}");
    println!("E3 DATA: Disconnected.reason = {reason:?}");
    println!("E3 DATA: Disconnected.last_server_error = {last_server_error:?}");

    // The contract we can assert: B learned it was disconnected, and when
    // the farewell got through it is attributed on the Disconnected event.
    if let Some((_, Some(code))) = &saw_farewell_error {
        assert_eq!(code, &ErrorCode::SlowConsumer);
        let info = last_server_error.expect("farewell must be attributed when received");
        assert_eq!(info.error_code, Some(ErrorCode::SlowConsumer));
    }
}

/// Exercises the complete v3 reconnect-token lifecycle against server 0.4.0:
/// capture the issued token, reconnect after an unexpected disconnect, and
/// observe the rotated replacement token and sender watermarks.
#[tokio::test]
#[ignore = "requires a live signal-fish server; set SIGNAL_FISH_SERVER_BIN or SIGNAL_FISH_E2E_URL"]
async fn e2e_reconnect_after_disconnect_uses_server_token() {
    let (_guard, url): (Option<ServerGuard>, String) = match external_url() {
        Some(url) => (None, url),
        None => match spawn_server(&[]).await {
            Some((guard, url)) => (Some(guard), url),
            None => {
                eprintln!("skipping: neither SIGNAL_FISH_E2E_URL nor SIGNAL_FISH_SERVER_BIN set");
                return;
            }
        },
    };

    // Join a v3 room, retain the server-issued token, then drop the connection
    // abruptly (no LeaveRoom and no graceful shutdown).
    let (a, mut a_events) =
        connect_authenticated(&url, SignalFishConfig::new(app_id()).enable_v3()).await;
    a.join_room(JoinRoomParams::new("e2e-reconnect", "alpha"))
        .expect("join_room");
    let joined = wait_for_event(&mut a_events, "RoomJoined", Duration::from_secs(5), |e| {
        matches!(e, SignalFishEvent::RoomJoined { .. })
    })
    .await;
    let SignalFishEvent::RoomJoined {
        room_id, player_id, ..
    } = joined
    else {
        unreachable!()
    };
    let first_token = a
        .snapshot()
        .reconnection_token
        .expect("v3 RoomJoined must issue a reconnection token");
    drop(a);
    drop(a_events);

    // Fresh v3 connection consumes the token.
    let (mut b, mut b_events) =
        connect_authenticated(&url, SignalFishConfig::new(app_id()).enable_v3()).await;
    b.reconnect(player_id, room_id, first_token.clone())
        .expect("queue Reconnect");

    let response = wait_for_event(
        &mut b_events,
        "reconnect outcome",
        Duration::from_secs(5),
        |e| {
            matches!(
                e,
                SignalFishEvent::Reconnected { .. }
                    | SignalFishEvent::ReconnectionFailed { .. }
                    | SignalFishEvent::Error { .. }
            )
        },
    )
    .await;

    let SignalFishEvent::Reconnected {
        reconnection_token,
        sender_watermarks,
        ..
    } = response
    else {
        panic!("server 0.4.0 must accept its issued reconnect token")
    };
    let rotated_token = reconnection_token.expect("Reconnected must rotate the token");
    assert_ne!(rotated_token, first_token, "reconnect token must rotate");
    assert!(
        sender_watermarks
            .iter()
            .any(|watermark| watermark.player_id == player_id),
        "Reconnected must carry the reconnecting player's watermark"
    );
    assert_eq!(
        b.snapshot().reconnection_token.as_deref(),
        Some(rotated_token.as_str()),
        "snapshot must expose the replacement token"
    );
    b.shutdown().await;
}

/// Smoke check that a flooding sender's own control plane stays healthy:
/// Pings sent during a sustained GameData flood still get Pongs promptly
/// (the sender's outbound queue is not the congested one).
#[tokio::test]
#[ignore = "requires a live signal-fish server; set SIGNAL_FISH_SERVER_BIN or SIGNAL_FISH_E2E_URL"]
async fn e2e_sender_ping_survives_own_game_data_flood() {
    let (_guard, url): (Option<ServerGuard>, String) = match external_url() {
        Some(url) => (None, url),
        None => match spawn_server(&[]).await {
            Some((guard, url)) => (Some(guard), url),
            None => {
                eprintln!("skipping: neither SIGNAL_FISH_E2E_URL nor SIGNAL_FISH_SERVER_BIN set");
                return;
            }
        },
    };

    let (mut a, mut a_events) = connect_authenticated(&url, SignalFishConfig::new(app_id())).await;
    a.join_room(JoinRoomParams::new("e2e-ping", "solo"))
        .expect("join_room");
    wait_for_event(&mut a_events, "RoomJoined", Duration::from_secs(5), |e| {
        matches!(e, SignalFishEvent::RoomJoined { .. })
    })
    .await;

    // Flood (room of one: relay fan-out is empty, but the inbound path and
    // parse work are real) while pinging once per 250ms.
    let payload = serde_json::json!({ "pad": "y".repeat(512) });
    let mut pongs = 0u32;
    let mut worst_rtt = Duration::ZERO;
    for _ in 0..8 {
        let _ = a.send_game_data(payload.clone());
        a.ping().expect("queue ping");
        let sent_at = Instant::now();
        wait_for_event(&mut a_events, "Pong", Duration::from_secs(3), |e| {
            matches!(e, SignalFishEvent::Pong)
        })
        .await;
        let rtt = sent_at.elapsed();
        worst_rtt = worst_rtt.max(rtt);
        pongs += 1;
        for _ in 0..50 {
            let _ = a.send_game_data(payload.clone());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    println!("E4 SMOKE DATA: {pongs} pongs, worst sender-side RTT {worst_rtt:?}");
    assert_eq!(pongs, 8);
    assert!(
        worst_rtt < Duration::from_secs(2),
        "sender-side Pong RTT should stay low during its own flood; got {worst_rtt:?}"
    );
    a.shutdown().await;
}
