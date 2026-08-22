# Signal Fish Client — Current Roadmap

## Current State

- Latest released client: 0.10.0.
- Canonical protocol fixture: Signal Fish Server 0.7.0 at commit
  `3f7f43d4cd4b3cc7f8fb893220dc35c9b1fad333`; Server 0.4
  generation-less signaling remains an explicit compatibility case.
- Post-0.7 protocol wire/spec authority is pinned separately at server commit
  `2d7c3836edf64bb734482b7fbb2b3db3f88fea8b`, including negotiated
  `room_operation_ids` support.
- The async and polling clients share protocol behavior through `ClientCore`.
- The lockstep Godot adapter supports native and official Godot 4.5 web
  exports, with blocking real-server gameplay coverage.
- Native WebSocket token binding supports disabled, optional, required, and
  client-certificate-fingerprint-bound Server 0.7 profiles on
  certificate-capable custom rustls connections.
- Release preparation and publication are workspace-aware, reproducible, and
  protected by required aggregate checks.
- A deterministic 28-cell `ClientCore` laboratory now pins latency,
  throughput, queue-age, and all six allocation-counter baselines. Required CI
  enforces debug/release ceilings while leaving timings diagnostic.
- Direct JSON string game payloads of at least 4 KiB use measured
  capacity-aware serialization in both drivers, keeping the pinned 4 KiB burst
  at four reallocations. Room-operation negotiation changes authentication
  bytes and adds an event-ledger slot, so all 28 protocol pins are refreshed
  while their semantic invariants remain unchanged.
- The approved Vector identity, accessible oceanic theme, and self-hosted
  typography now span the README and task-oriented MkDocs site, with exact
  asset provenance and desktop/mobile visual evidence.
- Both crates now publish only library source, required unit-test data, and
  package metadata; repository integration tests, other standalone wire
  fixtures, examples, progress records, and changelog history remain in the
  linked source repository.
- All 12 push workflows passed on `main` commit `c2b22b2`, including merged PR
  #132's bounded native WebSocket input. The separate live Repository Policy
  audit remains red because of issue #90.

Completed milestone history and verification evidence live in tracked files
under `progress/`.

## Priority Order

Work open issues in gameplay-impact order:

1. correctness and server interoperability;
2. client usability and safety;
3. runtime performance;
4. documentation-site presentation.

Finish one coherent PR and make it fully green before starting the next. Do
not stack dependent PRs.

## Next Milestone — Correctness and Performance Audit

[Issue #126](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/126)
is the next gameplay-impacting milestone. Execute it correctness-first against
the pinned Server 0.7 fixture and reference client implementation, and require
measured evidence before accepting performance complexity:

1. Inventory the all-feature matrix: protocol codecs and transitions,
   `ClientCore`, async and polling drivers, native and target-gated Emscripten
   WebSockets, Godot, token binding, and mesh/WebRTC. Audit setup, configuration,
   recovery, cross-driver parity, misuse resistance, and documented success
   paths as well as the wire behavior.
2. Pin the exact server/reference-client source commit and map every inventoried
   wire/interoperability behavior—including existing wire ledgers and
   real-server regressions—to that authority. Map SDK-owned behavior to explicit
   public contracts and tests. Add differential fixtures for uncovered cases
   and document every intentional compatibility deviation.
3. Exercise duplicate, delayed, logically out-of-transition, malformed,
   oversized, and disconnect-adjacent complete messages; cancellation, queue
   saturation, and reconnect races; and generation compatibility. Keep raw
   stream fragments, datagram reassembly, peer authentication, and UDP loss
   policy explicitly at the transport boundary—the SDK consumes complete
   ordered messages and must not imply guarantees it cannot provide.
   The first correctness slice is complete: typed player/spectator
   join/leave/reconnect responses with no compatible pending operation are
   rejected under every policy in both drivers, while Server 0.7's
   authoritative spectator exits remain valid. The
   [#128](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/128)
   same-kind response identity slice is now implemented against completed
   [signal-fish-server#395](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/395):
   v3-capable authentication requests `room_operation_ids`, exact echoes enable
   fresh UUID envelopes for all five directed room operations, and stale or
   mismatched results cannot consume the current fence. Missing echoes and
   released Server 0.7 retain the legacy wire. Upstream goldens, both-driver
   backpressure races, and all violation policies cover the boundary. The
   native WebSocket oversized-input slice is
   complete: all built-in connect paths now bound individual frames and
   fragmented-message assembly at a configurable 8 MiB default before
   `ClientCore`, preserve caller-owned `from_stream` limits, and fuse capacity
   errors. No finite value is a Server 0.7 compatibility guarantee, so
   [signal-fish-server#399](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/399)
   tracks a negotiated/enforced outbound contract. Disconnect-adjacent
   transitions, browser/engine boundary parity, and the remaining inventory
   cells remain independent client audit slices.
4. Re-run established unsafe-inventory, Miri, fuzz, dependency, and no-panic
   gates. Evaluate focused Loom models and additional sanitizer or lint coverage
   only where target support and owned boundaries make them applicable; promote
   only stable, actionable findings to warning-as-error policy. Turn every
   confirmed gap into a minimal regression before changing production code.
5. Profile representative signaling and 4 KiB burst paths before optimizing.
   Compare allocation counts, queue age, throughput, and wire ledgers against
   the existing 28-cell laboratory; accept batching, reuse, or pooling only
   when the gain is repeatable and protocol behavior and API simplicity remain
   unchanged.
6. Record the audit matrix and evidence in `progress/`, open narrowly scoped
   follow-up issues for independent findings, and land one coherent green PR at
   a time in correctness, usability/safety, then measured-performance order.

## Hosted Governance Blocker

[Issue #90](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/90)
tracks live drift from `.github/required-checks.json`. Ruleset #14801090 still
lacks approval and required-status-check rules, and the scheduled Repository
Policy workflow is genuinely red. Multiple PRs, including #121 and #122,
merged without the ruleset enforcing independent approval or required checks.

Restoring the live ruleset, disabling or funding the quota-broken Copilot
review rule, adding an eligible independent reviewer, confirming an empty
bypass list in the ruleset UI, and dispatching a green policy audit require
maintainer administration unavailable through the connected tools. The first
substantive non-bot PR opened after restoration must prove that approval,
thread resolution, branch freshness, and all 11 aggregate checks are enforced.

## Permanent Acceptance Gates

Every PR must satisfy:

```shell
cargo fmt && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features
```

In addition:

- all relevant repository policy, docs, MSRV, packaging, coverage,
  server-E2E, and Godot-E2E checks pass;
- user-visible changes appear under `CHANGELOG.md` `[Unreleased]`;
- `PLAN.md`, `.llm/context.md`, and `progress/` reflect actual state;
- all actionable reviewer feedback is resolved;
- the PR has one uniquely named required aggregate per blocking workflow and
  reaches a fully green state.
