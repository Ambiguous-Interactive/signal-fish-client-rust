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

Always validate `.lychee.toml` with a TOML parser before committing. The `scripts/ci-validate.sh` script includes automated TOML validation.

### lychee: Avoid flaky external docs for badges

Some external docs/blog hosts intermittently return `503` in CI even when links are valid. This creates nondeterministic failures in link-check jobs.

For MSRV badges and similar stable references, prefer canonical, long-lived documentation pages over blog posts:

- Prefer: `https://doc.rust-lang.org/stable/releases.html#...`
- Avoid in badges: `https://blog.rust-lang.org/...`

Regression policy:

- Keep `README.md` and `docs/index.md` MSRV links pointed at
  `doc.rust-lang.org/stable/releases.html`.
- `tests/ci_config_tests.rs` enforces this to prevent flaky link-check regressions.

### ShellCheck SC2317 and trap handlers

Functions used as `trap` handlers (`trap cleanup EXIT`) appear unreachable to
ShellCheck because they are called indirectly by the shell, not by any visible
call site. Suppress with a comment explaining why:

```bash
# shellcheck disable=SC2317  # called indirectly via trap
cleanup() {
    rm -rf "$TMPDIR"
}
trap cleanup EXIT
```

Do not use ` -- `, ` — `, or ` – ` on the directive line; keep rationale in a second comment segment using ` # ` so ShellCheck parses the directive reliably.

### ShellCheck SC2004 and array indexes

Array indexes in Bash are arithmetic context. Do not prefix index variables with
`$` inside `[...]` or ShellCheck will flag SC2004. This applies to both reads and writes.

```bash
# WRONG
if [ "${PHASE_RESULTS[$phase]}" != "FAIL" ]; then
    :
fi
PHASE_RESULTS[$phase]="FAIL"

# CORRECT
if [ "${PHASE_RESULTS[phase]}" != "FAIL" ]; then
    :
fi
PHASE_RESULTS[phase]="FAIL"
```

This pitfall is enforced by `tests/ci_config_tests.rs::ci_config_validation::check_all_script_avoids_shellcheck_sc2004_array_index_style`.

### cargo-machete false positives with serde attributes

Dependencies used only via `#[serde(with = "...")]` attributes (e.g., `serde_bytes`) are invisible to cargo-machete's static analysis because no `use` or `extern crate` statement references them. Add such crates to the ignore list in `Cargo.toml`:

```toml
[package.metadata.cargo-machete]
ignored = ["serde_bytes"]
```

### semver-checks on new crates

`cargo semver-checks` compares the current API against the base branch. When the base branch does not contain the crate at all (e.g., the initial PR for a new package), the tool will fail because there is no baseline to diff against. The CI workflow must check for package existence on the base branch before running semver-checks.

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

Keep comments in CI shell scripts behaviorally exact:

- Match stated scope (for example, `.rs` only vs docs/code blocks).
- If comments mention both `#![allow(...)]` and `#[allow(...)]`, checks must handle both (for example `grep -qE '#!?\[allow\('`).
- Remember `grep` is line-based; validate multi-line attributes with staged checks.
- Avoid broad patterns (`grep -q '#\[allow('`) when you intend a specific lint.

### Shell scripts: Guard subsequent logic after extraction failures

When a shell script extracts a value (for example from `awk`/`grep`) and
validates extraction, all dependent comparisons must stay inside the success
branch (`else`) or return early (`continue`/`exit`). Avoid fall-through that
produces confusing mismatch logs with empty values.

This pattern is enforced by the regression test
`ci_config_tests.rs::workflow_security::check_workflows_script_guards_empty_cargo_msrv`.

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

Prevention checks in this repository:

- `tests/ci_config_tests.rs::ci_workflow_policy::ci_msrv_matches_cargo_toml`
  validates the extracted `msrv` job block, not generic substring matches.
- `tests/ci_config_tests.rs::ci_workflow_policy::msrv_toolchain_step_regressions_are_caught`
  includes regression cases for semver-like refs (`@1.85.0`) and missing `with.toolchain`.
- `scripts/check-workflows.sh` fails fast on problematic
  `dtolnay/rust-toolchain@<digits-and-dots>` usage with actionable remediation text.

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
