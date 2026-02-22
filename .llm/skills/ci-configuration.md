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

Always validate `.lychee.toml` with a TOML parser before committing. The
`scripts/ci-validate.sh` script includes automated TOML validation.

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
- uses: dtolnay/rust-toolchain@1.85.0
- run: cargo generate-lockfile
- run: cargo build --locked --all-features
- run: cargo test --locked --all-features
```

## Validation Scripts

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
