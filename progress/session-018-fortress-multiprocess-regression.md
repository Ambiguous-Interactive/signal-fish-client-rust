# Session 018 — Fortress Multiprocess Regression

## Objective

Complete the issue #61 follow-up by auditing the accepted-send solution,
expanding data-driven regression coverage, adding a real two-process Godot +
Fortress + Signal Fish Server scenario, updating current documentation and
`llms.txt`, and publishing a PR with green CI and no unresolved reviewer
feedback.

## Progress

- Changed the Godot admission default to the latency-targeted adaptive policy
  and isolated the capacity/watermark decision for exhaustive testing.
- Added an exhaustive admission-decision specification sweep plus focused
  ownership-transfer, retry, FIFO, work-budget, and dual close-policy tests.
- Added a bounded Fortress Rollback 0.10.0 adapter with strict v3 MessagePack
  envelope validation, stable UUID-derived handles, exact confirmed-input and
  serialized-state checksums, forced rollback pressure, observable teardown,
  and structured diagnostics.
- Added browser automation that launches two independent Chromium/Godot 4.5
  processes against Signal Fish Server 0.4.0 and retains client/server
  artifacts on failure.
- Kept the existing 136 frames/s synthetic browser load as a separate
  throughput/conservation regression.
- Updated README, user documentation, canonical LLM context, the Godot skill,
  changelog, and a canonical MkDocs-published `llms.txt`.
- Made coverage blocking with a measured 93% line floor; the current suite
  measures 93.70% line coverage.

## Evidence

- Real Fortress browser run: both independent Godot/Chromium processes
  confirmed 600 frames in 10.532 s and 10.564 s, matched state checksum
  `3586543755030135558`, and reported 10/10 matching Fortress checksums. The
  creator recorded zero pre-impairment rollbacks, then B's hidden frame-120
  input transition produced exactly one rollback of depth 7, one state load,
  and seven resimulated frames after reintegration. Room/player identities
  cross-matched, all client and server delivery counts conserved, queues
  drained to zero, and the creator observed the joiner's v3
  epoch/final-sequence terminal watermark before a continuously quiescent
  close interval.
- Existing synthetic browser run: all 4,352 offered load frames arrived, peak
  aggregate queue depth was 6, final depth was zero, p99 latency was 67.230 ms,
  and maximum poll duration was 4.070 ms.
- `cargo llvm-cov --all-features --summary-only --fail-under-lines 93` — 93.70%
  line coverage.
- Strict MkDocs build passed and generated `site/llms.txt` was byte-identical
  to the canonical root file.
- Final mandatory Rust formatting, all-target/all-feature Clippy with warnings
  denied, and all-feature tests passed locally; the standalone fixture also
  passes all-target Clippy with warnings denied and a locked dependency graph.
- Two adversarial review rounds covered production semantics, close behavior,
  system-test determinism/cleanup, CI policy, and documentation. The final
  passes reported zero substantive findings; the exact lychee command checked
  187 links with zero errors.

## Next

- Commit and push the scoped work, open a draft PR, trigger all available
  automated reviewers, and iterate until every CI check and review is green.
- Publish through the connected GitHub integration, then monitor every check
  and automated review to completion.
