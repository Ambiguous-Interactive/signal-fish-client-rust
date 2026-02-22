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

Always validate `.lychee.toml` with a TOML parser before committing. The
`scripts/ci-validate.sh` script includes automated TOML validation.

### ShellCheck SC2317 and trap handlers

Functions used as `trap` handlers (`trap cleanup EXIT`) appear unreachable to
ShellCheck because they are called indirectly by the shell, not by any visible
call site. Suppress with a comment explaining why:

```bash
# shellcheck disable=SC2317 — called indirectly via trap
cleanup() {
    rm -rf "$TMPDIR"
}
trap cleanup EXIT
```

### cargo-machete false positives with serde attributes

Dependencies used only via `#[serde(with = "...")]` attributes (e.g.,
`serde_bytes`) are invisible to cargo-machete's static analysis because no
`use` or `extern crate` statement references them. Add such crates to the
ignore list in `Cargo.toml`:

```toml
[package.metadata.cargo-machete]
ignored = ["serde_bytes"]
```

### semver-checks on new crates

`cargo semver-checks` compares the current API against the base branch. When
the base branch does not contain the crate at all (e.g., the initial PR for a
new package), the tool will fail because there is no baseline to diff against.
The CI workflow must check for package existence on the base branch before
running semver-checks.

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

### typos: False positives on variable names

The `typos` spell checker may flag variable names as misspellings. Use
`[default.extend-identifiers]` for identifier-level suppressions:

```toml
[default.extend-identifiers]
# Variable name in test destructuring (player_name abbreviation)
pn = "pn"
```

Use `[default.extend-words]` for word-level suppressions (affects all
contexts, not just identifiers).

### Shell scripts: Comments must match behavior

When writing CI shell scripts, ensure that **every comment accurately describes
what the code does**. Common pitfalls:

- **Scope claims:** A comment saying "scans documentation code blocks" when the
  script only scans `.rs` files. If markdown validation is handled by a separate
  script, say so explicitly.
- **Attribute syntax:** If a comment says both `#![allow(...)]` (file-level) and
  `#[allow(...)]` (module-level) are accepted, the `grep` check must match both
  forms. Use `grep -qE '#!?\[allow\('` to make the `!` optional.
- **Multi-line attributes:** `grep` matches one line at a time. A regex like
  `#!\[allow\(.*clippy::unwrap_used` will fail on multi-line `#![allow( ... )]`
  blocks. Split into two passes: one to check for the attribute open, one to
  check for the specific lint name.
- **Overly broad checks:** `grep -q '#\[allow('` matches *any* allow attribute,
  not just panic-related ones. Use a second grep to verify a specific lint name
  is present (e.g., `clippy::unwrap_used`).

### Shell scripts: Use REPO_ROOT for path resolution

Scripts that use relative paths (e.g., `src`, `tests`) will silently fail if
invoked from the wrong directory. Always resolve the repo root:

```bash
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"
```

This matches the pattern used in `scripts/extract-rust-snippets.sh` and
`scripts/check-no-panics.sh`.

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

Avoid this anti-pattern:

```yaml
- uses: dtolnay/rust-toolchain@1.85.0
```

Prevention checks in this repository:

- `tests/ci_config_tests.rs::ci_workflow_policy::ci_msrv_matches_cargo_toml`
  validates the extracted `msrv` job block, not generic substring matches.
- `tests/ci_config_tests.rs::ci_workflow_policy::msrv_toolchain_step_regressions_are_caught`
  includes regression cases for `@1.*` refs and missing `with.toolchain`.
- `scripts/check-workflows.sh` fails fast on problematic
  `dtolnay/rust-toolchain@1.*` usage with actionable remediation text.

## Validation Scripts

### Failure triage checklist

Start with the first command in the matching row to localize failures quickly.

| Symptom in CI | First command/script to run |
|---|---|
| Workflow YAML/action pin/toolchain policy failure | `bash scripts/check-workflows.sh` |
| CI policy test failure in `tests/ci_config_tests.rs` | `cargo test --test ci_config_tests -- --nocapture` |
| Formatting/clippy/test drift vs required local workflow | `cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features` |
| Broken docs snippet extraction or markdown validation flow | `bash scripts/extract-rust-snippets.sh` then `bash scripts/ci-validate.sh` |

### `scripts/ci-validate.sh`

Lightweight local CI validation covering the most common failure points:

1. `cargo fmt --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test --all-features`
4. `typos --config .typos.toml` (optional)
5. `.lychee.toml` TOML syntax validation
6. `.markdownlint.json` JSON syntax validation

### `scripts/check-all.sh`

Full 17-phase CI parity script. Use `--quick` for the mandatory baseline
(phases 1-3 only).

### `scripts/install-hooks.sh`

Installs pre-commit and pre-push hooks. Pre-commit runs: LLM line limits,
cargo fmt, cargo clippy, typos (optional). Pre-push runs: cargo test.

## CI Config Tests

`tests/ci_config_tests.rs` contains data-driven tests that prevent
configuration drift:

| Test | What it validates |
|------|-------------------|
| `workflow_existence` | Required workflow files exist |
| `config_existence` | Config files (`.typos.toml`, etc.) exist |
| `script_existence` | Required scripts exist |
| `ci_msrv_matches_cargo_toml` | CI MSRV job matches `Cargo.toml` |
| `msrv_consistent_across_key_files` | Docs reference same MSRV as `Cargo.toml` |
| `all_workflows_have_permissions` | Every workflow declares `permissions` |
| `all_jobs_have_timeout` | Every job has `timeout-minutes` |
| `action_references_are_sha_pinned` | Actions use SHA pins (except dtolnay) |
| `index_md_uses_asterisk_emphasis` | Generated index.md uses `*` not `_` for emphasis |
| `pre_commit_script_footer_uses_asterisk` | Script string literals use `*` emphasis |

## Debugging CI Failures

### Workflow: Spell Check fails

1. Run `typos --config .typos.toml` locally
2. If false positive on a word: add to `[default.extend-words]`
3. If false positive on a variable name: add to `[default.extend-identifiers]`
4. If legitimate typo: fix the spelling

### Workflow: Link Check fails

1. Run `lychee --config .lychee.toml "**/*.md"` locally
2. If external URL is flaky: add pattern to `exclude` in `.lychee.toml`
3. If URL does not exist yet (pre-publish): add to `exclude`
4. If config syntax error: validate TOML with `python3 -c "import tomllib; ..."`

### Workflow: Markdownlint fails

1. Run `markdownlint-cli2 "**/*.md"` locally
2. If new rule is too strict: disable in `.markdownlint.json`
3. If formatting issue: fix the markdown
4. Check markdownlint version — new versions add new rules

### Workflow: MSRV fails

1. Check `Cargo.toml` `rust-version` value
2. Run `cargo +<msrv> build --all-features` locally
3. If dependency requires newer Rust: bump MSRV and update all references
4. Run `cargo test msrv_consistent` to verify no drift
