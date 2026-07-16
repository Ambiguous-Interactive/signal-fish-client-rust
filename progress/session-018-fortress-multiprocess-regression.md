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

- Draft PR #64 is open. The first GitHub run proved every check green except
  the new browser E2E, whose artifacts showed an environment-only 20-second
  settlement timeout at 461/464 confirmed frames with 7/7 checksums matched.
  The data-backed guard is now a hard 40-second settlement deadline; the
  independent browser wait is 60 seconds.
- Cursor Bugbot found that the synthetic fixture compared a historical buffer
  peak with a later adaptive watermark. The corrected gate uses an immutable
  adaptive ceiling plus exact accepted-send invariant counters captured with
  the contemporaneous watermark. It aggregates both JSON and binary client
  pairs and separately accounts for the empty-buffer single-frame escape.
- The rebuilt post-review browser runs pass: Fortress again confirms 600
  frames with 10/10 checksums, rollback and lifecycle proof; the synthetic run
  delivers all 4,352 frames with zero admission violations, zero escapes,
  drained queues, and 67.230 ms p99 latency.
- Coverage remains blocking at 93.69%. Strict MkDocs/`llms.txt`, 187-link
  lychee, mandatory Rust checks, fixture checks, and repository hooks pass.
- Commit and push the review/CI corrections, retrigger Cursor and Copilot, then
  monitor checks and resolve reviewer feedback to completion. Copilot's first
  attempt started but was stopped by the repository's external monthly quota.
