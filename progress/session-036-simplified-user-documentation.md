# Session 036 — Simplified User Documentation

## Scope and Priority

The live audit found current `main` at `3115c2b`, no open or draft pull
requests, no dependency update pull requests, and four initial open client
issues: #90, #110, #80, and #82. Issue #90 remains the highest-priority correctness
item, but its remaining ruleset, reviewer, Copilot quota, and workflow-dispatch
work requires maintainer administration unavailable through the connected
tools. The latest Repository Policy run remains red for the missing
`pull_request` and `required_status_checks` rules.

Issue #110 is therefore the highest-priority actionable usability work and was
already in progress when this audit opened correctness follow-up #120 for the
explicitly unsupported client-certificate-fingerprint token-binding profile.
Issue #120 is next after this coherent documentation PR. Issue #110's
Signal Fish Server prerequisite, issue #383, is closed completed. Performance
issue #82 and design-system issue #80 remain later milestones.

Current `main` commit `3115c2b` has all 11 required project workflows green;
Godot Web run 32448708101 completed its Server 0.7 soak and required aggregate
successfully. The separate scheduled Repository Policy failure is the live
governance drift tracked by issue #90, not a code-tree CI failure.

## Onboarding Audit

The pre-change first-contact path duplicated the advanced manual:

| Surface | Before | Problem |
| --- | ---: | --- |
| `README.md` | 413 lines | Repeated feature, transport, WASM/Godot, and contributor CI reference material |
| `docs/index.md` | 154 lines | Repeated installation and a full client example before routing by task |
| `docs/getting-started.md` | 189 lines | Mixed the first connection with the full feature and platform setup matrix |

The detailed material already had canonical homes in `docs/client.md`,
`docs/transport.md`, `docs/wasm.md`, `docs/protocol-versioning.md`, and the
other reference pages. Removing those pages would discard useful contracts;
making them advanced destinations keeps the information while shortening the
path to a first room join.

## Server Endpoint Correctness Sweep

Adversarial review found that the README, `basic_lobby`, Rustdoc, and multiple
guide examples used `ws://localhost:3536/ws` or `/signal`. The pinned Server
0.7 AsyncAPI fixture exposes only `/v2/ws` and `/v3/ws`; the unversioned paths
do not connect to that server.

The whole user-facing class is corrected, including native WebSocket,
Emscripten, Godot, polling-client, event-loop, transport, and example
documentation. The default v2 configuration now consistently uses `/v2/ws`.
A recursive regression test scans README, every guide and agent-skill Markdown
page, every example, and all Rustdoc under `src/` and `crates/` while accepting
only the two routes proven by the pinned server fixture.

## Documentation Shape

- The README is a short install/connect/join path, integration chooser,
  production checklist, and set of canonical links.
- The docs home contains no duplicate Rust example. It routes readers by task
  to quick start, the basic-lobby walkthrough, client commands, and platform
  integration.
- The quick-start page explains only installation, first connection, the basic
  lobby lifecycle, runtime choice, and a compact optional-capability table.
- The former 814-line multi-subsystem examples page is now a 109-line
  basic-lobby walkthrough. Custom transport, mesh, Godot, and load-lab details
  route to their compiling sources or canonical advanced guides.
- MkDocs navigation now separates Start Here, Use the SDK, Multiplayer Choices,
  and Advanced Reference. Contributor release operations sit at the end of the
  advanced group instead of occupying the onboarding path.
- A narrow test caps README growth, rejects the former duplicate reference
  sections, and requires links to each canonical detailed page.
- The user-visible guidance change and corrected server paths are recorded in
  `CHANGELOG.md` under `[Unreleased]`.

## Evidence

The focused documentation-policy tests pass: seven onboarding/endpoint tests
and ten MkDocs navigation/orphan tests. They include URL-parser table cases,
recursive endpoint coverage, and onboarding-size guards:

```text
cargo test --all-features --test ci_config_tests docs_onboarding_shape -- --nocapture
cargo test --all-features --test ci_config_tests mkdocs_nav_validation -- --nocapture
```

Strict documentation rendering passes all 17 checks, including `mkdocs build
--strict`. Extracted compilable Rust snippets pass; deliberately incomplete and
platform-specific `rust,ignore` fragments remain excluded by policy. The exact
mandatory repository gate passes with zero warnings or failures:

```text
cargo fmt
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Two fresh independent adversarial reviews followed the final content changes.
The verifier and documentation UX reviewer both reported zero remaining
findings.

## Hosted Pull Request

[PR #121](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/pull/121)
opened non-draft and mergeable. Its first Docs Validation run `32450621351`
found one hosted-only Markdownlint failure: `docs/index.md` combined a YAML
`title` with its visible H1, which violates MD025. Removing the redundant YAML
title preserved the rendered page and made the replacement Docs Validation run
green.

All 12 pull-request workflows passed on code-bearing head
`792539205f8b1ac130fedc6ed1e2753f6903a0c8`:

| Workflow | Run |
| --- | ---: |
| Docs Validation | 32450770155 |
| No Panics | 32450769678 |
| Security | 32450769971 |
| Workflow Lint | 32450769817 |
| Unused Deps | 32450769844 |
| Coverage | 32450769783 |
| Examples Validation | 32450769872 |
| Semver Checks | 32450769831 |
| WASM | 32450769796 |
| CI | 32450769805 |
| Deep Safety | 32450769867 |
| Godot Web | 32450769908 |

Cursor Bugbot later found that the endpoint scanner treated `]` as a URL
terminator and therefore falsely rejected a valid bracketed-IPv6 host. The
final fix preserves an IPv6 authority bracket while trimming a prose-closing
bracket, with table regressions for both forms. The submitted hosted reviews
are this addressed Bugbot comment and two quota-exhausted Copilot notices; the
Copilot limitation remains tracked by issue #90.
