# Session 002 — Server 0.4.0 conformance

## Outcome

Advanced PR A from its 0.7.0 baseline to an offline-green server-0.4.0 client
implementation. The default authentication wire remains the v2 relay floor;
protocol v3 is explicit opt-in.

## Completed

- Refreshed and checksum-bound the server v0.4.0 JSONL/AsyncAPI artifacts at
  commit `50b28a9a13dc2b99d301bfb2482c5fd6f768a2e8` through one compatibility
  manifest.
- Added v3 delivery classes, reports/counters, lifecycle stamps, relay stats,
  graceful drain, reconnect replay/watermarks/tokens, and new error codes.
- Ported the server native reference accountability state machine and exposed
  quarantine/disconnect/observe policies with coherent client snapshots.
- Added classified JSON sends, strict binary MessagePack envelopes, physical
  binary transport frames, and matching async/polling behavior.
- Migrated the object-safe `Transport` boundary to polling text/binary frames,
  including structured close metadata, WebSocket Pong flushing, and multi-poll
  close/send ownership.
- Made relay session plans authoritative mesh resets and refreshed consumer and
  LLM documentation, changelog entries, fuzz coverage, and reconnect E2E intent.

## Verification

- `cargo fmt`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-features`

All passed after the implementation pass. The pinned live-server ignored E2E,
including rotating-token reconnect, sender Ping/Pong under flood, and semantic
slow-consumer eviction, also passed against the exact server v0.4.0 binary.

## Adversarial hardening

- Made room/reconnect baselines transactional and malformed frames consume
  unsupported-format adjacency obligations.
- Corrected `Observe`, `Quarantine`, and `Disconnect` behavior, including
  physical transport close and authoritative rebaseline semantics.
- Enforced negotiated JSON/binary frame parity and binary-send guards, added
  strict v2/v3 physical decoders, and rejected explicit-null negotiation.
- Redacted reconnect credentials from event/snapshot debug output and cleared
  tokens/quarantine on room and spectator exit.
- Flushed peer WebSocket Close responses and corrected Emscripten C `bool`
  layout plus zero-length callback pointer handling.
- Final mandatory workflow and all applicable phases of `scripts/check-all.sh`
  pass; pinned server 0.4.0 E2E remains 3/3 green.
- Repeated adversarial review until the final pass reported zero actionable
  blockers.

CI and external bot reviews remain before PR A is complete.
