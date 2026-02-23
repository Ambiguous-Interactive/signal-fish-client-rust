# Crate Publishing

Reference for Cargo.toml metadata, docs.rs configuration, deny.toml, cargo-deny, and CI publishing.

## Cargo.toml Metadata Checklist

```toml
[package]
name = "signal-fish-client"
version = "0.2.0"
edition = "2021"
rust-version = "1.85.0"          # MSRV — enforced by cargo
license = "MIT"                   # SPDX identifier
authors = ["Ambiguous Interactive <eli@theambiguous.co>"]
description = "Transport-agnostic Rust client for the Signal Fish multiplayer signaling protocol"
repository = "https://github.com/Ambiguous-Interactive/signal-fish-client-rust"
homepage = "https://Ambiguous-Interactive.github.io/signal-fish-client-rust/"
documentation = "https://docs.rs/signal-fish-client"
readme = "README.md"
keywords = ["gamedev", "signaling", "multiplayer", "networking", "matchmaking"]
categories = ["game-engines", "network-programming"]
include = [
    "src/**/*",
    "examples/**/*",
    "tests/**/*",
    "Cargo.toml",
    "LICENSE",
    "README.md",
    "CHANGELOG.md",
]
```

- `keywords`: max 5, lowercase, hyphenated — used for crates.io search
- `categories`: must be from the official crates.io category list
- `include`: excludes `.llm/`, `scripts/`, `.github/`, target from the published package
- `homepage`: project website / user guide (GitHub Pages URL)
- `documentation`: API reference (docs.rs URL)
- `.llm/context.md` must list **both** URLs separately so that LLM
  agents can distinguish the user guide from the API docs

## docs.rs Configuration

```toml
[package.metadata.docs.rs]
all-features = true
rustdoc-args = ["--cfg", "docsrs"]
```

Use `docsrs` flag to annotate feature-gated items in docs:

```rust
#[cfg_attr(docsrs, doc(cfg(feature = "transport-websocket")))]
pub struct WebSocketTransport { /* ... */ }
```

Do not use `#![cfg_attr(docsrs, feature(doc_auto_cfg))]` in crate roots.
`doc_auto_cfg` was removed in Rust 1.92 (merged into `doc_cfg`) and breaks
docs.rs nightly builds.

## Local Documentation Build

```shell
# Build docs with all features, open in browser
RUSTDOCFLAGS="--cfg docsrs" cargo doc --all-features --open --no-deps

# Simulate docs.rs nightly behavior exactly as CI does
bash scripts/check-docsrs.sh

# Check for broken doc links
cargo doc --all-features 2>&1 | grep "warning\|error"
```

## cargo-deny Configuration (deny.toml)

`deny.toml` at crate root enforces license, security, and duplicate dependency policies:

```toml
[graph]
targets = []  # check all targets

[advisories]
db-path = "~/.cargo/advisory-db"
db-urls = ["https://github.com/rustsec/advisory-db"]
vulnerability = "deny"
unmaintained = "warn"
yanked = "deny"

[licenses]
allow = ["MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "ISC", "Unicode-DFS-2016"]
deny = ["GPL-3.0"]
copyleft = "warn"

[bans]
multiple-versions = "warn"
wildcards = "allow"
deny = [
    # No chrono (use String timestamps)
    { name = "chrono" },
    # No bytes (use Vec<u8>)
    { name = "bytes" },
]

[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
```

Run: `cargo deny check`

## Pre-publish Checklist

```shell
# 1. Verify mandatory workflow passes
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features

# 2. Check deny policies
cargo deny check

# 3. Verify what will be published
cargo package --list

# 4. Do a dry run
cargo publish --dry-run --allow-dirty

# 5. Check docs build cleanly (stable + docs.rs nightly simulation)
cargo doc --all-features --no-deps
bash scripts/check-docsrs.sh

# 6. Verify version in Cargo.toml matches tag
grep '^version' Cargo.toml
```

## CI Publishing Workflow

Example GitHub Actions workflow (`.github/workflows/publish.yml`):

```yaml
name: Publish

on:
  push:
    tags:
      - 'v*'

jobs:
  publish:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - uses: dtolnay/rust-toolchain@stable

      - name: Run tests
        run: cargo test --all-features

      - name: Check deny
        uses: EmbarkStudios/cargo-deny-action@v2

      - name: Publish to crates.io
        env:
          CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}
        run: cargo publish
```

## Versioning Workflow

```shell
# Bump version (0.1.0 → 0.2.0)
# 1. Update version in Cargo.toml
# 2. Update CHANGELOG.md
# 3. Commit: "chore: release 0.2.0"
# 4. Tag: git tag -s v0.2.0 -m "Release 0.2.0"
# 5. Push: git push && git push --tags
# CI then publishes automatically
```

## CHANGELOG.md Format

Follow [Keep a Changelog](https://keepachangelog.com/):

```markdown
## [Unreleased]

## [0.2.0] — 2024-02-22
### Changed
- `SignalFishError::ServerError.error_code` now uses `Option<ErrorCode>`
- Added migration guidance for error-code handling

## [0.1.0] — 2024-01-15
### Added
- Initial release
- `SignalFishClient` with WebSocket transport
- 26-variant `SignalFishEvent` enum
- 40-variant `ErrorCode` enum

[Unreleased]: https://github.com/Ambiguous-Interactive/signal-fish-client-rust/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Ambiguous-Interactive/signal-fish-client-rust/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Ambiguous-Interactive/signal-fish-client-rust/releases/tag/v0.1.0
```

## Common Publishing Issues

| Issue | Fix |
|-------|-----|
| `package.include` too broad | Use `cargo package --list` to verify |
| Private types in public API | Run `cargo doc` and check for `warning: public item not documented` |
| Feature not gated properly | Run `cargo check --no-default-features` |
| MSRV violation | Run `cargo +1.85.0 check --all-features` |
| Yanked dependency | Update in Cargo.toml, run `cargo update` |
| License mismatch | Run `cargo deny check licenses` |
