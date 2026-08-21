# Session 035 — Native WebSocket Token Binding v2

## Scope and Priority

The hosted audit found current `main` clean at `2a18bbc`, no open/draft PRs or
dependency updates, and open issues #117, #110, #90, #88, #80, and #82. Issue
#117 was an unscoped safety umbrella and was triaged against the already-shipped
Deep Safety program; it is now closed completed with the concrete Miri, fuzz,
mutation, and unsafe/FFI evidence. Issue #88 was therefore the highest-impact
implementation-ready correctness/interoperability item.

Issue #90 remains a separate maintainer-administration blocker. PR #118 merged
into current main with no approval and a quota-exhausted Copilot comment despite
green code-tree workflows, while the latest observable Repository Policy run is
red. This client change cannot repair or prove the live ruleset, approval,
reviewer, or quota state.

## Pinned Contract

All token-binding behavior is pinned to Signal Fish Server 0.7.0 commit
`3f7f43d4cd4b3cc7f8fb893220dc35c9b1fad333`. The server already contains merged
replay-resistant token binding and has no open token-binding dependency.

- WebSocket subprotocol: `signalfish.tokenbinding.v2`.
- The first application frame is a strict `TokenBindingChallenge`.
- HKDF-SHA-256 uses the decoded 16-byte `Sec-WebSocket-Key` as IKM, the decoded
  32-byte server nonce as salt, and
  `signalfish.tokenbinding.v2/session-key` as info.
- HMAC-SHA-256 covers the JSON/binary domain, one shared u64 big-endian
  sequence, and canonical JSON or the exact raw binary payload.
- JSON is duplicate-free, safe-integer-only, RFC8785-subset canonical JSON with
  UTF-16 object-key ordering. Binary uses the named-field MessagePack envelope.
- Required mode is valid only on a TLS-enabled Server 0.7 deployment.

`tests/token-binding/PROVENANCE.toml` records exact upstream source hashes and
binds the checked-in JSON/binary vectors to a checksum.

## Architecture and Public Surface

Token binding stays in native `WebSocketTransport`; it is not a protocol event,
`SignalFishConfig` capability, or `ClientCore` concern. The opt-in
`token-binding` Cargo feature keeps the default dependency graph crypto-free.

`WebSocketConnectOptions` exposes disabled, optional, and required modes plus a
challenge deadline. Disabled takes the exact old connection path. Optional
retries once without the offer only after tungstenite reports the precise
successful-upgrade/no-subprotocol condition; HTTP, TLS, network, unexpected
selection, and malformed challenge failures never downgrade. Required fails
with typed static `TokenBindingFailure` reasons.

The selected challenge is consumed before a ready transport can reach either
client driver, preventing `Authenticate` from racing setup. A safe status and
explicit challenge accessor are available; ambient `Debug`, tracing, and
errors exclude keys, nonces, proofs, signatures, fingerprints, URL credentials,
wrapped frames, and TLS client configuration. Handshake key material lives only
through derivation; terminal close/error/abort immediately drops the challenge
and zeroizes the derived key, with drop as the final guard.

`connect_with_tls_config` supports private roots and mTLS, but server profiles
with `require_client_fingerprint=true` remain unsupported because the SDK does
not add the leaf fingerprint to the proof/HMAC. Browser, Emscripten, Godot `WebSocketPeer`,
and post-handshake `from_stream` paths cannot expose the exact client handshake
key and therefore cannot support required mode. Both async and polling clients
can use a connected native `WebSocketTransport`.

## Ownership and Sequence Invariant

For an active session, `poll_send` waits for backend readiness, prepares a
protected frame from `frame.as_ref()`, and calls `start_send` before taking the
caller's original or committing the sequence. Preparation failure, `Pending`,
and `WriteBufferFull` leave the byte-exact original and sequence unchanged. A
rejected protected envelope is never restored into the caller slot, preventing
double wrapping. JSON and binary frames share the same sequence; exhaustion
fails closed.

## Evidence

- Exact default handshake test proves no subprotocol offer and unchanged raw
  application bytes.
- Mock handshakes cover selected/challenge-first ordering, optional fallback,
  required absence/malformed challenge, challenge timeout, and no HTTP retry.
- Ownership tests cover unsupported JSON and backend buffer refusal without
  frame mutation or sequence consumption.
- Server-derived goldens cover canonical JSON, full named-field MessagePack,
  shared JSON/binary sequence, forbidden numeric/duplicate inputs, challenge
  shape, and static redaction canaries.
- A required-WSS ignored E2E against a native build of the exact pinned Server
  0.7 commit passes authentication, room join, JSON relay, physical binary
  relay, and a final Pong. The CI matrix runs the same test against the
  checksummed published x86_64 Server 0.7 artifact.
- A second pinned raw-WSS E2E proves replay, payload/signature tamper,
  cross-connection wrong-key reuse, and malformed JSON and binary envelopes are
  rejected. Required downgrade and optional fallback boundaries remain covered
  by deterministic mock handshakes.
- Feature-off checks compile the core and native WebSocket transport without
  `base64`, `hkdf`, `hmac`, `sha2`, or `zeroize` in the normal dependency tree.

Local verification completed across the implementation and adversarial review
loop:

```text
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --all-features token_binding
cargo test --all-features optional_mode
cargo test --all-features selected_mode
cargo test --all-features --test token_binding_conformance_tests
cargo doc --workspace --all-features --no-deps
cargo check --no-default-features
cargo check --no-default-features --features transport-websocket
cargo clippy --no-default-features --features token-binding --all-targets -- -D warnings
cargo test --no-default-features --features token-binding
cargo +1.87.0 check -p signal-fish-client --all-features --lib
cargo test --all-features --test ci_config_tests
python3 -m unittest scripts.test_release
MKDOCS=/tmp/signal-fish-docs-venv/bin/mkdocs bash scripts/check-docs-rendering.sh
bash scripts/extract-rust-snippets.sh
SIGNAL_FISH_SERVER_BIN=/tmp/signal-fish-server-pinned/target/release/signal-fish-server \
  cargo test --all-features --test real_server_e2e \
  e2e_server_070_required_token_binding_wss -- --ignored --exact --test-threads=1
SIGNAL_FISH_SERVER_BIN=/tmp/signal-fish-server-pinned/target/release/signal-fish-server \
  cargo test --all-features --test real_server_e2e \
  e2e_server_070_rejects_invalid_token_binding_proofs -- --ignored --exact --test-threads=1
cargo fmt && cargo clippy --workspace --all-targets --all-features -- -D warnings && \
  cargo test --workspace --all-features
```

The exact mandatory repository gate passes with zero warnings on the final
local diff. Hosted PR checks/reviews and issue closure remain pending until the
branch head is published.

## Adversarial Review

Two independent read-only reviewers audited the implementation and acceptance
matrix. Pass 1 found and drove fixes for:

- feature-combination imports and missing token-binding-without-TLS CI rows;
- missing pinned replay/tamper/wrong-key/malformed-envelope negative evidence;
- a false mTLS fingerprint capability claim plus incomplete threat, replay, and
  key-lifetime documentation;
- derived key/challenge retention after terminal close/error/abort;
- structurally weak provenance checks and partially unused golden fields;
- incomplete public API changelog and Rustdoc error contracts;
- omitted release-preflight provenance participation;
- stale CI matrix policy tests/wording and an inaccurate custom-TLS secure log;
- canonical context line/URL policy regressions.

Every finding was fixed with executable coverage. Terminalization now drops the
challenge and zeroizes the active session while preserving historical `Active`
status, and the test pins that behavior. The exact provenance paths/hashes and
every golden field are consumed by tests. Pass 2 reached zero remaining concrete
findings.
