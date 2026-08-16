# Session 021 — Coalesced Unsupported-Format Gaps

## Objective

Resolve client issue #81 before the broader Signal Fish Server 0.7
compatibility milestone: accept the coalesced and mixed-reason
`unsupported_format` delivery reports emitted by newer servers without
weakening protocol-v3 accountability.

## Evidence

- The hosted issue remained open with no in-progress client PR.
- Current `main` is the merge of PR #85. All 11 blocking workflow runs on its
  PR head succeeded; subsequent scheduled Security, Unused Deps, and Coverage
  runs on the exact main SHA also succeeded. Deep Safety is explicitly
  informational/non-blocking and its latest run was cancelled during Miri.
- `DeliveryAccountability::record_report` still required exactly one
  single-sequence unsupported-format gap and armed supplemental-error causality
  from the report's first gap.
- Signal Fish Server reference-client commit
  `970be936c8438a892be987257604fe073ae73564` removes those shape assumptions
  while retaining range validation, in-report overlap checks, and exact
  counter-delta comparison.
- The merged server contract rate-limits supplemental advisories and requires
  only a prior unsupported-format report; the client still required immediate
  adjacency and rejected report rollover before an advisory. The native
  reference behavior is commit
  `522c9c6ba10f171957f49abda434d3c37425748d` from server PR #167.
- Red tests reproduced both failures against the current validator before the
  production change.

## Implemented

- Accept any valid unsupported-format inclusive range and multiple such ranges
  in one report.
- Accept optional rate-limited supplemental advisories only after a causal
  unsupported-format report, without requiring adjacency, and clear that
  room-scoped authorization on room reset.
- Preserve `ProtocolViolationKind::Causality` classification for an advisory
  that lacks a prior causal report.
- Reset room-scoped accountability from the shared `ClientCore::clear_room`
  path used by both room and spectator exits.
- Retain atomic validation, the 256-range bound, checked range lengths,
  monotonic cumulative counters, non-overlap, and exact reason-counter deltas.
- Add tests for an exact three-sequence range, understated and overstated
  counters, split unsupported ranges, a mixed report with the unsupported gap
  after another reason, non-adjacent advisories, report rollover, and room-reset
  invalidation.
- Update the consumer changelog, canonical context, and current roadmap.

## Validation

- Red: focused unsupported-format tests failed with the former
  `unsupported-format report must name exactly one sequence` violation.
- Green: all 17 accountability unit tests and all 18 async/polling parity tests
  pass with all features enabled.
- Mandatory `cargo fmt`, workspace/all-target/all-feature Clippy with warnings
  denied, and workspace/all-feature tests pass. The full suite includes 267
  core unit tests, 207 repository policy tests, 64 async integration tests, 121
  protocol tests, 18 parity tests, and 34 Godot adapter tests; the three live
  server tests remain intentionally ignored without a configured server binary.
- Workflow policy, LLM pre-commit validation, test-quality, and FFI-safety
  scripts pass. `actionlint` is not installed locally; hosted Workflow Lint is
  the authoritative gate.
- Two adversarial review rounds found and drove fixes for non-adjacent
  rate-limited advisories, diagnostic classification, authoritative room-exit
  reset, public driver parity, transactional rejection proof, and lifecycle
  documentation.
- Hosted PR checks and reviewer feedback remain to be recorded after publish.
