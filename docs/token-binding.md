# WebSocket Token Binding

Signal Fish Server 0.7 can require the negotiated
`signalfish.tokenbinding.v2` WebSocket subprotocol. The built-in native
`WebSocketTransport` supports it behind the opt-in `token-binding` Cargo
feature.

```toml
[dependencies]
signal-fish-client = { git = "https://github.com/Ambiguous-Interactive/signal-fish-client-rust", features = ["token-binding", "tls"] }
```

`token-binding` is not a default feature. Ordinary builds therefore keep the
previous dependency graph, WebSocket handshake, and `Authenticate` bytes.
Server deployments that require token binding also require TLS; enable `tls`
and use `wss://` for that profile.

## Connecting

Choose the negotiation policy on the transport, before constructing either
client driver:

```rust,no_run
use signal_fish_client::{
    TokenBindingMode, TokenBindingStatus, WebSocketConnectOptions,
    WebSocketTransport,
};

let options = WebSocketConnectOptions::new()
    .with_token_binding(TokenBindingMode::Required);
let transport = WebSocketTransport::connect_with_options(
    "wss://signal.example/v2/ws",
    options,
).await?;
assert_eq!(transport.token_binding_status(), TokenBindingStatus::Active);
```

| Mode | Behavior |
|---|---|
| `Disabled` | Default. Does not offer a subprotocol and preserves the old connection path exactly. |
| `Optional` | Offers v2. If the server completes the upgrade without selecting a subprotocol, reconnects once without the offer and reports `NotNegotiated`. It never retries an HTTP rejection, malformed challenge, unexpected selection, network error, or TLS failure. |
| `Required` | Fails with a typed `SignalFishError::TokenBinding` unless the server selects v2 and sends a valid first-message challenge before the configured deadline. |

Optional mode permits an explicit downgrade when the server accepts an
unsigned upgrade. TLS authenticates the handshake and prevents an on-path
party from stripping the server's selection. Use `Required` when downgrade is
not acceptable.

## Threat model and lifetime

The token-binding subprotocol authenticates the order and contents of client-to-server
application frames to the one physical WebSocket handshake. It does not protect
server-to-client frames and is not a substitute for TLS server authentication
or confidentiality. On plain `ws://`, an on-path observer can read both
`Sec-WebSocket-Key` and the server nonce, derive the same session key, and forge
proofs. Use `wss://`; Server 0.7 enforces TLS when token binding is required.

The server nonce, derived key, and sequence space are fresh for every physical
connection and are never reused across reconnects. Replayed or reordered
sequence numbers, payload/signature tampering, proofs made with another
connection's key, and malformed JSON/MessagePack proof envelopes fail closed.
The encoded and decoded client handshake key live only through challenge
validation and derivation. The derived key and validated challenge remain only
while the physical transport is live; terminal close/error/abort clears them
immediately, and their zeroizing storage is also a final drop guard.

`token_binding_status()` safely reports the result. The validated challenge is
available through `token_binding_challenge()` when an application has a
specific reason to inspect it; its `Debug` output redacts the nonce.

For private roots or mutual TLS, enable `tls` and call
`WebSocketTransport::connect_with_tls_config` with an
`Arc<rustls::ClientConfig>`. When token binding is offered, the transport
delegates to that configuration's resolver. If rustls chooses a compatible
X.509 client signer, active proofs bind to the exact selected leaf certificate.
The transport hashes the leaf's DER bytes with SHA-256, sends the lowercase
hexadecimal fingerprint, and signs those same ASCII bytes. It does not treat an
RFC 7250 raw public key as a certificate, and there is no caller-supplied claim.

The transport clones the custom configuration and disables TLS resumption on
that clone so every offered physical connection performs a certificate
selection that the proof signer can observe. In `Optional` mode this also
applies to the unsigned fallback connection after the server omits the
subprotocol. The caller's configuration and resumption cache are not mutated.
Server 0.7 profiles with `require_client_fingerprint=true` additionally require
built-in WSS, a trusted client CA, mTLS client authentication, and required
token binding.

## Wire and failure contract

The transport consumes the challenge before it can be passed to
`SignalFishClient` or `SignalFishPollingClient`, preventing `Authenticate` from
racing the handshake. It derives the per-connection key from the exact RFC 6455
`Sec-WebSocket-Key` and the server nonce, then protects every outbound JSON and
binary application frame. Text and binary frames share one strictly increasing
sequence. A frame consumes its sequence only when the WebSocket backend accepts
ownership; preparation failures, `Pending`, and `WriteBufferFull` leave both
the original caller frame and sequence unchanged.

The implementation follows Signal Fish Server 0.7.0 commit
`3f7f43d4cd4b3cc7f8fb893220dc35c9b1fad333`. Checked-in provenance and exact
JSON/MessagePack goldens live under `tests/token-binding/`. Unsupported JSON
forms—duplicate object members, floats/exponents/negative zero, and integers
outside JavaScript's safe range—fail closed with a typed, non-secret reason.

Handshake keys, derived keys, certificate fingerprints, proofs, signatures,
URL credentials, and wrapped payloads are not included in token-binding
`Debug`, tracing, or error text.

## Transport and platform capability

Token binding belongs to the physical WebSocket connection, not the Signal
Fish protocol core:

- Both the Tokio background client and `SignalFishPollingClient` work when
  given a connected native `WebSocketTransport`.
- `WebSocketTransport::from_stream` cannot enable token binding because the
  completed handshake no longer exposes the exact client key. A custom
  transport must own subprotocol negotiation, challenge consumption, proof
  wrapping, shared sequence state, and key zeroization itself.
- Browser WebSocket APIs do not expose `Sec-WebSocket-Key`. Consequently the
  browser, Emscripten, and Godot `WebSocketPeer` transports cannot support
  required token binding. Use a server profile where token binding is not
  required, or terminate the connection through a capable trusted native
  component.
