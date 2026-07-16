---
name: v3-delivery-accountability
description: Maintain protocol v3 delivery accountability and replay protection. Use when changing delivery classes, epochs, sequences, gap validation, reconnect baselines, quarantine, or violation events.
---

# Protocol v3 Delivery Accountability

Reference for Signal Fish Server 0.4.0 delivery classes, sequence validation,
reconnect baselines, and client violation policy.

## Negotiation and Compatibility

- `SignalFishConfig::new` remains the byte-identical v2 relay floor.
- `enable_v3()` advertises v3 relay/accountability without WebRTC.
- `enable_mesh()` calls `enable_v3()` and also advertises mesh/host/WebRTC.
- Existing `send_game_data` and `GameDataDelivery::Reliable` remain valid before
  v3 negotiation and omit `class`/`key` for v2 compatibility.
- `Latest { key }`, `Volatile`, and physical binary sends require negotiated
  protocol version `>= 3` and fail locally with `ProtocolUnsupported` otherwise.

## Wire Model

Every v3 relay payload carries a non-zero sender `epoch` and monotonically
increasing `seq`. JSON additionally echoes `DeliveryClass` and the required key
for `latest`. Binary delivery uses the strict physical MessagePack envelope in
`src/protocol/binary.rs`; it is always protocol-reliable.

Intentional lossy-class omissions are authorized only by the union of causally
prior exact `DeliveryReport.gaps` ranges for the same sender and epoch. Report
counters and `RelayStats` are cumulative diagnostics, never gap authorization.
One report carries at most `DELIVERY_REPORT_MAX_GAPS` (256) ranges.

## State Machine

`src/accountability.rs` is ported from the server v0.4.0 native reference client
at commit `50b28a9a13dc2b99d301bfb2482c5fd6f768a2e8`. Preserve that provenance
when changing it. The validator covers:

- exact v2/v3 snapshot metadata shapes and duplicate members;
- announced lifecycle epochs and same-epoch idempotence;
- exact prior gap coverage and monotonic per-class counters;
- `PlayerLeft.final_seq` retirement and lifecycle-overtaken stale tails;
- reconnect snapshot/watermark equality;
- immediate unsupported-format error/report causality;
- positive, stable, cumulative relay statistics.

Validate decoded text and binary messages through the same path before state or
event application. Stale lifecycle-overtaken data is valid but suppressed.
Physical decode failures are `DecodeFailed`; semantic accountability failures
are `ProtocolViolation` and must never be conflated.

## Violation Policies

- `Quarantine` (default): emit `ProtocolViolation`, suppress subsequent room
  game data, and expose `ClientSnapshot.quarantined = true`.
- `Disconnect`: emit the violation, observe terminal accountability state, and
  close the signaling connection.
- `Observe`: emit the violation and still deliver the offending decoded
  message, leaving recovery to the application.

Quarantine resets only on a new physical client state or an authoritative
`RoomJoined`, `SpectatorJoined`, or `Reconnected` baseline.

## Reconnect Tokens

V3 `RoomJoined` issues a secret token. `Reconnected` consumes it, provides
authoritative `sender_watermarks`, reports replay completeness, and rotates the
token. Both clients expose the latest token only through coherent
`ClientSnapshot`; never log it. The ignored real-server test must capture the
first snapshot token, reconnect unexpectedly, assert success, and assert token
rotation against the pinned server binary.

## Required Tests

- Preserve the server reference trace corpus and seeded counterexamples.
- Test all three policies with the same trace and assert event, connectivity,
  delivered data, and snapshot quarantine state.
- Exercise strict binary malformed/truncated/duplicate/trailing/zero-stamp cases.
- Keep async and polling text/binary policy behavior in parity.
- Run the mandatory workflow and the pinned server v0.4.0 E2E before merge.
