# Authentication & Credentials

One page for everything the SDK authenticates with: what is public, what is a
secret, how secrets rotate, and how to keep them out of logs. The pieces live
in several places; this page ties them together.

## What is public: the app ID

The `app_id` is a public application label, not a secret. It is the only
required field of [`SignalFishConfig`](client.md#signalfishconfig) and the
first field of the `Authenticate` message the client sends after connecting.
The server uses it to route the connection to your application; it does not
authenticate a tenant.

```rust
let config = SignalFishConfig::new("mb_app_abc123");
```

Choose the label your server operator allows. Never place a secret credential
into `app_id` (or into the WebSocket URL) to "smuggle" authentication — the
value travels in plaintext JSON and can surface in server-side logs, proxies,
and browser dev tools.

## What is secret

### Tenant connect tokens (optional, hosted deployments)

Hosted Signal Fish deployments can require a **tenant connect token**: a
short-lived credential minted by the deployment's control plane as
`sfct_v1.<base64url(payload)>.<base64url(signature)>` and signed with the
deployment's Ed25519 public key. The server verifies the token during
authentication and never logs or echoes it. Self-hosted deployments without
a verification key keep the public-`app_id` handshake and refuse any
presented token, so only set one when your deployment documents tenant
verification.

```rust
let config = SignalFishConfig::new("mb_app_abc123")
    .with_connect_token(token_from_your_control_plane);
```

- The SDK sends the exact string on `Authenticate` — including each
  reconnection round's fresh handshake — and never inspects its contents.
- Omitting it (the default) omits the wire field entirely; the handshake
  bytes are identical to previous SDK releases.
- It is a secret: SDK `Debug` impls and tracing report presence and byte
  length only.
- Tokens are short-lived by design (an upstream five-minute TTL plus a
  60-second clock-skew allowance — server PR #575's verification
  constants). A failed verification surfaces as the `ConnectTokenInvalid`
  error code; since the configured token is fixed for a client's lifetime,
  recovery means issuing a fresh token and starting a new client.

### Reconnection tokens

Protocol v3 rooms issue a reconnection token in `RoomJoined` and rotate it on
every successful `Reconnected`. The token restores your seat after an
unexpected disconnect, so it is a **connection secret**:

- Read it from `client.snapshot().reconnection_token` and persist it with the
  matching `player_id` and `room_id`.
- Never log it. Every SDK `Debug`/tracing path redacts it — your code must do
  the same.
- After `Reconnected`, persist the **rotated** replacement from the fresh
  snapshot; the old token is void.

The full recovery procedure — persist, fresh transport, `reconnect(...)`,
adopt the replay and new plan, fall back by error code — is in
[the `reconnect` section](client.md#reconnect).

### Token-binding and TLS key material

The opt-in `token-binding` feature proves the identity of one physical
WebSocket handshake; the `tls` feature adds `wss://` encryption, and
certificate-capable rustls connections can bind proofs to an mTLS client
certificate fingerprint. None of this key material is reachable through the
SDK: the handshake key is zeroized after derivation, and token-binding
failures carry status details only. Two boundaries to respect:

- Token binding authenticates the client to the server **only on `wss://`**.
  On plain `ws://` an on-path observer sees the handshake key and nonce and
  can forge proofs.
- The proof is not confidentiality. Application payloads are as private as
  your transport (TLS) makes them.

See [WebSocket Token Binding](token-binding.md) for the negotiation modes and
the mTLS fingerprint profile.

## Rotation guidance

| Secret | Issued | Rotate | On failure |
|---|---|---|---|
| Reconnection token | `RoomJoined` | Every `Reconnected` — persist the replacement | `ReconnectionExpired` / `ReconnectionTokenInvalid`: fall back to a normal `join_room` |
| Tenant connect token | Deployment control plane | Before expiry (upstream TTL: 5 minutes + 60-second skew) | `ConnectTokenInvalid`: issue a fresh token and start a new client |
| TLS session | Connect | Every new physical connection (fresh handshake, fresh proofs) | Reconnect with a fresh transport |
| App ID | Deployment | N/A — it is a public label | The server rejects unknown labels at authentication |

## Keeping secrets out of logs

The SDK treats ambient logs as a redaction boundary by design:

- SDK-internal `Debug` impls for events, snapshots, protocol messages, close
  reasons, and transport frames print variants, flags, and byte lengths —
  never payload text, room codes, tokens, or credentials. (Payload structs
  such as player names format their tested non-secret fields; reconnect
  tokens and key material are redacted everywhere.)
- Undecodable inbound frames are attacker-influencable, so their
  [`DecodeFailed`](events.md#decodefailed) diagnostics are handled with care:
  the serde error text is capped at 512 bytes, `raw_prefix`'s `Debug` redacts
  it entirely, and the `redacted_raw_prefix()` helper returns a
  content-masked view that keeps the frame's shape. Never log `raw_prefix`
  verbatim.

Follow the same rules in your own code: persist secrets encrypted or in a
keystore, and keep them out of logs, URLs, and error messages.

## Cloud credentials (status)

Signal Fish Server 0.8 authenticates connections with the public app ID
alone. Upstream has since ratified the optional tenant connect-token wire
contract (server PR #575): hosted deployments that enable verification
accept a control-plane-minted `sfct_v1.` token, and this SDK presents it
through
[`with_connect_token`](client.md#signalfishconfig). Legacy secret-form
application keys (`sfk_…`) remain unsupported: do not pass them as the
`app_id`, and never embed any credential in the URL. Credential-shaped
literals are kept out of this repository's examples and docs by a
repository hygiene guard.
