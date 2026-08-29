//! WebSocket transport implementation using `tokio-tungstenite`.
//!
//! This module provides [`WebSocketTransport`], a [`Transport`]
//! implementation that communicates over a WebSocket connection. `ws://` is
//! always available; `wss://` requires the optional `tls` feature (rustls with
//! the ring provider and bundled webpki roots), after which TLS is handled
//! transparently via [`MaybeTlsStream`](tokio_tungstenite::MaybeTlsStream).
//!
//! Connections disable Nagle's algorithm (`TCP_NODELAY`) by default for low
//! latency and reject inbound frames or assembled messages larger than 8 MiB;
//! see [`WebSocketConnectOptions`] to override either policy. Both policies
//! apply to connections this type dials — a post-handshake
//! [`from_stream`](WebSocketTransport::from_stream) connection applies neither
//! and preserves caller-owned policy.
//!
//! # Feature gate
//!
//! This module is only available when the `transport-websocket` feature is enabled
//! (it is enabled by default).
//!
//! # Example
//!
//! ```rust,no_run
//! # async fn example() -> Result<(), signal_fish_client::SignalFishError> {
//! use signal_fish_client::WebSocketTransport;
//!
//! let transport = WebSocketTransport::connect("ws://localhost:3536/v2/ws").await?;
//! let _transport = transport; // pass it to SignalFishClient::start
//! # Ok(())
//! # }
//! ```

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::{Sink, Stream};
#[cfg(feature = "token-binding")]
use futures_util::{SinkExt as _, StreamExt as _};
use tokio_tungstenite::tungstenite::{
    protocol::{Message, WebSocketConfig},
    Error as WebSocketError,
};

use crate::error::SignalFishError;
use crate::token_binding::{TokenBindingChallenge, TokenBindingMode, TokenBindingStatus};
use crate::transport::{Transport, TransportCloseInfo, TransportFrame};

/// Type alias for the underlying WebSocket stream.
///
/// Made public so that callers can construct a [`WebSocketTransport`] from an
/// existing stream via [`WebSocketTransport::from_stream`].
pub type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const MAX_SKIPPED_CONTROL_FRAMES_PER_POLL: usize = 64;
const DEFAULT_MAX_INBOUND_MESSAGE_SIZE: usize = 8 * 1024 * 1024;

/// Classify post-request-construction connect failures.
///
/// `validate_request_url` classifies every client-side request-construction
/// failure before I/O, so a `Url` error reaching this function is a
/// defense-in-depth catch (tungstenite re-builds the request after the
/// pre-check) and stays `InvalidConfig`: value- or build-determined, never a
/// network outcome. Every reachable `UrlError` formats as static text (the
/// blocking-only `UnableToConnect` that embeds the URL is unreachable on
/// these async paths), so echoing the message stays inside the ambient-log
/// boundary. `HttpFormat` here means tungstenite could not parse the
/// server's own handshake response after the connection was established — a
/// runtime server fault, which keeps the generic `Io` mapping instead of
/// blaming the caller's configuration.
fn map_connect_error(error: WebSocketError) -> SignalFishError {
    match &error {
        WebSocketError::Url(_) => SignalFishError::InvalidConfig {
            field: "url",
            problem: error.to_string(),
        },
        WebSocketError::Io(io) => SignalFishError::Io(std::io::Error::new(io.kind(), error)),
        _ => SignalFishError::Io(std::io::Error::other(error)),
    }
}

/// Classify client-side request-construction failures: an unparsable URL or
/// an otherwise unbuildable handshake request is caller configuration,
/// rejected before any network I/O.
fn map_request_config_error(error: WebSocketError) -> SignalFishError {
    match &error {
        WebSocketError::Url(_) | WebSocketError::HttpFormat(_) => SignalFishError::InvalidConfig {
            field: "url",
            problem: error.to_string(),
        },
        _ => map_connect_error(error),
    }
}

/// Build the WebSocket handshake request from `url` to validate it before any
/// network I/O, so unparsable URLs report `InvalidConfig` and post-connect
/// `HttpFormat` server-response failures cannot be mislabeled as
/// configuration errors. The scheme must be exactly lowercase `ws` or `wss`
/// (mirroring tungstenite's `uri_mode`, which otherwise runs after the TCP
/// connect): uppercase and foreign schemes are rejected here so the
/// classification never depends on network order. The `wss://` feature check
/// moves the missing-`tls` failure before the TCP connect for the same
/// reason.
fn validate_request_url(url: &str) -> Result<(), SignalFishError> {
    #[cfg(not(feature = "tls"))]
    if url.starts_with("wss://") {
        return Err(SignalFishError::InvalidConfig {
            field: "url",
            problem: "wss:// requires the opt-in `tls` feature".into(),
        });
    }
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    let request = url
        .into_client_request()
        .map_err(map_request_config_error)?;
    let scheme = request.uri().scheme_str();
    if !matches!(scheme, Some("ws") | Some("wss")) {
        return Err(SignalFishError::InvalidConfig {
            field: "url",
            problem: "unsupported URL scheme: must be ws:// or wss://".into(),
        });
    }
    Ok(())
}

fn websocket_config(options: WebSocketConnectOptions) -> Result<WebSocketConfig, SignalFishError> {
    if options.max_inbound_message_size == Some(0) {
        return Err(SignalFishError::InvalidConfig {
            field: "max_inbound_message_size",
            problem: "must be greater than zero or None".into(),
        });
    }
    Ok(WebSocketConfig::default()
        .max_frame_size(options.max_inbound_message_size)
        .max_message_size(options.max_inbound_message_size))
}

/// Reject a required client fingerprint on a connect path that can never
/// observe a certificate selection: only a custom-rustls token-binding
/// connection installs the tracking signer, so every other path fails before
/// network I/O instead of silently dropping the policy.
fn reject_unsatisfiable_client_fingerprint_requirement(
    options: &WebSocketConnectOptions,
) -> Result<(), SignalFishError> {
    if options.require_client_fingerprint {
        return Err(SignalFishError::TokenBinding(
            crate::TokenBindingFailure::MissingClientFingerprint,
        ));
    }
    Ok(())
}

#[cfg(feature = "tls")]
fn install_tls_provider() {
    use std::sync::Once;
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[cfg(all(feature = "tls", feature = "token-binding"))]
#[derive(Clone)]
struct ClientCertificateFingerprintTracker {
    selected: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

#[cfg(all(feature = "tls", feature = "token-binding"))]
impl std::fmt::Debug for ClientCertificateFingerprintTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientCertificateFingerprintTracker")
            .finish_non_exhaustive()
    }
}

#[cfg(all(feature = "tls", feature = "token-binding"))]
impl ClientCertificateFingerprintTracker {
    fn wrap(
        tls_config: std::sync::Arc<rustls::ClientConfig>,
    ) -> (
        std::sync::Arc<rustls::ClientConfig>,
        Option<ClientCertificateFingerprintTracker>,
    ) {
        let tracker = Self {
            selected: std::sync::Arc::new(std::sync::Mutex::new(None)),
        };
        let mut tracked_config = (*tls_config).clone();
        tracked_config.resumption = rustls::client::Resumption::disabled();
        tracked_config.client_auth_cert_resolver =
            std::sync::Arc::new(TrackingClientCertificateResolver {
                delegate: tls_config.client_auth_cert_resolver.clone(),
                tracker: tracker.clone(),
            });
        (std::sync::Arc::new(tracked_config), Some(tracker))
    }

    fn fingerprint(&self) -> Option<String> {
        match self.selected.lock() {
            Ok(selected) => selected.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn record(&self, fingerprint: Option<String>) {
        match self.selected.lock() {
            Ok(mut selected) => *selected = fingerprint,
            Err(poisoned) => *poisoned.into_inner() = fingerprint,
        }
    }
}

#[cfg(all(feature = "tls", feature = "token-binding"))]
struct TrackingClientCertificateResolver {
    delegate: std::sync::Arc<dyn rustls::client::ResolvesClientCert>,
    tracker: ClientCertificateFingerprintTracker,
}

#[cfg(all(feature = "tls", feature = "token-binding"))]
struct TrackingSigningKey {
    delegate: std::sync::Arc<dyn rustls::sign::SigningKey>,
    fingerprint: Option<String>,
    tracker: ClientCertificateFingerprintTracker,
}

#[cfg(all(feature = "tls", feature = "token-binding"))]
impl std::fmt::Debug for TrackingSigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrackingSigningKey").finish_non_exhaustive()
    }
}

#[cfg(all(feature = "tls", feature = "token-binding"))]
impl rustls::sign::SigningKey for TrackingSigningKey {
    fn choose_scheme(
        &self,
        offered: &[rustls::SignatureScheme],
    ) -> Option<Box<dyn rustls::sign::Signer>> {
        let signer = self.delegate.choose_scheme(offered);
        self.tracker
            .record(signer.as_ref().and(self.fingerprint.clone()));
        signer
    }

    fn public_key(&self) -> Option<rustls::pki_types::SubjectPublicKeyInfoDer<'_>> {
        self.delegate.public_key()
    }

    fn algorithm(&self) -> rustls::SignatureAlgorithm {
        self.delegate.algorithm()
    }
}

#[cfg(all(feature = "tls", feature = "token-binding"))]
impl std::fmt::Debug for TrackingClientCertificateResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrackingClientCertificateResolver")
            .finish_non_exhaustive()
    }
}

#[cfg(all(feature = "tls", feature = "token-binding"))]
fn client_certificate_fingerprint(certificate_der: &[u8]) -> String {
    use sha2::Digest as _;

    sha2::Sha256::digest(certificate_der)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(all(feature = "tls", feature = "token-binding"))]
impl rustls::client::ResolvesClientCert for TrackingClientCertificateResolver {
    fn resolve(
        &self,
        root_hint_subjects: &[&[u8]],
        sigschemes: &[rustls::SignatureScheme],
    ) -> Option<std::sync::Arc<rustls::sign::CertifiedKey>> {
        self.tracker.record(None);
        let selected = self.delegate.resolve(root_hint_subjects, sigschemes)?;
        let fingerprint = (!self.delegate.only_raw_public_keys())
            .then(|| selected.cert.first())
            .flatten()
            .map(|certificate| client_certificate_fingerprint(certificate.as_ref()));
        let mut tracked = (*selected).clone();
        tracked.key = std::sync::Arc::new(TrackingSigningKey {
            delegate: selected.key.clone(),
            fingerprint,
            tracker: self.tracker.clone(),
        });
        Some(std::sync::Arc::new(tracked))
    }

    fn only_raw_public_keys(&self) -> bool {
        self.delegate.only_raw_public_keys()
    }

    fn has_certs(&self) -> bool {
        self.delegate.has_certs()
    }
}

#[cfg(feature = "token-binding")]
async fn connect_with_token_binding(
    url: &str,
    options: WebSocketConnectOptions,
    websocket_config: WebSocketConfig,
    #[cfg(feature = "tls")] connector: Option<tokio_tungstenite::Connector>,
    #[cfg(feature = "tls")] client_fingerprint: Option<ClientCertificateFingerprintTracker>,
) -> Result<(WsStream, WebSocketTokenBinding), SignalFishError> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    use tokio_tungstenite::tungstenite::error::{ProtocolError, SubProtocolError};
    use tokio_tungstenite::tungstenite::http::header::{SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_PROTOCOL};

    let mut request = url
        .into_client_request()
        .map_err(map_request_config_error)?;
    let handshake_key = zeroize::Zeroizing::new(
        request
            .headers()
            .get(SEC_WEBSOCKET_KEY)
            .and_then(|value| value.to_str().ok())
            .ok_or(SignalFishError::TokenBinding(
                crate::TokenBindingFailure::InvalidHandshakeKey,
            ))?
            .to_owned(),
    );
    request.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        tokio_tungstenite::tungstenite::http::HeaderValue::from_static(
            crate::token_binding::TOKEN_BINDING_SUBPROTOCOL,
        ),
    );

    #[cfg(feature = "tls")]
    let connected = tokio_tungstenite::connect_async_tls_with_config(
        request,
        Some(websocket_config),
        options.disable_nagle,
        connector.clone(),
    )
    .await;
    #[cfg(not(feature = "tls"))]
    let connected = tokio_tungstenite::connect_async_with_config(
        request,
        Some(websocket_config),
        options.disable_nagle,
    )
    .await;
    let (mut stream, _response) = match connected {
        Ok(connected) => connected,
        Err(WebSocketError::Protocol(ProtocolError::SecWebSocketSubProtocolError(
            SubProtocolError::NoSubProtocol,
        ))) if options.token_binding == TokenBindingMode::Optional => {
            // Tungstenite rejects an otherwise successful 101 when an offered
            // subprotocol is omitted. That exact response proves the server
            // permits an unsigned connection; reconnect without the offer.
            #[cfg(feature = "tls")]
            let fallback = tokio_tungstenite::connect_async_tls_with_config(
                url,
                Some(websocket_config),
                options.disable_nagle,
                connector,
            )
            .await;
            #[cfg(not(feature = "tls"))]
            let fallback = tokio_tungstenite::connect_async_with_config(
                url,
                Some(websocket_config),
                options.disable_nagle,
            )
            .await;
            let (stream, _response) = fallback.map_err(map_connect_error)?;
            if options.require_client_fingerprint {
                // The unsigned fallback sends no proofs at all, so a locally
                // required certificate binding cannot be honored: fail closed
                // instead of silently proceeding fingerprint-less.
                return Err(SignalFishError::TokenBinding(
                    crate::TokenBindingFailure::MissingClientFingerprint,
                ));
            }
            return Ok((stream, WebSocketTokenBinding::NotNegotiated));
        }
        Err(WebSocketError::Protocol(ProtocolError::SecWebSocketSubProtocolError(
            SubProtocolError::NoSubProtocol,
        ))) => {
            return Err(SignalFishError::TokenBinding(
                crate::TokenBindingFailure::SubprotocolNotNegotiated,
            ))
        }
        Err(WebSocketError::Protocol(ProtocolError::SecWebSocketSubProtocolError(
            SubProtocolError::InvalidSubProtocol
            | SubProtocolError::ServerSentSubProtocolNoneRequested,
        ))) => {
            return Err(SignalFishError::TokenBinding(
                crate::TokenBindingFailure::UnexpectedSubprotocol,
            ))
        }
        Err(error) => return Err(map_connect_error(error)),
    };

    let challenge = tokio::time::timeout(
        options.token_binding_challenge_timeout,
        receive_token_binding_challenge(&mut stream),
    )
    .await
    .map_err(|_| SignalFishError::TokenBinding(crate::TokenBindingFailure::ChallengeTimeout))??;
    #[cfg(feature = "tls")]
    let selected_fingerprint = client_fingerprint.and_then(|tracker| tracker.fingerprint());
    #[cfg(not(feature = "tls"))]
    let selected_fingerprint = None;
    if options.require_client_fingerprint && selected_fingerprint.is_none() {
        return Err(SignalFishError::TokenBinding(
            crate::TokenBindingFailure::MissingClientFingerprint,
        ));
    }
    let session = crate::token_binding::TokenBindingSession::from_challenge(
        handshake_key.as_str(),
        challenge,
        selected_fingerprint,
    )?;
    Ok((stream, WebSocketTokenBinding::Active(Some(session))))
}

#[cfg(feature = "token-binding")]
async fn receive_token_binding_challenge(
    stream: &mut WsStream,
) -> Result<TokenBindingChallenge, SignalFishError> {
    for _ in 0..MAX_SKIPPED_CONTROL_FRAMES_PER_POLL {
        let message = stream
            .next()
            .await
            .ok_or(SignalFishError::TokenBinding(
                crate::TokenBindingFailure::MissingChallenge,
            ))?
            .map_err(|error| SignalFishError::TransportReceive(Box::new(error)))?;
        match message {
            Message::Text(text) => return crate::token_binding::parse_challenge(&text),
            Message::Ping(_) => stream
                .flush()
                .await
                .map_err(|error| SignalFishError::TransportSend(Box::new(error)))?,
            Message::Pong(_) | Message::Frame(_) => {}
            Message::Close(_) => {
                return Err(SignalFishError::TokenBinding(
                    crate::TokenBindingFailure::MissingChallenge,
                ))
            }
            Message::Binary(_) => {
                return Err(SignalFishError::TokenBinding(
                    crate::TokenBindingFailure::MalformedChallenge,
                ))
            }
        }
    }
    Err(SignalFishError::TokenBinding(
        crate::TokenBindingFailure::MalformedChallenge,
    ))
}

/// Options controlling how a [`WebSocketTransport`] connection is established.
///
/// Construct with [`new`](Self::new) (or [`Default`]) and adjust with the
/// `with_*` builders:
///
/// ```rust,no_run
/// # async fn example() -> Result<(), signal_fish_client::SignalFishError> {
/// use signal_fish_client::{WebSocketConnectOptions, WebSocketTransport};
///
/// // Restore the OS default (Nagle enabled) for a throughput-oriented link.
/// let options = WebSocketConnectOptions::new().with_disable_nagle(false);
/// let transport =
///     WebSocketTransport::connect_with_options("ws://localhost:3536/v2/ws", options).await?;
/// # let _ = transport;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebSocketConnectOptions {
    /// Disable Nagle's algorithm (`TCP_NODELAY`) on the underlying TCP socket.
    ///
    /// Defaults to `true`. Small, latency-sensitive game messages are then sent
    /// immediately instead of waiting on TCP's delayed-ACK timer (the classic
    /// Nagle + delayed-ACK stall, worth tens of milliseconds per round trip).
    /// Set to `false` to restore the OS default — Nagle enabled — which favors
    /// throughput for bulk transfers.
    ///
    /// Applied to the raw socket before any TLS handshake, so it covers both
    /// `ws://` and `wss://`.
    pub disable_nagle: bool,
    /// Maximum payload bytes accepted in one inbound WebSocket frame or
    /// assembled message.
    ///
    /// Defaults to 8 MiB, substantially below tungstenite's 64 MiB assembled
    /// message default while retaining headroom for ordinary Server 0.7 room
    /// snapshots. This is a protective client policy, not a protocol maximum:
    /// deployments with larger player metadata, spectator rosters, replay
    /// buffers, or server message limits must raise it. Set it to `None` to
    /// disable both tungstenite receive limits.
    ///
    /// `Some(0)` is invalid and makes connection setup fail before network I/O.
    pub max_inbound_message_size: Option<usize>,
    /// Token-binding-v2 negotiation policy.
    ///
    /// Defaults to [`TokenBindingMode::Disabled`]. Optional or required mode
    /// needs the crate's `token-binding` feature; otherwise connection fails
    /// with a typed [`SignalFishError::TokenBinding`] error.
    pub token_binding: TokenBindingMode,
    /// Maximum wait for the first token-binding challenge after negotiation.
    ///
    /// Defaults to 10 seconds and is ignored when token binding is not selected.
    pub token_binding_challenge_timeout: std::time::Duration,
    /// Require that token-binding proofs bind to a real X.509 client
    /// certificate (opt-in local enforcement of the strictest mTLS profile).
    ///
    /// Defaults to `false`, which mirrors the server-side contract: proofs
    /// simply omit the fingerprint claim when no client certificate is
    /// selected, and enforcement stays with the server's profile. When
    /// `true`, the connect fails with
    /// [`TokenBindingFailure::MissingClientFingerprint`](crate::TokenBindingFailure::MissingClientFingerprint)
    /// — before any network I/O on every path that cannot observe a
    /// certificate selection (every path except
    /// [`WebSocketTransport::connect_with_tls_config`](crate::WebSocketTransport::connect_with_tls_config)
    /// with token binding enabled, including built-in `wss://` connects,
    /// which perform TLS but install no tracking resolver). On the
    /// custom-TLS token-binding path the check runs after the handshake and
    /// challenge, failing when rustls selected no X.509 client signer; on a
    /// plain `ws://` URL the custom TLS configuration is not used at all, so
    /// no certificate can ever be selected and the failure still surfaces
    /// only after the challenge. Optional mode's unsigned fallback
    /// connection produces no proofs at all and fails the same way, so the
    /// policy can never be silently skipped.
    ///
    /// Only [`WebSocketTransport::connect_with_tls_config`] with token
    /// binding enabled can ever satisfy the policy; every other connect path
    /// rejects it up front.
    pub require_client_fingerprint: bool,
}

impl Default for WebSocketConnectOptions {
    fn default() -> Self {
        // NB: a *derived* `Default` would yield `false`; the low-latency default is `true`.
        Self {
            disable_nagle: true,
            max_inbound_message_size: Some(DEFAULT_MAX_INBOUND_MESSAGE_SIZE),
            token_binding: TokenBindingMode::Disabled,
            token_binding_challenge_timeout: std::time::Duration::from_secs(10),
            require_client_fingerprint: false,
        }
    }
}

impl WebSocketConnectOptions {
    /// Create options with the default low-latency settings (Nagle disabled).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set whether Nagle's algorithm is disabled (`TCP_NODELAY`) on connect.
    ///
    /// See [`disable_nagle`](Self::disable_nagle). Defaults to `true`.
    #[must_use]
    pub fn with_disable_nagle(mut self, disable_nagle: bool) -> Self {
        self.disable_nagle = disable_nagle;
        self
    }

    /// Set the maximum payload size for an inbound WebSocket frame or
    /// assembled message.
    ///
    /// The limit is inclusive. Set `None` to disable both receive limits.
    /// `Some(0)` is rejected by the connection methods.
    #[must_use]
    pub fn with_max_inbound_message_size(mut self, max_size: Option<usize>) -> Self {
        self.max_inbound_message_size = max_size;
        self
    }

    /// Set whether token-binding-v2 is disabled, optional, or required.
    #[must_use]
    pub fn with_token_binding(mut self, mode: TokenBindingMode) -> Self {
        self.token_binding = mode;
        self
    }

    /// Set the deadline for receiving the negotiated server challenge.
    ///
    /// A zero duration is not rejected: the challenge window expires
    /// immediately, so a selected connection fails with
    /// `TokenBindingFailure::ChallengeTimeout` before any challenge is read.
    #[must_use]
    pub fn with_token_binding_challenge_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.token_binding_challenge_timeout = timeout;
        self
    }

    /// Set whether token-binding proofs must bind to an X.509 client
    /// certificate.
    ///
    /// See [`require_client_fingerprint`](Self::require_client_fingerprint).
    /// Defaults to `false`.
    #[must_use]
    pub fn with_require_client_fingerprint(mut self, require: bool) -> Self {
        self.require_client_fingerprint = require;
        self
    }
}

/// A [`Transport`] implementation backed by a WebSocket connection.
///
/// Wraps a `tokio-tungstenite` [`WebSocketStream`](tokio_tungstenite::WebSocketStream)
/// and translates between the Signal Fish text-message protocol and WebSocket frames.
///
/// # Construction
///
/// Use [`WebSocketTransport::connect`] to establish a new connection:
///
/// ```rust,no_run
/// # async fn example() -> Result<(), signal_fish_client::SignalFishError> {
/// use signal_fish_client::WebSocketTransport;
///
/// let transport = WebSocketTransport::connect("ws://localhost:3536/v2/ws").await?;
/// # Ok(())
/// # }
/// ```
///
/// For advanced use-cases (custom TLS, proxy, headers) construct the stream
/// yourself and use [`WebSocketTransport::from_stream`]. Because that receives
/// an already-completed handshake, it cannot enable token binding and retains
/// the caller's WebSocket codec limits.
///
/// # Polling Safety
///
/// [`poll_recv`](Transport::poll_recv) preserves the WebSocket stream's partial
/// receive state across `Poll::Pending`, registers the supplied waker, and
/// bounds skipped control-frame work. EOF and terminal socket errors fuse the
/// transport so later receives, sends, and closes have deterministic outcomes.
/// After a terminal sink error, later sends are rejected while `poll_recv` may
/// still return already-buffered frames; the first backend `Pending`, receive
/// failure, EOF, close, or abort then fully fuses the transport. Pre-acceptance
/// token-binding errors and `WriteBufferFull` with exact frame restoration are
/// retryable refusals instead — for direct [`Transport`] operation. The
/// built-in client drivers choose fail-fast semantics and map any send error,
/// including a restored `WriteBufferFull`, to a terminal disconnect that drops
/// the connection and its retained frame.
pub struct WebSocketTransport {
    state: WebSocketState<WsStream>,
}

struct WebSocketState<S> {
    stream: Option<S>,
    closed: bool,
    close_info: Option<TransportCloseInfo>,
    send_started: bool,
    send_failed: bool,
    control_flush_pending: bool,
    peer_close_pending: bool,
    token_binding: WebSocketTokenBinding,
}

enum WebSocketTokenBinding {
    Disabled,
    #[cfg(feature = "token-binding")]
    NotNegotiated,
    #[cfg(feature = "token-binding")]
    Active(Option<crate::token_binding::TokenBindingSession>),
}

impl WebSocketTokenBinding {
    fn status(&self) -> TokenBindingStatus {
        match self {
            Self::Disabled => TokenBindingStatus::Disabled,
            #[cfg(feature = "token-binding")]
            Self::NotNegotiated => TokenBindingStatus::NotNegotiated,
            #[cfg(feature = "token-binding")]
            Self::Active(_) => TokenBindingStatus::Active,
        }
    }

    fn challenge(&self) -> Option<&TokenBindingChallenge> {
        match self {
            #[cfg(feature = "token-binding")]
            Self::Active(Some(session)) => Some(session.challenge()),
            #[cfg(feature = "token-binding")]
            Self::Active(None) => None,
            Self::Disabled => None,
            #[cfg(feature = "token-binding")]
            Self::NotNegotiated => None,
        }
    }

    fn is_active(&self) -> bool {
        matches!(self.status(), TokenBindingStatus::Active)
    }

    fn prepare(&self, frame: &TransportFrame) -> Result<TransportFrame, SignalFishError> {
        match self {
            #[cfg(feature = "token-binding")]
            Self::Active(Some(session)) => session.prepare(frame),
            #[cfg(feature = "token-binding")]
            Self::Active(None) => Err(SignalFishError::TransportClosed),
            Self::Disabled => Ok(frame.clone()),
            #[cfg(feature = "token-binding")]
            Self::NotNegotiated => Ok(frame.clone()),
        }
    }

    fn commit(&mut self) -> Result<(), SignalFishError> {
        match self {
            #[cfg(feature = "token-binding")]
            Self::Active(Some(session)) => session.commit(),
            #[cfg(feature = "token-binding")]
            Self::Active(None) => Err(SignalFishError::TransportClosed),
            Self::Disabled => Ok(()),
            #[cfg(feature = "token-binding")]
            Self::NotNegotiated => Ok(()),
        }
    }
}

impl std::fmt::Debug for WebSocketTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The stream codec can retain raw inbound/outbound protocol frames,
        // and close reasons are peer-controlled. Expose state only.
        f.debug_struct("WebSocketTransport")
            .field("has_stream", &self.state.stream.is_some())
            .field("closed", &self.state.closed)
            .field("has_close_info", &self.state.close_info.is_some())
            .field("send_started", &self.state.send_started)
            .field("send_failed", &self.state.send_failed)
            .field("control_flush_pending", &self.state.control_flush_pending)
            .field("peer_close_pending", &self.state.peer_close_pending)
            .field("token_binding", &self.state.token_binding.status())
            .finish()
    }
}

impl<S> WebSocketState<S> {
    fn new(stream: S) -> Self {
        Self::new_with_token_binding(stream, WebSocketTokenBinding::Disabled)
    }

    fn new_with_token_binding(stream: S, token_binding: WebSocketTokenBinding) -> Self {
        Self {
            stream: Some(stream),
            closed: false,
            close_info: None,
            send_started: false,
            send_failed: false,
            control_flush_pending: false,
            peer_close_pending: false,
            token_binding,
        }
    }

    fn mark_terminal(&mut self) {
        self.stream = None;
        self.closed = true;
        self.send_started = false;
        self.send_failed = true;
        self.control_flush_pending = false;
        self.peer_close_pending = false;
        #[cfg(feature = "token-binding")]
        if let WebSocketTokenBinding::Active(session) = &mut self.token_binding {
            // Drop and zeroize the derived key/challenge as soon as the
            // physical connection becomes terminal while preserving status.
            *session = None;
        }
    }

    fn mark_send_failed(&mut self) {
        self.send_started = false;
        self.send_failed = true;
        // A failed sink cannot safely flush auto-generated control output.
        // Keep only the read state needed to surface already-ready frames.
        self.control_flush_pending = false;
        #[cfg(feature = "token-binding")]
        if let WebSocketTokenBinding::Active(session) = &mut self.token_binding {
            *session = None;
        }
    }

    fn close_info(&self) -> Option<TransportCloseInfo> {
        self.close_info.clone()
    }

    fn abort(&mut self) {
        self.mark_terminal();
    }
}

impl WebSocketTransport {
    /// Establish a new WebSocket connection to the given URL.
    ///
    /// `ws://` is always supported. `wss://` requires the optional `tls` feature;
    /// without it a `wss://` URL fails with [`SignalFishError::InvalidConfig`].
    ///
    /// Nagle's algorithm is **disabled by default** (`TCP_NODELAY`) so small,
    /// latency-sensitive game messages are sent without delay. Inbound frames
    /// and assembled messages are limited to 8 MiB by default. Use
    /// [`connect_with_options`](Self::connect_with_options) to override either
    /// policy.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::InvalidConfig`] if the URL is invalid and
    /// [`SignalFishError::Io`] if the connection cannot be established. When
    /// the underlying error is an I/O error its
    /// [`ErrorKind`](std::io::ErrorKind) is preserved; all other errors are
    /// mapped to [`ErrorKind::Other`](std::io::ErrorKind::Other).
    pub async fn connect(url: &str) -> Result<Self, SignalFishError> {
        Self::connect_with_options(url, WebSocketConnectOptions::default()).await
    }

    /// Establish a new WebSocket connection using explicit
    /// [`WebSocketConnectOptions`].
    ///
    /// Behaves like [`connect`](Self::connect) but lets the caller control
    /// socket tuning, inbound frame/message size limits, and token-binding
    /// negotiation policy. Socket options are applied before any TLS handshake,
    /// so they cover both `ws://` and `wss://`. A selected token-binding
    /// challenge is consumed before this method returns.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::InvalidConfig`] when the URL cannot be
    /// parsed into a WebSocket request or a connect option is invalid on its
    /// face; neither attempts network I/O. Returns [`SignalFishError::Io`]
    /// when the connection cannot be established: when the underlying error
    /// is an I/O error its [`ErrorKind`](std::io::ErrorKind) is preserved, and
    /// all other connection errors map to
    /// [`ErrorKind::Other`](std::io::ErrorKind::Other) — including HTTP
    /// upgrade rejections, which carry the underlying status in every
    /// token-binding mode. Optional or required mode returns
    /// [`SignalFishError::TokenBinding`] when the feature
    /// is disabled or subprotocol negotiation, challenge validation, or key
    /// derivation fails. A zero inbound size limit returns
    /// [`SignalFishError::InvalidConfig`] before URL parsing or network I/O.
    /// When token binding is requested without the
    /// `token-binding` feature, its feature-disabled error takes precedence.
    /// The `require_client_fingerprint` policy always fails before network
    /// I/O here with [`SignalFishError::TokenBinding`], because this path can
    /// never observe a certificate selection.
    pub async fn connect_with_options(
        url: &str,
        options: WebSocketConnectOptions,
    ) -> Result<Self, SignalFishError> {
        #[cfg(not(feature = "token-binding"))]
        if options.token_binding != TokenBindingMode::Disabled {
            return Err(SignalFishError::TokenBinding(
                crate::TokenBindingFailure::FeatureDisabled,
            ));
        }
        reject_unsatisfiable_client_fingerprint_requirement(&options)?;
        let websocket_config = websocket_config(options)?;
        validate_request_url(url)?;

        #[cfg(feature = "tls")]
        install_tls_provider();

        tracing::debug!(
            secure = url.starts_with("wss://"),
            disable_nagle = options.disable_nagle,
            max_inbound_message_size = options.max_inbound_message_size,
            token_binding = ?options.token_binding,
            "connecting to WebSocket server"
        );

        #[cfg(feature = "token-binding")]
        let (stream, token_binding) = if options.token_binding == TokenBindingMode::Disabled {
            let (stream, _response) = tokio_tungstenite::connect_async_with_config(
                url,
                Some(websocket_config),
                options.disable_nagle,
            )
            .await
            .map_err(map_connect_error)?;
            (stream, WebSocketTokenBinding::Disabled)
        } else {
            #[cfg(feature = "tls")]
            let connected =
                connect_with_token_binding(url, options, websocket_config, None, None).await?;
            #[cfg(not(feature = "tls"))]
            let connected = connect_with_token_binding(url, options, websocket_config).await?;
            connected
        };

        #[cfg(not(feature = "token-binding"))]
        let (stream, token_binding) = {
            let (stream, _response) = tokio_tungstenite::connect_async_with_config(
                url,
                Some(websocket_config),
                options.disable_nagle,
            )
            .await
            .map_err(map_connect_error)?;
            (stream, WebSocketTokenBinding::Disabled)
        };

        tracing::info!(
            secure = url.starts_with("wss://"),
            "WebSocket connection established"
        );

        Ok(Self {
            state: WebSocketState::new_with_token_binding(stream, token_binding),
        })
    }

    /// Establish a native WebSocket with an explicit rustls client configuration.
    ///
    /// This is the token-binding-capable path for private roots, custom trust
    /// stores, and mTLS. The configuration is used only during connection setup
    /// and is not retained or formatted by the transport. When token binding is
    /// offered, the transport wraps the resolver and disables TLS resumption on
    /// its cloned configuration, including an unsigned fallback after optional
    /// negotiation. If rustls selects an X.509 client signer, active proofs bind
    /// to that exact leaf. The caller's configuration and cache are not mutated.
    ///
    /// # Errors
    ///
    /// Returns the same connection and token-binding errors as
    /// [`connect_with_options`](Self::connect_with_options), including
    /// [`SignalFishError::InvalidConfig`] for a zero inbound size limit.
    /// The `require_client_fingerprint` policy fails
    /// before network I/O when token binding is disabled (no proofs exist to
    /// bind), after the handshake and challenge when rustls selected no
    /// X.509 client signer, and after the unsigned fallback handshake (no
    /// challenge is consumed) when Optional mode fell back to an unsigned
    /// connection. On a plain `ws://` URL the custom TLS configuration is
    /// not used at all — tokio-tungstenite bypasses the connector, so no TLS
    /// handshake or certificate selection runs and a warning is logged (the
    /// plain-URL predicate matches tungstenite's lowercase-only `ws`/`wss`
    /// scheme handling); with
    /// token binding enabled the fingerprint policy still fails only after
    /// the completed challenge.
    #[cfg(feature = "tls")]
    pub async fn connect_with_tls_config(
        url: &str,
        options: WebSocketConnectOptions,
        tls_config: std::sync::Arc<rustls::ClientConfig>,
    ) -> Result<Self, SignalFishError> {
        #[cfg(not(feature = "token-binding"))]
        if options.token_binding != TokenBindingMode::Disabled {
            return Err(SignalFishError::TokenBinding(
                crate::TokenBindingFailure::FeatureDisabled,
            ));
        }
        #[cfg(not(feature = "token-binding"))]
        reject_unsatisfiable_client_fingerprint_requirement(&options)?;
        #[cfg(feature = "token-binding")]
        if options.token_binding == TokenBindingMode::Disabled {
            // A disabled connection produces no proofs, so a fingerprint
            // requirement can never be observed or enforced.
            reject_unsatisfiable_client_fingerprint_requirement(&options)?;
        }
        let websocket_config = websocket_config(options)?;
        validate_request_url(url)?;

        install_tls_provider();
        let connector = tokio_tungstenite::Connector::Rustls(tls_config.clone());
        let secure = url.starts_with("wss://");
        if !secure {
            // tokio-tungstenite bypasses the connector for plain URLs, so the
            // entire custom configuration — including mTLS client identity —
            // is silently inert. Never inline the URL here.
            tracing::warn!(
                custom_tls = true,
                secure,
                "custom TLS configuration is ignored: no TLS handshake or certificate \
                 selection runs on a plain URL"
            );
        }
        tracing::debug!(
            secure,
            disable_nagle = options.disable_nagle,
            max_inbound_message_size = options.max_inbound_message_size,
            token_binding = ?options.token_binding,
            custom_tls = true,
            "connecting to WebSocket server"
        );

        #[cfg(feature = "token-binding")]
        let (stream, token_binding) = if options.token_binding == TokenBindingMode::Disabled {
            let (stream, _response) = tokio_tungstenite::connect_async_tls_with_config(
                url,
                Some(websocket_config),
                options.disable_nagle,
                Some(connector),
            )
            .await
            .map_err(map_connect_error)?;
            (stream, WebSocketTokenBinding::Disabled)
        } else {
            let (tls_config, fingerprint_tracker) =
                ClientCertificateFingerprintTracker::wrap(tls_config);
            let connector = tokio_tungstenite::Connector::Rustls(tls_config);
            connect_with_token_binding(
                url,
                options,
                websocket_config,
                Some(connector),
                fingerprint_tracker,
            )
            .await?
        };

        #[cfg(not(feature = "token-binding"))]
        let (stream, token_binding) = {
            let (stream, _response) = tokio_tungstenite::connect_async_tls_with_config(
                url,
                Some(websocket_config),
                options.disable_nagle,
                Some(connector),
            )
            .await
            .map_err(map_connect_error)?;
            (stream, WebSocketTokenBinding::Disabled)
        };

        tracing::info!(
            secure = url.starts_with("wss://"),
            custom_tls = true,
            "WebSocket connection established"
        );
        Ok(Self {
            state: WebSocketState::new_with_token_binding(stream, token_binding),
        })
    }

    /// Create a [`WebSocketTransport`] from an already-established WebSocket stream.
    ///
    /// This is useful when you need custom TLS configuration, proxy headers, or
    /// any other connection setup that [`connect`](Self::connect) does not expose.
    ///
    /// Unlike [`connect`](Self::connect), this does **not** touch socket options:
    /// the caller owns the stream and is responsible for `TCP_NODELAY` (Nagle) or
    /// any other tuning on the underlying socket before wrapping it here.
    /// The caller likewise owns the stream's WebSocket codec configuration,
    /// including frame and assembled-message size limits; this constructor
    /// does not replace them.
    /// It also cannot enable token binding: an established stream no longer
    /// exposes the exact generated `Sec-WebSocket-Key`, so callers needing that
    /// extension must own the full handshake/proof wrapper or use a connect API.
    pub fn from_stream(stream: WsStream) -> Self {
        Self {
            state: WebSocketState::new(stream),
        }
    }

    /// Return the negotiated token-binding state for this physical connection.
    #[must_use]
    pub fn token_binding_status(&self) -> TokenBindingStatus {
        self.state.token_binding.status()
    }

    /// Return the validated server challenge when token binding is active.
    ///
    /// The transport consumes this challenge internally before it can sign any
    /// application frame. Callers normally need only [`token_binding_status`](Self::token_binding_status).
    #[must_use]
    pub fn token_binding_challenge(&self) -> Option<&TokenBindingChallenge> {
        self.state.token_binding.challenge()
    }

    /// Establish a new WebSocket connection with a timeout.
    ///
    /// Behaves identically to [`connect`](Self::connect) but fails with
    /// [`SignalFishError::Timeout`] if the connection is not established within
    /// the given duration.
    ///
    /// To pair a timeout with custom [`WebSocketConnectOptions`], wrap
    /// [`connect_with_options`](Self::connect_with_options), e.g.
    /// `tokio::time::timeout(dur, WebSocketTransport::connect_with_options(url, opts))`.
    ///
    /// # Errors
    ///
    /// Returns [`SignalFishError::Timeout`] if the deadline elapses, or any
    /// error that [`connect`](Self::connect) may return.
    pub async fn connect_with_timeout(
        url: &str,
        timeout: std::time::Duration,
    ) -> Result<Self, SignalFishError> {
        tokio::time::timeout(timeout, Self::connect(url))
            .await
            .map_err(|_| SignalFishError::Timeout)?
    }
}

impl<S> WebSocketState<S>
where
    S: Sink<Message, Error = WebSocketError>
        + Stream<Item = Result<Message, WebSocketError>>
        + Unpin,
{
    fn poll_send(
        &mut self,
        cx: &mut Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> Poll<Result<(), SignalFishError>> {
        if self.closed || self.send_failed || self.peer_close_pending {
            return Poll::Ready(Err(SignalFishError::TransportClosed));
        }
        if self.stream.is_none() {
            self.mark_terminal();
            return Poll::Ready(Err(SignalFishError::TransportClosed));
        }
        if !self.send_started && frame.is_none() {
            return Poll::Ready(Ok(()));
        }
        if !self.send_started {
            let ready = {
                let Some(stream) = self.stream.as_mut() else {
                    self.mark_terminal();
                    return Poll::Ready(Err(SignalFishError::TransportClosed));
                };
                Pin::new(stream).poll_ready(cx)
            };
            match ready {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => {
                    self.mark_send_failed();
                    return Poll::Ready(Err(SignalFishError::TransportSend(Box::new(error))));
                }
                Poll::Ready(Ok(())) => {}
            }
            let token_binding_active = self.token_binding.is_active();
            let Some(original_frame) = frame.as_ref() else {
                return Poll::Ready(Ok(()));
            };
            let accepted_frame = if token_binding_active {
                match self.token_binding.prepare(original_frame) {
                    Ok(prepared) => prepared,
                    Err(error) => return Poll::Ready(Err(error)),
                }
            } else {
                let Some(frame) = frame.take() else {
                    return Poll::Ready(Ok(()));
                };
                frame
            };
            let message = match accepted_frame {
                TransportFrame::Text(text) => Message::Text(text.into()),
                TransportFrame::Binary(bytes) => Message::Binary(bytes.into()),
            };
            let send_result = {
                let Some(stream) = self.stream.as_mut() else {
                    self.mark_terminal();
                    return Poll::Ready(Err(SignalFishError::TransportClosed));
                };
                Pin::new(stream).start_send(message)
            };
            if let Err(error) = send_result {
                // Classify by reference so the original backend error stays
                // intact: `TransportSend` boxes it as the `#[source]` cause.
                let retryable = if let WebSocketError::WriteBufferFull(message) = &error {
                    let restored = if token_binding_active {
                        // The exact original remains in the caller slot. Never
                        // restore the protected envelope or a retry would wrap it twice.
                        true
                    } else {
                        match message.as_ref() {
                            Message::Text(text) => {
                                *frame = Some(TransportFrame::Text(text.to_string()));
                                true
                            }
                            Message::Binary(bytes) => {
                                *frame = Some(TransportFrame::Binary(bytes.to_vec()));
                                true
                            }
                            Message::Frame(frame_data) => {
                                use tokio_tungstenite::tungstenite::protocol::frame::coding::{
                                    Data, OpCode,
                                };

                                match frame_data.header().opcode {
                                    OpCode::Data(Data::Text) => {
                                        match frame_data.clone().into_text() {
                                            Ok(text) => {
                                                *frame =
                                                    Some(TransportFrame::Text(text.to_string()));
                                                true
                                            }
                                            Err(_) => false,
                                        }
                                    }
                                    OpCode::Data(Data::Binary) => {
                                        *frame = Some(TransportFrame::Binary(
                                            frame_data.payload().to_vec(),
                                        ));
                                        true
                                    }
                                    _ => false,
                                }
                            }
                            _ => false,
                        }
                    };
                    restored
                } else {
                    false
                };
                if !retryable {
                    self.mark_send_failed();
                }
                return Poll::Ready(Err(SignalFishError::TransportSend(Box::new(error))));
            }
            if token_binding_active {
                let _accepted_original = frame.take();
                if let Err(error) = self.token_binding.commit() {
                    self.mark_send_failed();
                    return Poll::Ready(Err(error));
                }
            }
            self.send_started = true;
        }
        let flush = {
            let Some(stream) = self.stream.as_mut() else {
                self.mark_terminal();
                return Poll::Ready(Err(SignalFishError::TransportClosed));
            };
            Pin::new(stream).poll_flush(cx)
        };
        match flush {
            Poll::Ready(Ok(())) => {
                self.send_started = false;
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => {
                self.mark_send_failed();
                Poll::Ready(Err(SignalFishError::TransportSend(Box::new(error))))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<TransportFrame, SignalFishError>>> {
        if self.closed {
            return Poll::Ready(None);
        }
        if self.stream.is_none() {
            self.mark_terminal();
            return Poll::Ready(None);
        }

        let mut skipped_control_frames = 0;
        loop {
            if self.control_flush_pending {
                let flush = {
                    let Some(stream) = self.stream.as_mut() else {
                        self.mark_terminal();
                        return Poll::Ready(None);
                    };
                    Pin::new(stream).poll_flush(cx)
                };
                match flush {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Err(error)) => {
                        self.mark_terminal();
                        return Poll::Ready(Some(Err(SignalFishError::TransportReceive(
                            Box::new(error),
                        ))));
                    }
                    Poll::Ready(Ok(())) => {
                        self.control_flush_pending = false;
                        if self.peer_close_pending {
                            self.mark_terminal();
                            return Poll::Ready(None);
                        }
                    }
                }
            }

            if skipped_control_frames == MAX_SKIPPED_CONTROL_FRAMES_PER_POLL {
                if self.send_failed {
                    self.mark_terminal();
                    return Poll::Ready(None);
                }
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }

            let next = {
                let Some(stream) = self.stream.as_mut() else {
                    self.mark_terminal();
                    return Poll::Ready(None);
                };
                Pin::new(stream).poll_next(cx)
            };
            let msg = match next {
                Poll::Pending => {
                    if self.send_failed {
                        self.mark_terminal();
                        return Poll::Ready(None);
                    }
                    return Poll::Pending;
                }
                Poll::Ready(value) => match value {
                    Some(Ok(msg)) => msg,
                    Some(Err(e)) => {
                        self.mark_terminal();
                        return Poll::Ready(Some(Err(SignalFishError::TransportReceive(
                            Box::new(e),
                        ))));
                    }
                    None => {
                        self.mark_terminal();
                        return Poll::Ready(None);
                    }
                },
            };

            match msg {
                // `Utf8Bytes::to_string()` copies the payload into a new `String`
                // because `Utf8Bytes` does not expose the inner buffer by value.
                Message::Text(text) => {
                    return Poll::Ready(Some(Ok(TransportFrame::Text(text.to_string()))))
                }
                Message::Binary(bytes) => {
                    return Poll::Ready(Some(Ok(TransportFrame::Binary(bytes.to_vec()))))
                }
                Message::Close(frame) => {
                    tracing::debug!(
                        code = frame.as_ref().map(|frame| u16::from(frame.code)),
                        has_reason = frame.as_ref().is_some_and(|frame| !frame.reason.is_empty()),
                        "received WebSocket close frame"
                    );
                    // Remember structured close metadata so the client can
                    // attribute the disconnect via `close_info()`.
                    if let Some(frame) = frame {
                        self.close_info = Some(TransportCloseInfo {
                            code: Some(frame.code.into()),
                            reason: (!frame.reason.is_empty()).then(|| frame.reason.to_string()),
                            clean: None,
                            initiated_by_peer: true,
                        });
                    } else {
                        self.close_info = Some(TransportCloseInfo {
                            initiated_by_peer: true,
                            ..TransportCloseInfo::default()
                        });
                    }
                    // Tungstenite has queued the mandatory close response. Drive
                    // its flush before reporting the terminal receive state so a
                    // polling client cannot strand the handshake after seeing
                    // `None` and ceasing to poll the transport.
                    if self.send_failed {
                        self.mark_terminal();
                        return Poll::Ready(None);
                    }
                    self.peer_close_pending = true;
                    self.control_flush_pending = true;
                }
                Message::Ping(_) => {
                    tracing::trace!("received WebSocket ping (auto-pong handled by tungstenite)");
                    if !self.send_failed {
                        self.control_flush_pending = true;
                    }
                    skipped_control_frames = skipped_control_frames.saturating_add(1);
                }
                Message::Pong(_) => {
                    tracing::trace!("received WebSocket pong (ignored)");
                    skipped_control_frames = skipped_control_frames.saturating_add(1);
                }
                Message::Frame(_) => {
                    // This variant is never produced by the read half of the stream;
                    // it exists only for exhaustiveness against future `Message`
                    // variants. We keep the arm to satisfy exhaustiveness checks.
                    tracing::trace!("received raw WebSocket frame, skipping");
                    skipped_control_frames = skipped_control_frames.saturating_add(1);
                }
            }
        }
    }

    fn poll_close(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), SignalFishError>> {
        if self.closed {
            return Poll::Ready(Ok(()));
        }
        if self.stream.is_none() {
            self.mark_terminal();
            return Poll::Ready(Ok(()));
        }
        if self.send_failed {
            self.mark_terminal();
            return Poll::Ready(Ok(()));
        }
        if self.peer_close_pending {
            let flush = {
                let Some(stream) = self.stream.as_mut() else {
                    self.mark_terminal();
                    return Poll::Ready(Ok(()));
                };
                Pin::new(stream).poll_flush(cx)
            };
            return match flush {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Ok(())) => {
                    self.mark_terminal();
                    Poll::Ready(Ok(()))
                }
                Poll::Ready(Err(error)) => {
                    self.mark_terminal();
                    Poll::Ready(Err(SignalFishError::TransportSend(Box::new(error))))
                }
            };
        }
        let close = {
            let Some(stream) = self.stream.as_mut() else {
                self.mark_terminal();
                return Poll::Ready(Ok(()));
            };
            Pin::new(stream).poll_close(cx)
        };
        match close {
            Poll::Ready(Ok(())) => {
                self.mark_terminal();
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => {
                self.mark_terminal();
                Poll::Ready(Err(SignalFishError::TransportSend(Box::new(error))))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Transport for WebSocketTransport {
    fn poll_send(
        &mut self,
        cx: &mut Context<'_>,
        frame: &mut Option<TransportFrame>,
    ) -> Poll<Result<(), SignalFishError>> {
        self.state.poll_send(cx, frame)
    }

    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<TransportFrame, SignalFishError>>> {
        self.state.poll_recv(cx)
    }

    fn poll_close(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), SignalFishError>> {
        self.state.poll_close(cx)
    }

    fn close_info(&self) -> Option<TransportCloseInfo> {
        self.state.close_info()
    }

    fn abort(&mut self) {
        self.state.abort();
    }
}

#[cfg(test)]
#[cfg(feature = "transport-websocket")]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing,
    clippy::result_large_err
)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    #[cfg(all(feature = "tls", feature = "token-binding"))]
    use rustls::client::ResolvesClientCert as _;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::task::{Wake, Waker};
    use tokio::io::AsyncWriteExt;

    #[test]
    fn websocket_transport_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<WebSocketTransport>();
    }

    #[test]
    fn websocket_transport_is_debug() {
        fn assert_debug<T: std::fmt::Debug>() {}
        assert_debug::<WebSocketTransport>();
    }

    #[test]
    fn connect_options_apply_inbound_frame_and_message_limits_together() {
        let default_config = websocket_config(WebSocketConnectOptions::default())
            .expect("default inbound limit must be valid");
        assert_eq!(
            default_config.max_frame_size,
            Some(DEFAULT_MAX_INBOUND_MESSAGE_SIZE)
        );
        assert_eq!(
            default_config.max_message_size,
            Some(DEFAULT_MAX_INBOUND_MESSAGE_SIZE)
        );

        for (limit, expected) in [(Some(1_024), Some(1_024)), (None, None)] {
            let config = websocket_config(
                WebSocketConnectOptions::new().with_max_inbound_message_size(limit),
            )
            .expect("positive or disabled inbound limit must be valid");
            assert_eq!(config.max_frame_size, expected);
            assert_eq!(config.max_message_size, expected);
        }
    }

    #[tokio::test]
    async fn zero_inbound_message_limit_is_rejected_before_url_parsing() {
        let error = WebSocketTransport::connect_with_options(
            "not-a-valid-url",
            WebSocketConnectOptions::new().with_max_inbound_message_size(Some(0)),
        )
        .await
        .expect_err("zero must not create a degenerate WebSocket codec");
        assert_eq!(
            error.to_string(),
            "invalid configuration: max_inbound_message_size: must be greater than zero or None"
        );
    }

    #[cfg(feature = "token-binding")]
    #[tokio::test]
    async fn zero_inbound_limit_precedes_token_binding_network_work_when_supported() {
        let error = WebSocketTransport::connect_with_options(
            "not-a-valid-url",
            WebSocketConnectOptions::new()
                .with_token_binding(TokenBindingMode::Required)
                .with_max_inbound_message_size(Some(0)),
        )
        .await
        .expect_err("zero must be rejected before token-binding handshake work");
        assert!(matches!(
            error,
            SignalFishError::InvalidConfig {
                field: "max_inbound_message_size",
                ..
            }
        ));
    }

    #[cfg(all(feature = "tls", feature = "token-binding"))]
    #[test]
    fn client_certificate_fingerprint_is_lowercase_sha256_and_redacted() {
        let fingerprint = client_certificate_fingerprint(b"abc");
        assert_eq!(
            fingerprint,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let tracker = ClientCertificateFingerprintTracker {
            selected: Arc::new(std::sync::Mutex::new(Some(fingerprint.clone()))),
        };
        assert!(!format!("{tracker:?}").contains(&fingerprint));
    }

    #[cfg(all(feature = "tls", feature = "token-binding"))]
    #[test]
    fn custom_tls_tracking_clones_even_when_resolver_reports_no_certificates() {
        install_tls_provider();
        let config = Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(rustls::RootCertStore::empty())
                .with_no_client_auth(),
        );
        let (wrapped, tracker) = ClientCertificateFingerprintTracker::wrap(config.clone());
        assert!(!Arc::ptr_eq(&wrapped, &config));
        assert!(!wrapped.client_auth_cert_resolver.has_certs());
        assert!(!config.client_auth_cert_resolver.has_certs());
        assert_eq!(
            tracker.expect("tracking must be installed").fingerprint(),
            None
        );
    }

    #[cfg(all(feature = "tls", feature = "token-binding"))]
    #[derive(Debug)]
    struct TestSigningKey {
        compatible: bool,
    }

    #[cfg(all(feature = "tls", feature = "token-binding"))]
    impl rustls::sign::SigningKey for TestSigningKey {
        fn choose_scheme(
            &self,
            offered: &[rustls::SignatureScheme],
        ) -> Option<Box<dyn rustls::sign::Signer>> {
            (self.compatible && offered.contains(&rustls::SignatureScheme::ECDSA_NISTP256_SHA256))
                .then(|| Box::new(TestSigner) as Box<dyn rustls::sign::Signer>)
        }

        fn algorithm(&self) -> rustls::SignatureAlgorithm {
            rustls::SignatureAlgorithm::ECDSA
        }
    }

    #[cfg(all(feature = "tls", feature = "token-binding"))]
    #[derive(Debug)]
    struct TestSigner;

    #[cfg(all(feature = "tls", feature = "token-binding"))]
    impl rustls::sign::Signer for TestSigner {
        fn sign(&self, _message: &[u8]) -> Result<Vec<u8>, rustls::Error> {
            Ok(vec![1])
        }

        fn scheme(&self) -> rustls::SignatureScheme {
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256
        }
    }

    #[cfg(all(feature = "tls", feature = "token-binding"))]
    #[derive(Debug)]
    struct TestClientCertificateResolver {
        certified_key: Arc<rustls::sign::CertifiedKey>,
        raw_public_key: bool,
        reports_certificates: bool,
    }

    #[cfg(all(feature = "tls", feature = "token-binding"))]
    impl rustls::client::ResolvesClientCert for TestClientCertificateResolver {
        fn resolve(
            &self,
            _root_hint_subjects: &[&[u8]],
            _sigschemes: &[rustls::SignatureScheme],
        ) -> Option<Arc<rustls::sign::CertifiedKey>> {
            Some(self.certified_key.clone())
        }

        fn only_raw_public_keys(&self) -> bool {
            self.raw_public_key
        }

        fn has_certs(&self) -> bool {
            self.reports_certificates
        }
    }

    #[cfg(all(feature = "tls", feature = "token-binding"))]
    fn test_client_certificate_resolver(
        compatible: bool,
        raw_public_key: bool,
        reports_certificates: bool,
    ) -> Arc<dyn rustls::client::ResolvesClientCert> {
        let certified_key = rustls::sign::CertifiedKey::new(
            vec![rustls::pki_types::CertificateDer::from(vec![1, 2, 3])],
            Arc::new(TestSigningKey { compatible }),
        );
        Arc::new(TestClientCertificateResolver {
            certified_key: Arc::new(certified_key),
            raw_public_key,
            reports_certificates,
        })
    }

    #[cfg(all(feature = "tls", feature = "token-binding"))]
    #[test]
    fn fingerprint_is_recorded_only_when_rustls_selects_an_x509_signer() {
        let offered = [rustls::SignatureScheme::ECDSA_NISTP256_SHA256];
        for (compatible, raw_public_key, expected) in [
            (
                true,
                false,
                Some(client_certificate_fingerprint(&[1, 2, 3])),
            ),
            (false, false, None),
            (true, true, None),
        ] {
            let tracker = ClientCertificateFingerprintTracker {
                selected: Arc::new(std::sync::Mutex::new(None)),
            };
            let resolver = TrackingClientCertificateResolver {
                delegate: test_client_certificate_resolver(compatible, raw_public_key, false),
                tracker: tracker.clone(),
            };
            let selected = resolver
                .resolve(&[], &offered)
                .expect("dynamic test resolver returns a key");
            assert_eq!(tracker.fingerprint(), None);
            assert_eq!(selected.key.choose_scheme(&offered).is_some(), compatible);
            assert_eq!(tracker.fingerprint(), expected);
        }
    }

    #[tokio::test]
    async fn connect_fails_with_invalid_url() {
        let result = WebSocketTransport::connect("not-a-valid-url").await;
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            SignalFishError::InvalidConfig { field: "url", .. }
        ));
        assert!(
            !err.to_string().contains("not-a-valid-url"),
            "URL parse failures must not echo the caller's URL: {err}"
        );
    }

    #[tokio::test]
    async fn unparsable_url_is_invalid_config_from_the_pre_connect_check() {
        // A space makes `Uri::from_str` fail, which tungstenite classifies as
        // `HttpFormat` — post-connect that would be a server fault mapped to
        // `Io`, so this pin holds only while the pre-connect request
        // construction classifies it as caller configuration.
        let error = WebSocketTransport::connect("ws://exa mple.invalid")
            .await
            .expect_err("a URL that cannot be parsed must fail before I/O");
        assert!(matches!(
            error,
            SignalFishError::InvalidConfig { field: "url", .. }
        ));
        assert!(
            !error.to_string().contains("exa mple.invalid"),
            "URL parse failures must not echo the caller's URL: {error}"
        );
    }

    #[cfg(not(feature = "tls"))]
    #[tokio::test]
    async fn wss_url_without_the_tls_feature_is_invalid_config_before_io() {
        let error = WebSocketTransport::connect("wss://example.invalid")
            .await
            .expect_err("wss:// without the tls feature must fail before I/O");
        assert!(matches!(
            error,
            SignalFishError::InvalidConfig { field: "url", .. }
        ));
    }

    #[tokio::test]
    async fn non_ws_schemes_are_invalid_config_regardless_of_reachability() {
        // tungstenite's own scheme check runs after the TCP connect, so an
        // uppercase or foreign scheme with an explicit port used to dial
        // first: `InvalidConfig` on a reachable port but `Io` on a closed
        // one. The pre-connect check must reject every such URL before I/O,
        // making the classification value-determined — an unreachable port
        // is what makes this pin red against the old behavior.
        for url in [
            "http://127.0.0.1:1",
            "ftp://127.0.0.1:1",
            "WS://127.0.0.1:1",
            "WSS://127.0.0.1:1",
        ] {
            let error = WebSocketTransport::connect(url)
                .await
                .expect_err("only exact lowercase ws:// and wss:// are valid");
            assert!(
                matches!(error, SignalFishError::InvalidConfig { field: "url", .. }),
                "{url} must be InvalidConfig, got: {error}"
            );
            assert!(
                !error.to_string().contains("127.0.0.1"),
                "scheme rejections must not echo the caller's URL: {error}"
            );
        }
    }

    #[tokio::test]
    async fn foreign_scheme_with_a_reachable_port_is_rejected_before_io() {
        // Companion to `non_ws_schemes_are_invalid_config_regardless_of_reachability`:
        // a listening port must not change the classification — and no dial
        // may happen, which the empty listener's accept queue proves.
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("dynamic test listener binds");
        let port = listener.local_addr().expect("bound address").port();
        let error = WebSocketTransport::connect(&format!("http://127.0.0.1:{port}"))
            .await
            .expect_err("a foreign scheme is caller configuration, not a network outcome");
        assert!(matches!(
            error,
            SignalFishError::InvalidConfig { field: "url", .. }
        ));
        listener
            .set_nonblocking(true)
            .expect("listener must switch to non-blocking");
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "the rejected connect must not have dialed the listener"
        );
    }

    #[tokio::test]
    async fn connect_fails_with_unreachable_host() {
        let result = WebSocketTransport::connect("ws://127.0.0.1:1").await;
        let err = result.unwrap_err();
        assert!(matches!(err, SignalFishError::Io(_)));
    }

    #[cfg(not(feature = "token-binding"))]
    #[tokio::test]
    async fn non_disabled_mode_requires_the_token_binding_feature() {
        let result = WebSocketTransport::connect_with_options(
            "ws://127.0.0.1:1",
            WebSocketConnectOptions::new().with_token_binding(TokenBindingMode::Required),
        )
        .await;
        assert!(matches!(
            result,
            Err(SignalFishError::TokenBinding(
                crate::TokenBindingFailure::FeatureDisabled
            ))
        ));
    }

    #[cfg(not(feature = "token-binding"))]
    #[tokio::test]
    async fn unavailable_token_binding_precedes_an_invalid_inbound_limit() {
        let result = WebSocketTransport::connect_with_options(
            "not-a-valid-url",
            WebSocketConnectOptions::new()
                .with_token_binding(TokenBindingMode::Required)
                .with_max_inbound_message_size(Some(0)),
        )
        .await;
        assert!(matches!(
            result,
            Err(SignalFishError::TokenBinding(
                crate::TokenBindingFailure::FeatureDisabled
            ))
        ));
    }

    #[cfg(not(feature = "token-binding"))]
    #[tokio::test]
    async fn unavailable_token_binding_precedes_a_required_client_fingerprint() {
        // `not-a-valid-url` proves the precedence without any network I/O:
        // the feature-disabled fence must win over the fingerprint policy so
        // callers see the actual capability gap first.
        let result = WebSocketTransport::connect_with_options(
            "not-a-valid-url",
            WebSocketConnectOptions::new()
                .with_token_binding(TokenBindingMode::Required)
                .with_require_client_fingerprint(true),
        )
        .await;
        assert!(matches!(
            result,
            Err(SignalFishError::TokenBinding(
                crate::TokenBindingFailure::FeatureDisabled
            ))
        ));
    }

    #[tokio::test]
    async fn required_client_fingerprint_fails_before_io_on_plain_connect_paths() {
        // `not-a-valid-url` proves the rejection happens before URL parsing:
        // no path without a custom-rustls token-binding connect can observe a
        // certificate selection, so the policy must fail closed immediately.
        let result = WebSocketTransport::connect_with_options(
            "not-a-valid-url",
            WebSocketConnectOptions::new()
                .with_require_client_fingerprint(true)
                .with_max_inbound_message_size(Some(0)),
        )
        .await;
        assert!(matches!(
            result,
            Err(SignalFishError::TokenBinding(
                crate::TokenBindingFailure::MissingClientFingerprint
            ))
        ));
    }

    #[cfg(all(feature = "tls", not(feature = "token-binding")))]
    #[tokio::test]
    async fn required_client_fingerprint_fails_closed_without_the_feature_on_custom_tls() {
        let tls_config = std::sync::Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(rustls::RootCertStore::empty())
                .with_no_client_auth(),
        );
        let result = WebSocketTransport::connect_with_tls_config(
            "not-a-valid-url",
            WebSocketConnectOptions::new().with_require_client_fingerprint(true),
            tls_config,
        )
        .await;
        assert!(matches!(
            result,
            Err(SignalFishError::TokenBinding(
                crate::TokenBindingFailure::MissingClientFingerprint
            ))
        ));
    }

    #[cfg(all(feature = "tls", feature = "token-binding"))]
    #[tokio::test]
    async fn required_client_fingerprint_fails_fast_without_token_binding_on_custom_tls() {
        // Disabled token binding produces no proofs, so the requirement is
        // unsatisfiable and must be rejected before network I/O.
        let tls_config = std::sync::Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(rustls::RootCertStore::empty())
                .with_no_client_auth(),
        );
        let result = WebSocketTransport::connect_with_tls_config(
            "not-a-valid-url",
            WebSocketConnectOptions::new()
                .with_require_client_fingerprint(true)
                .with_max_inbound_message_size(Some(0)),
            tls_config,
        )
        .await;
        assert!(matches!(
            result,
            Err(SignalFishError::TokenBinding(
                crate::TokenBindingFailure::MissingClientFingerprint
            ))
        ));
    }

    // ── Mock-stream helpers ──────────────────────────────────────────────

    use tokio::net::TcpListener;

    /// Start a local WebSocket server that runs `handler` on the accepted
    /// connection. Tests retain the task so handler panics cannot be hidden.
    async fn start_mock_server<F, Fut>(handler: F) -> (String, tokio::task::JoinHandle<()>)
    where
        F: FnOnce(tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) -> Fut
            + Send
            + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("TcpListener must bind to localhost");
        let addr = listener
            .local_addr()
            .expect("TcpListener must have a local address");

        let server_task = tokio::spawn(async move {
            let (tcp, _) = listener
                .accept()
                .await
                .expect("TcpListener must accept a connection");
            let ws = tokio_tungstenite::accept_async(tcp)
                .await
                .expect("WebSocket handshake must succeed");
            handler(ws).await;
        });

        (format!("ws://{addr}"), server_task)
    }

    async fn finish_mock_server(server_task: tokio::task::JoinHandle<()>) {
        tokio::time::timeout(std::time::Duration::from_secs(1), server_task)
            .await
            .expect("mock WebSocket server must finish promptly")
            .expect("mock WebSocket server must not panic");
    }

    #[cfg(feature = "token-binding")]
    fn required_token_binding_options() -> WebSocketConnectOptions {
        WebSocketConnectOptions::new().with_token_binding(TokenBindingMode::Required)
    }

    #[cfg(feature = "token-binding")]
    fn challenge_json() -> &'static str {
        r#"{"type":"TokenBindingChallenge","data":{"version":2,"scheme":"server_nonce_hkdf_sha256","nonce":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=","first_sequence":1}}"#
    }

    #[tokio::test]
    async fn default_connect_does_not_offer_a_subprotocol() {
        use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("default-handshake listener must bind");
        let addr = listener
            .local_addr()
            .expect("default-handshake listener must have an address");
        let server_task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("server must accept client");
            let mut ws = tokio_tungstenite::accept_hdr_async(
                tcp,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                    assert!(
                        request.headers().get(SEC_WEBSOCKET_PROTOCOL).is_none(),
                        "disabled/default mode must not change the WebSocket handshake"
                    );
                    Ok(response)
                },
            )
            .await
            .expect("default handshake must succeed");
            assert_eq!(
                ws.next()
                    .await
                    .expect("client must send one frame")
                    .expect("client frame must be valid"),
                Message::Text(r#"{"type":"Ping"}"#.into())
            );
        });

        let mut transport = WebSocketTransport::connect(&format!("ws://{addr}"))
            .await
            .expect("default connect must succeed");
        assert_eq!(
            transport.token_binding_status(),
            TokenBindingStatus::Disabled
        );
        let mut frame = Some(TransportFrame::Text(r#"{"type":"Ping"}"#.to_string()));
        std::future::poll_fn(|cx| transport.poll_send(cx, &mut frame))
            .await
            .expect("default frame must send");
        assert!(frame.is_none());
        finish_mock_server(server_task).await;
    }

    #[cfg(feature = "token-binding")]
    #[tokio::test]
    async fn required_mode_selects_and_consumes_challenge_before_sending() {
        use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("required-handshake listener must bind");
        let addr = listener
            .local_addr()
            .expect("required-handshake listener must have an address");
        let server_task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("server must accept client");
            let mut ws = tokio_tungstenite::accept_hdr_async(
                tcp,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request, mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    assert_eq!(
                        request
                            .headers()
                            .get(SEC_WEBSOCKET_PROTOCOL)
                            .and_then(|value| value.to_str().ok()),
                        Some(crate::token_binding::TOKEN_BINDING_SUBPROTOCOL)
                    );
                    response.headers_mut().insert(
                        SEC_WEBSOCKET_PROTOCOL,
                        tokio_tungstenite::tungstenite::http::HeaderValue::from_static(
                            crate::token_binding::TOKEN_BINDING_SUBPROTOCOL,
                        ),
                    );
                    Ok(response)
                },
            )
            .await
            .expect("required handshake must succeed");
            ws.send(Message::Text(challenge_json().into()))
                .await
                .expect("server must send challenge");
            let signed = ws
                .next()
                .await
                .expect("client must send after challenge")
                .expect("signed client frame must be valid")
                .into_text()
                .expect("Ping must remain text");
            let signed: serde_json::Value =
                serde_json::from_str(&signed).expect("signed Ping must parse");
            assert_eq!(signed["type"], "Ping");
            assert_eq!(signed["token_binding"]["sequence"], 1);
            assert!(signed["token_binding"]["signature"].is_string());
            ws.send(Message::Binary(vec![0xA5; 257].into()))
                .await
                .expect("server must send an oversized post-challenge message");
        });

        let mut transport = WebSocketTransport::connect_with_options(
            &format!("ws://{addr}"),
            required_token_binding_options().with_max_inbound_message_size(Some(256)),
        )
        .await
        .expect("required token binding must connect");
        assert_eq!(transport.token_binding_status(), TokenBindingStatus::Active);
        assert!(transport.token_binding_challenge().is_some());
        let mut frame = Some(TransportFrame::Text(r#"{"type":"Ping"}"#.to_string()));
        std::future::poll_fn(|cx| transport.poll_send(cx, &mut frame))
            .await
            .expect("signed Ping must send");
        assert!(frame.is_none());
        assert!(matches!(
            crate::transport::recv_frame(&mut transport).await,
            Some(Err(SignalFishError::TransportReceive(_)))
        ));
        finish_mock_server(server_task).await;
    }

    #[cfg(all(feature = "tls", feature = "token-binding"))]
    #[tokio::test]
    async fn required_client_fingerprint_fails_after_an_unobserved_certificate_selection() {
        use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fingerprint listener must bind");
        let addr = listener
            .local_addr()
            .expect("fingerprint listener must have an address");
        let server_task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("server must accept");
            let mut ws = tokio_tungstenite::accept_hdr_async(
                tcp,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                 mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    assert_eq!(
                        request
                            .headers()
                            .get(SEC_WEBSOCKET_PROTOCOL)
                            .and_then(|value| value.to_str().ok()),
                        Some(crate::token_binding::TOKEN_BINDING_SUBPROTOCOL)
                    );
                    response.headers_mut().insert(
                        SEC_WEBSOCKET_PROTOCOL,
                        tokio_tungstenite::tungstenite::http::HeaderValue::from_static(
                            crate::token_binding::TOKEN_BINDING_SUBPROTOCOL,
                        ),
                    );
                    Ok(response)
                },
            )
            .await
            .expect("handshake must succeed");
            ws.send(Message::Text(challenge_json().into()))
                .await
                .expect("server must send challenge");
            // The client must reject the fingerprint-less selection and drop
            // the connection instead of producing unsigned proofs.
            let _dropped = ws.next().await;
        });

        let tls_config = std::sync::Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(rustls::RootCertStore::empty())
                .with_no_client_auth(),
        );
        let result = WebSocketTransport::connect_with_tls_config(
            &format!("ws://{addr}"),
            required_token_binding_options().with_require_client_fingerprint(true),
            tls_config,
        )
        .await;
        assert!(matches!(
            result,
            Err(SignalFishError::TokenBinding(
                crate::TokenBindingFailure::MissingClientFingerprint
            ))
        ));
        finish_mock_server(server_task).await;
    }

    #[cfg(all(feature = "tls", feature = "token-binding"))]
    #[tokio::test]
    async fn required_client_fingerprint_fails_the_optional_unsigned_fallback() {
        use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fallback listener must bind");
        let addr = listener
            .local_addr()
            .expect("fallback listener must have an address");
        let server_task = tokio::spawn(async move {
            let (first, _) = listener.accept().await.expect("server must accept offer");
            // Complete the upgrade without selecting the offered subprotocol:
            // the exact server behavior that permits an unsigned fallback.
            let mut first = tokio_tungstenite::accept_hdr_async(
                first,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                 response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    assert!(request.headers().get(SEC_WEBSOCKET_PROTOCOL).is_some());
                    Ok(response)
                },
            )
            .await
            .expect("unsigned-capable first upgrade must succeed");
            let _closed_first_attempt = first.next().await;

            let (second, _) = listener
                .accept()
                .await
                .expect("server must accept fallback");
            let mut second = tokio_tungstenite::accept_hdr_async(
                second,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                 response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    assert!(request.headers().get(SEC_WEBSOCKET_PROTOCOL).is_none());
                    Ok(response)
                },
            )
            .await
            .expect("unsigned fallback handshake must succeed");
            // The client must reject the fallback and drop the connection.
            let _dropped = second.next().await;
        });

        let tls_config = std::sync::Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(rustls::RootCertStore::empty())
                .with_no_client_auth(),
        );
        let result = WebSocketTransport::connect_with_tls_config(
            &format!("ws://{addr}"),
            WebSocketConnectOptions::new()
                .with_token_binding(TokenBindingMode::Optional)
                .with_require_client_fingerprint(true),
            tls_config,
        )
        .await;
        assert!(matches!(
            result,
            Err(SignalFishError::TokenBinding(
                crate::TokenBindingFailure::MissingClientFingerprint
            ))
        ));
        finish_mock_server(server_task).await;
    }

    #[cfg(feature = "token-binding")]
    #[tokio::test]
    async fn optional_mode_retries_only_after_unsigned_upgrade_is_permitted() {
        use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("optional-handshake listener must bind");
        let addr = listener
            .local_addr()
            .expect("optional-handshake listener must have an address");
        let server_task = tokio::spawn(async move {
            let (first, _) = listener.accept().await.expect("server must accept offer");
            let mut first = tokio_tungstenite::accept_hdr_async(
                first,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                    assert!(request.headers().get(SEC_WEBSOCKET_PROTOCOL).is_some());
                    Ok(response)
                },
            )
            .await
            .expect("server permits unsigned first upgrade");
            let _closed_first_attempt = first.next().await;

            let (second, _) = listener
                .accept()
                .await
                .expect("server must accept fallback");
            let mut second = tokio_tungstenite::accept_hdr_async(
                second,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                    assert!(request.headers().get(SEC_WEBSOCKET_PROTOCOL).is_none());
                    Ok(response)
                },
            )
            .await
            .expect("unsigned fallback handshake must succeed");
            second
                .send(Message::Binary(vec![0x5A; 257].into()))
                .await
                .expect("server must send an oversized fallback message");
        });

        let mut transport = WebSocketTransport::connect_with_options(
            &format!("ws://{addr}"),
            WebSocketConnectOptions::new()
                .with_token_binding(TokenBindingMode::Optional)
                .with_max_inbound_message_size(Some(256)),
        )
        .await
        .expect("optional mode must accept server-permitted fallback");
        assert_eq!(
            transport.token_binding_status(),
            TokenBindingStatus::NotNegotiated
        );
        assert!(matches!(
            crate::transport::recv_frame(&mut transport).await,
            Some(Err(SignalFishError::TransportReceive(_)))
        ));
        finish_mock_server(server_task).await;
    }

    #[cfg(feature = "token-binding")]
    #[tokio::test]
    async fn http_rejections_keep_io_classification_in_every_token_binding_mode() {
        for mode in [
            TokenBindingMode::Disabled,
            TokenBindingMode::Optional,
            TokenBindingMode::Required,
        ] {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("rejection listener must bind");
            let addr = listener
                .local_addr()
                .expect("rejection listener must have an address");
            let server_task = tokio::spawn(async move {
                let (mut tcp, _) = listener.accept().await.expect("server must accept offer");
                tcp.write_all(
                    b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                )
                .await
                .expect("server must reject the offered handshake");
                drop(tcp);

                assert!(
                    tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
                        .await
                        .is_err(),
                    "HTTP rejection must not trigger an unsigned fallback"
                );
            });

            let error = WebSocketTransport::connect_with_options(
                &format!("ws://{addr}"),
                WebSocketConnectOptions::new().with_token_binding(mode),
            )
            .await
            .expect_err("an HTTP rejection must fail the connect in every mode");
            // An HTTP rejection says nothing about the offered subprotocol, so
            // it surfaces through the same Io mapping as the disabled path and
            // keeps the underlying status instead of a negotiation-specific
            // label.
            let message = error.to_string();
            assert!(
                matches!(error, SignalFishError::Io(_)),
                "HTTP rejection must keep the generic I/O classification in {mode:?}: {message}"
            );
            assert!(
                message.contains("403"),
                "HTTP rejection must preserve the server's status code in {mode:?}: {message}"
            );
            finish_mock_server(server_task).await;
        }
    }

    #[tokio::test]
    async fn malformed_server_handshake_response_is_io_not_invalid_config() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("malformed listener must bind");
        let addr = listener
            .local_addr()
            .expect("malformed listener must have an address");
        let server_task = tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.expect("server must accept offer");
            // An unparsable status line makes tungstenite fail the handshake
            // with an `HttpFormat` error after the TCP connection exists —
            // a runtime server fault, not caller configuration.
            tcp.write_all(b"HTTP/1.1 099 Bogus\r\n\r\n")
                .await
                .expect("server must write the malformed response");
        });

        let error = WebSocketTransport::connect(&format!("ws://{addr}"))
            .await
            .expect_err("a malformed server response must fail the connect");
        let message = error.to_string();
        assert!(
            matches!(error, SignalFishError::Io(_)),
            "a server-side response parse failure must stay an I/O error, not \
             blame the caller's URL: {message}"
        );
        assert!(
            !matches!(error, SignalFishError::InvalidConfig { .. }),
            "post-connect HttpFormat failures must not wear the configuration costume: {message}"
        );
        finish_mock_server(server_task).await;
    }

    /// Pins connect-future cancellation for required token binding: dropping
    /// the future while it waits for the server challenge must tear down the
    /// selected socket promptly instead of leaking a half-open connection.
    #[cfg(feature = "token-binding")]
    #[tokio::test]
    async fn dropping_required_connect_mid_challenge_closes_the_socket() {
        use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mid-challenge listener must bind");
        let addr = listener
            .local_addr()
            .expect("mid-challenge listener must have an address");
        let server_task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("server must accept client");
            let mut ws = tokio_tungstenite::accept_hdr_async(
                tcp,
                |_request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                 mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    response.headers_mut().insert(
                        SEC_WEBSOCKET_PROTOCOL,
                        tokio_tungstenite::tungstenite::http::HeaderValue::from_static(
                            crate::token_binding::TOKEN_BINDING_SUBPROTOCOL,
                        ),
                    );
                    Ok(response)
                },
            )
            .await
            .expect("selected handshake must succeed");
            // The client never receives its challenge: dropping the connect
            // future must end this stream within the timeout below.
            match tokio::time::timeout(std::time::Duration::from_secs(1), ws.next()).await {
                Ok(None | Some(Err(_))) => {}
                Ok(Some(Ok(message))) => {
                    panic!("dropped connect future must not send frames: {message:?}")
                }
                Err(_) => panic!("the socket stayed open after the connect future was dropped"),
            }
        });

        let url = format!("ws://{addr}");
        let mut connect = Box::pin(WebSocketTransport::connect_with_options(
            &url,
            required_token_binding_options(),
        ));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), connect.as_mut())
                .await
                .is_err(),
            "connect must stay parked awaiting the challenge"
        );
        drop(connect);
        finish_mock_server(server_task).await;
    }

    #[cfg(feature = "token-binding")]
    #[tokio::test]
    async fn challengeless_control_flood_reports_no_valid_challenge() {
        use futures_util::SinkExt as _;
        use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("control-flood listener must bind");
        let addr = listener
            .local_addr()
            .expect("control-flood listener must have an address");
        let server_task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("server must accept client");
            let mut ws = tokio_tungstenite::accept_hdr_async(
                tcp,
                |_request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                 mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    response.headers_mut().insert(
                        SEC_WEBSOCKET_PROTOCOL,
                        tokio_tungstenite::tungstenite::http::HeaderValue::from_static(
                            crate::token_binding::TOKEN_BINDING_SUBPROTOCOL,
                        ),
                    );
                    Ok(response)
                },
            )
            .await
            .expect("selected handshake must succeed");
            // One control frame over the skip budget without any application
            // frame is a server that never speaks the challenge protocol.
            for _ in 0..MAX_SKIPPED_CONTROL_FRAMES_PER_POLL {
                if ws.send(Message::Pong(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        });

        let error = WebSocketTransport::connect_with_options(
            &format!("ws://{addr}"),
            required_token_binding_options(),
        )
        .await
        .expect_err("a connection that never delivers a challenge must fail");
        assert!(matches!(
            error,
            SignalFishError::TokenBinding(crate::TokenBindingFailure::MalformedChallenge)
        ));
        finish_mock_server(server_task).await;
    }

    #[cfg(feature = "token-binding")]
    #[tokio::test]
    async fn selected_mode_times_out_waiting_for_the_first_challenge() {
        use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("challenge-timeout listener must bind");
        let addr = listener
            .local_addr()
            .expect("challenge-timeout listener must have an address");
        let server_task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("server must accept client");
            let _ws = tokio_tungstenite::accept_hdr_async(
                tcp,
                |_request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                 mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    response.headers_mut().insert(
                        SEC_WEBSOCKET_PROTOCOL,
                        tokio_tungstenite::tungstenite::http::HeaderValue::from_static(
                            crate::token_binding::TOKEN_BINDING_SUBPROTOCOL,
                        ),
                    );
                    Ok(response)
                },
            )
            .await
            .expect("selected handshake must succeed");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        });

        let options = required_token_binding_options()
            .with_token_binding_challenge_timeout(std::time::Duration::from_millis(20));
        let error = WebSocketTransport::connect_with_options(&format!("ws://{addr}"), options)
            .await
            .expect_err("selected connection must not wait forever for its challenge");
        assert!(matches!(
            error,
            SignalFishError::TokenBinding(crate::TokenBindingFailure::ChallengeTimeout)
        ));
        finish_mock_server(server_task).await;
    }

    #[cfg(feature = "token-binding")]
    #[tokio::test]
    async fn required_mode_rejects_missing_selection_and_malformed_challenge() {
        use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

        for (send_selection, challenge, expected) in [
            (
                false,
                None,
                crate::TokenBindingFailure::SubprotocolNotNegotiated,
            ),
            (
                true,
                Some(r#"{"type":"Authenticated","data":{}}"#),
                crate::TokenBindingFailure::MalformedChallenge,
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("negative-handshake listener must bind");
            let addr = listener
                .local_addr()
                .expect("negative-handshake listener must have an address");
            let server_task = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.expect("server must accept client");
                let mut ws = tokio_tungstenite::accept_hdr_async(
                    tcp,
                    move |_request: &tokio_tungstenite::tungstenite::handshake::server::Request, mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                        if send_selection {
                            response.headers_mut().insert(
                                SEC_WEBSOCKET_PROTOCOL,
                                tokio_tungstenite::tungstenite::http::HeaderValue::from_static(
                                    crate::token_binding::TOKEN_BINDING_SUBPROTOCOL,
                                ),
                            );
                        }
                        Ok(response)
                    },
                )
                .await
                .expect("server-side negative handshake must complete");
                if let Some(challenge) = challenge {
                    ws.send(Message::Text(challenge.into()))
                        .await
                        .expect("server must send malformed challenge");
                }
            });
            let error = WebSocketTransport::connect_with_options(
                &format!("ws://{addr}"),
                required_token_binding_options(),
            )
            .await
            .expect_err("required mode must fail closed");
            assert!(matches!(
                error,
                SignalFishError::TokenBinding(actual) if actual == expected
            ));
            finish_mock_server(server_task).await;
        }
    }

    #[cfg(feature = "token-binding")]
    #[tokio::test]
    async fn required_mode_rejects_a_foreign_subprotocol_selection() {
        use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("subprotocol listener must bind");
        let addr = listener
            .local_addr()
            .expect("subprotocol listener must have an address");
        let server_task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("server must accept client");
            let _ws = tokio_tungstenite::accept_hdr_async(
                tcp,
                move |_request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                      mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    response.headers_mut().insert(
                        SEC_WEBSOCKET_PROTOCOL,
                        tokio_tungstenite::tungstenite::http::HeaderValue::from_static(
                            "other-protocol",
                        ),
                    );
                    Ok(response)
                },
            )
            .await
            .expect("server-side handshake must complete");
        });
        let error = WebSocketTransport::connect_with_options(
            &format!("ws://{addr}"),
            required_token_binding_options(),
        )
        .await
        .expect_err("a selection outside the offered set must fail closed");
        assert!(matches!(
            error,
            SignalFishError::TokenBinding(crate::TokenBindingFailure::UnexpectedSubprotocol)
        ));
        finish_mock_server(server_task).await;
    }

    #[cfg(feature = "token-binding")]
    #[tokio::test]
    async fn required_mode_reports_missing_challenge_when_the_stream_ends_or_closes() {
        use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

        for send_close_frame in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("challengeless listener must bind");
            let addr = listener
                .local_addr()
                .expect("challengeless listener must have an address");
            let server_task = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.expect("server must accept client");
                let mut ws = tokio_tungstenite::accept_hdr_async(
                    tcp,
                    move |_request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                          mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                        response.headers_mut().insert(
                            SEC_WEBSOCKET_PROTOCOL,
                            tokio_tungstenite::tungstenite::http::HeaderValue::from_static(
                                crate::token_binding::TOKEN_BINDING_SUBPROTOCOL,
                            ),
                        );
                        Ok(response)
                    },
                )
                .await
                .expect("server-side handshake must complete");
                if send_close_frame {
                    ws.send(Message::Close(None))
                        .await
                        .expect("server must send its close frame");
                }
                // Without a close frame the stream is dropped at task end, so
                // the client observes a clean EOF instead.
            });
            let error = WebSocketTransport::connect_with_options(
                &format!("ws://{addr}"),
                required_token_binding_options(),
            )
            .await
            .expect_err("a selected connection with no challenge must fail closed");
            if send_close_frame {
                assert!(
                    matches!(
                        error,
                        SignalFishError::TokenBinding(crate::TokenBindingFailure::MissingChallenge)
                    ),
                    "close-before-challenge must report MissingChallenge, got: {error}"
                );
            } else {
                // An abrupt TCP drop inside the challenge window carries no
                // close metadata, so it keeps the transport-receive
                // classification instead of being relabeled.
                assert!(
                    matches!(error, SignalFishError::TransportReceive(_)),
                    "an abrupt drop must stay a transport receive error, got: {error}"
                );
            }
            finish_mock_server(server_task).await;
        }
    }

    type MemoryWebSocket = tokio_tungstenite::WebSocketStream<tokio::io::DuplexStream>;

    struct SyntheticWebSocket {
        ended: bool,
    }

    struct SendErrorBufferedWebSocket {
        buffered: VecDeque<Message>,
    }

    impl Stream for SendErrorBufferedWebSocket {
        type Item = Result<Message, WebSocketError>;

        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            self.buffered
                .pop_front()
                .map_or(Poll::Pending, |message| Poll::Ready(Some(Ok(message))))
        }
    }

    impl Sink<Message> for SendErrorBufferedWebSocket {
        type Error = WebSocketError;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Err(WebSocketError::ConnectionClosed))
        }

        fn start_send(self: Pin<&mut Self>, _item: Message) -> Result<(), Self::Error> {
            Err(WebSocketError::ConnectionClosed)
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Err(WebSocketError::ConnectionClosed))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Err(WebSocketError::ConnectionClosed))
        }
    }

    impl Stream for SyntheticWebSocket {
        type Item = Result<Message, WebSocketError>;

        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            if self.ended {
                Poll::Ready(None)
            } else {
                Poll::Pending
            }
        }
    }

    impl Sink<Message> for SyntheticWebSocket {
        type Error = WebSocketError;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }

        fn start_send(self: Pin<&mut Self>, _item: Message) -> Result<(), Self::Error> {
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }
    }

    async fn memory_websocket_pair(
        capacity: usize,
    ) -> (WebSocketState<MemoryWebSocket>, MemoryWebSocket) {
        use tokio_tungstenite::tungstenite::protocol::Role;

        let (client_io, server_io) = tokio::io::duplex(capacity);
        let (client, server) = tokio::join!(
            tokio_tungstenite::WebSocketStream::from_raw_socket(client_io, Role::Client, None),
            tokio_tungstenite::WebSocketStream::from_raw_socket(server_io, Role::Server, None),
        );
        (WebSocketState::new(client), server)
    }

    #[derive(Default)]
    struct CountingWake {
        count: AtomicUsize,
    }

    impl Wake for CountingWake {
        fn wake(self: Arc<Self>) {
            self.count.fetch_add(1, Ordering::SeqCst);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.count.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn counting_waker() -> (Arc<CountingWake>, Waker) {
        let counter = Arc::new(CountingWake::default());
        let waker = Waker::from(Arc::clone(&counter));
        (counter, waker)
    }

    fn expect_received_frame(
        result: Option<Result<TransportFrame, SignalFishError>>,
    ) -> TransportFrame {
        result
            .expect("receive must produce a frame")
            .expect("received frame must be valid")
    }

    async fn connect_to_reset_peer() -> WebSocketTransport {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("reset listener must bind to localhost");
        let addr = listener
            .local_addr()
            .expect("reset listener must have a local address");
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let (reset_tx, reset_rx) = tokio::sync::oneshot::channel();

        let server_task = tokio::spawn(async move {
            let (tcp, _) = listener
                .accept()
                .await
                .expect("reset listener must accept a connection");
            let ws = tokio_tungstenite::accept_async(tcp)
                .await
                .expect("reset peer WebSocket handshake must succeed");
            release_rx
                .await
                .expect("client must release the reset peer after connecting");
            ws.get_ref()
                .set_zero_linger()
                .expect("reset peer must enable zero linger");
            drop(ws);
            let _ = reset_tx.send(());
        });

        let transport = WebSocketTransport::connect(&format!("ws://{addr}"))
            .await
            .expect("client must connect before the peer resets");
        release_tx
            .send(())
            .expect("reset peer must still be waiting for the client");
        reset_rx
            .await
            .expect("reset peer must report dropping the socket");
        finish_mock_server(server_task).await;
        transport
    }

    fn plain_tcp_stream(transport: &WebSocketTransport) -> &tokio::net::TcpStream {
        match transport
            .state
            .stream
            .as_ref()
            .expect("transport must hold a live stream after connect")
            .get_ref()
        {
            tokio_tungstenite::MaybeTlsStream::Plain(tcp) => tcp,
            _ => panic!("a ws:// connection must use the plain (non-TLS) stream variant"),
        }
    }

    /// Read `TCP_NODELAY` from the underlying socket of a `ws://` (non-TLS)
    /// transport. The inline test module can reach the private stream, and a
    /// `ws://` client always resolves to the plain variant.
    fn plain_tcp_nodelay(transport: &WebSocketTransport) -> bool {
        plain_tcp_stream(transport)
            .nodelay()
            .expect("querying TCP_NODELAY on the loopback socket must succeed")
    }

    #[test]
    fn debug_omits_stream_contents_and_peer_close_reason() {
        let secret = "websocket-debug-secret";
        let transport = WebSocketTransport {
            state: WebSocketState {
                stream: None,
                closed: true,
                close_info: Some(TransportCloseInfo {
                    reason: Some(secret.into()),
                    ..TransportCloseInfo::default()
                }),
                send_started: false,
                send_failed: true,
                control_flush_pending: false,
                peer_close_pending: false,
                token_binding: WebSocketTokenBinding::Disabled,
            },
        };

        let output = format!("{transport:?}");
        assert!(!output.contains(secret), "debug output leaked: {output}");
        assert_eq!(
            output,
            "WebSocketTransport { has_stream: false, closed: true, has_close_info: true, \
             send_started: false, send_failed: true, control_flush_pending: false, peer_close_pending: false, \
             token_binding: Disabled }"
        );
    }

    // ── Mock-stream tests ────────────────────────────────────────────────

    #[tokio::test]
    async fn connect_disables_nagle_by_default() {
        let (url, server_task) = start_mock_server(|mut ws| async move {
            // Hold the connection open until the client disconnects.
            while let Some(Ok(_)) = ws.next().await {}
        })
        .await;

        let transport = WebSocketTransport::connect(&url)
            .await
            .expect("WebSocket connect must succeed");

        assert!(
            plain_tcp_nodelay(&transport),
            "connect() must disable Nagle (TCP_NODELAY) by default for low-latency game traffic"
        );
        drop(transport);
        finish_mock_server(server_task).await;
    }

    #[tokio::test]
    async fn connect_with_options_controls_nagle() {
        // (disable_nagle requested, expected TCP_NODELAY on the socket)
        for (disable_nagle, expected_nodelay) in [(true, true), (false, false)] {
            let (url, server_task) =
                start_mock_server(
                    |mut ws| async move { while let Some(Ok(_)) = ws.next().await {} },
                )
                .await;

            let options = WebSocketConnectOptions::new().with_disable_nagle(disable_nagle);
            let transport = WebSocketTransport::connect_with_options(&url, options)
                .await
                .expect("WebSocket connect_with_options must succeed");

            assert_eq!(
                plain_tcp_nodelay(&transport),
                expected_nodelay,
                "disable_nagle={disable_nagle} must produce TCP_NODELAY={expected_nodelay}"
            );
            drop(transport);
            finish_mock_server(server_task).await;
        }
    }

    #[cfg(feature = "tls")]
    #[tokio::test]
    async fn wss_has_a_working_tls_provider() {
        // A plain-TCP loopback that never speaks TLS. A `wss://` connect must
        // attempt a real TLS handshake — proving a rustls crypto provider is
        // wired — and fail with an error rather than panicking with
        // "no process-level CryptoProvider available".
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener must bind to localhost");
        let addr = listener
            .local_addr()
            .expect("listener must have an address");
        let server_task = tokio::spawn(async move {
            // Accept one connection and drop it so the client's TLS handshake
            // fails cleanly instead of hanging.
            let _ = listener.accept().await;
        });

        let result = WebSocketTransport::connect(&format!("wss://{addr}")).await;
        assert!(
            result.is_err(),
            "wss:// to a non-TLS peer must fail via a TLS/IO error (proving the provider is wired), \
             not succeed"
        );
        finish_mock_server(server_task).await;
    }

    #[tokio::test]
    async fn control_frame_budget_self_wakes_before_buffered_application_data() {
        let (mut state, mut server) = memory_websocket_pair(16 * 1024).await;
        for _ in 0..MAX_SKIPPED_CONTROL_FRAMES_PER_POLL - 1 {
            server
                .send(Message::Pong(Vec::new().into()))
                .await
                .expect("server must buffer the control-frame flood");
        }
        server
            .send(Message::Ping(b"boundary".to_vec().into()))
            .await
            .expect("server must buffer the boundary Ping");
        server
            .send(Message::Text("after-control-flood".into()))
            .await
            .expect("server must buffer application data after the control flood");

        let (wake_counter, waker) = counting_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(state.poll_recv(&mut cx).is_pending());
        assert!(
            wake_counter.count.load(Ordering::SeqCst) > 0,
            "exhausting the control budget must self-wake for buffered work"
        );
        let response = tokio::time::timeout(std::time::Duration::from_secs(1), server.next())
            .await
            .expect("the boundary Ping response must be flushed before the bounded yield")
            .expect("the server stream must remain open")
            .expect("the boundary Ping response must be valid");
        assert_eq!(response, Message::Pong(b"boundary".to_vec().into()));

        let Poll::Ready(received) = state.poll_recv(&mut cx) else {
            panic!("buffered application data must be ready after the bounded yield");
        };
        assert_eq!(
            expect_received_frame(received),
            TransportFrame::Text("after-control-flood".into())
        );
    }

    #[tokio::test]
    async fn ping_auto_pong_is_flushed_before_later_application_reads() {
        let (mut state, mut server) = memory_websocket_pair(1024).await;
        let server_task = tokio::spawn(async move {
            server
                .send(Message::Ping(b"heartbeat".to_vec().into()))
                .await
                .expect("server must send Ping");
            let response = tokio::time::timeout(std::time::Duration::from_secs(1), server.next())
                .await
                .expect("client must flush the automatic Pong promptly")
                .expect("client stream must remain open")
                .expect("automatic Pong must be a valid WebSocket message");
            assert_eq!(response, Message::Pong(b"heartbeat".to_vec().into()));
            server
                .send(Message::Text("after-pong".into()))
                .await
                .expect("server must send application data after observing Pong");
        });

        let received = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            std::future::poll_fn(|cx| state.poll_recv(cx)),
        )
        .await
        .expect("client receive must make progress after Ping");
        assert_eq!(
            expect_received_frame(received),
            TransportFrame::Text("after-pong".into())
        );
        server_task
            .await
            .expect("Ping/Pong server task must finish without panicking");
    }

    #[tokio::test]
    async fn partial_frame_registers_and_notifies_the_real_waker() {
        use tokio_tungstenite::tungstenite::protocol::Role;

        let (client_io, mut server_io) = tokio::io::duplex(64);
        let client =
            tokio_tungstenite::WebSocketStream::from_raw_socket(client_io, Role::Client, None)
                .await;
        let mut state = WebSocketState::new(client);
        let (wake_counter, waker) = counting_waker();
        let mut cx = Context::from_waker(&waker);

        assert!(state.poll_recv(&mut cx).is_pending());
        server_io
            .write_all(&[0x81, 0x05, b'l'])
            .await
            .expect("server must write a partial unmasked text frame");
        assert!(state.poll_recv(&mut cx).is_pending());

        wake_counter.count.store(0, Ordering::SeqCst);
        server_io
            .write_all(&[0x61, 0x74, 0x65, 0x72])
            .await
            .expect("server must finish the partial text frame");
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while wake_counter.count.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("finishing a partial frame must notify the registered waker");

        let Poll::Ready(received) = state.poll_recv(&mut cx) else {
            panic!("completed partial frame must be ready after its waker fires");
        };
        assert_eq!(
            expect_received_frame(received),
            TransportFrame::Text("later".into())
        );
    }

    #[tokio::test]
    async fn pending_flush_retains_the_second_frame_and_preserves_fifo() {
        let (mut state, mut server) = memory_websocket_pair(64).await;
        let first_payload = vec![0xA5; 1024];
        let mut first = Some(TransportFrame::Binary(first_payload.clone()));
        let (_wake_counter, waker) = counting_waker();
        let mut cx = Context::from_waker(&waker);

        assert!(state.poll_send(&mut cx, &mut first).is_pending());
        assert!(
            first.is_none(),
            "the first frame must be accepted exactly once"
        );
        assert!(state.send_started);

        let mut second = Some(TransportFrame::Text("second".into()));
        assert!(state.poll_send(&mut cx, &mut second).is_pending());
        assert_eq!(
            second,
            Some(TransportFrame::Text("second".into())),
            "a pending first flush must not consume the second caller frame"
        );

        let server_task = tokio::spawn(async move {
            let first = server
                .next()
                .await
                .expect("server must receive the first frame")
                .expect("first frame must be valid");
            let second = server
                .next()
                .await
                .expect("server must receive the second frame")
                .expect("second frame must be valid");
            (first, second)
        });

        std::future::poll_fn(|cx| state.poll_send(cx, &mut second))
            .await
            .expect("the accepted first frame must finish flushing");
        assert!(
            second.is_some(),
            "first completion must leave the second frame owned by its caller"
        );
        std::future::poll_fn(|cx| state.poll_send(cx, &mut second))
            .await
            .expect("the second frame must send after the first completes");
        assert!(second.is_none());

        let (first, second) = server_task
            .await
            .expect("FIFO server task must finish without panicking");
        assert_eq!(first, Message::Binary(first_payload.into()));
        assert_eq!(second, Message::Text("second".into()));
    }

    #[test]
    fn empty_send_slot_completes_without_polling_backend_readiness() {
        let mut state = WebSocketState::new(SyntheticWebSocket { ended: false });
        let mut frame = None;
        let (_wake_counter, waker) = counting_waker();
        let mut cx = Context::from_waker(&waker);

        assert!(matches!(
            state.poll_send(&mut cx, &mut frame),
            Poll::Ready(Ok(()))
        ));
    }

    #[test]
    fn send_error_preserves_one_buffered_inbound_farewell_until_ready_drain() {
        let farewell = r#"{"type":"Error","data":{"message":"Disconnected as a slow consumer","error_code":"SLOW_CONSUMER"}}"#;
        let mut state = WebSocketState::new(SendErrorBufferedWebSocket {
            buffered: VecDeque::from([
                Message::Ping(b"before-farewell".to_vec().into()),
                Message::Text(farewell.into()),
            ]),
        });
        let (_wake_counter, waker) = counting_waker();
        let mut cx = Context::from_waker(&waker);
        let expected = TransportFrame::Text("caller-owned".into());
        let mut offered = Some(expected.clone());

        assert!(matches!(
            state.poll_send(&mut cx, &mut offered),
            Poll::Ready(Err(SignalFishError::TransportSend(_)))
        ));
        assert_eq!(offered, Some(expected));
        assert!(state.send_failed);
        assert!(!state.closed);
        assert!(state.stream.is_some());

        assert_eq!(
            expect_received_frame(match state.poll_recv(&mut cx) {
                Poll::Ready(received) => received,
                Poll::Pending => panic!("buffered farewell must remain immediately ready"),
            }),
            TransportFrame::Text(farewell.into())
        );
        assert!(matches!(state.poll_recv(&mut cx), Poll::Ready(None)));
        assert!(state.closed);
        assert!(state.stream.is_none());
    }

    #[test]
    fn raw_eof_fuses_receive_send_and_close_operations() {
        let mut state = WebSocketState::new(SyntheticWebSocket { ended: true });
        state.send_started = true;
        let (_wake_counter, waker) = counting_waker();
        let mut cx = Context::from_waker(&waker);

        assert!(matches!(state.poll_recv(&mut cx), Poll::Ready(None)));
        assert!(state.closed);
        assert!(state.stream.is_none());
        assert!(!state.send_started);
        assert!(!state.control_flush_pending);
        assert!(!state.peer_close_pending);
        assert!(matches!(state.poll_recv(&mut cx), Poll::Ready(None)));

        let expected = TransportFrame::Text("caller-owned-after-eof".into());
        let mut offered = Some(expected.clone());
        assert!(matches!(
            state.poll_send(&mut cx, &mut offered),
            Poll::Ready(Err(SignalFishError::TransportClosed))
        ));
        assert_eq!(offered, Some(expected));
        assert!(matches!(state.poll_close(&mut cx), Poll::Ready(Ok(()))));
        assert!(matches!(state.poll_close(&mut cx), Poll::Ready(Ok(()))));
    }

    #[tokio::test]
    async fn rejected_write_buffer_full_restores_the_exact_caller_frame() {
        use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};

        for expected in [
            TransportFrame::Text("x".repeat(64)),
            TransportFrame::Binary(vec![0x5A; 64]),
        ] {
            let (client_io, _server_io) = tokio::io::duplex(64);
            let config = WebSocketConfig::default()
                .write_buffer_size(0)
                .max_write_buffer_size(16);
            let client = tokio_tungstenite::WebSocketStream::from_raw_socket(
                client_io,
                Role::Client,
                Some(config),
            )
            .await;
            let mut state = WebSocketState::new(client);
            let mut frame = Some(expected.clone());
            let (_wake_counter, waker) = counting_waker();
            let mut cx = Context::from_waker(&waker);

            let result = state.poll_send(&mut cx, &mut frame);
            assert!(matches!(
                result,
                Poll::Ready(Err(SignalFishError::TransportSend(_)))
            ));
            assert_eq!(frame, Some(expected));
            assert!(!state.closed, "a per-message refusal is retryable");
            assert!(!state.send_failed, "the restored frame may be retried");
            assert!(!state.send_started);
            // The structured backend cause survives the frame-restoration
            // classification, so programmatic handling can still reach the
            // exact `tungstenite::Error` (here `WriteBufferFull`).
            let Poll::Ready(Err(SignalFishError::TransportSend(cause))) = result else {
                panic!("the write-buffer refusal must surface as TransportSend");
            };
            assert!(matches!(
                cause.downcast_ref::<WebSocketError>(),
                Some(WebSocketError::WriteBufferFull(_))
            ));
        }
    }

    #[cfg(feature = "token-binding")]
    #[tokio::test]
    async fn token_binding_failures_preserve_original_frame_and_sequence() {
        use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};

        let (client_io, _server_io) = tokio::io::duplex(64);
        let config = WebSocketConfig::default()
            .write_buffer_size(0)
            .max_write_buffer_size(16);
        let client = tokio_tungstenite::WebSocketStream::from_raw_socket(
            client_io,
            Role::Client,
            Some(config),
        )
        .await;
        let challenge = crate::token_binding::parse_challenge(challenge_json())
            .expect("fixture challenge must parse");
        let fingerprint = "1376a851f01e89a6b4784fefcd761ec41187b7a5de02da57b764d9d920cf7107";
        let session = crate::token_binding::TokenBindingSession::from_challenge(
            "MDEyMzQ1Njc4OWFiY2RlZg==",
            challenge,
            Some(fingerprint.to_string()),
        )
        .expect("fixture handshake key must derive a session");
        let mut state = WebSocketState::new(client);
        state.token_binding = WebSocketTokenBinding::Active(Some(session));
        let (_wake_counter, waker) = counting_waker();
        let mut cx = Context::from_waker(&waker);

        let unsupported = TransportFrame::Text(r#"{"type":"Ping","bad":9007199254740992}"#.into());
        let mut offered = Some(unsupported.clone());
        assert!(matches!(
            state.poll_send(&mut cx, &mut offered),
            Poll::Ready(Err(SignalFishError::TokenBinding(
                crate::TokenBindingFailure::UnsupportedJson
            )))
        ));
        assert_eq!(offered, Some(unsupported));

        let original = TransportFrame::Text(format!(
            r#"{{"type":"Ping","padding":"{}"}}"#,
            "x".repeat(64)
        ));
        let mut offered = Some(original.clone());
        assert!(matches!(
            state.poll_send(&mut cx, &mut offered),
            Poll::Ready(Err(SignalFishError::TransportSend(_)))
        ));
        assert_eq!(offered, Some(original.clone()));
        assert!(!state.closed, "buffer refusal must remain retryable");

        let TransportFrame::Text(retry) = state
            .token_binding
            .prepare(&original)
            .expect("retry preparation must succeed")
        else {
            panic!("text input must produce a text proof");
        };
        let retry: serde_json::Value =
            serde_json::from_str(&retry).expect("retry proof must parse");
        assert_eq!(
            retry["token_binding"]["sequence"], 1,
            "preparation and backend refusal must not consume a sequence"
        );
        assert_eq!(retry["token_binding"]["fingerprint"], fingerprint);

        state.mark_terminal();
        assert_eq!(state.token_binding.status(), TokenBindingStatus::Active);
        assert!(
            state.token_binding.challenge().is_none(),
            "terminalization must drop challenge and zeroize the active session"
        );
        assert!(state.stream.is_none());
    }

    #[tokio::test]
    async fn recv_receives_text_messages() {
        let (url, server_task) = start_mock_server(|mut ws| async move {
            ws.send(Message::Text("hello".into()))
                .await
                .expect("server must send 'hello'");
            ws.send(Message::Text("world".into()))
                .await
                .expect("server must send 'world'");
            ws.close(None).await.expect("server must close cleanly");
        })
        .await;

        let mut transport = WebSocketTransport::connect(&url)
            .await
            .expect("WebSocket connect must succeed");

        let msg1 = crate::transport::recv_frame(&mut transport)
            .await
            .expect("recv must return Some")
            .expect("recv must return Ok");
        assert_eq!(msg1, TransportFrame::Text("hello".into()));

        let msg2 = crate::transport::recv_frame(&mut transport)
            .await
            .expect("recv must return Some")
            .expect("recv must return Ok");
        assert_eq!(msg2, TransportFrame::Text("world".into()));
        drop(transport);
        finish_mock_server(server_task).await;
    }

    #[tokio::test]
    async fn recv_returns_none_on_close_frame() {
        let (url, server_task) = start_mock_server(|mut ws| async move {
            ws.close(None).await.expect("server must close cleanly");
        })
        .await;

        let mut transport = WebSocketTransport::connect(&url)
            .await
            .expect("WebSocket connect must succeed");
        let result = crate::transport::recv_frame(&mut transport).await;
        assert!(result.is_none());
        assert_eq!(
            transport.close_info(),
            Some(TransportCloseInfo {
                code: None,
                reason: None,
                clean: None,
                initiated_by_peer: true,
            })
        );

        assert!(crate::transport::recv_frame(&mut transport).await.is_none());
        let expected = TransportFrame::Text("after-peer-close".into());
        let mut offered = Some(expected.clone());
        let send_result = std::future::poll_fn(|cx| transport.poll_send(cx, &mut offered)).await;
        assert!(matches!(send_result, Err(SignalFishError::TransportClosed)));
        assert_eq!(offered, Some(expected));
        crate::transport::close_transport(&mut transport)
            .await
            .expect("close after peer close must remain idempotent");
        finish_mock_server(server_task).await;
    }

    #[tokio::test]
    async fn close_frame_reason_is_captured() {
        use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
        use tokio_tungstenite::tungstenite::protocol::frame::CloseFrame;

        let (url, server_task) = start_mock_server(|mut ws| async move {
            ws.close(Some(CloseFrame {
                code: CloseCode::Policy,
                reason: "slow consumer".into(),
            }))
            .await
            .expect("server must close with a frame");
        })
        .await;

        let mut transport = WebSocketTransport::connect(&url)
            .await
            .expect("WebSocket connect must succeed");
        let result = crate::transport::recv_frame(&mut transport).await;
        assert!(result.is_none());

        let info = transport
            .close_info()
            .expect("close frame explanation must be captured");
        assert_eq!(info.code, Some(u16::from(CloseCode::Policy)));
        assert_eq!(info.reason.as_deref(), Some("slow consumer"));
        assert_eq!(info.clean, None);
        assert!(info.initiated_by_peer);
        finish_mock_server(server_task).await;
    }

    #[tokio::test]
    async fn peer_close_response_is_flushed_before_recv_finishes() {
        use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
        use tokio_tungstenite::tungstenite::protocol::frame::CloseFrame;

        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        let (url, server_task) = start_mock_server(move |mut ws| async move {
            ws.send(Message::Close(Some(CloseFrame {
                code: CloseCode::Away,
                reason: "server draining".into(),
            })))
            .await
            .expect("server must send a peer close frame");

            let response = tokio::time::timeout(std::time::Duration::from_secs(1), ws.next()).await;
            let observed_close_response = matches!(
                response,
                Ok(Some(Ok(Message::Close(Some(CloseFrame {
                    code: CloseCode::Away,
                    ..
                })))))
            );
            let _ = response_tx.send(observed_close_response);
        })
        .await;

        let mut transport = WebSocketTransport::connect(&url)
            .await
            .expect("WebSocket connect must succeed");
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            crate::transport::recv_frame(&mut transport),
        )
        .await
        .expect("receiving a peer close must make progress");

        assert!(result.is_none());
        assert!(
            response_rx
                .await
                .expect("server task must report whether it received the response"),
            "client must flush a matching close response before recv returns None"
        );
        assert!(transport.state.closed);
        assert!(!transport.state.peer_close_pending);
        finish_mock_server(server_task).await;
    }

    #[tokio::test]
    async fn recv_passes_binary_frames_through() {
        let (url, server_task) = start_mock_server(|mut ws| async move {
            ws.send(Message::Binary(vec![0xDE, 0xAD].into()))
                .await
                .expect("server must send binary frame");
            ws.send(Message::Text("after_binary".into()))
                .await
                .expect("server must send 'after_binary'");
            ws.close(None).await.expect("server must close cleanly");
        })
        .await;

        let mut transport = WebSocketTransport::connect(&url)
            .await
            .expect("WebSocket connect must succeed");

        let msg = crate::transport::recv_frame(&mut transport)
            .await
            .expect("recv must return Some")
            .expect("recv must return Ok");
        assert_eq!(msg, TransportFrame::Binary(vec![0xDE, 0xAD]));
        let next = crate::transport::recv_frame(&mut transport)
            .await
            .expect("recv must return Some")
            .expect("recv must return Ok");
        assert_eq!(next, TransportFrame::Text("after_binary".into()));
        drop(transport);
        finish_mock_server(server_task).await;
    }

    #[tokio::test]
    async fn inbound_limit_is_inclusive_and_can_be_disabled() {
        for limit in [Some(257), None] {
            let (url, server_task) = start_mock_server(|mut ws| async move {
                ws.send(Message::Binary(vec![0xC7; 257].into()))
                    .await
                    .expect("server must send the boundary message");
            })
            .await;
            let options = WebSocketConnectOptions::new().with_max_inbound_message_size(limit);
            let mut transport = WebSocketTransport::connect_with_options(&url, options)
                .await
                .expect("custom inbound policy must connect");

            let live_config = transport
                .state
                .stream
                .as_ref()
                .expect("connected transport must retain its WebSocket stream")
                .get_config();
            assert_eq!(live_config.max_frame_size, limit);
            assert_eq!(live_config.max_message_size, limit);

            assert_eq!(
                expect_received_frame(crate::transport::recv_frame(&mut transport).await),
                TransportFrame::Binary(vec![0xC7; 257]),
                "limit {limit:?} must admit this complete message"
            );
            finish_mock_server(server_task).await;
        }
    }

    #[cfg(feature = "tls")]
    #[tokio::test]
    async fn explicit_rustls_connect_path_applies_inbound_size_policy() {
        let (url, server_task) = start_mock_server(|mut ws| async move {
            ws.send(Message::Binary(vec![0xC6; 257].into()))
                .await
                .expect("server must send the oversized message");
        })
        .await;
        install_tls_provider();
        let tls_config = Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(rustls::RootCertStore::empty())
                .with_no_client_auth(),
        );
        let options = WebSocketConnectOptions::new().with_max_inbound_message_size(Some(256));
        let mut transport = WebSocketTransport::connect_with_tls_config(&url, options, tls_config)
            .await
            .expect("explicit rustls connect path must support a plain ws endpoint");

        let live_config = transport
            .state
            .stream
            .as_ref()
            .expect("connected transport must retain its WebSocket stream")
            .get_config();
        assert_eq!(live_config.max_frame_size, Some(256));
        assert_eq!(live_config.max_message_size, Some(256));
        assert!(matches!(
            crate::transport::recv_frame(&mut transport).await,
            Some(Err(SignalFishError::TransportReceive(_)))
        ));
        finish_mock_server(server_task).await;
    }

    #[tokio::test]
    async fn oversized_inbound_frame_is_reported_once_and_fuses_transport() {
        let (url, server_task) = start_mock_server(|mut ws| async move {
            ws.send(Message::Binary(vec![0xC8; 257].into()))
                .await
                .expect("server must send the oversized message");
        })
        .await;
        let options = WebSocketConnectOptions::new().with_max_inbound_message_size(Some(256));
        let mut transport = WebSocketTransport::connect_with_options(&url, options)
            .await
            .expect("bounded WebSocket must connect");

        assert!(matches!(
            crate::transport::recv_frame(&mut transport).await,
            Some(Err(SignalFishError::TransportReceive(_)))
        ));
        assert!(transport.state.closed);
        assert!(transport.state.stream.is_none());
        assert!(crate::transport::recv_frame(&mut transport).await.is_none());

        let expected = TransportFrame::Text("caller-retains-oversize-retry".into());
        let mut offered = Some(expected.clone());
        let send_result = std::future::poll_fn(|cx| transport.poll_send(cx, &mut offered)).await;
        assert!(matches!(send_result, Err(SignalFishError::TransportClosed)));
        assert_eq!(offered, Some(expected));
        crate::transport::close_transport(&mut transport)
            .await
            .expect("close after size rejection must be idempotent");
        crate::transport::close_transport(&mut transport)
            .await
            .expect("repeated close after size rejection must remain idempotent");
        finish_mock_server(server_task).await;
    }

    #[tokio::test]
    async fn inbound_fragmented_limit_is_inclusive_and_rejects_boundary_plus_one() {
        use tokio_tungstenite::tungstenite::protocol::frame::coding::{Data, OpCode};
        use tokio_tungstenite::tungstenite::protocol::frame::Frame;

        for (final_fragment_size, accepted) in [(128, true), (129, false)] {
            let (url, server_task) = start_mock_server(move |mut ws| async move {
                ws.send(Message::Frame(Frame::message(
                    vec![0xC9; 128],
                    OpCode::Data(Data::Binary),
                    false,
                )))
                .await
                .expect("server must send the first fragment");
                ws.send(Message::Frame(Frame::message(
                    vec![0xCA; final_fragment_size],
                    OpCode::Data(Data::Continue),
                    true,
                )))
                .await
                .expect("server must send the final fragment");
            })
            .await;
            let options = WebSocketConnectOptions::new().with_max_inbound_message_size(Some(256));
            let mut transport = WebSocketTransport::connect_with_options(&url, options)
                .await
                .expect("bounded WebSocket must connect");

            let received = crate::transport::recv_frame(&mut transport).await;
            if accepted {
                let mut expected = vec![0xC9; 128];
                expected.extend(vec![0xCA; final_fragment_size]);
                assert_eq!(
                    expect_received_frame(received),
                    TransportFrame::Binary(expected),
                    "an assembled message exactly at the limit must be accepted"
                );
            } else {
                assert!(matches!(
                    received,
                    Some(Err(SignalFishError::TransportReceive(_)))
                ));
                assert!(crate::transport::recv_frame(&mut transport).await.is_none());
            }
            finish_mock_server(server_task).await;
        }
    }

    #[tokio::test]
    async fn socket_receive_error_is_reported_once_then_transport_is_terminal() {
        let mut transport = connect_to_reset_peer().await;
        let first = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            crate::transport::recv_frame(&mut transport),
        )
        .await
        .expect("reset socket must wake the receive poll");
        assert!(matches!(
            first,
            Some(Err(SignalFishError::TransportReceive(_)))
        ));
        assert!(transport.state.closed);
        assert!(transport.state.stream.is_none());
        assert!(!transport.state.send_started);
        assert!(!transport.state.control_flush_pending);
        assert!(!transport.state.peer_close_pending);

        assert!(crate::transport::recv_frame(&mut transport).await.is_none());
        let expected = TransportFrame::Text("must-remain-caller-owned".into());
        let mut offered = Some(expected.clone());
        let send_result = std::future::poll_fn(|cx| transport.poll_send(cx, &mut offered)).await;
        assert!(matches!(send_result, Err(SignalFishError::TransportClosed)));
        assert_eq!(offered, Some(expected));
        crate::transport::close_transport(&mut transport)
            .await
            .expect("close after receive failure must be idempotent");
        crate::transport::close_transport(&mut transport)
            .await
            .expect("repeated close after receive failure must remain idempotent");
    }

    #[tokio::test]
    async fn socket_send_error_rejects_later_sends_and_receive_fuses_transport() {
        let mut transport = connect_to_reset_peer().await;
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            plain_tcp_stream(&transport).readable(),
        )
        .await
        .expect("reset socket must become readable")
        .expect("querying reset socket readiness must succeed");

        let first = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            crate::transport::send_frame(&mut transport, TransportFrame::Binary(vec![0xC3; 1024])),
        )
        .await
        .expect("reset socket must wake the send poll");
        assert!(matches!(first, Err(SignalFishError::TransportSend(_))));
        assert!(transport.state.send_failed);
        assert!(!transport.state.closed);
        assert!(transport.state.stream.is_some());

        let expected = TransportFrame::Text("second".into());
        let mut offered = Some(expected.clone());
        let second = std::future::poll_fn(|cx| transport.poll_send(cx, &mut offered)).await;
        assert!(matches!(second, Err(SignalFishError::TransportClosed)));
        assert_eq!(offered, Some(expected));
        let _ = crate::transport::recv_frame(&mut transport).await;
        assert!(transport.state.closed);
        assert!(transport.state.stream.is_none());
        crate::transport::close_transport(&mut transport)
            .await
            .expect("close after send failure must be idempotent");
    }

    #[tokio::test]
    async fn send_after_close_returns_transport_closed() {
        let (url, server_task) = start_mock_server(|mut ws| async move {
            // Read until the client closes.
            while let Some(Ok(_)) = ws.next().await {}
        })
        .await;

        let mut transport = WebSocketTransport::connect(&url)
            .await
            .expect("WebSocket connect must succeed");
        crate::transport::close_transport(&mut transport)
            .await
            .expect("close must succeed");

        let err = crate::transport::send_frame(&mut transport, TransportFrame::Text("oops".into()))
            .await
            .unwrap_err();
        assert!(matches!(err, SignalFishError::TransportClosed));
        finish_mock_server(server_task).await;
    }

    #[tokio::test]
    async fn double_close_is_idempotent() {
        let (url, server_task) =
            start_mock_server(|mut ws| async move { while let Some(Ok(_)) = ws.next().await {} })
                .await;

        let mut transport = WebSocketTransport::connect(&url)
            .await
            .expect("WebSocket connect must succeed");
        crate::transport::close_transport(&mut transport)
            .await
            .expect("first close must succeed");
        // Second close should also succeed.
        crate::transport::close_transport(&mut transport)
            .await
            .expect("second close must succeed (idempotent)");
        finish_mock_server(server_task).await;
    }

    #[tokio::test]
    async fn abort_drops_the_socket_without_waiting_for_a_close_handshake() {
        let (disconnected_tx, disconnected_rx) = tokio::sync::oneshot::channel();
        let (url, server_task) = start_mock_server(move |mut ws| async move {
            let disconnected = matches!(
                tokio::time::timeout(std::time::Duration::from_secs(1), ws.next()).await,
                Ok(None | Some(Err(_)))
            );
            let _ = disconnected_tx.send(disconnected);
        })
        .await;

        let mut transport = WebSocketTransport::connect(&url)
            .await
            .expect("WebSocket connect must succeed");
        transport.abort();
        transport.abort();

        assert!(transport.state.closed);
        assert!(transport.state.stream.is_none());
        let expected = TransportFrame::Text("caller still owns this".into());
        let mut offered = Some(expected.clone());
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        assert!(matches!(
            transport.poll_send(&mut cx, &mut offered),
            Poll::Ready(Err(SignalFishError::TransportClosed))
        ));
        assert_eq!(offered, Some(expected));
        assert!(matches!(transport.poll_recv(&mut cx), Poll::Ready(None)));
        assert!(matches!(transport.poll_close(&mut cx), Poll::Ready(Ok(()))));
        assert!(
            disconnected_rx
                .await
                .expect("server task must report the disconnect"),
            "dropping the client stream must release the server connection"
        );
        finish_mock_server(server_task).await;
    }

    #[tokio::test]
    async fn connect_with_timeout_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("TcpListener must bind to localhost");
        let addr = listener
            .local_addr()
            .expect("TcpListener must have a local address");
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let server_task = tokio::spawn(async move {
            let (_tcp, _) = listener
                .accept()
                .await
                .expect("timeout server must accept the TCP connection");
            let _ = accepted_tx.send(());
            std::future::pending::<()>().await;
        });

        let result = WebSocketTransport::connect_with_timeout(
            &format!("ws://{addr}"),
            std::time::Duration::from_millis(50),
        )
        .await;

        let err = result.unwrap_err();
        assert!(matches!(err, SignalFishError::Timeout));
        tokio::time::timeout(std::time::Duration::from_secs(1), accepted_rx)
            .await
            .expect("timeout server must accept promptly")
            .expect("timeout server must report acceptance");
        server_task.abort();
        assert!(matches!(
            server_task.await,
            Err(error) if error.is_cancelled()
        ));
    }

    #[tokio::test]
    async fn from_stream_constructor_works() {
        let (url, server_task) = start_mock_server(|mut ws| async move {
            ws.send(Message::Text("from_stream_msg".into()))
                .await
                .expect("server must send 'from_stream_msg'");
            ws.close(None).await.expect("server must close cleanly");
        })
        .await;

        // Connect the raw stream ourselves, then wrap it.
        let (ws_stream, _) = tokio_tungstenite::connect_async(&url)
            .await
            .expect("raw WebSocket connect must succeed");
        let mut transport = WebSocketTransport::from_stream(ws_stream);

        let msg = crate::transport::recv_frame(&mut transport)
            .await
            .expect("recv must return Some")
            .expect("recv must return Ok");
        assert_eq!(msg, TransportFrame::Text("from_stream_msg".into()));
        drop(transport);
        finish_mock_server(server_task).await;
    }

    #[tokio::test]
    async fn from_stream_preserves_the_callers_codec_limit() {
        let (url, server_task) = start_mock_server(|mut ws| async move {
            ws.send(Message::Binary(vec![0xCB; 129].into()))
                .await
                .expect("server must send the caller-oversized message");
        })
        .await;
        let caller_config = WebSocketConfig::default()
            .max_frame_size(Some(128))
            .max_message_size(Some(128));
        let (ws_stream, _) =
            tokio_tungstenite::connect_async_with_config(&url, Some(caller_config), false)
                .await
                .expect("raw WebSocket connect must succeed");
        let mut transport = WebSocketTransport::from_stream(ws_stream);

        assert!(matches!(
            crate::transport::recv_frame(&mut transport).await,
            Some(Err(SignalFishError::TransportReceive(_)))
        ));
        finish_mock_server(server_task).await;
    }

    #[tokio::test]
    async fn text_and_binary_send_round_trip_in_fifo_order() {
        let (url, server_task) = start_mock_server(|mut ws| async move {
            for _ in 0..2 {
                let message = ws
                    .next()
                    .await
                    .expect("server must receive each application message")
                    .expect("application message must be valid");
                ws.send(message)
                    .await
                    .expect("server must echo each application message");
            }
        })
        .await;

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            let mut transport = WebSocketTransport::connect(&url)
                .await
                .expect("WebSocket connect must succeed");
            for expected in [
                TransportFrame::Text("ping_echo".into()),
                TransportFrame::Binary(vec![0x00, 0x7F, 0xFF]),
            ] {
                crate::transport::send_frame(&mut transport, expected.clone())
                    .await
                    .expect("send must succeed");
                let received = crate::transport::recv_frame(&mut transport).await;
                assert_eq!(expect_received_frame(received), expected);
            }
        })
        .await
        .expect("text/binary round trip must complete promptly");
        finish_mock_server(server_task).await;
    }

    #[tokio::test]
    async fn recv_after_local_close_returns_exact_terminal_none() {
        let (url, server_task) =
            start_mock_server(|mut ws| async move { while let Some(Ok(_)) = ws.next().await {} })
                .await;

        let mut transport = WebSocketTransport::connect(&url)
            .await
            .expect("WebSocket connect must succeed");
        crate::transport::close_transport(&mut transport)
            .await
            .expect("close must succeed");

        assert!(crate::transport::recv_frame(&mut transport).await.is_none());
        finish_mock_server(server_task).await;
    }
}
