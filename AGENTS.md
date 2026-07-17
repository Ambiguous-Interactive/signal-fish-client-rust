# Signal Fish Client SDK — Agent Guidelines

This is the **Signal Fish Client SDK** by Ambiguous Interactive — a transport-agnostic Rust client for the Signal Fish multiplayer signaling protocol.

## Canonical Reference

Read `.llm/context.md` for the full project context (architecture, design decisions, dependencies, and conventions). That file is the authoritative source of truth for this repository.

## Mandatory Workflow

```shell
cargo fmt && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features
```

Run this before every commit. All three steps must pass with zero warnings.

## CI/CD Action Reference Policy

Use version tags in workflow `uses:` references, not commit hashes.

- Use: `owner/action@vN` or `owner/action@vN.N.N`
- Exception: `dtolnay/rust-toolchain@stable|nightly|beta`

## GitHub Tool Order

For every GitHub operation, follow
`.llm/skills/github-operations/SKILL.md`: prefer the VS Code GitHub
connector/extension first, use local `git` second, and use GitHub CLI (`gh`)
only as the final fallback. Missing `gh` authentication does not block a
connector- or `git`-capable workflow.

## Changelog Policy

For any user-visible change, update `CHANGELOG.md` in the same PR under
`## [Unreleased]`, following Keep a Changelog categories.

## Skills

Focused Agent Skills live in `.llm/skills/<name>/SKILL.md`; the generated
`.llm/skills/index.md` catalogs them. When a task matches a skill's frontmatter
description, read its complete `SKILL.md` before acting and resolve relative
resources from that skill directory.

Key skills: `async-rust-patterns`, `shared-core-drivers`, `transport-abstraction`, `websocket-client`, `error-handling`, `serde-patterns`, `testing-async`, `public-api-design`, `tracing-instrumentation`, `crate-publishing`, `github-operations`, `changelog-discipline`, `keep-a-changelog-format`.
