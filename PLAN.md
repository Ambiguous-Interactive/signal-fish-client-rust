# Signal Fish Client — Current Roadmap

## Current State

- Latest released client: 0.10.0.
- Canonical protocol fixture: Signal Fish Server 0.7.0 at commit
  `3f7f43d4cd4b3cc7f8fb893220dc35c9b1fad333`; server 0.4 generation-less
  signaling remains an explicit compatibility case.
- The async and polling clients share protocol behavior through `ClientCore`.
- The lockstep Godot adapter supports native and official Godot 4.5 web
  exports, with blocking real-server gameplay coverage.
- Release preparation and publication are workspace-aware, reproducible, and
  protected by required aggregate checks.

Completed milestone history and verification evidence live in `progress/`.

## Priority Order

Work open issues in gameplay-impact order:

1. correctness and server interoperability;
2. client usability and safety;
3. runtime performance;
4. documentation-site presentation.

Finish one coherent PR and make it fully green before starting the next. Do
not stack dependent PRs.

## In-flight Delivery — Signal Fish Server 0.7 Compatibility

Track [issue #86](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/86).
The breaking 0.11-compatible source and conformance work is complete locally:
generation-fenced signaling, Direct endpoint exposure, 0.7 error coverage,
exact fixture provenance, async/polling parity, a pinned live 0.7 host-replan
smoke, and both 0.7 and legacy-0.4 Godot gates. Remaining delivery work is
hosted in [PR #89](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/pull/89):
pass every required check, obtain reviewer approval, merge, and close #86.
Token-binding-v2 remains a separate negotiated
transport feature because server 0.7 leaves it disabled by default.

## Next Major Milestone — Safety and Static Analysis

Consolidate [issue #78](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/78)
and [issue #84](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/84)
into one evidence-backed hardening milestone.

- Inventory existing lint, unsafe-code, dependency, fuzz, and mutation gates
  before adding tools.
- Add only analyzers that find an actionable defect class with acceptable
  runtime and stable Rust/MSRV behavior.
- Prefer crate-level unsafe policy and narrowly justified exceptions over
  source-text heuristics.
- Keep warnings denied in every supported feature/target combination.
- Close or re-scope the duplicate issue once one canonical implementation plan
  is accepted.

## Following Milestone — Allocation and Performance Evidence

Track [issue #82](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/82).

- Define representative lobby, JSON relay, binary relay, classified-delivery,
  reconnect, and polling workloads.
- Establish deterministic throughput, latency, allocation, and queue-age
  baselines before optimizing.
- Optimize only measured bottlenecks and retain regression thresholds that are
  stable enough for CI.
- Preserve frame ownership, backpressure, exact accountability, and event
  delivery invariants in every optimization.

## Later Milestone — Documentation Design System

Track [issue #80](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/80).

- Inventory and validate the supplied design assets and their licensing.
- Translate the design system into MkDocs theme overrides without weakening
  strict builds, accessibility, responsive behavior, or code readability.
- Update repository and published-site branding together, with rendered visual
  review at desktop and mobile sizes.

## Permanent Acceptance Gates

Every PR must satisfy:

```shell
cargo fmt && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features
```

In addition:

- all relevant repository policy, docs, MSRV, packaging, coverage,
  server-E2E, and Godot-E2E checks pass;
- user-visible changes appear under `CHANGELOG.md` `[Unreleased]`;
- `PLAN.md`, `.llm/context.md`, and `progress/` reflect the actual state;
- all actionable reviewer feedback is resolved;
- the PR has one uniquely named required aggregate result per blocking
  workflow and reaches a fully green state.
