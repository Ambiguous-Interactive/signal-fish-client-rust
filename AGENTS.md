# Signal Fish Client SDK — Agent Guidelines

This is the **Signal Fish Client SDK** by Ambiguous Interactive — a transport-agnostic Rust client for the Signal Fish multiplayer signaling protocol.

## Canonical Reference

Read `.llm/context.md` for the full project context (architecture, design decisions, dependencies, and conventions). That file is the authoritative source of truth for this repository.

## Mandatory Workflow

```shell
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features
```

Run this before every commit. All three steps must pass with zero warnings.

## Skills

Focused reference guides live in `.llm/skills/`. See `.llm/skills/index.md` for a full listing.

Key skills: `async-rust-patterns`, `transport-abstraction`, `websocket-client`, `error-handling`, `serde-patterns`, `testing-async`, `public-api-design`, `tracing-instrumentation`, `crate-publishing`.
