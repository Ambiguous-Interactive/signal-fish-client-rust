# Signal Fish Client — Current Roadmap

## Current State

- Latest released client: 0.10.0.
- Canonical protocol fixture: Signal Fish Server 0.4.0 at commit
  `50b28a9a13dc2b99d301bfb2482c5fd6f768a2e8`.
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

## Next Major Milestone — Signal Fish Server 0.7 Compatibility

Track [issue #86](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/86).
The client must interoperate cleanly with Signal Fish Server 0.7.0 and adopt
reference-client behavior where it strengthens the Rust SDK without weakening
the transport abstraction or the v2 relay floor.

### Discovery and Contract

- Pin the exact server 0.7.0 tag and commit before changing code.
- Diff the 0.4.0 and 0.7.0 AsyncAPI, golden wire samples, protocol guide,
  error-code registry, and both upstream reference clients.
- Classify every delta as wire-required, negotiated/additive, server-internal,
  test-only, or intentionally out of scope.
- Record the resulting compatibility contract and migration impact before
  choosing a client release version.

### Implementation

- Refresh vendored protocol artifacts and their provenance/checksums as one
  atomic conformance change.
- Implement every required protocol, accountability, lifecycle, transport,
  and error-model delta in the shared core so async and polling drivers remain
  behaviorally identical.
- Preserve byte-identical default v2 authentication and relay behavior unless
  the new server contract proves that impossible.
- Keep public types exhaustive; treat any required new enum variant as a
  deliberate semver-breaking change.
- Sweep documentation, examples, `.llm/context.md`, focused skills, and the
  changelog for every changed guarantee.

### Verification

- Add red-green golden, malformed-input, negotiation, accountability, and
  async/polling parity tests for every observable delta.
- Run pinned-server E2E for the v2 relay floor and every negotiated v3 path,
  including JSON/binary exchange, classified delivery, reconnect, graceful
  drain, close metadata, and Godot browser gameplay.
- Make fixture provenance and compatibility markers agree through offline
  policy tests.
- Run packaging, docs, MSRV, coverage, fuzz/mutation, and mandatory Rust gates
  before publication.

## Following Milestones

### Safety and Static Analysis

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

### Allocation and Performance Evidence

Track [issue #82](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/82).

- Define representative lobby, JSON relay, binary relay, classified-delivery,
  reconnect, and polling workloads.
- Establish deterministic throughput, latency, allocation, and queue-age
  baselines before optimizing.
- Optimize only measured bottlenecks and retain regression thresholds that are
  stable enough for CI.
- Preserve frame ownership, backpressure, exact accountability, and event
  delivery invariants in every optimization.

### Documentation Design System

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
