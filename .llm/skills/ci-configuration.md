# CI Configuration

Reference for CI/CD tool configuration, common pitfalls, identifier boundary matching in code scanners, and consistency enforcement in this crate.

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

The `header` field must be a TOML **array** of `"key=value"` strings (using `=`, not `:`), not a map:

```toml
# WRONG — map syntax (lychee v0.18+ rejects with "invalid type: map, expected a sequence")
[header]
Accept = "text/html"

# WRONG — colon syntax (lychee v0.18+ requires key=value)
header = ["Accept: text/html"]

# CORRECT — array of key=value strings
header = ["Accept=text/html"]
```

### lychee: Avoid flaky external docs for badges

Some external docs/blog hosts intermittently return `503` in CI. For MSRV badges, prefer `https://doc.rust-lang.org/stable/releases.html#...` over `https://blog.rust-lang.org/...`. Keep `README.md` and `docs/index.md` MSRV links pointed at `doc.rust-lang.org/stable/releases.html`. Enforced by `tests/ci_config_tests.rs`.

### ShellCheck SC2317 and trap handlers

Functions used as `trap` handlers appear unreachable to ShellCheck. Suppress with `# shellcheck disable=SC2317  # called indirectly via trap`. Keep rationale in a second `#` segment (no em-dashes) so ShellCheck parses the directive.

### ShellCheck SC2004 and array indexes

Array indexes in Bash are arithmetic context. Do not prefix index variables with `$` inside `[...]` or ShellCheck will flag SC2004. This applies to both reads and writes: `${PHASE_RESULTS[phase]}` not `${PHASE_RESULTS[$phase]}`, and `PHASE_RESULTS[phase]="FAIL"` not `PHASE_RESULTS[$phase]="FAIL"`. Enforced by `ci_config_tests.rs::ci_config_validation::check_all_script_avoids_shellcheck_sc2004_array_index_style`.

### SC2001 — prefer parameter expansion over `echo | sed`

`echo "$var" | sed 's/pat/rep/'` triggers SC2001 when expressible with parameter expansion (e.g., `trimmed="${code#"${code%%[![:space:]]*}"}"` instead of `echo "$code" | sed 's/^[[:space:]]*//'`). Multi-stage `sed` pipelines and `printf '%s' "$var" | sed ...` do not trigger the warning.

### Intra-doc links to target-gated types

Types gated on `target_os = "emscripten"` are never in scope on Linux CI hosts. Use `` `TypeName` `` (plain backticks) not `` [`TypeName`] `` (intra-doc link). Enforced by `docsrs_policy` tests in `ci_config_tests.rs`.

### Unused dependency detection: cargo-machete vs cargo-udeps

`cargo-machete` uses heuristic grep-based detection; `cargo-udeps` uses build-based analysis. Machete is fast but may miss deps that udeps catches (e.g., a dev-dependency like `tokio-test` listed but never imported). Always treat `cargo-udeps` as authoritative. Dependencies used only via `#[serde(with = "...")]` attributes (e.g., `serde_bytes`) are invisible to machete -- add them to `[package.metadata.cargo-machete] ignored` in `Cargo.toml`. Dev-dependencies go stale when code is refactored (e.g., switching from `tokio-test` utilities to a custom `MockTransport`). The `dev_dependency_usage` tests in `ci_config_tests.rs` verify every `[dev-dependencies]` entry is actually referenced in test code. The `uuid` duplicate-package warning in `cargo-udeps` output is a benign Cargo resolver artifact from platform-specific feature overrides and can be ignored.

### Identifier boundary matching in code scanners

Simple substring checks (`line.contains(name)`) produce false positives when one name is a prefix of another (e.g., `tokio` matches `tokio_tungstenite`). Always enforce word boundaries: the character before and after the match must not be `[A-Za-z0-9_]`. Use `line.match_indices(ident)` and check the adjacent bytes. Canonical implementation: `ci_config_tests.rs::line_references_crate`. See also `skills/source-code-scanning.md` for raw-string handling, recursive directory traversal, and the `strip_non_code()` function.

### Dual-listed dependency awareness

Dev-dependencies that also appear in `[dependencies]` are found in `src/` via the regular dependency entry. Scanning `src/` for such crates always produces false positives. Only scan test-context directories (`tests/`, `examples/`, `benches/`) for dual-listed deps. Deps that elude the scanner for any reason go in `DEV_DEP_USAGE_EXCEPTIONS` with a reason string. Enforced by `ci_config_tests.rs::dev_dependency_usage`.

### semver-checks on new crates

`cargo semver-checks` fails when the base branch lacks the crate. The CI workflow must check for package existence before running semver-checks.

### markdownlint: Emphasis conventions

Use **asterisks** for emphasis (`*text*`, `**text**`), not underscores. This avoids MD049/MD050 violations with the default markdownlint configuration.

This applies to **auto-generated markdown too**. The `scripts/pre-commit-llm.py` script generates `.llm/skills/index.md` -- its footer must use `*...*` (asterisk), not `_..._` (underscore). Regression tests enforce this in both `tests/ci_config_tests.rs` (`llm_index_validation` module) and `scripts/test_pre_commit_llm.py` (`TestGenerateIndex` class).

Bold text that acts as a section heading should be converted to a proper heading (`###`, `####`) rather than using `**Heading**` on its own line.

### markdownlint: Heading spacing (MD022)

Markdown headings must be surrounded by blank lines (leave an empty line before and after each heading block).

This rule is enforced in CI by markdownlint and by `tests/ci_config_tests.rs::markdown_policy_validation`.

### markdownlint: List spacing (MD032)

Markdown lists must be surrounded by blank lines. If a paragraph introduces a list (for example ending with a colon), add an empty line before the first list item.

This is enforced in CI by markdownlint and by `tests/ci_config_tests.rs::markdown_policy_validation::list_introduction_lines_require_blank_spacing_before_list_items`.

### markdownlint: New rules in updates

When `markdownlint-cli2` is updated in CI (e.g., via Dependabot or `@latest` tag), new rules may be introduced that cause mass failures.

Example: markdownlint v0.40.0 added MD060 (table column style) which generated 300+ violations across existing tables.

**Strategy:** Disable overly strict new rules in `.markdownlint.json` rather than reformatting all existing content:

```json
{
    "MD058": false,
    "MD060": false
}
```

Review new rules individually and enable only those that add genuine value.

### typos: US English locale and false positives

The project uses `locale = "en-us"` in `.typos.toml`. Use American English spellings (e.g., "recognize" not "recognise"). Use `[default.extend-identifiers]` for code identifiers (e.g., variable `pn`) and `[default.extend-words]` for standalone words in comments/strings (e.g., `Pn` in grep flags like `-Pn`). When a token triggers in both contexts, add entries to both sections and cross-reference them with comments.

### Cargo parallelism: never run cargo subcommands in parallel locally

Any cargo subcommands sharing the same `target/` directory or Cargo package lock must not be run in parallel in local scripts/hooks. Two problems arise: (1) **Feature-flag conflicts** — different flag combinations (e.g., `--all-features` vs `--no-default-features`) cause constant cache invalidation, each process rebuilding what the other just compiled. (2) **Package-lock contention** — even non-compiling subcommands like `cargo fmt --check` acquire the Cargo package lock; running them in parallel with `cargo clippy` gains nothing because one blocks on the lock while the other holds it, and output becomes interleaved and non-deterministic.

**Correct pattern (two-phase hooks):** Phase 1 runs non-cargo checks in parallel (typos, shellcheck, markdownlint, etc.). Phase 2 runs cargo commands sequentially to avoid lock contention and cache thrashing. The two hooks differ in Phase 2: the **pre-commit hook** runs `cargo fmt` in the foreground first (fast, no compilation), then backgrounds `cargo clippy` alongside remaining non-cargo checks; the **pre-push hook** runs `cargo clippy` then `cargo test` sequentially (no `cargo fmt`). In CI, matrix strategies give each job its own runner and `target/` directory, so parallel execution across jobs is safe. Enforced by `ci_config_tests.rs::ci_config_validation::install_hooks_pre_push_cargo_commands_must_not_run_in_parallel` and `install_hooks_pre_commit_cargo_fmt_must_run_before_clippy`. Reference: `scripts/install-hooks.sh`.

### Shell scripts: Comments must match behavior

Keep comments in CI shell scripts behaviorally exact:

- Match stated scope (for example, `.rs` only vs docs/code blocks).
- If comments mention both `#![allow(...)]` and `#[allow(...)]`, checks must handle both (for example `grep -qE '#!?\[allow\('`).
- Remember `grep` is line-based; validate multi-line attributes with staged checks.
- Avoid broad patterns (`grep -q '#\[allow('`) when you intend a specific lint.

### check-no-panics.sh: Compound cfg(test) attributes

`check-no-panics.sh` uses a POSIX ERE pattern to match both `#[cfg(test)]` and compound forms like `#[cfg(all(test, ...))]`. When adding test modules in `src/`, prefer plain `#[cfg(test)]`.

### Shell scripts: Guard logic after extraction failures

When a script extracts a value (e.g., from `awk`/`grep`), all dependent comparisons must stay inside the success branch or return early (`continue`/`exit`). Enforced by `ci_config_tests.rs::workflow_security::check_workflows_script_guards_empty_cargo_msrv`.

### Shell scripts: Distinguish "no tool" from "validation failure"

When validating files with optional tools, never conflate "tool unavailable" with "file invalid." Use distinct exit codes (exit 0 = valid, exit 2 = no parser, exit 1 = invalid) and check them with `if/elif/else` branches. Do not use nested `if !` patterns that short-circuit when an import fails. Reference: `scripts/ci-validate.sh` (Check 5) and `scripts/install-hooks.sh` TOML validation.

### Shell scripts: Use REPO_ROOT for path resolution

Scripts using relative paths silently fail if invoked from the wrong directory. Resolve with `SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"` then `REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"` and `cd "$REPO_ROOT"`.

### Shell scripts: CRLF line endings break Bash parsing

Bash `read -r` preserves `\r` from CRLF files. Strip with `line="${line//$'\r'/}"` and `| tr -d '\r'` in pipelines. Use `.gitattributes` with `* text=auto eol=lf`.

### Shell scripts: Portable regex (no grep -P, no sed -r, no PCRE shorthand in ERE)

`grep -P` (PCRE mode) and `sed -r` (GNU extended regex) are GNU-only flags that break on macOS/BSD. Always use `grep -E` (POSIX extended regex) and `sed -E` (POSIX extended regex) instead.

When converting from PCRE to ERE, replace shorthand character classes with POSIX equivalents:

| PCRE | ERE (POSIX) | Meaning |
|------|-------------|---------|
| `\s` | `[[:space:]]` | Whitespace |
| `\S` | `[^[:space:]]` | Non-whitespace |
| `\w` | `[[:alnum:]_]` | Word character |
| `\W` | `[^[:alnum:]_]` | Non-word character |
| `\d` | `[[:digit:]]` | Digit |
| `\D` | `[^[:digit:]]` | Non-digit |
| `(?:...)` | `(...)` | Non-capturing group (ERE has no distinction) |

For PCRE features with no ERE equivalent (like `\K` match reset), use `sed -nE 's/.../\1/p'` or a Python snippet (Python is available in any environment that has MkDocs).

Do **not** use `\s`, `\w`, or `\d` inside `grep -E` patterns -- these are GNU extensions that are not recognized by BSD grep. Always use the POSIX bracket expressions above.

Enforced by `scripts/test_shell_portability.sh` and `tests/ci_config_tests.rs::shell_script_portability`.

### Shell scripts: Flag detection must check anywhere in the flag group

Short flags can be combined (`-rnP`, `-inEo`). When detecting a specific flag character, check if it appears **anywhere** in the group, not just at the end. In Rust: `flags.contains('P')` not `flags.ends_with('P')` (strip leading `-` first). In shell regex: `-[a-zA-Z]*P[a-zA-Z]*` not `-[a-zA-Z]*P$`. This applies to `grep -P`, `sed -r`, `grep -E`, and all similar flag checks. Reference: `tests/ci_config_tests.rs` uses `flags.contains('P')`, `flags.contains('r')`, and `flags.contains('E')`.

### Shell scripts: Pipeline ordering for context-dependent filters

Context-dependent filters (e.g., `grep -v '^#'` to remove comments) must appear **before** extraction commands that discard context. An extraction like `grep -oE` strips the leading `#`, making a downstream `grep -v '^#'` a no-op. Correct: `grep -vE '^[[:space:]]*#' file | grep -oE 'pattern'`. Reference: `scripts/validate-docs.sh`.

### Shell scripts: Validate environment variables used as commands

When a script accepts an env var as a command path (e.g., `MKDOCS`), validate before first use: for absolute paths use `[ -x "$VAR" ]`, for bare commands use `command -v "$VAR"`. Reference: `scripts/check-docs-rendering.sh`.

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

A common MSRV breakage pattern: a transitive dependency publishes a new version requiring a newer Rust edition or language features. Example:

- `getrandom 0.4.1` requires `edition = "2024"` (Rust 1.85.0+)
- The crate itself uses `edition = "2021"` but cannot build on older Rust

**Fix:** Bump the MSRV to the minimum version that can compile all transitive dependencies. Use `cargo generate-lockfile` + `--locked` in CI for reproducible MSRV testing:

```yaml
- uses: dtolnay/rust-toolchain@stable
  with:
    toolchain: 1.85.0
- run: cargo generate-lockfile
- run: cargo build --locked --all-features
- run: cargo test --locked --all-features
```

### MSRV workflow incident: dtolnay ref vs explicit toolchain

`dtolnay/rust-toolchain` action refs are action release refs, **not** Rust toolchain versions. A ref like `@1.100.0` can exist while being unrelated to the intended MSRV and silently run a newer compiler than expected.

Use this pattern for MSRV jobs:

```yaml
- uses: dtolnay/rust-toolchain@stable
  with:
    toolchain: <msrv-from-Cargo.toml>
```

Enforced by `ci_config_tests.rs::ci_workflow_policy` tests (`ci_msrv_matches_cargo_toml`, `msrv_toolchain_step_regressions_are_caught`) and `scripts/check-workflows.sh`.

### Documentation drift on quantitative claims

Scripts like `check-all.sh` define phase counts and `--quick` boundaries referenced in this file. Update docs in the same commit as script changes. Enforced by `ci_config_tests.rs::check_all_documentation_accuracy` tests (total phase count, quick phase count, PHASE_NAMES vs TOTAL_PHASES).

### Path-based exclusions in tests: check full path components

When test code skips files by module name (e.g., excluding `emscripten_websocket`), match against **all path components**, not just the filename. Filename-only matching breaks when a flat module is refactored into a directory module. Use `path.components().any(|c| c.as_os_str().to_str().is_some_and(|s| s == "emscripten_websocket" || s == "emscripten_websocket.rs"))` instead of `path.file_name().starts_with(...)`.

### Action version pinning: major-only vs patch-level

Major-version tags like `@v2` are mutable (supply-chain risk). Prefer patch-level pinning (e.g., `@v2.8.2`). Exceptions (keep in sync with `scripts/check-workflows.sh` Phase 7 `MAJOR_ONLY_EXCEPTIONS` array):

- `dtolnay/rust-toolchain` — uses channels (`@stable`, `@nightly`, `@beta`)
- `mymindstorm/setup-emsdk` — only publishes major-version tags
- `taiki-e/install-action` — releases near-daily; patch pins go stale fast

Phase 7 emits non-blocking warnings for major-only pins. Verified by `ci_config_tests.rs::workflow_security::check_workflows_script_detects_major_only_version_tags`.

### Action version consistency across workflow files

All uses of the same action across workflow `.yml` files must use the same version tag (e.g., `actions/checkout@v6.0.2` everywhere, not `@v6.0.1` in one file). `dtolnay/rust-toolchain` is excluded (uses channel refs). Enforced by `ci_config_tests.rs::workflow_security::all_action_versions_are_consistent_across_workflows`.

### taiki-e/install-action: pin tool versions

Always include a version pin in the `tool:` parameter: `cargo-audit@0.22.1`, not bare `cargo-audit`. Unpinned tools break CI when upstream ships breaking changes. Enforced by `ci_config_tests.rs::workflow_security::install_action_tools_have_version_pins`.

### Documentation accuracy for WASM target capabilities

`SignalFishClient::start()` requires `tokio::spawn` -- unavailable on any WASM target. The correct client for all WASM targets is `SignalFishPollingClient` (requires `polling-client` feature). Cross-reference capability claims in tables against `docs/wasm.md` "What you do not get" sections to avoid contradictions.

### Nightly clippy may flag different issues than stable

The emscripten WASM target requires nightly, which may introduce lints (e.g., `needless_borrow`) not flagged by stable. Fix code for both when possible; otherwise use `#[allow(clippy::lint_name)]` with a comment.

## Validation Scripts

### Failure triage checklist

| Symptom in CI | First command/script to run |
|---|---|
| Workflow YAML/action pin/toolchain policy failure | `bash scripts/check-workflows.sh` |
| CI policy test failure in `tests/ci_config_tests.rs` | `cargo test --test ci_config_tests -- --nocapture` |
| Formatting/clippy/test drift vs required local workflow | `cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features` |
| Broken docs snippet extraction or markdown validation flow | `bash scripts/extract-rust-snippets.sh` then `bash scripts/ci-validate.sh` |
| Unresolved intra-doc link (`rustdoc::broken_intra_doc_links`) | `RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps` — check for target-gated types needing plain backtick formatting |

| Script | Purpose |
|---|---|
| `scripts/validate.sh` | Pre-flight: cargo fmt/clippy/test + `.lychee.toml` validation + markdownlint |
| `scripts/ci-validate.sh` | Lightweight local CI (13 checks): fmt, clippy, test, typos, TOML/JSON, shell portability, test I/O unwrap |
| `scripts/check-all.sh` | Full 20-phase CI parity. `--quick` for mandatory baseline (phases 1-4) |
| `scripts/check-test-io-unwrap.sh` | Scans test `.rs` for bare `.unwrap()` on I/O ops (Phase 20 / Check 13) |
