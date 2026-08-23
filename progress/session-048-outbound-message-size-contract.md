# Session 048 — Negotiated Outbound Message-Size Contract

## Priority and Audit

The session began from clean `main` at
`27c31480dca9f195a8533fd54571198a108c0f5c`, the merge of PR #138. Hosted state
showed no open or draft pull requests, all 12 push workflows green on `main`,
and two open issues: #90 (maintainer-administration governance blocker) and
#126 (the correctness/performance audit milestone).

Upstream research found that [signal-fish-server#399](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/399)
— the negotiated/enforced outbound message-size contract that PLAN.md and
`docs/transport.md` explicitly deferred to — had landed via server PR #401
(merged 2026-08-22) together with PR #404's liveness change. Because
`.github/workflows/protocol-sync.yml` compares vendored evidence against
upstream `main`, the weekly drift detector would fail on the next run.
Incorporating the contract was therefore both the top interop-correctness
slice of issue #126 and a required evidence refresh.

## Contract Incorporated

Server deployments advertise an aggregate outbound WebSocket application-
payload limit (`security.max_outbound_message_size`, default 8 MiB,
configurable 1..=64 MiB), counted after Signal Fish protocol encoding and
before WebSocket framing. Discovery paths: v3 `ProtocolInfo.max_outbound_message_size`
(absent on frozen v2), the `/v2/client-config` and `/v3/client-config`
endpoints, and the `x-signal-fish-max-outbound-message-size` upgrade response
header. An over-limit server delivery is rejected whole and closes that
connection with RFC 6455 code 1009 (`outbound_message_too_large`).

## Implementation

- `ProtocolInfoPayload` gains the v3-only optional field
  `max_outbound_message_size: Option<usize>` with the same serde contract as
  its siblings (`default`, omission-skipping, explicit-null rejection).
- `ClientSnapshot` gains `server_max_outbound_message_size`, set from the
  negotiated `ProtocolInfo`, cleared by `clear_session()`, and included in
  its redacted `Debug` form. Both drivers expose it through their existing
  coherent `snapshot()` surface.
- Vendored evidence refreshed from server main at commit
  `d5b3135fda53a2a7de69c5ea54faefa95ca9a5b9`: `v3-server-messages.jsonl`,
  the AsyncAPI spec, both `PROVENANCE.toml` markers,
  `tests/compatibility.toml`'s `[protocol_authority]` pin, and the manifest
  test's expected-commit literal.
- All 28 perf-lab protocol digests regenerated via the documented
  `--emit-protocol-baselines` mode after the fixture gained the field. A
  structural diff proved digest-only changes; allocation and timing baselines
  are untouched because fixture construction stays outside every measured
  region.
- `docs/transport.md` now describes the landed contract (discovery paths,
  sizing guidance, close 1009 semantics) instead of citing #399 as an open
  gap; `docs/protocol.md` documents the new field and the previously omitted
  `transports` row; `CHANGELOG.md`, `PLAN.md`, and `.llm/context.md` reflect
  the advanced pin.

## Regression Evidence

- Wire golden round-trip now enforces the upstream sample bytes including
  `max_outbound_message_size`.
- New serde tests cover v3 presence/round-trip, frozen-v2 absence, and
  explicit-null rejection for the new key (data-driven loop extended).
- A shared-core lifecycle test pins: pre-negotiation `None`; v3 negotiation
  records the advertised limit; v2 negotiation and a v3 info omitting the
  field stay `None`; terminal disconnect clears it.
- Existing driver-parity close-reason tests already prove peer close codes
  propagate verbatim inside the formatted `Disconnected` reason, covering
  the documented 1009 behavior without a duplicate case.
- Full mandatory chain green locally: `cargo fmt`, clippy `-D warnings`
  (all targets/features), complete workspace test suite.

## Adversarial Review

An independent adversarial audit verified serde equivalence with sibling
fields against upstream source, all twelve construction sites, exact reset-
path tracking, digest-only baseline regeneration (empirically re-running
`perf-smoke` and `perf-allocations`), every documentation claim against the
pinned upstream tree, changelog accuracy, and stale-reference sweeps across
Godot, examples, and benchmarks. Findings were limited to one stray untracked
npm lockfile (deleted) and wording precision ("surfaces verbatim" softened);
both applied. No functional defects.

## Follow-ups Identified

Filed as narrowly scoped issues after the audit review:

- [#140](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/140)
  — outbound room operations are not gated on authentication; a premature
  `join_room`/`reconnect` can arm a fence that a pre-auth rejection strands
  under non-terminal policies.
- [#141](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/141)
  — terminal peer-close event delivery has no deadline escape when the
  consumer wedges without shutdown, and shutdown mid-batch can abandon more
  than the documented one in-flight event.
- [#142](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/142)
  — reliable-send cancellation safety is untested, and dequeue-time
  serialization failure would strand a fence in both drivers (currently
  unreachable).

Also noted for a future reviewed design (not filed separately): capture the
upgrade response header in `WebSocketTransport` diagnostics or auto-size
inbound caps from the advertised value while preserving explicit caller
policy and `from_stream` semantics.
