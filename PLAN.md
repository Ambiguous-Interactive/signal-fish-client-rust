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
workflow is genuinely red, and PRs #91, #92, and #94 demonstrated the impact
by merging without an approval or enforced required checks. PR #92 auto-merged
while a required aggregate was still running, then produced no push CI on its
zero-file merge commit. The checked-in `GITHUB_TOKEN` Dependabot auto-merge path
is now removed; repository policy rejects Dependabot-specific workflows,
automated-merge primitives, and non-allowlisted workflow write permissions.
PR #94 then merged despite its explicit do-not-merge gates and zero approvals.
Ordinary reviewed merges remain policy but are not enforced until issue #90 is
fixed. Restoring the live ruleset, disabling or funding the quota-broken
Copilot review gate, adding an eligible independent reviewer (the repository
currently has only the PR author's collaborator account), and dispatching a
green policy audit still require maintainer administration. The next valid
non-bot PR remains the end-to-end enforcement proof.

## Completed Milestone — Safety and Static Analysis

[Issues #78](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/78)
and [#84](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/84)
were delivered by PR #91 and closed when it merged as `963a9fc`.

- Required compiler, rustdoc, dependency, panic, coverage, target-build, and
  FFI gates are complemented by scheduled fail-closed Miri, fuzz, and mutation
  lanes. Broad pedantic/nursery lint expansion remains rejected as noisy, and
  native ASan still cannot execute the Emscripten-only owned unsafe path.
- Core unsafe Rust is denied by default, the Godot adapter forbids it, and the
  sole Emscripten FFI exception is source-audited and target-compiled.
- Deep Safety runs the 125 production protocol tests under Miri, all JSON and
  binary v2/v3 fuzz targets with isolated corpora, and a zero-survivor mutation
  scope. Change-scoped PR triggers provide hosted evidence when covered inputs
  change.
- PR #91 passed all eleven project aggregates plus Deep Safety and Protocol
  Sync, but merged without the approval and quota-green state it explicitly
  required. That governance failure remains tracked in issue #90.

## Completed Correctness Follow-up — FFI and Dependency Automation

- Emscripten callback state is reclaimed only when its owner consumes a typed
  authorization produced after native close was attempted or observed and browser callback
  unregistration succeeded. Logical receive failures still drive native close,
  deletion failures remain retryable, and terminal cleanup deliberately leaks
  the small allocation instead of risking a late-callback use-after-free. The
  host-tested state machine and required FFI checker enforce this condition
  with 43 checker self-tests.
- Dependabot's first default-branch Cargo run after PR #91 failed because its
  temporary file set omitted `crates/signal-fish-client-godot/Cargo.toml`, then
  opened zero-file PR #92. The three-directory attempt also failed in run
  31964370221 with eleven resolution errors and opened zero-file PR #96:
  Dependabot processes each Cargo directory independently rather than as one
  file set. The corrective updater now starts only at `/`, where Cargo
  discovers the adapter workspace member. The exact minimum/latest Godot
  fixtures stay standalone and are maintained through their dedicated locked
  E2E compatibility evidence. Post-merge root-workspace updater run
  [31969079956](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/actions/runs/31969079956)
  completed successfully on `main`, discovered both workspace crates, reported
  no dependency-resolution errors, and opened no Cargo PR. This satisfies the
  hosted proof tracked by issue #95.
- The checked-in `GITHUB_TOKEN` Dependabot auto-merge path is removed. Until
  issue #90 is fixed, dependency PRs must not bypass review/check policy or
  suppress CI for their merge SHA.

## Completed Correctness Study — First Hardening Slice

Track [issue #97](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/97).

- Audited protocol/state, public API, transport/network behavior, the pinned
  Server 0.7 contract, and native/browser reference-client assumptions through
  three independent subsystem matrices and adversarial review loops.
- Fixed stale reliable signaling and game data across queue waits, including
  legacy generation-less plans and room changes; async send/receive/close
  liveness; and the ambient `Debug`/tracing credential and payload leak class.
- Decomposed every unresolved finding into issues #99–#107 with explicit
  invariants and executable acceptance evidence. Session 026 records the
  finding matrix and proof.

## Completed Correctness Milestone — Protocol State Validation

[Issue #99](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/99)
adds shared-core lifecycle/version gates, canonical Server 0.7 SessionPlan
shape and roster validation, and bidirectional signal-peer authorization.
Lifecycle/plan/signaling-invalid frames are observable but transactional under
every policy; async and polling drivers retain identical behavior. Session 027
records the rule-to-test/source matrix and verification evidence.

## Completed Correctness Milestone — Negotiation and Reconnect State

Issues #100 and #101 shipped together in PR #111 because both depend on the
first authoritative `ProtocolInfo` and reconnect state transaction.

- Requested and effective game-data formats are distinct public state. The
  shared core resolves Server 0.7's canonical format advertisement atomically,
  and async/polling plus pinned-server evidence covers supported and fallback
  paths before transport admission.
- Reconnect restoration is version-strict and transactional across replay,
  token rotation, accountability, membership, and plan state under every
  violation policy. The old WebRTC plan is fenced until Server 0.7's fresh live
  post-reconnect plan arrives.
- Session 028 records the rule-to-source-to-test matrix, pinned-server evidence,
  hosted review fixes, and green aggregate checks.

## Completed Correctness Milestone — Mesh Capability and Liveness

[Issue #102](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/102)
separates negotiated local capability from the authoritative selected plan,
normalizes controller-owned WebRTC configuration, and makes peer liveness
transport-specific. Session 029 records the configuration, state, transition,
and async/polling/controller parity evidence.

## Completed Correctness Milestone — WebSocket Liveness

[Issue #105](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/105)
bounds native WebSocket control-frame work, preserves automatic Pong/Close
flush ordering, and makes EOF and socket errors terminal for direct Transport
callers. Session 030 records socket, ownership, wake, and terminal-state evidence.

## Current Correctness Milestone — Client Membership and Operation State

[Issue #103](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/103)
exposes authoritative player/spectator role state, clears room-scoped identity
on exit, and enforces one shared operation matrix across async and polling
drivers. Admission-time room transitions fence later FIFO commands, authority
state is validated and tracked, and invalid calls fail before consuming bounded
queue capacity. Session 031 records the operation/state matrix, pinned-server
contract, and verification evidence.

## Next Correctness Milestones — Readiness, Counters, and Transport Deadlines

Continue with #104, #106, and #107 before usability, performance, token
binding, or presentation milestones.

## Following Milestone — Negotiated Token Binding

Track [issue #88](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/88).

- Preserve byte-for-byte default connections while adding explicit disabled,
  optional, and required token-binding-v2 modes.
- Bind the native proof algorithm and negative cases to one exact server
  release/commit, including replay, tamper, downgrade, and malformed envelopes.
- Make handshake-material capability differences explicit for native, browser,
  Godot, polling, and custom transports, with typed required-mode failures.
- Keep keys, proofs, and handshake secrets out of `Debug`, tracing, and errors.

## Later Milestone — Allocation and Performance Evidence

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
