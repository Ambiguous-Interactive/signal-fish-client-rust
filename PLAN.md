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
- All 11 required project workflows are green on current `main` commit
  `3f38367`; the separate live Repository Policy audit remains red because of
  issue #90.

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
Policy workflow is genuinely red. Multiple PRs, most recently #121, merged
without an approval or enforced required checks; #121 merged as current `main`
commit `3f38367` with zero approvals and a quota-failed Copilot review check.

Restoring the live ruleset, disabling or funding the quota-broken Copilot
review rule, adding an eligible independent reviewer, confirming an empty
bypass list in the ruleset UI, and dispatching a green policy audit require
maintainer administration unavailable through the connected tools. The first
substantive non-bot PR opened after restoration must prove that approval,
thread resolution, branch freshness, and all 11 aggregate checks are enforced.

## Current Work — Certificate-Fingerprint Token Binding

Track [issue #120](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/120).

- Bind the native token proof to the exact leaf certificate presented by the
  configured rustls client for Server 0.7 profiles that require a client
  fingerprint.
- Pin positive and adversarial mTLS/fingerprint interoperability evidence to
  the exact server contract without changing disabled/default connections.
- Preserve explicit incapability boundaries for browser, Emscripten, Godot,
  and post-handshake transports.

## Later Milestone — Allocation and Performance Evidence

Track [issue #82](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/82).

- Define representative lobby, JSON relay, binary relay,
  classified-delivery, reconnect, and polling workloads.
- Establish deterministic throughput, latency, allocation, and queue-age
  baselines before optimizing.
- Optimize only measured bottlenecks and retain stable regression thresholds.
- Preserve frame ownership, backpressure, exact accountability, and event
  delivery invariants in every optimization.

The stale `agent/fortress-rollback-throughput-e2e` branch is research input
only. Its pre-session-018 fixture was superseded by the merged two-browser
Godot Fortress suite and must not be cherry-picked.

## Later Milestone — Documentation Design System

Track [issue #80](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/80).

- Inventory and validate the supplied design assets and their licensing.
- Translate the design system into MkDocs theme overrides without weakening
  strict builds, accessibility, responsive behavior, or code readability.
- Update repository and published-site branding together, with rendered
  desktop and mobile review.

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
