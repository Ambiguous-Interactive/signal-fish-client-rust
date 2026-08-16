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
- Signal Fish Server 0.7 compatibility shipped on `main` in
  [PR #89](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/pull/89),
  closing issue #86.

Curated milestone history and verification evidence live in tracked files
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
lacks approval and required-status-check rules, the scheduled Repository Policy
workflow is genuinely red, and PR #89 demonstrated the impact by merging
without the intended enforcement. Restoring the ruleset, resolving the
non-actionable Copilot quota gate, and dispatching a green policy audit require
maintainer-level repository administration. The next open PR is the enforcement
proof because #89 is already merged.

## In-flight Milestone — Safety and Static Analysis

Consolidate [issue #78](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/78)
and [issue #84](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/84)
into one evidence-backed hardening milestone.

- The analyzer inventory is complete: required Clippy, rustdoc, dependency,
  panic, coverage, target-build, and FFI gates plus scheduled fail-closed Miri,
  fuzz, and mutation lanes cover the useful defect classes. Broad
  pedantic/nursery lint expansion was rejected after more than 170 mostly
  stylistic findings and one verified false positive; native ASan cannot
  exercise the Emscripten-only owned unsafe path.
- Compiler policy now denies unsafe Rust in the core, forbids it in the Godot
  adapter, and documents the sole Emscripten FFI exception. The required WASM
  workflow hosts the FFI checker and its 25-case self-test.
- Deep Safety is being repaired to fail closed: Miri runs the 125 production
  protocol tests; fuzz uses the actual host, isolated writable corpora, and all
  JSON/binary v2/v3 targets; mutation testing has zero surviving mutants.
  Path-scoped PR triggers provide immediate hosted proof when analyzer or
  covered-code inputs change without making the variable-runtime lane required.
- The incompatible standalone-fixture Dependabot updater is consolidated with
  the root workspace so its file set can include local path dependencies;
  default-branch updater confirmation remains pending.
- Local evidence is green. PR #91 has all eleven required project-owned
  aggregates green plus green change-scoped Deep Safety and Protocol Sync runs.
  Independent and Bugbot reviews report no actionable findings. Literal
  all-checks-green acceptance remains externally blocked: Copilot's reviewer
  check failed on quota, there is no approval, and the live ruleset still does
  not enforce either condition. Issue #90 requires this next open PR to prove
  that governance before merge. Issues #78 and #84 close with the PR once that
  blocker is resolved or explicitly re-scoped by a maintainer. Default-branch
  Dependabot confirmation remains after merge.

## Next Major Milestone — Negotiated Token Binding

Track [issue #88](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/88).

- Preserve byte-for-byte default connections while adding explicit disabled,
  optional, and required token-binding-v2 modes.
- Bind the native proof algorithm and negative cases to one exact server
  release/commit, including replay, tamper, downgrade, and malformed envelopes.
- Make handshake-material capability differences explicit for native, browser,
  Godot, polling, and custom transports, with typed required-mode failures.
- Keep keys, proofs, and handshake secrets out of `Debug`, tracing, and errors.

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
