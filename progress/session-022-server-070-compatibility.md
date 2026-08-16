# Session 022 — Signal Fish Server 0.7 Compatibility

Date: 2026-08-16

## Objective

Advance issue #86 from protocol drift to one reviewable, breaking-change PR
without weakening the byte-identical v2 relay floor or legacy server 0.4 mesh
compatibility.

## Audit and Contract

- Pinned server tag `v0.7.0` to dereferenced commit
  `3f7f43d4cd4b3cc7f8fb893220dc35c9b1fad333` and compared it with `v0.4.0`
  commit `50b28a9a13dc2b99d301bfb2482c5fd6f768a2e8`.
- Identified the protocol-v3 mesh break introduced without a version bump:
  server 0.6+ requires one UUID generation on `SessionPlan` and every
  client/server `Signal`; recipients must fence stale generations and rebuild
  retained pairs when the generation changes.
- Classified Direct endpoint support and `UNSUPPORTED_PROTOCOL_VERSION` as
  additive wire requirements. Token-binding-v2 remains a separate negotiated
  transport feature because server 0.7 disables it by default.
- Hosted audit found no open PR or Dependabot PR. Issue #86 was the highest
  gameplay-correctness priority; #78/#84 remain the next safety milestone.

## Red-Green Journal

1. Refreshing the exact 0.7 JSONL/spec artifacts made signal, session-plan,
   error-token, and provenance conformance red. Adding adaptive generation
   fields, `DirectEndpoint`, `UnsupportedProtocolVersion`, and the explicit six
   legacy `ErrorCode::NON_EMITTED` variants made all conformance tests green.
2. Requiring a plan before signal sends made older async/polling tests red.
   Supplying authoritative plans, restoring a generation from reconnect replay,
   and adding explicit pre-plan/legacy tests restored complete parity.
3. Generation-binding `WebRtcDriver` made stale output and replan cases
   observable. Tests now prove same-peer/same-role rebuild, stale pending and
   driver-output suppression, current-plan sender membership, and that Direct
   or Relay plans never drive WebRTC.
4. The first local live 0.7 test used the server's default relay topology and
   correctly found no WebRTC peers. Pinning the test server to host+WebRTC made
   initial generation-stamped signaling green. The attempted mesh-member
   departure produced no replan by design; switching to host departure proved a
   fresh generation reaches both survivors and signaling continues.
5. Adversarial review exposed a free-queue race where the shared core could
   observe a replacement plan before `MeshController` consumed its event.
   Generation-bound typed/raw sends now validate and queue under one core lock;
   regression tests prove stale output is refused rather than retagged in both
   async and polling clients. The same pass tightened `NewPeer` to require an
   authoritative WebRTC plan and expanded parity comparison to every new field.
6. The first hosted Godot matrix passed the shared build/export and legacy 0.4
   browser job, while all three Server 0.7 scenarios failed before protocol
   traffic with HTTP 403. Uploaded server evidence identified
   `outcome="rejected_origin"`: 0.7 enforces its default fixed-port browser
   allowlist on WebSocket upgrades, but the harness uses an ephemeral HTTP
   port. The workflow now opts its isolated test server into wildcard origins
   in both clean and network-namespace launch paths, with a policy regression
   assertion covering both paths.

## Delivered Surface

- Shared-core generation state, outgoing stamping, pre-plan refusal, stale
  inbound suppression, reconnect restoration, and room/disconnect clearing.
- Generation/direct-endpoint protocol, event, snapshot, and `MeshSession`
  surfaces with generation-less 0.4 adaptation.
- Generation-bearing `WebRtcDriver`/`DriverEvent` and generation-fenced
  `MeshController` choreography.
- Exact 0.7 wire/spec bytes, SHA-256 provenance, compatibility manifest, error
  conformance, docs, examples, focused skills, and changelog migration notes.
- Required pinned 0.7 live host-replan CI plus Godot clean/impaired/soak on 0.7
  and a retained clean Godot 0.4 legacy gate.

## Evidence

- `cargo test --test ci_config_tests`: 207 passed.
- `bash scripts/check-workflows.sh`: all blocking phases passed (actionlint was
  unavailable locally; the workflow policy tests and YAML lint passed).
- The mandatory `cargo fmt` + all-target/all-feature Clippy + workspace tests
  gate passed. The expanded 22-phase preflight also passed every installed
  analyzer after its no-default-feature import finding was fixed: docs/docs.rs,
  dependency policy and audit, panic/FFI policy, unused dependencies,
  examples/snippets, workflow lint, shell portability, and devcontainer checks.
  Optional tools absent locally remain delegated to hosted CI.
- Pinned native server 0.7 source build plus
  `e2e_server_070_generation_signal_and_host_replan`: passed.
- Targeted unit/integration/conformance suites passed, including 31 WebRTC
  tests, 68 async client tests, 124 protocol tests, polling parity, exact wire
  goldens, compatibility checks, and error-code conformance.
- Final adversarial review found no production-code blockers. Its remaining
  documentation and common-command parity observations were folded in before
  publication.
- Draft [PR #89](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/pull/89)
  is open. Ten hosted workflows, the Godot build/export job, and the legacy
  Server 0.4 browser scenario passed on the first revision; Server 0.7 browser
  evidence exposed and localized the origin-policy launch defect described
  above.

## Remaining Delivery

Validate and push the hosted-origin fix, wait for every required hosted check
and reviewer approval, then merge/close #86. The separate token-binding-v2
follow-up is tracked in
[issue #88](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/88).
