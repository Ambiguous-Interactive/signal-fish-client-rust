---
name: crate-publishing
description: Prepare and publish the lockstep Signal Fish Rust crates. Use when changing Cargo metadata, docs.rs configuration, cargo-deny policy, crate versions, package contents, or publishing workflows.
---

# Crate Publishing

Reference for Cargo metadata, docs.rs configuration, cargo-deny, and lockstep
publishing of the core and Godot adapter crates.

## Cargo.toml Metadata Checklist

```toml
[package]
name = "signal-fish-client"
version.workspace = true
publish = ["crates-io"]
edition = "2021"
rust-version = "1.87.0"          # MSRV — enforced by cargo
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
    "!tests/godot_adapter_policy_tests.rs",
    "Cargo.toml",
    "LICENSE",
    "README.md",
    "CHANGELOG.md",
]
```

The workspace owns the lockstep version and exact internal requirement:

```toml
[workspace.package]
version = "0.9.0"

[workspace.dependencies]
signal-fish-client = { version = "=0.9.0", path = ".", default-features = false }
```

The adapter inherits that version and dependency, uses Rust 1.94.0, and has its
own docs.rs URL:

```toml
[package]
name = "signal-fish-client-godot"
version.workspace = true
publish = ["crates-io"]
rust-version = "1.94.0"

[dependencies]
godot = { version = ">=0.4.5, <0.6", features = ["experimental-wasm", "experimental-wasm-nothreads", "lazy-function-tables"] }
signal-fish-client = { workspace = true, default-features = false, features = ["polling-client"] }
```

Never publish mismatched versions or a non-exact adapter-to-core requirement.
Internal publishable dependencies must inherit with `workspace = true`; an
inline exact version can look correct in Cargo metadata while becoming stale
when the workspace version is prepared.
The release plan keeps each workspace dependency key separate from its real
package name so a renamed entry is bumped by its manifest key and published by
its crates.io name.

- `keywords`: max 5, lowercase, hyphenated — used for crates.io search
- `categories`: must be from the official crates.io category list
- `include`: excludes `.llm/`, `scripts/`, `.github/`, target, and any
  repository-policy test that reads those paths from the published package
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
`doc_auto_cfg` was removed from rustdoc (its behavior merged into `doc_cfg`) and breaks
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

### Intra-doc links and target-gated types

Types behind `#[cfg(target_os = "emscripten")]` or similar target restrictions
are never in scope when building docs on a different host. Do **not** use
intra-doc link syntax (`[`TypeName`]`) for these types — use plain backtick
formatting (`\`TypeName\``) instead. See the *ci-configuration* skill for details.

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
allow = ["MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "ISC", "MPL-2.0", "Unicode-DFS-2016"]
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
cargo fmt && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features

# 2. Check deny policies
cargo deny check

# 3. Inspect the discovered dependency-first publication plan
python3 scripts/release.py workspace-plan

# 4. Package and dry-run all planned crates together (Rust 1.96.1)
cargo package -p signal-fish-client -p signal-fish-client-godot
cargo publish --dry-run -p signal-fish-client -p signal-fish-client-godot

# 5. Check docs build cleanly (stable + docs.rs nightly simulation)
cargo doc --all-features --no-deps
bash scripts/check-docsrs.sh

# 6. Verify the derived version and registry recovery plan
python3 scripts/release.py package-version
```

Cargo 1.90+ can package and publish multiple interdependent workspace members.
The workflows pin Rust 1.96.1, derive package arguments from `workspace-plan`,
and do not carry a special-case local patch for the adapter. Before publication,
`registry-plan` reproduces every `.crate`, verifies already-published checksums,
and returns only absent packages in dependency order. Attach every crate,
checksum, SBOM, and attestation to one tag and GitHub Release.

## CI Publishing Workflow

`.github/workflows/publish.yml` is manual-only, derives its version from the
workspace, requires the protected `crates-io` environment, and treats the tag
as an output rather than a trigger. Do not add a version input or tag-push
publication path.

## Versioning Workflow

```shell
# Preview and prepare a lockstep release.
python3 scripts/release.py release-intent
# In normal operations, dispatch Prepare Release instead; it creates the PR.
```

Prepare Release takes no version, bump, breaking, or crate-selection input.
`release-intent` derives the lockstep target from `[Unreleased]`: an explicit
`**Breaking:**` entry selects major (pre-1.0 minor), feature-facing Keep a
Changelog categories select minor, and Fixed/Security-only content selects
patch. The workflow discovers every publishable crate and explicitly dispatches
the checked-in required-check workflow list with its built-in token; never add a
release App or PAT to restore implicit pull-request events.

### Version bump checklist

A version bump must update **all** references, not just `Cargo.toml`:

- `Cargo.toml` (`[workspace.package].version` and exact workspace requirements)
- every publishable member continues to set `version.workspace = true`
- root and standalone fixture lockfile path-package versions
- `README.md` (dependency snippet, badge if present)
- `docs/getting-started.md` (dependency snippets)
- `docs/index.md` (dependency snippet)
- `docs/wasm.md` (dependency snippets)
- `docs/examples.md` (dependency snippet)
- `docs/client.md` (`sdk_version` example)
- `docs/protocol.md` (`sdk_version` JSON example)
- `.llm/context.md` (Version field)
- `.llm/skills/crate-publishing/SKILL.md` (Cargo.toml metadata example)

The `crate_version_consistency` tests in `tests/ci_config_tests.rs` catch
stale version references, but it is better to update them all in the same
commit as the bump.

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
- 50-variant `ErrorCode` enum

[Unreleased]: https://github.com/Ambiguous-Interactive/signal-fish-client-rust/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Ambiguous-Interactive/signal-fish-client-rust/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Ambiguous-Interactive/signal-fish-client-rust/releases/tag/v0.1.0
```

## Common Publishing Issues

| Issue | Fix |
|-------|-----|
| `package.include` too broad | Use `cargo package --list -p <package>` to verify |
| Private types in public API | Run `cargo doc` and check for `warning: public item not documented` |
| Feature not gated properly | Run `cargo check --no-default-features` |
| Core MSRV violation | Run `cargo +1.87.0 check -p signal-fish-client --all-features` |
| Adapter MSRV violation | Run `cargo +1.94.0 check -p signal-fish-client-godot` |
| Adapter unavailable during pre-publish verification | Verify extracted packages with `[patch.crates-io]`, then dry-run after core is visible |
| Yanked dependency | Update in Cargo.toml, run `cargo update` |
| License mismatch | Run `cargo deny check licenses` |

## Protocol v2/v3 (0.5.0) Notes

The `0.4.1 → 0.5.0` release adds the protocol v2/v3 mesh surface. Beyond the
standard version-sync locations, this release introduced new files to keep in
mind on future bumps:

- `tests/wire-samples/PROVENANCE.toml` — its `synced` date is human-maintained
  (refresh at release; see [protocol-wire-conformance](../protocol-wire-conformance/SKILL.md)).
- New skills: `protocol-versioning-and-negotiation.md`, `webrtc-mesh-signaling.md`,
  `../protocol-wire-conformance/SKILL.md` (auto-indexed; no version literal).
- New feature `mesh` (pure-std, zero deps). Concrete WebRTC backends (str0m,
  web-sys) are documented integrations, not bundled as heavy deps — the crate
  stays dependency-light.

Adding the new public enum variants/types is a breaking change, so this is a
MINOR bump for a 0.x crate; `cargo semver-checks` will (correctly) flag it.
