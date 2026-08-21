# Signal Fish Client — Current Roadmap

## Current State

- Latest released client: 0.10.0.
- Canonical protocol fixture: Signal Fish Server 0.7.0 at commit
  `3f7f43d4cd4b3cc7f8fb893220dc35c9b1fad333`; Server 0.4
  generation-less signaling remains an explicit compatibility case.
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
- Direct JSON string game payloads of at least 4 KiB now use measured
  capacity-aware serialization in both drivers. The pinned 4 KiB burst drops
  from 132 to four reallocations while all 28 wire ledgers remain unchanged.
- The approved Vector identity, accessible oceanic theme, and self-hosted
  typography now span the README and task-oriented MkDocs site, with exact
  asset provenance and desktop/mobile visual evidence.
- All 12 push workflows completed successfully on current `main` commit
  `40a030b` after PR #125 merged. The separate live Repository Policy audit
  remains red because of issue #90.

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
