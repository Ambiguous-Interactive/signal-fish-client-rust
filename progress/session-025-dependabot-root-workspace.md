# Session 025 — Dependabot Root Workspace Recovery

Date: 2026-08-16

## Objective

Re-audit the post-PR-94 hosted state, repair the still-failing Cargo dependency
updater without weakening compatibility coverage, incorporate the real Syn 3
update behind zero-file PR #96, and restore truthful roadmap evidence.

## Baseline Audit

- Default-branch Cargo updater run 31964370221 failed with eleven
  `dependency_file_not_resolvable` errors after attempting all three configured
  directories.
- Dependabot processed directories independently: adapter processing lacked
  the root workspace metadata, while the standalone web fixture lacked its
  sibling adapter path dependency.
- The failed run opened PR #96 for Syn 3.0.3, but its commit tree was identical
  to `main` and the PR contained zero changed files. The root manifest still
  required Syn 2.
- Main had ten green required aggregates while Godot Web was still running;
  Repository Policy remained red because the live ruleset lacked review and
  required-status-check rules.
- Open correctness issue #97 superseded token binding as the next
  gameplay-impact priority but was absent from `PLAN.md`.

## Implementation

- Replaced the multi-directory Cargo updater with one `directory: /` updater.
  Cargo discovers the adapter through actual workspace membership, avoiding
  the isolated-subdirectory failure mode.
- Kept the minimum and latest Godot fixtures standalone. This preserves their
  exact incompatible Godot endpoints and avoids making ordinary mandatory
  workspace gates require the engine-dependent browser fixture. Their locked
  dependencies remain deliberate manual upgrades gated by compatibility and
  browser E2E.
- Replaced the false policy test that equated YAML directory listing with a
  shared updater file set. The new test enforces one resolvable root updater,
  real adapter workspace membership, and standalone compatibility fixtures.
- Incorporated the actual Syn 3 dependency update. Migrated the safety AST
  scanner to Syn 3's `Safety` and `TypeFnPtr` APIs and added adversarial
  regression cases for safety-bearing nodes, parsed attribute expressions,
  unparsed macro tokens, and unsafe assembly macros.
- Updated canonical context, roadmap priority, session 024's hosted follow-up,
  and this progress record. No changelog entry was added because all changes
  are internal dependency/CI/test maintenance.
- Rewrote issue #95 around a truthful root-only hosted proof while retaining
  explicit manual compatibility maintenance for the standalone fixtures.

## Verification

- All 213 repository-policy tests pass with Syn 3.
- The mandatory workflow passed: `cargo fmt`, workspace/all-target/all-feature
  Clippy with `-D warnings`, and workspace/all-feature tests.
- `scripts/check-all.sh --quick` passed formatting, the FFI safety policy,
  three Clippy feature combinations, and three test feature combinations.
- PR #98 is ready for review with all twelve project workflows and all eleven
  required aggregates green. The repository's non-required automated Copilot
  review check failed twice because the configured account has exhausted its
  quota; it produced no actionable feedback or unresolved threads and remains
  an issue #90 maintainer-administration blocker.

## Hosted Proof

- Zero-file PR #96 was closed without merging after PR #98 incorporated the
  real Syn 3 update and the updater correction.
- Default-branch Cargo updater run
  [31969079956](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/actions/runs/31969079956)
  completed successfully on merged `main` commit `3461789`. Its dependency
  snapshot included both the root `signal-fish-client` package and the
  workspace-member `godot` dependency, reported no
  `dependency_file_not_resolvable` errors, and completed without creating a
  Cargo dependency PR. This satisfies issue #95's hosted proof without another
  empty update.
- Issue #90 still requires maintainer administration and an eligible
  independent reviewer before repository review/check enforcement can pass.
- Begin issue #97 as bounded subsystem audits after this single PR is green.
