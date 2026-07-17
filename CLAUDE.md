# Signal Fish Client SDK — Claude AI Guidelines

This is the **Signal Fish Client SDK** by Ambiguous Interactive — a transport-agnostic Rust client for the Signal Fish multiplayer signaling protocol.

## Canonical Reference

Read `.llm/context.md` for the full project context (architecture, design decisions, dependencies, and conventions).

## Mandatory Workflow

```shell
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features
```

Run this before every commit. All three steps must pass with zero warnings.

## GitHub Tool Order

For every GitHub operation, follow
`.llm/skills/github-operations/SKILL.md`: prefer the VS Code GitHub
connector/extension first, use local `git` second, and use GitHub CLI (`gh`)
only as the final fallback. Missing `gh` authentication does not block a
connector- or `git`-capable workflow.

## Skills

Focused Agent Skills live in `.llm/skills/<name>/SKILL.md`; the generated
`.llm/skills/index.md` catalogs them. When a task matches a skill's frontmatter
description, read its complete `SKILL.md` before acting and resolve relative
resources from that skill directory.

Key skills: `async-rust-patterns`, `transport-abstraction`, `websocket-client`, `error-handling`, `serde-patterns`, `testing-async`, `public-api-design`, `tracing-instrumentation`, `crate-publishing`, `github-operations`, `ci-configuration`, `markdown-and-doc-validation`.
