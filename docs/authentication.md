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
| TLS session | Connect | Every new physical connection (fresh handshake, fresh proofs) | Reconnect with a fresh transport |
| App ID | Deployment | N/A — it is a public label | The server rejects unknown labels at authentication |

## Keeping secrets out of logs

The SDK treats ambient logs as a redaction boundary by design:

- `Debug` impls for events, snapshots, protocol messages, close reasons, and
  transport frames print variants, flags, and byte lengths — never payload
  text, room codes, tokens, or credentials.
- The one deliberate carve-out is
  [`SignalFishEvent::DecodeFailed::raw_prefix`](events.md): undecodable frames
  are attacker-influencable, and a hostile server can plant
  credential-looking strings in them. Its `Debug` redacts the prefix
  entirely; when you need the frame's shape for diagnostics, prefer the
  `redacted_raw_prefix()` helper, which masks every string literal's content
  while preserving the JSON skeleton.

Follow the same rules in your own code: persist secrets encrypted or in a
keystore, and keep them out of logs, URLs, and error messages.

## Cloud credentials (status)

Signal Fish Server 0.8 authenticates connections with the public app ID
alone. If your deployment's control plane issues secret-form application keys
(`sfk_…`), the SDK has **no surface for them today**: do not pass them as the
`app_id`, and never embed them in the URL. A dedicated credentials API is
being designed with the upstream wire contract; it will be explicit,
redacted in all diagnostics, and announced in the
[changelog](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/blob/main/CHANGELOG.md).
