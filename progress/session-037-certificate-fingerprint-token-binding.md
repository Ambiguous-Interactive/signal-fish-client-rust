# Session 037 — Certificate-Fingerprint Token Binding

## Priority and Scope

The hosted audit found no open pull requests and four open issues: #90, #120,
#82, and #80. All 11 required aggregates are green on `main` commit `3f38367`.
The separate Repository Policy audit remains red because issue #90 requires
maintainer ruleset, reviewer, and Copilot-quota administration unavailable to
this session. Issue #120 is therefore the highest-impact actionable correctness
work: native clients could not use Signal Fish Server 0.7's strict mTLS plus
client-certificate-fingerprint token-binding profile.

## Pinned Contract

The implementation remains pinned to Signal Fish Server 0.7.0 commit
`3f7f43d4cd4b3cc7f8fb893220dc35c9b1fad333`. The server hashes the exact
authenticated leaf certificate DER with SHA-256, encodes 64 lowercase
hexadecimal characters, places that string in `token_binding.fingerprint`, and
appends those ASCII bytes after the domain, big-endian sequence, and payload in
the HMAC input. Provenance now includes the upstream TLS extraction source as
well as the existing verifier, WebSocket, documentation, and mTLS E2E sources.

## Client Design

The public API accepts no certificate fingerprint or independent certificate
claim. On an active token-binding connection created with
`WebSocketTransport::connect_with_tls_config`, a private resolver wrapper
delegates rustls's real `resolve` and signing-scheme selection calls, then
fingerprints the returned X.509 leaf only when rustls chooses a compatible
signer. This supports fixed and dynamic resolvers without guessing the server's
CA hints or signature schemes and excludes RFC 7250 raw public keys.

Rustls does not expose the client certificate used by a resumed connection.
The transport therefore clones custom configurations for token-binding offers,
installs the tracking resolver and signer, and disables resumption only on that
clone. This includes an unsigned fallback after Optional negotiation. The
caller's configuration and cache remain unchanged. Disabled token binding
retains the pre-existing connect path and byte/dependency compatibility.

The selected fingerprint is zeroized with session state, omitted from ambient
`Debug`, tracing, and errors, serialized in each JSON/MessagePack proof, and
signed using the same shared JSON/binary sequence. Backend refusal and proof
preparation failures preserve the original frame and sequence.

## Evidence

Checked-in deterministic vectors use the pinned server's `client-101` leaf
fingerprint and independently pinned JSON/binary MAC inputs and signatures.
Unit tests cover lowercase SHA-256 encoding, compatible signer selection,
dynamic `has_certs` behavior, raw-public-key exclusion, proof fields and HMAC
ordering, redaction, shared sequence behavior, and retry after
`WriteBufferFull`.

Three ignored tests run against the checksum-verified published Server 0.7.0
binary under required WSS, required mTLS, and
`require_client_fingerprint=true`:

- the Tokio background client authenticates, joins, sends protected JSON and
  binary data, then receives Pong;
- `SignalFishPollingClient` proves the same connected transport and mixed
  traffic path;
- raw adversarial clients prove rejection of missing and wrong fingerprints,
  exact proof replay on a fresh connection, payload tampering, and signature
  tampering.

All three passed locally against the published ARM64 Server 0.7.0 artifact.
The blocking CI matrix contains equivalent checksum-pinned x86_64 rows.
The adversarial test now requires the server's exact `UNAUTHORIZED` error and
case-specific message before accepting each rejection; EOF, Close, and socket
errors fail the test. The pre-existing non-fingerprint negative matrix passes
the same stronger assertion.

Two independent review loops covered correctness, security, and test/docs
behavior. The first found signer compatibility, dynamic-resolver,
raw-public-key, Optional-fallback documentation, rejection-evidence,
provenance, and shutdown-boundary gaps. Those were fixed. The second loop
reported zero remaining substantive findings. Comprehensive `ci-validate.sh`
passed all 12 available checks; optional typos, Markdownlint, and Docker checks
were unavailable locally. The exact mandatory fmt, Clippy, and all-feature test
gate is run once more immediately before commit.

## Documentation and Roadmap

The token-binding and transport guides now describe automatic leaf binding,
the fresh-handshake/resumption boundary, and the absence of a caller claim.
Browser, Emscripten, Godot, and `from_stream` remain explicitly incapable.
`CHANGELOG.md`, `.llm/context.md`, applicable transport skills, and `PLAN.md`
reflect the new supported profile and current hosted state.
