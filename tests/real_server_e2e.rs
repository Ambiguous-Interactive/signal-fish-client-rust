#![cfg(feature = "transport-websocket")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing,
    clippy::unreachable,
    clippy::arithmetic_side_effects
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

#[cfg(all(feature = "tls", feature = "token-binding"))]
use futures_util::{SinkExt as _, StreamExt as _};
#[cfg(all(feature = "tls", feature = "token-binding"))]
use rustls::pki_types::pem::PemObject as _;
#[cfg(all(feature = "tls", feature = "token-binding"))]
use sha2::{Digest as _, Sha256};
use signal_fish_client::protocol::{
    ConnectionInfo, DeliveryClass, DeliveryGapReason, GameDataEncoding, RelayTransport,
};
#[cfg(all(feature = "tls", feature = "token-binding", feature = "polling-client"))]
use signal_fish_client::SignalFishPollingClient;
#[cfg(all(feature = "tls", feature = "token-binding"))]
use signal_fish_client::TokenBindingFailure;
use signal_fish_client::{
    ErrorCode, GameDataDelivery, JoinRoomParams, SignalFishClient, SignalFishConfig,
    SignalFishError, SignalFishEvent, SpectatorStateChangeReason, WebSocketTransport,
};
#[cfg(all(feature = "tls", feature = "token-binding"))]
use signal_fish_client::{TokenBindingMode, TokenBindingStatus, WebSocketConnectOptions};

// ── Harness ─────────────────────────────────────────────────────────

struct ServerGuard {
    child: Child,
}

#[cfg(all(feature = "tls", feature = "token-binding"))]
struct TlsFixture {
    directory: std::path::PathBuf,
    ca_certificate: std::path::PathBuf,
    certificate: std::path::PathBuf,
    private_key: std::path::PathBuf,
    client_certificate: std::path::PathBuf,
    client_private_key: std::path::PathBuf,
}

#[cfg(all(feature = "tls", feature = "token-binding"))]
impl TlsFixture {
    fn generate() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "signal-fish-token-binding-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&directory).expect("create token-binding TLS fixture directory");
        let ca_certificate = directory.join("ca-cert.pem");
        let ca_private_key = directory.join("ca-key.pem");
        let certificate = directory.join("server-cert.pem");
        let private_key = directory.join("server-key.pem");
        let server_csr = directory.join("server.csr");
        let server_extensions = directory.join("server-ext.cnf");
        let client_certificate = directory.join("client-cert.pem");
        let client_private_key = directory.join("client-key.pem");
        let client_csr = directory.join("client.csr");
        let client_extensions = directory.join("client-ext.cnf");
        let run = |command: &mut Command, operation: &str| {
            let status = command
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap_or_else(|error| panic!("run openssl to {operation}: {error}"));
            assert!(status.success(), "openssl must {operation}");
        };

        let mut command = Command::new("openssl");
        command
            .args([
                "req", "-x509", "-newkey", "rsa:2048", "-sha256", "-nodes", "-keyout",
            ])
            .arg(&ca_private_key)
            .arg("-out")
            .arg(&ca_certificate)
            .args([
                "-days",
                "1",
                "-subj",
                "/CN=Signal Fish test CA",
                "-addext",
                "basicConstraints=critical,CA:TRUE",
                "-addext",
                "keyUsage=critical,keyCertSign,cRLSign",
            ]);
        run(&mut command, "generate the test CA");

        let mut command = Command::new("openssl");
        command
            .args([
                "req", "-new", "-newkey", "rsa:2048", "-sha256", "-nodes", "-keyout",
            ])
            .arg(&private_key)
            .arg("-out")
            .arg(&server_csr)
            .args(["-subj", "/CN=localhost"]);
        run(&mut command, "generate the server certificate request");
        std::fs::write(
            &server_extensions,
            "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost,IP:127.0.0.1\n",
        )
        .expect("write server certificate extensions");
        let mut command = Command::new("openssl");
        command
            .args(["x509", "-req", "-in"])
            .arg(&server_csr)
            .arg("-CA")
            .arg(&ca_certificate)
            .arg("-CAkey")
            .arg(&ca_private_key)
            .args(["-CAcreateserial", "-out"])
            .arg(&certificate)
            .args(["-days", "1", "-sha256", "-extfile"])
            .arg(&server_extensions);
        run(&mut command, "sign the server certificate");

        let mut command = Command::new("openssl");
        command
            .args([
                "req", "-new", "-newkey", "rsa:2048", "-sha256", "-nodes", "-keyout",
            ])
            .arg(&client_private_key)
            .arg("-out")
            .arg(&client_csr)
            .args(["-subj", "/CN=signal-fish-client-test"]);
        run(&mut command, "generate the client certificate request");
        std::fs::write(
            &client_extensions,
            "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=clientAuth\n",
        )
        .expect("write client certificate extensions");
        let mut command = Command::new("openssl");
        command
            .args(["x509", "-req", "-in"])
            .arg(&client_csr)
            .arg("-CA")
            .arg(&ca_certificate)
            .arg("-CAkey")
            .arg(&ca_private_key)
            .args(["-CAcreateserial", "-out"])
            .arg(&client_certificate)
            .args(["-days", "1", "-sha256", "-extfile"])
            .arg(&client_extensions);
        run(&mut command, "sign the client certificate");

        Self {
            directory,
            ca_certificate,
            certificate,
            private_key,
            client_certificate,
            client_private_key,
        }
    }

    fn client_config(&self) -> std::sync::Arc<rustls::ClientConfig> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let certificate = rustls::pki_types::CertificateDer::from_pem_file(&self.ca_certificate)
            .expect("parse generated CA certificate");
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(certificate)
            .expect("trust generated token-binding server certificate");
        std::sync::Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        )
    }

    fn mtls_client_config(&self) -> std::sync::Arc<rustls::ClientConfig> {
        let ca_certificate = rustls::pki_types::CertificateDer::from_pem_file(&self.ca_certificate)
            .expect("parse generated CA certificate");
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(ca_certificate)
            .expect("trust generated CA certificate");
        let certificate =
            rustls::pki_types::CertificateDer::from_pem_file(&self.client_certificate)
                .expect("parse generated client certificate");
        let private_key = rustls::pki_types::PrivateKeyDer::from_pem_file(&self.client_private_key)
            .expect("parse generated client private key");
        std::sync::Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_client_auth_cert(vec![certificate], private_key)
                .expect("configure generated client identity"),
        )
    }

    fn client_fingerprint(&self) -> String {
        let certificate =
            rustls::pki_types::CertificateDer::from_pem_file(&self.client_certificate)
                .expect("parse generated client certificate");
        Sha256::digest(certificate.as_ref())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

#[cfg(all(feature = "tls", feature = "token-binding"))]
impl Drop for TlsFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[cfg(all(feature = "tls", feature = "token-binding"))]
type RawTlsWebSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[cfg(all(feature = "tls", feature = "token-binding"))]
async fn raw_token_binding_connect(
    url: &str,
    tls_config: std::sync::Arc<rustls::ClientConfig>,
) -> (RawTlsWebSocket, zeroize::Zeroizing<[u8; 32]>) {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    use tokio_tungstenite::tungstenite::http::header::{SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_PROTOCOL};

    let mut request = url
        .into_client_request()
        .expect("raw negative client URL must be valid");
    let handshake_key = request
        .headers()
        .get(SEC_WEBSOCKET_KEY)
        .and_then(|value| value.to_str().ok())
        .expect("tungstenite must generate a handshake key");
    let handshake_key = zeroize::Zeroizing::new(
        STANDARD
            .decode(handshake_key)
            .expect("generated handshake key must be base64"),
    );
    request.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        tokio_tungstenite::tungstenite::http::HeaderValue::from_static(
            "signalfish.tokenbinding.v2",
        ),
    );
    let (mut stream, response) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio_tungstenite::connect_async_tls_with_config(
            request,
            None,
            true,
            Some(tokio_tungstenite::Connector::Rustls(tls_config)),
        )
        .await
    })
    .await
    .expect("raw token-binding handshake must complete within 10s")
    .expect("raw token-binding handshake must succeed");
    assert_eq!(
        response
            .headers()
            .get(SEC_WEBSOCKET_PROTOCOL)
            .and_then(|value| value.to_str().ok()),
        Some("signalfish.tokenbinding.v2")
    );
    let challenge = stream
        .next()
        .await
        .expect("server must send a challenge")
        .expect("challenge frame must be valid")
        .into_text()
        .expect("challenge must be text");
    let challenge: serde_json::Value =
        serde_json::from_str(&challenge).expect("challenge must parse");
    let nonce = challenge["data"]["nonce"]
        .as_str()
        .expect("challenge nonce must be a string");
    let nonce = zeroize::Zeroizing::new(STANDARD.decode(nonce).expect("nonce must be base64"));
    let hkdf = hkdf::Hkdf::<sha2::Sha256>::new(Some(nonce.as_slice()), handshake_key.as_slice());
    let mut secret = zeroize::Zeroizing::new([0_u8; 32]);
    hkdf.expand(b"signalfish.tokenbinding.v2/session-key", secret.as_mut())
        .expect("raw negative client must derive the session key");
    (stream, secret)
}

#[cfg(all(feature = "tls", feature = "token-binding"))]
fn raw_signed_json(
    secret: &[u8],
    sequence: u64,
    signed_payload: &str,
    sent_type: &str,
    fingerprint: Option<&str>,
) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use hmac::Mac as _;

    type HmacSha256 = hmac::Hmac<sha2::Sha256>;
    let mut mac = <HmacSha256 as hmac::KeyInit>::new_from_slice(secret)
        .expect("SHA-256 accepts the derived key");
    mac.update(b"signalfish.tokenbinding.v2\0json\0");
    mac.update(&sequence.to_be_bytes());
    mac.update(signed_payload.as_bytes());
    if let Some(fingerprint) = fingerprint {
        mac.update(fingerprint.as_bytes());
    }
    serde_json::json!({
        "type": sent_type,
        "token_binding": {
            "version": 2,
            "scheme": "server_nonce_hkdf_sha256",
            "sequence": sequence,
            "signature": STANDARD.encode(mac.finalize().into_bytes()),
            "fingerprint": fingerprint
        }
    })
    .to_string()
}

#[cfg(all(feature = "tls", feature = "token-binding"))]
async fn expect_token_binding_rejection(stream: &mut RawTlsWebSocket, expected_message: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(message) = stream.next().await {
            match message {
                Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                    let value: serde_json::Value =
                        serde_json::from_str(&text).expect("server response must parse");
                    assert_ne!(value["type"], "Pong", "invalid proof must not reach Ping");
                    if value["type"] == "Error" {
                        assert_eq!(value["data"]["error_code"], "UNAUTHORIZED");
                        assert_eq!(value["data"]["message"], expected_message);
                        return;
                    }
                }
                Ok(tokio_tungstenite::tungstenite::Message::Close(close)) => {
                    panic!("server closed before its token-binding error: {close:?}")
                }
                Err(error) => {
                    panic!("server socket failed before its token-binding error: {error}")
                }
                Ok(_) => {}
            }
        }
        panic!("server ended the stream before its token-binding error");
    })
    .await
    .expect("server must reject an invalid proof promptly");
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl ServerGuard {
    /// Deliver SIGTERM so the server starts its graceful shutdown drain:
    /// v3 clients receive a `GoingAway` advisory and, after the configured
    /// grace, the authoritative WebSocket close with semantic code 4000.
    #[cfg(unix)]
    fn terminate_gracefully(&self) {
        let pid = self.child.id().to_string();
        let status = Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .expect("spawn kill to deliver SIGTERM to the server");
        assert!(status.success(), "kill -TERM {pid} must succeed");
    }

    #[cfg(not(unix))]
    fn terminate_gracefully(&self) {
        panic!("the graceful SIGTERM drain cell requires a Unix host");
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

#[cfg(all(feature = "tls", feature = "token-binding"))]
async fn spawn_required_fingerprint_server(tls: &TlsFixture) -> Option<(ServerGuard, String)> {
    let certificate = tls.certificate.to_string_lossy().into_owned();
    let private_key = tls.private_key.to_string_lossy().into_owned();
    let client_ca = tls.ca_certificate.to_string_lossy().into_owned();
    let (guard, url) = spawn_server(&[
        ("SIGNAL_FISH__SECURITY__TRANSPORT__TLS__ENABLED", "true"),
        (
            "SIGNAL_FISH__SECURITY__TRANSPORT__TLS__CERTIFICATE_PATH",
            certificate.as_str(),
        ),
        (
            "SIGNAL_FISH__SECURITY__TRANSPORT__TLS__PRIVATE_KEY_PATH",
            private_key.as_str(),
        ),
        (
            "SIGNAL_FISH__SECURITY__TRANSPORT__TLS__CLIENT_CA_CERT_PATH",
            client_ca.as_str(),
        ),
        (
            "SIGNAL_FISH__SECURITY__TRANSPORT__TLS__CLIENT_AUTH",
            "require",
        ),
        (
            "SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__ENABLED",
            "true",
        ),
        (
            "SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__REQUIRED",
            "true",
        ),
        (
            "SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__REQUIRE_CLIENT_FINGERPRINT",
            "true",
        ),
    ])
    .await?;
    Some((guard, url.replacen("ws://127.0.0.1", "wss://localhost", 1)))
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
    let transport = WebSocketTransport::connect_with_timeout(url, Duration::from_secs(10))
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

#[cfg(all(feature = "tls", feature = "token-binding", feature = "polling-client"))]
async fn wait_for_polling_event(
    client: &mut SignalFishPollingClient<WebSocketTransport>,
    what: &str,
    deadline: Duration,
    mut predicate: impl FnMut(&SignalFishEvent) -> bool,
) -> SignalFishEvent {
    let end = Instant::now() + deadline;
    loop {
        for event in client.poll() {
            if predicate(&event) {
                return event;
            }
        }
        assert!(Instant::now() < end, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn recv_next_event(
    events: &mut tokio::sync::mpsc::Receiver<SignalFishEvent>,
    what: &str,
) -> SignalFishEvent {
    match tokio::time::timeout(Duration::from_secs(5), events.recv()).await {
        Ok(Some(event)) => event,
        Ok(None) => panic!("event stream ended while waiting for {what}"),
        Err(_) => panic!("timed out waiting for {what}"),
    }
}

// ── Tests ───────────────────────────────────────────────────────────

/// Native token-binding-v2 interoperability against required WSS on the pinned
/// Server 0.8 release. A Pong after mixed JSON/binary traffic proves the server
/// accepted the shared proof sequence instead of disconnecting the client.
#[cfg(all(feature = "tls", feature = "token-binding"))]
#[tokio::test]
#[ignore = "requires pinned Signal Fish Server 0.8 and openssl; set SIGNAL_FISH_SERVER_BIN"]
async fn e2e_server_080_required_token_binding_wss() {
    let tls = TlsFixture::generate();
    let certificate = tls.certificate.to_string_lossy().into_owned();
    let private_key = tls.private_key.to_string_lossy().into_owned();
    let Some((_guard, url)) = spawn_server(&[
        ("SIGNAL_FISH__SECURITY__TRANSPORT__TLS__ENABLED", "true"),
        (
            "SIGNAL_FISH__SECURITY__TRANSPORT__TLS__CERTIFICATE_PATH",
            certificate.as_str(),
        ),
        (
            "SIGNAL_FISH__SECURITY__TRANSPORT__TLS__PRIVATE_KEY_PATH",
            private_key.as_str(),
        ),
        (
            "SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__ENABLED",
            "true",
        ),
        (
            "SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__REQUIRED",
            "true",
        ),
    ])
    .await
    else {
        eprintln!("skipping: SIGNAL_FISH_SERVER_BIN not set");
        return;
    };
    let url = url.replacen("ws://127.0.0.1", "wss://localhost", 1);
    let options = WebSocketConnectOptions::new().with_token_binding(TokenBindingMode::Required);
    let transport = WebSocketTransport::connect_with_tls_config(&url, options, tls.client_config())
        .await
        .expect("required token-binding WSS must connect");
    assert_eq!(transport.token_binding_status(), TokenBindingStatus::Active);

    let mut config = SignalFishConfig::new(app_id()).enable_v3();
    config.game_data_format = Some(GameDataEncoding::MessagePack);
    let (mut client, mut events) = SignalFishClient::start(transport, config);
    wait_for_event(
        &mut events,
        "Authenticated",
        Duration::from_secs(5),
        |event| matches!(event, SignalFishEvent::Authenticated { .. }),
    )
    .await;
    client
        .join_room(JoinRoomParams::new("e2e-token-binding", "protected-client"))
        .expect("token-bound client must queue JoinRoom");
    wait_for_event(&mut events, "RoomJoined", Duration::from_secs(5), |event| {
        matches!(event, SignalFishEvent::RoomJoined { .. })
    })
    .await;
    client
        .send_game_data(serde_json::json!({"protected": 1}))
        .expect("token-bound JSON must queue");
    client
        .send_binary_game_data(vec![1, 2, 3, 4])
        .expect("token-bound MessagePack frame must queue");
    client.ping().expect("token-bound Ping must queue");
    let pong = wait_for_event(&mut events, "Pong", Duration::from_secs(5), |event| {
        matches!(
            event,
            SignalFishEvent::Pong | SignalFishEvent::Disconnected { .. }
        )
    })
    .await;
    assert!(
        matches!(pong, SignalFishEvent::Pong),
        "Server 0.8 must accept mixed token-bound traffic: {pong:?}"
    );
    client.shutdown().await;
}

/// Required WSS + mTLS interoperability proves the SDK binds every proof to
/// the leaf certificate rustls actually selected for this connection.
#[cfg(all(feature = "tls", feature = "token-binding"))]
#[tokio::test]
#[ignore = "requires pinned Signal Fish Server 0.8 and openssl; set SIGNAL_FISH_SERVER_BIN"]
async fn e2e_server_080_required_client_fingerprint_token_binding_wss() {
    let tls = TlsFixture::generate();
    let Some((_guard, url)) = spawn_required_fingerprint_server(&tls).await else {
        eprintln!("skipping: SIGNAL_FISH_SERVER_BIN not set");
        return;
    };
    let options = WebSocketConnectOptions::new()
        .with_token_binding(TokenBindingMode::Required)
        .with_require_client_fingerprint(true);
    let transport =
        WebSocketTransport::connect_with_tls_config(&url, options, tls.mtls_client_config())
            .await
            .expect("required fingerprint-bound token-binding WSS must connect");
    assert_eq!(transport.token_binding_status(), TokenBindingStatus::Active);

    let mut config = SignalFishConfig::new(app_id()).enable_v3();
    config.game_data_format = Some(GameDataEncoding::MessagePack);
    let (mut client, mut events) = SignalFishClient::start(transport, config);
    wait_for_event(
        &mut events,
        "Authenticated",
        Duration::from_secs(5),
        |event| matches!(event, SignalFishEvent::Authenticated { .. }),
    )
    .await;
    client
        .join_room(JoinRoomParams::new(
            "e2e-fingerprint-token-binding",
            "fingerprint-client",
        ))
        .expect("fingerprint-bound client must queue JoinRoom");
    wait_for_event(&mut events, "RoomJoined", Duration::from_secs(5), |event| {
        matches!(event, SignalFishEvent::RoomJoined { .. })
    })
    .await;
    client
        .send_game_data(serde_json::json!({"fingerprint_protected": 1}))
        .expect("fingerprint-bound JSON must queue");
    client
        .send_binary_game_data(vec![5, 6, 7, 8])
        .expect("fingerprint-bound MessagePack frame must queue");
    client.ping().expect("fingerprint-bound Ping must queue");
    let pong = wait_for_event(&mut events, "Pong", Duration::from_secs(5), |event| {
        matches!(
            event,
            SignalFishEvent::Pong | SignalFishEvent::Disconnected { .. }
        )
    })
    .await;
    assert!(
        matches!(pong, SignalFishEvent::Pong),
        "Server 0.8 must accept mixed fingerprint-bound traffic: {pong:?}"
    );
    client.shutdown().await;
}

/// Local enforcement of the strictest profile: against a server that requires
/// token binding but never requests a client certificate, the opt-in policy
/// must fail the connect locally after the unsigned-capable handshake instead
/// of trusting the server to reject the fingerprint-less proof.
#[cfg(all(feature = "tls", feature = "token-binding"))]
#[tokio::test]
#[ignore = "requires pinned Signal Fish Server 0.8 and openssl; set SIGNAL_FISH_SERVER_BIN"]
async fn e2e_server_080_require_client_fingerprint_option_rejects_fingerprint_less_signer() {
    let tls = TlsFixture::generate();
    let certificate = tls.certificate.to_string_lossy().into_owned();
    let private_key = tls.private_key.to_string_lossy().into_owned();
    let Some((_guard, url)) = spawn_server(&[
        ("SIGNAL_FISH__SECURITY__TRANSPORT__TLS__ENABLED", "true"),
        (
            "SIGNAL_FISH__SECURITY__TRANSPORT__TLS__CERTIFICATE_PATH",
            certificate.as_str(),
        ),
        (
            "SIGNAL_FISH__SECURITY__TRANSPORT__TLS__PRIVATE_KEY_PATH",
            private_key.as_str(),
        ),
        (
            "SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__ENABLED",
            "true",
        ),
        (
            "SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__REQUIRED",
            "true",
        ),
    ])
    .await
    else {
        eprintln!("skipping: SIGNAL_FISH_SERVER_BIN not set");
        return;
    };
    let url = url.replacen("ws://127.0.0.1", "wss://localhost", 1);
    let options = WebSocketConnectOptions::new()
        .with_token_binding(TokenBindingMode::Required)
        .with_require_client_fingerprint(true);
    let error = WebSocketTransport::connect_with_tls_config(
        &url,
        options,
        tls.client_config(), // trusted server roots, no client certificate
    )
    .await
    .expect_err("fingerprint-less signer must fail the local requirement");
    assert!(
        matches!(
            error,
            SignalFishError::TokenBinding(TokenBindingFailure::MissingClientFingerprint)
        ),
        "unexpected error: {error:?}"
    );
}

/// The caller-driven client uses the same fully connected native transport and
/// fingerprint-bound signer as the Tokio background driver.
#[cfg(all(feature = "tls", feature = "token-binding", feature = "polling-client"))]
#[tokio::test]
#[ignore = "requires pinned Signal Fish Server 0.8 and openssl; set SIGNAL_FISH_SERVER_BIN"]
async fn e2e_server_080_polling_client_fingerprint_token_binding_wss() {
    let tls = TlsFixture::generate();
    let Some((_guard, url)) = spawn_required_fingerprint_server(&tls).await else {
        eprintln!("skipping: SIGNAL_FISH_SERVER_BIN not set");
        return;
    };
    let options = WebSocketConnectOptions::new()
        .with_token_binding(TokenBindingMode::Required)
        .with_require_client_fingerprint(true);
    let transport =
        WebSocketTransport::connect_with_tls_config(&url, options, tls.mtls_client_config())
            .await
            .expect("polling fingerprint-bound token-binding WSS must connect");
    let mut config = SignalFishConfig::new(app_id()).enable_v3();
    config.game_data_format = Some(GameDataEncoding::MessagePack);
    let mut client = SignalFishPollingClient::new(transport, config);

    wait_for_polling_event(
        &mut client,
        "Authenticated",
        Duration::from_secs(5),
        |event| matches!(event, SignalFishEvent::Authenticated { .. }),
    )
    .await;
    client
        .join_room(JoinRoomParams::new(
            "e2e-polling-fingerprint-token-binding",
            "polling-fingerprint-client",
        ))
        .expect("polling fingerprint-bound client must queue JoinRoom");
    wait_for_polling_event(&mut client, "RoomJoined", Duration::from_secs(5), |event| {
        matches!(event, SignalFishEvent::RoomJoined { .. })
    })
    .await;
    client
        .send_game_data(serde_json::json!({"polling_fingerprint_protected": 1}))
        .expect("polling fingerprint-bound JSON must queue");
    client
        .send_binary_game_data(vec![9, 10, 11, 12])
        .expect("polling fingerprint-bound MessagePack frame must queue");
    client
        .ping()
        .expect("polling fingerprint-bound Ping must queue");
    let pong = wait_for_polling_event(&mut client, "Pong", Duration::from_secs(5), |event| {
        matches!(
            event,
            SignalFishEvent::Pong | SignalFishEvent::Disconnected { .. }
        )
    })
    .await;
    assert!(
        matches!(pong, SignalFishEvent::Pong),
        "Server 0.8 must accept polling fingerprint-bound traffic: {pong:?}"
    );
    client.close();
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.is_closing() {
            let _ = client.poll();
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("polling fingerprint-bound client must close promptly");
}

/// The pinned server rejects missing/wrong certificate claims, cross-connection
/// replay, and payload/signature tampering under the fingerprint-required profile.
#[cfg(all(feature = "tls", feature = "token-binding"))]
#[tokio::test]
#[ignore = "requires pinned Signal Fish Server 0.8 and openssl; set SIGNAL_FISH_SERVER_BIN"]
async fn e2e_server_080_rejects_invalid_client_fingerprint_proofs() {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use tokio_tungstenite::tungstenite::Message;

    let tls = TlsFixture::generate();
    let Some((_guard, url)) = spawn_required_fingerprint_server(&tls).await else {
        eprintln!("skipping: SIGNAL_FISH_SERVER_BIN not set");
        return;
    };
    let fingerprint = tls.client_fingerprint();

    let (mut stream, secret) = raw_token_binding_connect(&url, tls.mtls_client_config()).await;
    let missing = raw_signed_json(&secret[..], 1, r#"{"type":"Ping"}"#, "Ping", None);
    stream
        .send(Message::Text(missing.into()))
        .await
        .expect("missing-fingerprint proof must reach the server verifier");
    expect_token_binding_rejection(&mut stream, "Client fingerprint required").await;

    let (mut stream, secret) = raw_token_binding_connect(&url, tls.mtls_client_config()).await;
    let wrong = raw_signed_json(
        &secret[..],
        1,
        r#"{"type":"Ping"}"#,
        "Ping",
        Some("a6e096040a2324b64f87a60b559e08bc2d5f76f971737a27c1c53958a3789777"),
    );
    stream
        .send(Message::Text(wrong.into()))
        .await
        .expect("wrong-fingerprint proof must reach the server verifier");
    expect_token_binding_rejection(&mut stream, "Client fingerprint mismatch").await;

    let (mut first_stream, first_secret) =
        raw_token_binding_connect(&url, tls.mtls_client_config()).await;
    let replay = raw_signed_json(
        &first_secret[..],
        1,
        r#"{"type":"Ping"}"#,
        "Ping",
        Some(&fingerprint),
    );
    first_stream
        .send(Message::Text(replay.clone().into()))
        .await
        .expect("valid fingerprint-bound Ping must send");
    let valid_pong = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(message) = first_stream.next().await {
            if let Ok(Message::Text(text)) = message {
                let value: serde_json::Value =
                    serde_json::from_str(&text).expect("valid Ping response must parse");
                if value["type"] == "Pong" {
                    return true;
                }
            }
        }
        false
    })
    .await
    .expect("valid fingerprint-bound Ping must receive a prompt response");
    assert!(valid_pong, "the source proof must be valid before replay");
    let (mut replay_stream, _fresh_secret) =
        raw_token_binding_connect(&url, tls.mtls_client_config()).await;
    replay_stream
        .send(Message::Text(replay.into()))
        .await
        .expect("cross-connection replay must reach the server verifier");
    expect_token_binding_rejection(&mut replay_stream, "Invalid token binding signature").await;

    let (mut stream, secret) = raw_token_binding_connect(&url, tls.mtls_client_config()).await;
    let payload_tamper = raw_signed_json(
        &secret[..],
        1,
        r#"{"type":"Pong"}"#,
        "Ping",
        Some(&fingerprint),
    );
    stream
        .send(Message::Text(payload_tamper.into()))
        .await
        .expect("payload tamper must reach the server verifier");
    expect_token_binding_rejection(&mut stream, "Invalid token binding signature").await;

    let (mut stream, secret) = raw_token_binding_connect(&url, tls.mtls_client_config()).await;
    let valid = raw_signed_json(
        &secret[..],
        1,
        r#"{"type":"Ping"}"#,
        "Ping",
        Some(&fingerprint),
    );
    let mut signature_tamper: serde_json::Value =
        serde_json::from_str(&valid).expect("valid proof must parse");
    signature_tamper["token_binding"]["signature"] =
        serde_json::Value::String(STANDARD.encode([0_u8; 32]));
    stream
        .send(Message::Text(signature_tamper.to_string().into()))
        .await
        .expect("signature tamper must reach the server verifier");
    expect_token_binding_rejection(&mut stream, "Invalid token binding signature").await;
}

/// Raw negative cases prove that the pinned server rejects proof replay,
/// signature/payload mismatch, a wrong per-connection key, and malformed proof
/// envelopes. Each case gets a fresh challenge and sequence space.
#[cfg(all(feature = "tls", feature = "token-binding"))]
#[tokio::test]
#[ignore = "requires pinned Signal Fish Server 0.8 and openssl; set SIGNAL_FISH_SERVER_BIN"]
async fn e2e_server_080_rejects_invalid_token_binding_proofs() {
    use tokio_tungstenite::tungstenite::Message;

    let tls = TlsFixture::generate();
    let certificate = tls.certificate.to_string_lossy().into_owned();
    let private_key = tls.private_key.to_string_lossy().into_owned();
    let Some((_guard, url)) = spawn_server(&[
        ("SIGNAL_FISH__SECURITY__TRANSPORT__TLS__ENABLED", "true"),
        (
            "SIGNAL_FISH__SECURITY__TRANSPORT__TLS__CERTIFICATE_PATH",
            certificate.as_str(),
        ),
        (
            "SIGNAL_FISH__SECURITY__TRANSPORT__TLS__PRIVATE_KEY_PATH",
            private_key.as_str(),
        ),
        (
            "SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__ENABLED",
            "true",
        ),
        (
            "SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__REQUIRED",
            "true",
        ),
    ])
    .await
    else {
        eprintln!("skipping: SIGNAL_FISH_SERVER_BIN not set");
        return;
    };
    let url = url.replacen("ws://127.0.0.1", "wss://localhost", 1);

    // Replay: the first sequence-1 Ping succeeds, then the same proof fails.
    let (mut stream, secret) = raw_token_binding_connect(&url, tls.client_config()).await;
    let signed_ping = raw_signed_json(&secret[..], 1, r#"{"type":"Ping"}"#, "Ping", None);
    stream
        .send(Message::Text(signed_ping.clone().into()))
        .await
        .expect("valid raw Ping must send");
    let pong = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(message) = stream.next().await {
            let message = message.expect("valid Ping response must be a frame");
            if let Message::Text(text) = message {
                let value: serde_json::Value =
                    serde_json::from_str(&text).expect("valid Ping response must parse");
                if value["type"] == "Pong" {
                    return true;
                }
            }
        }
        false
    })
    .await
    .expect("valid signed Ping must receive a prompt response");
    assert!(pong, "the control Ping must prove sequence 1 was accepted");
    stream
        .send(Message::Text(signed_ping.into()))
        .await
        .expect("replayed frame must reach the server verifier");
    expect_token_binding_rejection(&mut stream, "Invalid token binding version or sequence").await;

    // Tamper: authenticate a different canonical payload than the one sent.
    let (mut stream, secret) = raw_token_binding_connect(&url, tls.client_config()).await;
    let tampered = raw_signed_json(&secret[..], 1, r#"{"type":"Pong"}"#, "Ping", None);
    stream
        .send(Message::Text(tampered.into()))
        .await
        .expect("tampered frame must reach the server verifier");
    expect_token_binding_rejection(&mut stream, "Invalid token binding signature").await;

    // Wrong key/cross-connection reuse: sign B with A's derived key.
    let (stale_stream, stale_secret) = raw_token_binding_connect(&url, tls.client_config()).await;
    let (mut stream, _fresh_secret) = raw_token_binding_connect(&url, tls.client_config()).await;
    drop(stale_stream);
    let wrong_key = raw_signed_json(&stale_secret[..], 1, r#"{"type":"Ping"}"#, "Ping", None);
    stream
        .send(Message::Text(wrong_key.into()))
        .await
        .expect("wrong-key frame must reach the server verifier");
    expect_token_binding_rejection(&mut stream, "Invalid token binding signature").await;

    // Malformed envelope: a proof member must be the exact structured object.
    let (mut stream, _secret) = raw_token_binding_connect(&url, tls.client_config()).await;
    stream
        .send(Message::Text(
            r#"{"type":"Ping","token_binding":"malformed"}"#.into(),
        ))
        .await
        .expect("malformed proof must reach the server verifier");
    expect_token_binding_rejection(&mut stream, "Invalid token binding proof").await;

    let (mut stream, _secret) = raw_token_binding_connect(&url, tls.client_config()).await;
    stream
        .send(Message::Binary(vec![0x81, 0xad, b't'].into()))
        .await
        .expect("malformed binary envelope must reach the server verifier");
    expect_token_binding_rejection(&mut stream, "Invalid token-bound binary frame").await;
}

/// Server 0.8 fallback smoke: an unsupported Rkyv preference produces the
/// warning-before-authentication sequence, then resolves coherently to JSON.
#[tokio::test]
#[ignore = "requires Signal Fish Server 0.8; set SIGNAL_FISH_SERVER_BIN or SIGNAL_FISH_E2E_URL"]
async fn e2e_server_080_rkyv_request_resolves_to_json() {
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

    let transport = WebSocketTransport::connect_with_timeout(&url, Duration::from_secs(10))
        .await
        .expect("connect to real Server 0.8");
    let mut config = SignalFishConfig::new(app_id()).enable_v3();
    config.game_data_format = Some(GameDataEncoding::Rkyv);
    let (mut client, mut events) = SignalFishClient::start(transport, config);

    assert!(matches!(
        recv_next_event(&mut events, "Connected").await,
        SignalFishEvent::Connected
    ));
    assert!(matches!(
        recv_next_event(&mut events, "unsupported-format Error").await,
        SignalFishEvent::Error {
            error_code: Some(ErrorCode::UnsupportedGameDataFormat),
            ..
        }
    ));
    assert!(matches!(
        recv_next_event(&mut events, "Authenticated").await,
        SignalFishEvent::Authenticated { .. }
    ));
    let protocol_info = recv_next_event(&mut events, "ProtocolInfo").await;
    let SignalFishEvent::ProtocolInfo(protocol_info) = protocol_info else {
        panic!("expected ProtocolInfo after authentication, got {protocol_info:?}")
    };
    assert_eq!(
        protocol_info.game_data_formats,
        vec![GameDataEncoding::Json, GameDataEncoding::MessagePack]
    );
    assert_eq!(
        client.requested_game_data_format(),
        Some(GameDataEncoding::Rkyv)
    );
    assert_eq!(
        client.effective_game_data_format(),
        Some(GameDataEncoding::Json)
    );
    assert!(matches!(
        client.send_binary_game_data(vec![1, 2, 3]),
        Err(SignalFishError::NotInRoom)
    ));

    client
        .join_room(JoinRoomParams::new("e2e-format-fallback", "json-client"))
        .expect("fallback client should queue JoinRoom");
    wait_for_event(&mut events, "RoomJoined", Duration::from_secs(5), |event| {
        matches!(event, SignalFishEvent::RoomJoined { .. })
    })
    .await;
    assert!(matches!(
        client.send_binary_game_data(vec![1, 2, 3]),
        Err(SignalFishError::BinaryFormatNotNegotiated)
    ));
    client
        .send_game_data(serde_json::json!({"fallback": "json"}))
        .expect("fallback client should send JSON game data");
    client.ping().expect("queue fallback send fence");
    let fence = wait_for_event(
        &mut events,
        "fallback send fence",
        Duration::from_secs(5),
        |event| matches!(event, SignalFishEvent::Pong | SignalFishEvent::Error { .. }),
    )
    .await;
    assert!(
        matches!(fence, SignalFishEvent::Pong),
        "Server 0.8 must accept JSON after Rkyv fallback, got {fence:?}"
    );
    assert_eq!(client.stats().game_data_sent, 1);
    client.shutdown().await;
}

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
    let (mut a, mut a_events) = connect_authenticated(&url, SignalFishConfig::new(app_id())).await;
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
    let (mut _b, mut b_events) = connect_authenticated(
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

/// Exercises the complete v3 reconnect-token lifecycle against server 0.4+:
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
    let (mut a, mut a_events) =
        connect_authenticated(&url, SignalFishConfig::new(app_id()).enable_mesh()).await;
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

    a.set_ready().expect("set ready before finalizing room");
    a.ping().expect("queue readiness fence ping");
    wait_for_event(
        &mut a_events,
        "readiness fence Pong",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::Pong),
    )
    .await;
    a.start_game().expect("finalize room before reconnect");
    let first_plan = wait_for_event(
        &mut a_events,
        "initial SessionPlan",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::SessionPlan { .. }),
    )
    .await;
    let SignalFishEvent::SessionPlan {
        generation: Some(first_generation),
        ..
    } = first_plan
    else {
        panic!("server 0.8 initial SessionPlan must carry a generation")
    };
    drop(a);
    drop(a_events);

    // Fresh v3 connection consumes the token.
    let (mut b, mut b_events) =
        connect_authenticated(&url, SignalFishConfig::new(app_id()).enable_mesh()).await;
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
        current_players,
        replay,
        reconnection_token,
        sender_watermarks,
        ..
    } = response
    else {
        panic!("a compatible server must accept its issued reconnect token")
    };
    let rotated_token = reconnection_token.expect("Reconnected must rotate the token");
    assert!(rotated_token != first_token, "reconnect token must rotate");
    assert!(replay.is_some(), "v3 Reconnected must report replay status");
    let player_ids = current_players
        .iter()
        .map(|player| player.id)
        .collect::<std::collections::BTreeSet<_>>();
    let watermark_ids = sender_watermarks
        .iter()
        .map(|watermark| watermark.player_id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        watermark_ids, player_ids,
        "Reconnected watermarks must exactly cover the current-player snapshot"
    );
    assert!(
        b.snapshot().reconnection_token.as_deref() == Some(rotated_token.as_str()),
        "snapshot must expose the replacement token"
    );

    let replacement_plan = wait_for_event(
        &mut b_events,
        "replacement SessionPlan",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::SessionPlan { .. }),
    )
    .await;
    assert!(
        matches!(
            replacement_plan,
            SignalFishEvent::SessionPlan {
                generation: Some(generation),
                ..
            } if generation != first_generation
        ),
        "a finalized reconnect must receive a fresh plan generation"
    );
    b.shutdown().await;
}

/// Server 0.8 WebRTC smoke: signals carry the authoritative plan generation,
/// and host failover publishes a new generation to every survivor.
#[tokio::test]
#[ignore = "requires Signal Fish Server 0.8; set SIGNAL_FISH_SERVER_BIN or SIGNAL_FISH_E2E_URL"]
async fn e2e_server_080_generation_signal_and_host_replan() {
    let (_guard, url): (Option<ServerGuard>, String) = match external_url() {
        Some(url) => (None, url),
        None => match spawn_server(&[("SIGNAL_FISH__SESSION__DEFAULT_TOPOLOGY", "host")]).await {
            Some((guard, url)) => (Some(guard), url),
            None => {
                eprintln!("skipping: neither SIGNAL_FISH_E2E_URL nor SIGNAL_FISH_SERVER_BIN set");
                return;
            }
        },
    };

    let config = || SignalFishConfig::new(app_id()).enable_mesh();
    let (mut a, mut a_events) = connect_authenticated(&url, config()).await;
    a.join_room(JoinRoomParams::new("e2e-generation", "alpha"))
        .expect("A join_room");
    let a_joined = wait_for_event(&mut a_events, "A RoomJoined", Duration::from_secs(5), |e| {
        matches!(e, SignalFishEvent::RoomJoined { .. })
    })
    .await;
    let SignalFishEvent::RoomJoined {
        room_code,
        player_id: a_id,
        ..
    } = a_joined
    else {
        unreachable!()
    };

    let (mut b, mut b_events) = connect_authenticated(&url, config()).await;
    b.join_room(JoinRoomParams::new("e2e-generation", "bravo").with_room_code(&room_code))
        .expect("B join_room");
    let b_joined = wait_for_event(&mut b_events, "B RoomJoined", Duration::from_secs(5), |e| {
        matches!(e, SignalFishEvent::RoomJoined { .. })
    })
    .await;
    let SignalFishEvent::RoomJoined {
        player_id: b_id, ..
    } = b_joined
    else {
        unreachable!()
    };

    let (mut c, mut c_events) = connect_authenticated(&url, config()).await;
    c.join_room(JoinRoomParams::new("e2e-generation", "charlie").with_room_code(&room_code))
        .expect("C join_room");
    let c_joined = wait_for_event(&mut c_events, "C RoomJoined", Duration::from_secs(5), |e| {
        matches!(e, SignalFishEvent::RoomJoined { .. })
    })
    .await;
    let SignalFishEvent::RoomJoined {
        player_id: c_id, ..
    } = c_joined
    else {
        unreachable!()
    };

    for client in [&mut a, &mut b, &mut c] {
        client.set_ready().expect("set_ready");
        client.ping().expect("queue readiness fence ping");
    }
    for events in [&mut a_events, &mut b_events, &mut c_events] {
        wait_for_event(
            events,
            "readiness fence Pong",
            Duration::from_secs(5),
            |e| matches!(e, SignalFishEvent::Pong),
        )
        .await;
    }
    a.start_game().expect("start game");

    let first_a = wait_for_event(
        &mut a_events,
        "A SessionPlan",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::SessionPlan { .. }),
    )
    .await;
    let SignalFishEvent::SessionPlan {
        generation: Some(first_generation),
        peers,
        ..
    } = first_a
    else {
        panic!("server 0.8 SessionPlan must carry generation")
    };
    assert!(peers.iter().any(|peer| peer.player_id == b_id));

    for (events, who) in [
        (&mut b_events, "B SessionPlan"),
        (&mut c_events, "C SessionPlan"),
    ] {
        let plan = wait_for_event(events, who, Duration::from_secs(5), |e| {
            matches!(e, SignalFishEvent::SessionPlan { .. })
        })
        .await;
        assert!(matches!(
            plan,
            SignalFishEvent::SessionPlan {
                generation: Some(generation),
                ..
            } if generation == first_generation
        ));
    }

    a.send_offer(b_id, "first-generation-offer")
        .expect("send first-generation offer");
    let first_signal = wait_for_event(
        &mut b_events,
        "first-generation Signal",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::SignalReceived { .. }),
    )
    .await;
    assert!(matches!(
        first_signal,
        SignalFishEvent::SignalReceived {
            from,
            generation: Some(generation),
            ..
        } if from == a_id && generation == first_generation
    ));

    a.leave_room().expect("host A leave room");
    let second_b = wait_for_event(
        &mut b_events,
        "B replacement SessionPlan",
        Duration::from_secs(5),
        |e| {
            matches!(
                e,
                SignalFishEvent::SessionPlan {
                    generation: Some(generation),
                    ..
                } if *generation != first_generation
            )
        },
    )
    .await;
    let SignalFishEvent::SessionPlan {
        generation: Some(second_generation),
        peers,
        ..
    } = second_b
    else {
        unreachable!()
    };
    assert_ne!(second_generation, first_generation);
    assert!(peers.iter().any(|peer| peer.player_id == c_id));

    let second_c = wait_for_event(
        &mut c_events,
        "C replacement SessionPlan",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::SessionPlan { .. }),
    )
    .await;
    assert!(matches!(
        second_c,
        SignalFishEvent::SessionPlan {
            generation: Some(generation),
            ..
        } if generation == second_generation
    ));

    b.send_offer(c_id, "second-generation-offer")
        .expect("send second-generation offer");
    let second_signal = wait_for_event(
        &mut c_events,
        "second-generation Signal",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::SignalReceived { .. }),
    )
    .await;
    assert!(matches!(
        second_signal,
        SignalFishEvent::SignalReceived {
            from,
            generation: Some(generation),
            ..
        } if from == b_id && generation == second_generation
    ));

    a.shutdown().await;
    b.shutdown().await;
    c.shutdown().await;
}

/// Server 0.4 compatibility smoke: authoritative plans and relayed signals
/// remain generation-less end to end.
#[tokio::test]
#[ignore = "requires Signal Fish Server 0.4; set SIGNAL_FISH_SERVER_BIN or SIGNAL_FISH_E2E_URL"]
async fn e2e_server_040_generationless_mesh_signal() {
    let (_guard, url): (Option<ServerGuard>, String) = match external_url() {
        Some(url) => (None, url),
        None => match spawn_server(&[("SIGNAL_FISH__SESSION__DEFAULT_TOPOLOGY", "mesh")]).await {
            Some((guard, url)) => (Some(guard), url),
            None => {
                eprintln!("skipping: neither SIGNAL_FISH_E2E_URL nor SIGNAL_FISH_SERVER_BIN set");
                return;
            }
        },
    };

    let config = || SignalFishConfig::new(app_id()).enable_mesh();
    let (mut a, mut a_events) = connect_authenticated(&url, config()).await;
    a.join_room(JoinRoomParams::new("e2e-legacy-mesh", "alpha"))
        .expect("A join_room");
    let a_joined = wait_for_event(&mut a_events, "A RoomJoined", Duration::from_secs(5), |e| {
        matches!(e, SignalFishEvent::RoomJoined { .. })
    })
    .await;
    let SignalFishEvent::RoomJoined { room_code, .. } = a_joined else {
        unreachable!()
    };

    let (mut b, mut b_events) = connect_authenticated(&url, config()).await;
    b.join_room(JoinRoomParams::new("e2e-legacy-mesh", "bravo").with_room_code(&room_code))
        .expect("B join_room");
    let b_joined = wait_for_event(&mut b_events, "B RoomJoined", Duration::from_secs(5), |e| {
        matches!(e, SignalFishEvent::RoomJoined { .. })
    })
    .await;
    let SignalFishEvent::RoomJoined {
        player_id: b_id, ..
    } = b_joined
    else {
        unreachable!()
    };

    for client in [&mut a, &mut b] {
        client.set_ready().expect("set_ready");
        client.ping().expect("queue readiness fence ping");
    }
    for events in [&mut a_events, &mut b_events] {
        wait_for_event(
            events,
            "readiness fence Pong",
            Duration::from_secs(5),
            |e| matches!(e, SignalFishEvent::Pong),
        )
        .await;
    }
    a.start_game().expect("start game");

    let a_plan = wait_for_event(
        &mut a_events,
        "A generation-less SessionPlan",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::SessionPlan { .. }),
    )
    .await;
    assert!(matches!(
        a_plan,
        SignalFishEvent::SessionPlan {
            generation: None,
            ..
        }
    ));
    let b_plan = wait_for_event(
        &mut b_events,
        "B generation-less SessionPlan",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::SessionPlan { .. }),
    )
    .await;
    assert!(matches!(
        b_plan,
        SignalFishEvent::SessionPlan {
            generation: None,
            ..
        }
    ));

    a.send_offer(b_id, "legacy-generationless-offer")
        .expect("send legacy offer");
    let signal = wait_for_event(
        &mut b_events,
        "generation-less Signal",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::SignalReceived { .. }),
    )
    .await;
    assert!(matches!(
        signal,
        SignalFishEvent::SignalReceived {
            generation: None,
            ..
        }
    ));

    a.shutdown().await;
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
    // parse work are real). Each synchronous burst puts a substantial batch in
    // the client command queue ahead of its Ping without yielding to the
    // transport task, so the measured Pong proves ordered control-plane
    // progress behind real bulk traffic rather than after a pre-ping quiet
    // interval.
    const PING_ROUNDS: u32 = 8;
    const BURST_FRAMES: u32 = 512;
    let payload = serde_json::json!({ "pad": "y".repeat(512) });
    let mut pongs = 0u32;
    let mut accepted_game_data = 0u32;
    let mut worst_rtt = Duration::ZERO;
    for _ in 0..PING_ROUNDS {
        for _ in 0..BURST_FRAMES {
            a.send_game_data(payload.clone())
                .expect("the bounded pre-ping flood must be admitted");
            accepted_game_data += 1;
        }
        let sent_at = Instant::now();
        a.ping().expect("queue ping");
        wait_for_event(&mut a_events, "Pong", Duration::from_secs(3), |e| {
            matches!(e, SignalFishEvent::Pong)
        })
        .await;
        let rtt = sent_at.elapsed();
        worst_rtt = worst_rtt.max(rtt);
        pongs += 1;
    }
    println!(
        "E4 SMOKE DATA: {accepted_game_data} accepted GameData frames, \
         {pongs} pongs, worst sender-side RTT {worst_rtt:?}"
    );
    assert_eq!(
        accepted_game_data,
        PING_ROUNDS * BURST_FRAMES,
        "every substantial pre-ping flood batch must be accepted"
    );
    assert_eq!(pongs, PING_ROUNDS);
    assert!(
        worst_rtt < Duration::from_secs(2),
        "sender-side Pong RTT should stay low during its own flood; got {worst_rtt:?}"
    );
    a.shutdown().await;
}

fn unix_epoch_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("wall clock must be after the Unix epoch")
        .as_millis()
        .try_into()
        .expect("epoch milliseconds must fit u64")
}

/// The GoingAway → close-4000 pair (issue #190): a connected v3 client must
/// observe the graceful-shutdown advisory with an honest deadline and retry
/// hint, and then the authoritative coded close attributed to the server.
/// The drain must be triggerable with a bounded grace, so this cell is
/// spawn-mode only.
#[tokio::test]
#[ignore = "requires pinned Signal Fish Server 0.8; set SIGNAL_FISH_SERVER_BIN"]
async fn e2e_server_080_going_away_close_4000() {
    const GRACE_SECS: u64 = 2;
    // The deadline must be honored within a scheduling-slack budget on top of
    // the configured grace, not stretched by another full grace window.
    const DEADLINE_SLACK_MS: u64 = 5_000;
    let grace = GRACE_SECS.to_string();
    let Some((guard, url)) =
        spawn_server(&[("SIGNAL_FISH__SERVER__DRAIN_GRACE_SECS", &grace)]).await
    else {
        eprintln!("skipping: SIGNAL_FISH_SERVER_BIN not set");
        return;
    };

    let (_client, mut events) =
        connect_authenticated(&url, SignalFishConfig::new(app_id()).enable_v3()).await;

    let signal_epoch_ms = unix_epoch_ms_now();
    guard.terminate_gracefully();

    // The advisory is best-effort but reaches an idle connection: a missing
    // GoingAway before the close must fail this cell loudly.
    let advisory = wait_for_event(&mut events, "GoingAway", Duration::from_secs(10), |e| {
        matches!(e, SignalFishEvent::GoingAway { .. })
    })
    .await;
    let SignalFishEvent::GoingAway {
        deadline_ms,
        retry_after_secs,
    } = advisory
    else {
        unreachable!()
    };
    assert!(
        deadline_ms >= signal_epoch_ms
            && deadline_ms <= signal_epoch_ms + GRACE_SECS * 1_000 + DEADLINE_SLACK_MS,
        "GoingAway deadline {deadline_ms} must be a near-future epoch-ms deadline \
         issued at SIGTERM time ({signal_epoch_ms} + {GRACE_SECS}s grace)"
    );
    assert_eq!(
        retry_after_secs,
        Some(GRACE_SECS),
        "server 0.8 derives the retry hint from the configured grace"
    );

    // The close frame is authoritative: the terminal event must carry the
    // semantic drain code, attributed to the server (never a send failure).
    let disconnected = wait_for_event(&mut events, "Disconnected", Duration::from_secs(10), |e| {
        matches!(e, SignalFishEvent::Disconnected { .. })
    })
    .await;
    let SignalFishEvent::Disconnected {
        reason,
        last_server_error,
    } = disconnected
    else {
        unreachable!()
    };
    let reason = reason.expect("a coded server close must surface close metadata");
    assert!(
        reason.starts_with("closed by server:"),
        "a drain close is peer-initiated; got {reason:?}"
    );
    assert!(
        reason.contains("code=Some(4000)"),
        "the drain must close with semantic code 4000; got {reason:?}"
    );
    assert!(
        last_server_error.is_none(),
        "a drain close carries no error farewell; got {last_server_error:?}"
    );
    println!("E5 DATA: GoingAway deadline_ms={deadline_ms} retry_after_secs={retry_after_secs:?}; close reason={reason:?}");
}

/// Spectator lifecycle live smoke (issue #190): join → observe → exit against
/// the pinned Server 0.8, exercising the live baseline snapshot, the room
/// broadcasts, and the voluntary-exit acknowledgment that round 20/22 covered
/// only through vendored-spec fixtures.
#[tokio::test]
#[ignore = "requires Signal Fish Server 0.8; set SIGNAL_FISH_SERVER_BIN or SIGNAL_FISH_E2E_URL"]
async fn e2e_server_080_spectator_live_smoke() {
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

    // A player creates the room the spectator will watch.
    let (mut a, mut a_events) = connect_authenticated(&url, SignalFishConfig::new(app_id())).await;
    a.join_room(JoinRoomParams::new("e2e-spectator-smoke", "host"))
        .expect("A join_room");
    let joined = wait_for_event(&mut a_events, "A RoomJoined", Duration::from_secs(5), |e| {
        matches!(e, SignalFishEvent::RoomJoined { .. })
    })
    .await;
    let SignalFishEvent::RoomJoined {
        room_id,
        room_code,
        player_id,
        ..
    } = joined
    else {
        unreachable!()
    };

    // The spectator joins by room code and observes the live player roster.
    let (mut s, mut s_events) = connect_authenticated(&url, SignalFishConfig::new(app_id())).await;
    s.join_as_spectator(
        "e2e-spectator-smoke".to_string(),
        room_code.clone(),
        "watcher".to_string(),
    )
    .expect("S join_as_spectator");
    let spectated = wait_for_event(
        &mut s_events,
        "S SpectatorJoined",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::SpectatorJoined { .. }),
    )
    .await;
    let SignalFishEvent::SpectatorJoined {
        room_id: spectated_room_id,
        room_code: spectated_room_code,
        spectator_id,
        current_players,
        ..
    } = spectated
    else {
        unreachable!()
    };
    assert_eq!(spectated_room_id, room_id, "the join must target A's room");
    assert_eq!(
        spectated_room_code, room_code,
        "the join must echo the requested room code"
    );
    assert_ne!(spectator_id, player_id);
    assert!(
        current_players
            .iter()
            .any(|p| p.id == player_id && p.name == "host"),
        "the spectator baseline must include the live player roster; got {current_players:?}"
    );

    // The room observes the spectator join.
    let observed = wait_for_event(
        &mut a_events,
        "A NewSpectatorJoined",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::NewSpectatorJoined { .. }),
    )
    .await;
    let SignalFishEvent::NewSpectatorJoined { spectator, .. } = observed else {
        unreachable!()
    };
    assert_eq!(spectator.id, spectator_id);

    // Voluntary exit: the acknowledgment must name the room and drain the
    // spectator roster; the room must observe the departure.
    s.leave_spectator().expect("S leave_spectator");
    let left = wait_for_event(
        &mut s_events,
        "S SpectatorLeft",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::SpectatorLeft { .. }),
    )
    .await;
    let SignalFishEvent::SpectatorLeft {
        room_id: left_room_id,
        room_code: left_room_code,
        reason,
        current_spectators,
    } = left
    else {
        unreachable!()
    };
    assert_eq!(
        left_room_id,
        Some(room_id),
        "server 0.8 names the room on a voluntary SpectatorLeft"
    );
    assert_eq!(left_room_code, Some(room_code.clone()));
    assert_eq!(reason, Some(SpectatorStateChangeReason::VoluntaryLeave));
    assert!(
        !current_spectators
            .iter()
            .any(|spectator| spectator.id == spectator_id),
        "the departing spectator must not be counted in the remaining roster"
    );

    let departed = wait_for_event(
        &mut a_events,
        "A SpectatorDisconnected",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::SpectatorDisconnected { .. }),
    )
    .await;
    let SignalFishEvent::SpectatorDisconnected {
        spectator_id: departed_id,
        reason: departed_reason,
        ..
    } = departed
    else {
        unreachable!()
    };
    assert_eq!(departed_id, spectator_id);
    assert_eq!(
        departed_reason,
        Some(SpectatorStateChangeReason::VoluntaryLeave)
    );
    s.shutdown().await;
    a.shutdown().await;
}

/// The v3 data plane against the pinned Server 0.8 authority: creator
/// auto-authority, an explicit release/handoff, the local `AuthorityRequired`
/// gate, `ConnectionInfo` propagation through `GameStarting`, and
/// `Latest`-keyed coalescing as observed through `GameData` plus the
/// accountable `DeliveryReport`. Spawn-mode only: the coalescing leg needs
/// the server's batch window enabled, because with batching off the outbound
/// drain may legitimately pop the first frame before the successor's enqueue
/// wins the race — server-correct, but not the behavior this cell pins.
#[tokio::test]
#[ignore = "requires pinned Signal Fish Server 0.8; set SIGNAL_FISH_SERVER_BIN"]
async fn e2e_server_080_authority_handoff_and_latest_delivery() {
    let Some((guard, url)) = spawn_server(&[
        ("SIGNAL_FISH__WEBSOCKET__ENABLE_BATCHING", "true"),
        ("SIGNAL_FISH__WEBSOCKET__BATCH_INTERVAL_MS", "1000"),
    ])
    .await
    else {
        eprintln!("skipping: SIGNAL_FISH_SERVER_BIN not set");
        return;
    };
    let _guard = guard;

    // ── A joins first; the creator auto-holds authority ─────────────
    let (mut a, mut a_events) =
        connect_authenticated(&url, SignalFishConfig::new(app_id()).enable_v3()).await;
    a.join_room(JoinRoomParams::new("e2e-authority-latest", "alice").with_supports_authority(true))
        .expect("join_room");
    let a_joined = wait_for_event(&mut a_events, "A RoomJoined", Duration::from_secs(5), |e| {
        matches!(e, SignalFishEvent::RoomJoined { .. })
    })
    .await;
    let SignalFishEvent::RoomJoined {
        room_code,
        player_id: player_a,
        is_authority: a_authority,
        ..
    } = a_joined
    else {
        unreachable!()
    };
    assert!(
        a_authority,
        "the creator must auto-hold authority (server default)"
    );

    // ── B joins by code and observes A's authority ──────────────────
    let (mut b, mut b_events) =
        connect_authenticated(&url, SignalFishConfig::new(app_id()).enable_v3()).await;
    b.join_room(
        JoinRoomParams::new("e2e-authority-latest", "bob")
            .with_room_code(&room_code)
            .with_supports_authority(true),
    )
    .expect("join_room by code");
    let b_joined = wait_for_event(&mut b_events, "B RoomJoined", Duration::from_secs(5), |e| {
        matches!(e, SignalFishEvent::RoomJoined { .. })
    })
    .await;
    let SignalFishEvent::RoomJoined {
        player_id: player_b,
        is_authority: b_authority,
        ..
    } = b_joined
    else {
        unreachable!()
    };
    assert!(!b_authority, "the joiner must not hold authority");
    let _ = wait_for_event(
        &mut a_events,
        "A PlayerJoined",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::PlayerJoined { .. }),
    )
    .await;

    // ── A releases; B claims ────────────────────────────────────────
    a.request_authority(false).expect("queue release");
    wait_for_event(
        &mut a_events,
        "A release grant",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::AuthorityResponse { granted: true, .. }),
    )
    .await;
    for (events, who) in [(&mut a_events, "A"), (&mut b_events, "B")] {
        let changed = wait_for_event(
            events,
            &format!("{who} AuthorityChanged after release"),
            Duration::from_secs(5),
            |e| matches!(e, SignalFishEvent::AuthorityChanged { .. }),
        )
        .await;
        let SignalFishEvent::AuthorityChanged {
            authority_player,
            you_are_authority,
        } = changed
        else {
            unreachable!()
        };
        assert_eq!(authority_player, None, "{who} must observe a vacant seat");
        assert!(!you_are_authority);
    }

    b.request_authority(true).expect("queue claim");
    wait_for_event(
        &mut b_events,
        "B claim grant",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::AuthorityResponse { granted: true, .. }),
    )
    .await;
    let b_changed = wait_for_event(
        &mut b_events,
        "B AuthorityChanged after claim",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::AuthorityChanged { .. }),
    )
    .await;
    let SignalFishEvent::AuthorityChanged {
        authority_player: claimed,
        you_are_authority: b_is_authority,
    } = b_changed
    else {
        unreachable!()
    };
    assert_eq!(claimed, Some(player_b));
    assert!(b_is_authority);
    let a_changed = wait_for_event(
        &mut a_events,
        "A AuthorityChanged after claim",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::AuthorityChanged { .. }),
    )
    .await;
    let SignalFishEvent::AuthorityChanged {
        authority_player: observed,
        you_are_authority,
    } = a_changed
    else {
        unreachable!()
    };
    assert_eq!(observed, Some(player_b));
    assert!(
        !you_are_authority,
        "A must see the claim as a non-authority"
    );

    // Re-claiming while already authority is a conflict, reported honestly.
    b.request_authority(true).expect("queue duplicate claim");
    let denied = wait_for_event(
        &mut b_events,
        "B duplicate-claim denial",
        Duration::from_secs(5),
        |e| matches!(e, SignalFishEvent::AuthorityResponse { granted: false, .. }),
    )
    .await;
    let SignalFishEvent::AuthorityResponse {
        error_code: Some(code),
        ..
    } = denied
    else {
        panic!("the duplicate-claim denial must carry a structured error code")
    };
    assert_eq!(code, ErrorCode::AuthorityConflict);

    // ── Connection info flows through GameStarting to the peer ──────
    a.provide_connection_info(ConnectionInfo::Relay {
        host: "e2e-relay-host".into(),
        port: 7777,
        transport: RelayTransport::Udp,
        allocation_id: "e2e-alloc".into(),
        token: "e2e-token".into(),
        client_id: None,
    })
    .expect("queue connection info");

    a.set_ready().expect("A ready");
    b.set_ready().expect("B ready");
    a.ping().expect("queue A readiness fence");
    wait_for_event(&mut a_events, "A fence Pong", Duration::from_secs(5), |e| {
        matches!(e, SignalFishEvent::Pong)
    })
    .await;
    b.ping().expect("queue B readiness fence");
    wait_for_event(&mut b_events, "B fence Pong", Duration::from_secs(5), |e| {
        matches!(e, SignalFishEvent::Pong)
    })
    .await;

    let a_start = a.start_game();
    assert!(
        matches!(&a_start, Err(SignalFishError::AuthorityRequired)),
        "a non-authority start_game must be refused locally, got {a_start:?}"
    );
    b.start_game().expect("authority starts the game");
    for (events, who) in [(&mut a_events, "A"), (&mut b_events, "B")] {
        let starting = wait_for_event(
            events,
            &format!("{who} GameStarting"),
            Duration::from_secs(5),
            |e| matches!(e, SignalFishEvent::GameStarting { .. }),
        )
        .await;
        let SignalFishEvent::GameStarting {
            peer_connections, ..
        } = starting
        else {
            unreachable!()
        };
        let peer = peer_connections
            .iter()
            .find(|peer| peer.player_id == player_a)
            .expect("A must appear in peer_connections");
        assert!(!peer.is_authority, "A must not hold authority anymore");
        match &peer.connection_info {
            Some(ConnectionInfo::Relay {
                host,
                allocation_id,
                ..
            }) => {
                assert_eq!(host, "e2e-relay-host");
                assert_eq!(allocation_id, "e2e-alloc");
            }
            other => panic!("must observe A's Relay connection info, got {other:?}"),
        }
    }

    // ── Latest-keyed coalescing with an accountable report ──────────
    b.send_game_data_with_delivery(
        serde_json::json!({ "n": 1 }),
        GameDataDelivery::Latest { key: 1 },
    )
    .expect("queue latest n=1");
    b.send_game_data_with_delivery(
        serde_json::json!({ "n": 2 }),
        GameDataDelivery::Latest { key: 1 },
    )
    .expect("queue latest n=2");

    // The report rides the latency-sensitive control lane and typically
    // arrives *before* the batch-windowed survivor frame, so collect both in
    // one loop regardless of order.
    let mut survivor = None;
    let mut payload = None;
    {
        let end = Instant::now() + Duration::from_secs(5);
        while (survivor.is_none() || payload.is_none()) && Instant::now() < end {
            let remaining = end.saturating_duration_since(Instant::now());
            match tokio::time::timeout(remaining, a_events.recv()).await {
                Ok(Some(SignalFishEvent::GameData {
                    from_player,
                    data,
                    key: Some(1),
                    class,
                    ..
                })) => {
                    // Only the newest value may ever arrive: a server that
                    // relays the superseded n=1 frame fails right here.
                    assert_eq!(
                        data,
                        serde_json::json!({ "n": 2 }),
                        "only the newest value survives"
                    );
                    survivor = Some((from_player, data, class));
                }
                Ok(Some(SignalFishEvent::DeliveryReport(report))) => payload = Some(report),
                Ok(Some(_)) => {}
                Ok(None) => panic!("event stream ended while waiting for the survivor/report"),
                Err(_) => break,
            }
        }
    }
    let (from_player, data, class) =
        survivor.expect("the surviving latest frame must be delivered");
    assert_eq!(from_player, player_b);
    assert_eq!(
        data,
        serde_json::json!({ "n": 2 }),
        "only the newest value survives"
    );
    assert_eq!(class, Some(DeliveryClass::Latest));
    let payload = payload.expect("the coalescing report must reach the recipient");
    assert!(
        payload.per_class.latest.superseded >= 1,
        "the coalesced frame must be accounted as superseded"
    );
    assert!(
        payload
            .gaps
            .iter()
            .any(|gap| gap.from_player == player_b
                && gap.reason == DeliveryGapReason::LatestSuperseded),
        "the superseded sequence must appear as a LatestSuperseded gap"
    );

    // Nothing else may arrive: the superseded n=1 is gone for good. This
    // bounded negative drain is what gives the pin teeth — a server (or
    // config) that relays both frames fails here.
    let drain_end = Instant::now() + Duration::from_secs(2);
    while Instant::now() < drain_end {
        let remaining = drain_end.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, a_events.recv()).await {
            Ok(Some(SignalFishEvent::GameData {
                from_player, data, ..
            })) if from_player == player_b => {
                panic!("the superseded frame must never be delivered: {data}")
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("A's event stream ended during the coalescing drain"),
            Err(_) => break,
        }
    }

    // The accountability machine must digest the report without complaint.
    let drain_end = Instant::now() + Duration::from_millis(500);
    while Instant::now() < drain_end {
        let remaining = drain_end.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, a_events.recv()).await {
            Ok(Some(event)) => match event {
                SignalFishEvent::ProtocolViolation { .. } => {
                    panic!("accepting a LatestSuperseded report must not violate the protocol")
                }
                SignalFishEvent::DecodeFailed { .. } => {
                    panic!("the DeliveryReport must decode cleanly")
                }
                _ => {}
            },
            Ok(None) | Err(_) => break,
        }
    }

    a.shutdown().await;
    b.shutdown().await;
}
