# CI Configuration

Reference for CI/CD tool configuration, common pitfalls, and consistency enforcement in this crate.

## Config File Inventory

| File | Tool | Format | Purpose |
|------|------|--------|---------|
| `.typos.toml` | typos-cli | TOML | Spell checking config with locale and suppressions |
| `.lychee.toml` | lychee | TOML | Link checker config with URL exclusions and retry settings |
| `.markdownlint.json` | markdownlint-cli2 | JSON | Markdown linting rules (disable/enable per rule) |
| `.yamllint.yml` | yamllint | YAML | YAML lint config for GitHub Actions workflows |
| `deny.toml` | cargo-deny | TOML | License, advisory, ban, and source policies |
| `.markdownlint-cli2.jsonc` | markdownlint-cli2 | JSONC | Directory ignores for markdownlint |

## Common Pitfalls

### GitHub Actions refs: tags, not commit hashes

Use `uses: owner/action@vN` (or `@vN.N.N`) and do not use commit-SHA refs. Only `dtolnay/rust-toolchain` may use channels (`@stable`, `@nightly`, `@beta`); policy is enforced by `scripts/check-workflows.sh` and `tests/ci_config_tests.rs`.

### lychee: TOML vs CLI syntax

The lychee link checker has different syntax for TOML config vs CLI flags.

```toml
# WRONG — "2xx" shorthand only works on CLI, not in TOML
accept = ["2xx", "429"]

# CORRECT — use inclusive range syntax in TOML config
accept = ["200..=299", "429"]
```

The `header` field must be a TOML **map**, not an array:

```toml
# WRONG — array syntax
header = ["Accept: text/html"]

# CORRECT — map syntax
[header]
Accept = "text/html"
```

Always validate `.lychee.toml` with a TOML parser before committing. The `scripts/ci-validate.sh` script includes automated TOML validation.

### lychee: Avoid flaky external docs for badges

Some external docs/blog hosts intermittently return `503` in CI. For MSRV
badges, prefer `https://doc.rust-lang.org/stable/releases.html#...` over
`https://blog.rust-lang.org/...`. Keep `README.md` and `docs/index.md` MSRV
links pointed at `doc.rust-lang.org/stable/releases.html`.
`tests/ci_config_tests.rs` enforces this to prevent flaky link-check regressions.

### ShellCheck SC2317 and trap handlers

Functions used as `trap` handlers appear unreachable to ShellCheck. Suppress
with `# shellcheck disable=SC2317  # called indirectly via trap`. Keep
rationale in a second `#` segment (no em-dashes) so ShellCheck parses the
directive reliably.

### ShellCheck SC2004 and array indexes

Array indexes in Bash are arithmetic context. Do not prefix index variables with `$` inside `[...]` or ShellCheck will flag SC2004. This applies to both reads and writes: `${PHASE_RESULTS[phase]}` not `${PHASE_RESULTS[$phase]}`, and `PHASE_RESULTS[phase]="FAIL"` not `PHASE_RESULTS[$phase]="FAIL"`. Enforced by `ci_config_tests.rs::ci_config_validation::check_all_script_avoids_shellcheck_sc2004_array_index_style`.

### SC2001 — prefer parameter expansion over `echo | sed`

`echo "$var" | sed 's/pat/rep/'` triggers SC2001 when expressible with parameter expansion (e.g., `trimmed="${code#"${code%%[![:space:]]*}"}"` instead of `echo "$code" | sed 's/^[[:space:]]*//'`). Multi-stage `sed` pipelines and `printf '%s' "$var" | sed ...` do not trigger the warning.

### Intra-doc links to target-gated types

Types gated on `target_os = "emscripten"` are never in scope on Linux CI hosts.
Use `` `TypeName` `` (plain backticks) not `` [`TypeName`] `` (intra-doc link). Enforced by `docsrs_policy` tests in `ci_config_tests.rs`.

### cargo-machete false positives with serde attributes

Dependencies used only via `#[serde(with = "...")]` attributes (e.g., `serde_bytes`) are invisible to cargo-machete. Add them to `[package.metadata.cargo-machete] ignored` in `Cargo.toml`.

### semver-checks on new crates

`cargo semver-checks` fails when the base branch does not contain the crate (no baseline to diff). The CI workflow must check for package existence before running semver-checks.

### markdownlint: Emphasis conventions

Use **asterisks** for emphasis (`*text*`, `**text**`), not underscores. This
avoids MD049/MD050 violations with the default markdownlint configuration.

This applies to **auto-generated markdown too**. The `scripts/pre-commit-llm.py`
script generates `.llm/skills/index.md` — its footer must use `*...*` (asterisk),
not `_..._` (underscore). Regression tests enforce this in both
`tests/ci_config_tests.rs` (`llm_index_validation` module) and
`scripts/test_pre_commit_llm.py` (`TestGenerateIndex` class).

Bold text that acts as a section heading should be converted to a proper
heading (`###`, `####`) rather than using `**Heading**` on its own line.

### markdownlint: Heading spacing (MD022)

Markdown headings must be surrounded by blank lines (leave an empty line before
and after each heading block).

This rule is enforced in CI by markdownlint and by
`tests/ci_config_tests.rs::markdown_policy_validation`.

### markdownlint: List spacing (MD032)

Markdown lists must be surrounded by blank lines. If a paragraph introduces a
list (for example ending with a colon), add an empty line before the first list
item.

This is enforced in CI by markdownlint and by
`tests/ci_config_tests.rs::markdown_policy_validation::list_introduction_lines_require_blank_spacing_before_list_items`.

### markdownlint: New rules in updates

When `markdownlint-cli2` is updated in CI (e.g., via Dependabot or
`@latest` tag), new rules may be introduced that cause mass failures.

Example: markdownlint v0.40.0 added MD060 (table column style) which
generated 300+ violations across existing tables.

**Strategy:** Disable overly strict new rules in `.markdownlint.json`
rather than reformatting all existing content:

```json
{
    "MD058": false,
    "MD060": false
}
```

Review new rules individually and enable only those that add genuine value.

### typos: US English locale and false positives

The project uses `locale = "en-us"` in `.typos.toml`. Use American English
spellings in code, comments, and docs (e.g., "queuing" not "queueing").

The `typos` spell checker may flag variable names as misspellings. Use
`[default.extend-identifiers]` for identifier-level suppressions (e.g.,
`pn = "pn"`) and `[default.extend-words]` for word-level suppressions.

### Shell scripts: Comments must match behavior

Keep comments in CI shell scripts behaviorally exact:

- Match stated scope (for example, `.rs` only vs docs/code blocks).
- If comments mention both `#![allow(...)]` and `#[allow(...)]`, checks must handle both (for example `grep -qE '#!?\[allow\('`).
- Remember `grep` is line-based; validate multi-line attributes with staged checks.
- Avoid broad patterns (`grep -q '#\[allow('`) when you intend a specific lint.

### check-no-panics.sh: Compound cfg(test) attributes

`check-no-panics.sh` uses `#\[cfg(.*\btest\b` to match both `#[cfg(test)]`
and compound forms like `#[cfg(all(test, feature = "..."))]`. When adding test
modules in `src/`, prefer plain `#[cfg(test)]` over compound attributes.

### Shell scripts: Guard subsequent logic after extraction failures

When a shell script extracts a value (e.g., from `awk`/`grep`) and validates extraction, all dependent comparisons must stay inside the success branch or return early (`continue`/`exit`). Enforced by `ci_config_tests.rs::workflow_security::check_workflows_script_guards_empty_cargo_msrv`.

### Shell scripts: Use REPO_ROOT for path resolution

Scripts that use relative paths (e.g., `src`, `tests`) will silently fail if
invoked from the wrong directory. Always resolve the repo root:

```bash
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"
```

### Shell scripts: CRLF line endings break Bash parsing

Bash `read -r` strips `\n` but preserves `\r` from Windows CRLF files. Always strip `\r` when reading files line-by-line: `line="${line//$'\r'/}"`. Also add `| tr -d '\r'` to `awk`/`sed` pipelines processing file content. Include a `.gitattributes` with `* text=auto eol=lf` in the repo root to prevent CRLF issues in CI and cross-platform development.

### MSRV drift

When bumping the MSRV in `Cargo.toml`, many other files reference the
version and must be updated in sync:

- `Cargo.toml` (authoritative source: `rust-version`)
- `.github/workflows/ci.yml` (MSRV job)
- `README.md` (badge + text)
- `docs/index.md` (badge)
- `docs/getting-started.md` (prerequisites)
- `.llm/context.md`
- `.llm/skills/public-api-design.md`
- `.llm/skills/crate-publishing.md`
- `.llm/skills/async-rust-patterns.md`
- `.devcontainer/Dockerfile`
- `scripts/check-all.sh`

The `ci_config_tests.rs::ci_workflow_policy::msrv_consistent_across_key_files`
test enforces consistency between `Cargo.toml` and key documentation files.

### MSRV and transitive dependencies

A common MSRV breakage pattern: a transitive dependency publishes a new
version requiring a newer Rust edition or language features. Example:

- `getrandom 0.4.1` requires `edition = "2024"` (Rust 1.85.0+)
- The crate itself uses `edition = "2021"` but cannot build on older Rust

**Fix:** Bump the MSRV to the minimum version that can compile all
transitive dependencies. Use `cargo generate-lockfile` + `--locked` in CI
for reproducible MSRV testing:

```yaml
- uses: dtolnay/rust-toolchain@stable
  with:
    toolchain: 1.85.0
- run: cargo generate-lockfile
- run: cargo build --locked --all-features
- run: cargo test --locked --all-features
```

### MSRV workflow incident: dtolnay ref vs explicit toolchain

`dtolnay/rust-toolchain` action refs are action release refs, **not** Rust
toolchain versions. A ref like `@1.100.0` can exist while being unrelated to
the intended MSRV and silently run a newer compiler than expected.

Use this pattern for MSRV jobs:

```yaml
- uses: dtolnay/rust-toolchain@stable
  with:
    toolchain: <msrv-from-Cargo.toml>
```

Enforced by `ci_config_tests.rs::ci_workflow_policy` tests
(`ci_msrv_matches_cargo_toml`, `msrv_toolchain_step_regressions_are_caught`)
and `scripts/check-workflows.sh`.

### Documentation drift on quantitative claims

Scripts like `check-all.sh` define phase counts and `--quick` boundaries referenced in this file. Update docs in the same commit as script changes. Enforced by `ci_config_tests.rs::check_all_documentation_accuracy` tests (total phase count, quick phase count, PHASE_NAMES vs TOTAL_PHASES).

### Path-based exclusions in tests: check full path components

When test code skips files by module name (e.g., excluding `emscripten_websocket`),
match against **all path components**, not just the filename. Filename-only matching
(`path.file_name().starts_with(...)`) breaks when a flat module file is refactored
into a directory module. Use `path.components().any(|c| ...)` instead:

```rust
if path.components().any(|c| {
    c.as_os_str().to_str().is_some_and(|s| {
        s == "emscripten_websocket" || s == "emscripten_websocket.rs"
    })
}) {
    continue;
}
```

### Action version pinning: major-only vs patch-level

Major-version tags like `@v2` are mutable (supply-chain risk). Prefer
patch-level pinning (e.g., `@v2.8.2`). Exceptions (keep in sync with
`scripts/check-workflows.sh` Phase 7 `MAJOR_ONLY_EXCEPTIONS` array):

- `dtolnay/rust-toolchain` — uses channels (`@stable`, `@nightly`, `@beta`)
- `mymindstorm/setup-emsdk` — only publishes major-version tags
- `taiki-e/install-action` — releases near-daily; patch pins go stale fast

Phase 7 emits non-blocking warnings for major-only pins. Verified by
`ci_config_tests.rs::workflow_security::check_workflows_script_detects_major_only_version_tags`.

### Documentation accuracy for WASM target capabilities

`SignalFishClient::start()` requires `tokio::spawn` — unavailable on any WASM
target. The correct client for all WASM targets is `SignalFishPollingClient`
(requires `polling-client` feature). Cross-reference capability claims in tables
against `docs/wasm.md` "What you do not get" sections to avoid contradictions.

## Validation Scripts

### Failure triage checklist

Start with the first command in the matching row to localize failures quickly.

| Symptom in CI | First command/script to run |
|---|---|
| Workflow YAML/action pin/toolchain policy failure | `bash scripts/check-workflows.sh` |
| CI policy test failure in `tests/ci_config_tests.rs` | `cargo test --test ci_config_tests -- --nocapture` |
| Formatting/clippy/test drift vs required local workflow | `cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features` |
| Broken docs snippet extraction or markdown validation flow | `bash scripts/extract-rust-snippets.sh` then `bash scripts/ci-validate.sh` |
| Unresolved intra-doc link (`rustdoc::broken_intra_doc_links`) | `RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps` — check for target-gated types needing plain backtick formatting |

### `scripts/ci-validate.sh`

Lightweight local CI validation: fmt check, clippy, test, typos, TOML/JSON syntax validation.

### `scripts/check-all.sh`

Full 18-phase CI parity script. Use `--quick` for the mandatory baseline (phases 1-4: fmt, FFI safety, clippy, test).
