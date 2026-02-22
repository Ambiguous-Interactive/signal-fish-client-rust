#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! CI configuration policy tests for Signal Fish Client.
//!
//! These tests verify that CI workflow files, config files, scripts, and
//! Cargo.toml lints conform to project policy. If any test fails, it means
//! CI configuration has drifted from the agreed-upon standards.
//!
//! All checks are synchronous filesystem reads — no network access or async
//! runtime needed.

use std::path::PathBuf;

/// Returns the project root directory (where Cargo.toml lives).
fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Reads a file relative to the project root and returns its contents.
fn read_project_file(relative_path: &str) -> String {
    let path = project_root().join(relative_path);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "Failed to read '{}': {}. This file is required by project policy.",
            path.display(),
            e
        )
    })
}

/// Returns true if a file (not directory) exists relative to the project root.
fn project_file_exists(relative_path: &str) -> bool {
    project_root().join(relative_path).is_file()
}

/// All required workflow files, relative to the project root.
const REQUIRED_WORKFLOW_PATHS: &[&str] = &[
    ".github/workflows/ci.yml",
    ".github/workflows/coverage.yml",
    ".github/workflows/deep-safety.yml",
    ".github/workflows/docs-deploy.yml",
    ".github/workflows/docs-validation.yml",
    ".github/workflows/examples-validation.yml",
    ".github/workflows/no-panics.yml",
    ".github/workflows/security-supply-chain.yml",
    ".github/workflows/semver-checks.yml",
    ".github/workflows/unused-deps.yml",
    ".github/workflows/workflow-lint.yml",
];

// ─────────────────────────────────────────────────────────────────────────────
// Module: workflow_existence
// ─────────────────────────────────────────────────────────────────────────────

mod workflow_existence {
    use super::*;

    #[test]
    fn all_required_workflow_files_exist() {
        for path in REQUIRED_WORKFLOW_PATHS {
            assert!(
                project_file_exists(path),
                "Required workflow file '{path}' is missing. \
                 All CI workflow files must be present to maintain the project's \
                 automated quality gates."
            );
        }
    }

    #[test]
    fn no_unexpected_yaml_extension() {
        // Workflow files should use .yml, not .yaml, for consistency.
        let workflows_dir = project_root().join(".github/workflows");
        if workflows_dir.is_dir() {
            for entry in std::fs::read_dir(&workflows_dir).unwrap() {
                let entry = entry.unwrap();
                let name = entry.file_name().to_string_lossy().to_string();
                assert!(
                    !name.ends_with(".yaml"),
                    "Workflow file '{name}' uses .yaml extension. \
                     Project convention requires .yml for consistency."
                );
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module: config_existence
// ─────────────────────────────────────────────────────────────────────────────

mod config_existence {
    use super::*;

    const REQUIRED_CONFIGS: &[(&str, &str)] = &[
        (
            ".markdownlint.json",
            "Markdownlint config ensures consistent markdown style across the project.",
        ),
        (
            ".markdownlint-cli2.jsonc",
            "Markdownlint CLI2 config is required by the docs-validation workflow.",
        ),
        (
            ".typos.toml",
            "Typos config is required for spell-checking in the docs-validation workflow.",
        ),
        (
            ".lychee.toml",
            "Lychee config is required for link-checking in the docs-validation workflow.",
        ),
        (
            ".github/dependabot.yml",
            "Dependabot config is required to keep dependencies and actions up to date.",
        ),
        (
            ".yamllint.yml",
            "Yamllint config is required by the workflow-lint pipeline.",
        ),
        (
            ".pre-commit-config.yaml",
            "Pre-commit config ensures local developer checks match CI.",
        ),
    ];

    #[test]
    fn all_required_config_files_exist() {
        for (path, reason) in REQUIRED_CONFIGS {
            assert!(
                project_file_exists(path),
                "Required config file '{path}' is missing. {reason}"
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module: script_existence
// ─────────────────────────────────────────────────────────────────────────────

mod script_existence {
    use super::*;

    const REQUIRED_SCRIPTS: &[(&str, &str)] = &[
        (
            "scripts/check-all.sh",
            "The check-all script runs the complete local verification suite.",
        ),
        (
            "scripts/check-no-panics.sh",
            "The panic-free policy check script is used by the no-panics workflow.",
        ),
        (
            "scripts/check-workflows.sh",
            "The workflow check script validates CI configuration locally.",
        ),
        (
            "scripts/extract-rust-snippets.sh",
            "The snippet extraction script validates Rust code blocks in markdown files.",
        ),
    ];

    #[test]
    fn all_required_scripts_exist() {
        for (path, reason) in REQUIRED_SCRIPTS {
            assert!(
                project_file_exists(path),
                "Required script '{path}' is missing. {reason}"
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module: ci_workflow_policy
// ─────────────────────────────────────────────────────────────────────────────

mod ci_workflow_policy {
    use super::*;

    fn ci_contents() -> String {
        read_project_file(".github/workflows/ci.yml")
    }

    const REQUIRED_JOBS: &[&str] = &[
        "fmt",
        "clippy",
        "test",
        "msrv",
        "doc",
        "deny",
        "publish-dry-run",
    ];

    fn cargo_msrv_version() -> String {
        let cargo = read_project_file("Cargo.toml");
        cargo
            .lines()
            .find(|line| line.starts_with("rust-version"))
            .and_then(|line| line.split('"').nth(1))
            .map(std::string::ToString::to_string)
            .expect("Cargo.toml must declare a quoted rust-version")
    }

    fn extract_job_block(contents: &str, job_name: &str) -> Option<String> {
        let header = format!("  {job_name}:");
        let mut in_job = false;
        let mut job_lines = Vec::new();

        for line in contents.lines() {
            if !in_job {
                if line.trim_end() == header {
                    in_job = true;
                    job_lines.push(line);
                }
                continue;
            }

            let is_next_job = line.starts_with("  ")
                && !line.starts_with("    ")
                && line.trim_end().ends_with(':');
            if is_next_job {
                break;
            }
            job_lines.push(line);
        }

        in_job.then(|| job_lines.join("\n"))
    }

    fn validate_msrv_toolchain_step(msrv_job_block: &str, version: &str) -> Result<(), String> {
        let has_numeric_dtolnay_ref = msrv_job_block.lines().any(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("- uses: dtolnay/rust-toolchain@")
                .or_else(|| trimmed.strip_prefix("uses: dtolnay/rust-toolchain@"))
                .and_then(|reference| reference.chars().next())
                .is_some_and(|first| first.is_ascii_digit())
        });

        if has_numeric_dtolnay_ref {
            return Err(
                "MSRV job uses a numeric dtolnay/rust-toolchain ref. Use @stable with explicit with.toolchain instead."
                    .to_string(),
            );
        }

        if !msrv_job_block.contains("uses: dtolnay/rust-toolchain@stable") {
            return Err("MSRV job is missing 'uses: dtolnay/rust-toolchain@stable'.".to_string());
        }

        let parsed_toolchain = msrv_job_block.lines().find_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("toolchain:")
                .map(|value| value.trim().trim_matches('"').trim_matches('\''))
                .map(std::string::ToString::to_string)
        });

        if parsed_toolchain.as_deref() != Some(version) {
            return Err(format!(
                "MSRV job is missing explicit 'toolchain: {version}' (quoted or unquoted) in the rust-toolchain step."
            ));
        }

        Ok(())
    }

    #[test]
    fn ci_contains_all_required_jobs() {
        let contents = ci_contents();
        for job in REQUIRED_JOBS {
            // Job definitions appear as `<job_name>:` at the start of a line
            // (with leading whitespace) under the `jobs:` key.
            let pattern = format!("  {job}:");
            assert!(
                contents.contains(&pattern),
                "ci.yml is missing the required job '{job}'. \
                 The main CI pipeline must include all of: {REQUIRED_JOBS:?}."
            );
        }
    }

    #[test]
    fn ci_has_concurrency_block() {
        let contents = ci_contents();
        assert!(
            contents.contains("concurrency:"),
            "ci.yml is missing a 'concurrency:' block. \
             Concurrency groups prevent redundant CI runs and save resources."
        );
    }

    #[test]
    fn ci_has_cancel_in_progress() {
        let contents = ci_contents();
        assert!(
            contents.contains("cancel-in-progress: true"),
            "ci.yml is missing 'cancel-in-progress: true' in its concurrency block. \
             Without cancel-in-progress, superseded CI runs will continue to consume \
             resources instead of being cancelled when a new run starts."
        );
    }

    #[test]
    fn ci_msrv_matches_cargo_toml() {
        let ci = ci_contents();
        let version = cargo_msrv_version();
        let msrv_job = extract_job_block(&ci, "msrv").unwrap_or_else(|| {
            panic!(
                "ci.yml is missing the 'msrv' job block under jobs:. \
                 Expected a sibling job header '  msrv:' in .github/workflows/ci.yml"
            )
        });

        let validation = validate_msrv_toolchain_step(&msrv_job, &version);

        assert!(
            validation.is_ok(),
            "MSRV job in ci.yml does not match Cargo.toml rust-version '{version}'.\n\
             Validation error: {}\n\
             Extracted msrv job block:\n{msrv_job}",
            validation
                .err()
                .unwrap_or_else(|| "unknown MSRV validation error".to_string())
        );
    }

    #[test]
    fn msrv_toolchain_step_regressions_are_caught() {
        struct Case {
            name: &'static str,
            job_block: &'static str,
            expected_ok: bool,
        }

        let cases = [
            Case {
                name: "valid_stable_with_explicit_toolchain",
                job_block: "  msrv:\n    steps:\n      - uses: dtolnay/rust-toolchain@stable\n        with:\n          toolchain: 1.85.0",
                expected_ok: true,
            },
            Case {
                name: "valid_stable_with_quoted_toolchain",
                job_block: "  msrv:\n    steps:\n      - uses: dtolnay/rust-toolchain@stable\n        with:\n          toolchain: \"1.85.0\"",
                expected_ok: true,
            },
            Case {
                name: "ref_only_numeric_version_without_with_toolchain",
                job_block: "  msrv:\n    steps:\n      - uses: dtolnay/rust-toolchain@1.85.0",
                expected_ok: false,
            },
            Case {
                name: "stable_without_explicit_with_toolchain",
                job_block: "  msrv:\n    steps:\n      - uses: dtolnay/rust-toolchain@stable",
                expected_ok: false,
            },
        ];

        for case in cases {
            let result = validate_msrv_toolchain_step(case.job_block, "1.85.0");
            assert_eq!(
                result.is_ok(),
                case.expected_ok,
                "MSRV validator regression for case '{}'.\n\
                 Expected success: {}\n\
                 Result: {:?}\n\
                 Job block:\n{}",
                case.name,
                case.expected_ok,
                result,
                case.job_block
            );
        }
    }

    /// Verify that key documentation and config files reference the same MSRV
    /// as Cargo.toml. Prevents drift where Cargo.toml is bumped but docs or
    /// scripts are left with the old version.
    #[test]
    #[allow(clippy::indexing_slicing)]
    fn msrv_consistent_across_key_files() {
        let version = cargo_msrv_version();

        // Files that should reference the canonical MSRV value.
        // Keep this list in sync with the MSRV drift section in
        // .llm/skills/ci-configuration.md.
        let files_to_check = [
            ".llm/context.md",
            "README.md",
            "docs/index.md",
            "docs/getting-started.md",
            ".llm/skills/public-api-design.md",
            ".llm/skills/crate-publishing.md",
            ".llm/skills/async-rust-patterns.md",
            ".devcontainer/Dockerfile",
            "scripts/check-all.sh",
        ];

        for path in files_to_check {
            let contents = read_project_file(path);
            assert!(
                contents.contains(&version),
                "{path} does not reference the MSRV '{version}' from Cargo.toml. \
                 Update the MSRV reference in this file to match Cargo.toml."
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module: workflow_security
// ─────────────────────────────────────────────────────────────────────────────

mod workflow_security {
    use super::*;

    #[test]
    fn all_workflows_have_permissions() {
        for workflow_path in REQUIRED_WORKFLOW_PATHS {
            let contents = read_project_file(workflow_path);
            assert!(
                contents.contains("permissions:"),
                "Workflow '{workflow_path}' is missing a 'permissions:' block. \
                 Every workflow must declare explicit permissions to follow the \
                 principle of least privilege and prevent token scope escalation."
            );
        }
    }

    #[test]
    fn all_workflows_have_timeout() {
        // Regex-like check: count job definitions (lines starting with exactly
        // 2 spaces + identifier + colon, appearing after `jobs:`) and verify
        // there are at least as many `timeout-minutes:` lines. This is
        // approximate but catches partial omissions.
        for workflow_path in REQUIRED_WORKFLOW_PATHS {
            let contents = read_project_file(workflow_path);

            let mut in_jobs_section = false;
            let mut job_count: usize = 0;
            let mut timeout_count: usize = 0;

            for line in contents.lines() {
                // Detect the `jobs:` top-level key.
                if line == "jobs:" {
                    in_jobs_section = true;
                    continue;
                }

                // A top-level key (no leading whitespace, ends with ':') after
                // `jobs:` means we've left the jobs section.
                if in_jobs_section
                    && !line.is_empty()
                    && !line.starts_with(' ')
                    && !line.starts_with('#')
                {
                    break;
                }

                if in_jobs_section {
                    // Job definitions are at exactly 2-space indentation:
                    //   `  job-name:`
                    // The line starts with exactly 2 spaces (third char is not
                    // a space, `#`, or `-`), and contains `:` before any `#`.
                    if line.starts_with("  ") && !line.starts_with("   ") {
                        let trimmed = line.trim_start();
                        if !trimmed.starts_with('#') && !trimmed.starts_with('-') {
                            // Check for `:` appearing before any trailing comment.
                            let before_comment = trimmed.split('#').next().unwrap_or("");
                            if before_comment.contains(':') {
                                job_count += 1;
                            }
                        }
                    }

                    if line.contains("timeout-minutes:") {
                        timeout_count += 1;
                    }
                }
            }

            assert!(
                job_count > 0,
                "Workflow '{workflow_path}' has no detectable jobs under `jobs:`. \
                 This may indicate a parsing issue or an empty workflow."
            );

            assert!(
                timeout_count >= job_count,
                "Workflow '{workflow_path}' has {job_count} job(s) but only \
                 {timeout_count} `timeout-minutes:` declaration(s). Every job must \
                 set a timeout to prevent hung runners from consuming CI minutes \
                 indefinitely."
            );
        }
    }

    // Verifies that action `uses:` references are SHA-pinned.
    //
    // A SHA-pinned reference looks like:
    //   `uses: actions/checkout@b4ffde65f46336ab88eb53be808477a3936bae11`
    //
    // A tag reference like `uses: actions/checkout@v4` is NOT acceptable
    // because tags are mutable and can be moved to point at different commits.
    //
    // We allow `dtolnay/rust-toolchain@<channel>` to be non-SHA in this test.
    // MSRV policy is validated separately: the `msrv` job must use
    // `dtolnay/rust-toolchain@stable` with explicit `with.toolchain` matching
    // Cargo.toml rust-version.
    #[test]
    fn action_references_are_sha_pinned() {
        for workflow_path in REQUIRED_WORKFLOW_PATHS {
            let contents = read_project_file(workflow_path);

            for (line_num, line) in contents.lines().enumerate() {
                let trimmed = line.trim();

                // Only check lines with `uses:`.
                if !trimmed.starts_with("- uses:") && !trimmed.starts_with("uses:") {
                    continue;
                }

                // Extract the action reference (everything after `uses:`).
                let reference = trimmed
                    .split("uses:")
                    .nth(1)
                    .map(|s| s.trim())
                    .unwrap_or("");
                let reference = reference.trim_matches('"');

                // Skip dtolnay/rust-toolchain which uses channel names by design.
                if reference.contains("dtolnay/rust-toolchain") {
                    continue;
                }

                // Skip local actions (e.g., `./my-action`) — these don't need pinning.
                if reference.starts_with("./") {
                    continue;
                }

                // Skip Docker actions — these reference container images, not GitHub repos.
                if reference.starts_with("docker://") {
                    continue;
                }

                // Every non-local action must have an `@` version reference.
                let at_pos = reference.find('@');
                assert!(
                    at_pos.is_some(),
                    "Action reference in '{workflow_path}' line {} has no version: \
                     `{reference}`. All non-local action references must include \
                     `@<sha>` to be version-pinned.",
                    line_num + 1,
                );

                // The reference must contain `@` followed by what looks like a
                // 40-character hex SHA.
                let at_pos = at_pos.unwrap();
                let after_at = &reference[at_pos + 1..];
                // Remove any trailing comments or whitespace.
                let version_ref = after_at.split_whitespace().next().unwrap_or("");

                let is_sha_pinned =
                    version_ref.len() >= 40 && version_ref.chars().all(|c| c.is_ascii_hexdigit());

                assert!(
                    is_sha_pinned,
                    "Action reference in '{workflow_path}' line {} is not SHA-pinned: \
                     `{reference}`. Action references must use full commit SHAs \
                     (40+ hex chars) to prevent supply-chain attacks via tag mutation. \
                     Add a version comment after the SHA for readability, e.g.: \
                     `actions/checkout@<sha> # v4`.",
                    line_num + 1,
                );
            }
        }
    }

    #[test]
    fn check_workflows_script_enforces_msrv_toolchain_match() {
        let contents = read_project_file("scripts/check-workflows.sh");

        assert!(
            contents.contains("mktemp -t signal-fish-toolchain-violations"),
            "scripts/check-workflows.sh must use mktemp for temporary toolchain scan output."
        );

        assert!(
            contents.contains("trap cleanup EXIT"),
            "scripts/check-workflows.sh must register trap cleanup for temp file removal."
        );

        assert!(
            contents.contains("grep_status=$?"),
            "scripts/check-workflows.sh must capture grep exit status to distinguish no-match vs execution errors."
        );

        assert!(
            contents.contains("[ \"$grep_status\" -gt 1 ]"),
            "scripts/check-workflows.sh must fail when grep exits with status > 1 (execution error)."
        );

        assert!(
            contents.contains("CARGO_MSRV="),
            "scripts/check-workflows.sh must read Cargo.toml rust-version as canonical MSRV."
        );

        assert!(
            contents.contains("CI_MSRV_TOOLCHAIN="),
            "scripts/check-workflows.sh must extract the msrv job toolchain from ci.yml."
        );

        assert!(
            contents.contains("[ \"$CI_MSRV_TOOLCHAIN\" != \"$CARGO_MSRV\" ]"),
            "scripts/check-workflows.sh must fail when ci.yml msrv toolchain does not match Cargo.toml rust-version."
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module: panic_policy
// ─────────────────────────────────────────────────────────────────────────────

mod panic_policy {
    use super::*;

    const REQUIRED_DENY_LINTS: &[&str] = &[
        "unwrap_used",
        "expect_used",
        "panic",
        "todo",
        "unimplemented",
        "indexing_slicing",
    ];

    #[test]
    fn cargo_toml_has_all_panic_free_lints() {
        let cargo = read_project_file("Cargo.toml");

        for lint in REQUIRED_DENY_LINTS {
            let pattern = format!("{lint} = \"deny\"");
            assert!(
                cargo.contains(&pattern),
                "Cargo.toml is missing `{pattern}` in [lints.clippy]. \
                 All panic-prone lints must be set to deny level to enforce \
                 the project's panic-free policy in library code."
            );
        }
    }

    #[test]
    fn cargo_toml_has_lints_clippy_section() {
        let cargo = read_project_file("Cargo.toml");
        assert!(
            cargo.contains("[lints.clippy]"),
            "Cargo.toml is missing [lints.clippy] section. \
             This section is required to declare deny-level lints for \
             the panic-free policy."
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module: dependency_policy
// ─────────────────────────────────────────────────────────────────────────────

mod dependency_policy {
    use super::*;

    #[test]
    fn dependabot_monitors_cargo_ecosystem() {
        let contents = read_project_file(".github/dependabot.yml");
        assert!(
            contents.contains("package-ecosystem: cargo"),
            "dependabot.yml does not monitor the 'cargo' ecosystem. \
             Dependabot must monitor Cargo dependencies to receive automated \
             security and version update PRs for Rust crates."
        );
    }

    #[test]
    fn dependabot_monitors_github_actions_ecosystem() {
        let contents = read_project_file(".github/dependabot.yml");
        assert!(
            contents.contains("package-ecosystem: github-actions"),
            "dependabot.yml does not monitor the 'github-actions' ecosystem. \
             Dependabot must monitor GitHub Actions to receive automated updates \
             for workflow action versions, including security patches."
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module: ci_config_validation
// ─────────────────────────────────────────────────────────────────────────────

mod ci_config_validation {
    use super::*;

    /// Verify that `.lychee.toml` parses as valid TOML and that the `header`
    /// field is an inline table (map), not an array. An array-typed `header`
    /// was a real failure in CI — lychee silently ignores malformed headers.
    #[test]
    fn lychee_config_header_is_a_map() {
        let contents = read_project_file(".lychee.toml");
        let parsed: toml::Value =
            toml::from_str(&contents).expect(".lychee.toml must be valid TOML");

        let header = parsed.get("header").expect(
            ".lychee.toml must have a 'header' field to set a User-Agent \
             for link checking requests.",
        );

        assert!(
            header.is_table(),
            ".lychee.toml 'header' field must be an inline table (map), \
             not an array. lychee expects headers as key-value pairs, e.g.: \
             header = {{ user-agent = \"...\" }}. Found type: {}",
            header.type_str()
        );
    }

    /// Verify that `scripts/verify-sccache.sh` has the `shellcheck disable=SC2317`
    /// directive where trap handlers are used. SC2317 warns about functions that
    /// appear unreachable — but trap handlers are called indirectly by the shell,
    /// not via direct call sites. Without this directive, ShellCheck produces
    /// false positives on the `cleanup()` function.
    #[test]
    fn verify_sccache_has_shellcheck_sc2317_disable() {
        let contents = read_project_file("scripts/verify-sccache.sh");

        // The script must contain a SC2317 disable directive.
        assert!(
            contents.contains("shellcheck disable=SC2317"),
            "scripts/verify-sccache.sh is missing '# shellcheck disable=SC2317'. \
             This directive is required to suppress false positives on trap handler \
             functions that ShellCheck incorrectly flags as unreachable."
        );

        // The script must also use `trap` to confirm it actually has trap handlers.
        assert!(
            contents.contains("trap "),
            "scripts/verify-sccache.sh does not contain a 'trap' command. \
             The SC2317 disable directive only makes sense if the script uses \
             trap handlers."
        );
    }

    /// Verify that `serde_bytes` is in the cargo-machete ignore list.
    /// `serde_bytes` is used via `#[serde(with = "serde_bytes")]` attribute
    /// annotations, which cargo-machete cannot detect as usage — it only looks
    /// for explicit `use` statements and function calls. Without the ignore
    /// entry, cargo-machete would incorrectly report it as an unused dependency.
    #[test]
    fn cargo_machete_ignores_serde_bytes() {
        let cargo = read_project_file("Cargo.toml");
        let parsed: toml::Value = toml::from_str(&cargo).expect("Cargo.toml must be valid TOML");

        let ignored = parsed
            .get("package")
            .and_then(|p| p.get("metadata"))
            .and_then(|m| m.get("cargo-machete"))
            .and_then(|cm| cm.get("ignored"))
            .and_then(|i| i.as_array())
            .expect(
                "Cargo.toml must have [package.metadata.cargo-machete] ignored = [...] \
                 to suppress false positives from cargo-machete.",
            );

        let has_serde_bytes = ignored.iter().any(|v| v.as_str() == Some("serde_bytes"));

        assert!(
            has_serde_bytes,
            "Cargo.toml [package.metadata.cargo-machete] ignored list must include \
             'serde_bytes'. This crate is used via #[serde(with = \"serde_bytes\")] \
             attribute annotations which cargo-machete cannot detect as usage."
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module: mkdocs_nav_validation
// ─────────────────────────────────────────────────────────────────────────────

mod mkdocs_nav_validation {
    use super::*;

    /// Extracts all markdown file references from the mkdocs.yml nav section.
    /// Nav entries look like `      - Label: filename.md` with varying indentation.
    fn extract_nav_file_references(mkdocs_content: &str) -> Vec<(usize, String)> {
        let mut results = Vec::new();
        let mut in_nav = false;

        for (line_num, line) in mkdocs_content.lines().enumerate() {
            let trimmed = line.trim();

            // Detect the start of the nav section
            if trimmed == "nav:" {
                in_nav = true;
                continue;
            }

            // Detect exit from nav section (top-level key)
            if in_nav && !line.is_empty() && !line.starts_with(' ') && !line.starts_with('#') {
                break;
            }

            if !in_nav {
                continue;
            }

            // Nav entries look like: `  - Label: filename.md`
            // or: `      - Label: filename.md`
            // or bare entries: `  - filename.md`
            if let Some(pos) = trimmed.strip_prefix("- ") {
                if let Some(colon_pos) = pos.rfind(": ") {
                    // Labeled entry — split on the LAST `: ` to handle labels with colons
                    let file_ref = pos[colon_pos + 2..].trim();
                    if file_ref.ends_with(".md") {
                        results.push((line_num + 1, file_ref.to_string()));
                    }
                } else if pos.trim().ends_with(".md") {
                    // Bare entry without a label (e.g., `- filename.md`)
                    results.push((line_num + 1, pos.trim().to_string()));
                }
            }
        }

        results
    }

    #[test]
    fn all_nav_referenced_files_exist_in_docs_dir() {
        let mkdocs = read_project_file("mkdocs.yml");
        let nav_refs = extract_nav_file_references(&mkdocs);

        assert!(
            !nav_refs.is_empty(),
            "Could not extract any file references from mkdocs.yml nav section. \
             Either the nav section is missing or the parser needs updating."
        );

        let docs_dir = project_root().join("docs");
        assert!(
            docs_dir.is_dir(),
            "docs/ directory does not exist. MkDocs requires a docs directory."
        );

        for (line_num, file_ref) in &nav_refs {
            let full_path = docs_dir.join(file_ref);
            assert!(
                full_path.is_file(),
                "mkdocs.yml nav (line {line_num}) references '{file_ref}' but \
                 the file does not exist at '{}'. \
                 Every file referenced in the mkdocs.yml nav section must exist \
                 in the docs/ directory, otherwise `mkdocs build --strict` will fail.",
                full_path.display()
            );
        }
    }

    /// Verify that nav references cover all markdown files in docs/ (excluding
    /// includes/ and other special directories). This catches orphaned pages.
    ///
    /// NOTE: This test intentionally only checks top-level files in docs/.
    /// It does not recurse into subdirectories because all current nav entries
    /// reference top-level files. If subdirectory pages are added in the future,
    /// this test should be extended to walk docs/ recursively and build relative
    /// paths for comparison.
    #[test]
    fn no_orphaned_docs_pages() {
        let mkdocs = read_project_file("mkdocs.yml");
        let nav_refs = extract_nav_file_references(&mkdocs);

        // Build a set of nav references as-is (full paths like
        // "api/overview.md" or top-level names like "index.md").
        // Since this test only checks top-level docs/ files, a
        // top-level file must appear as a bare filename in the nav
        // to be considered referenced.
        let nav_files: std::collections::HashSet<String> =
            nav_refs.into_iter().map(|(_, f)| f).collect();

        let docs_dir = project_root().join("docs");
        if !docs_dir.is_dir() {
            return;
        }

        for entry in std::fs::read_dir(&docs_dir).unwrap() {
            let entry = entry.unwrap();
            let file_name = entry.file_name().to_string_lossy().to_string();

            // Skip directories — this test only checks top-level files.
            if entry.file_type().unwrap().is_dir() {
                continue;
            }

            // Only check .md files
            if !file_name.ends_with(".md") {
                continue;
            }

            assert!(
                nav_files.contains(&file_name),
                "docs/{file_name} exists but is not referenced in mkdocs.yml nav. \
                 Either add it to the nav section or remove it to prevent orphaned pages."
            );
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn standard_labeled_entry() {
            let input = "nav:\n  - Home: index.md\n";
            let refs = extract_nav_file_references(input);
            assert_eq!(refs, vec![(2, "index.md".to_string())]);
        }

        #[test]
        fn bare_entry_without_label() {
            let input = "nav:\n  - index.md\n";
            let refs = extract_nav_file_references(input);
            assert_eq!(refs, vec![(2, "index.md".to_string())]);
        }

        #[test]
        fn label_with_colons() {
            let input = "nav:\n  - API: Client: client.md\n";
            let refs = extract_nav_file_references(input);
            assert_eq!(refs, vec![(2, "client.md".to_string())]);
        }

        #[test]
        fn section_only_header_no_file() {
            let input = "nav:\n  - Section:\n";
            let refs = extract_nav_file_references(input);
            assert!(
                refs.is_empty(),
                "Section-only headers should not produce file references"
            );
        }

        #[test]
        fn deeply_nested_entry() {
            let input = "nav:\n        - Deep: deep.md\n";
            let refs = extract_nav_file_references(input);
            assert_eq!(refs, vec![(2, "deep.md".to_string())]);
        }

        #[test]
        fn empty_nav_section() {
            let input = "nav:\ntheme:\n  name: material\n";
            let refs = extract_nav_file_references(input);
            assert!(
                refs.is_empty(),
                "Empty nav section should produce no references"
            );
        }

        #[test]
        fn subdirectory_file_reference() {
            let input = "nav:\n  - Overview: api/overview.md\n";
            let refs = extract_nav_file_references(input);
            assert_eq!(refs, vec![(2, "api/overview.md".to_string())]);
        }

        #[test]
        fn multiple_nav_entries() {
            let input = "\
nav:
  - Home: index.md
  - Guide:
    - Getting Started: getting-started.md
    - API: Client: api/client.md
  - other.md
";
            let refs = extract_nav_file_references(input);
            assert_eq!(
                refs,
                vec![
                    (2, "index.md".to_string()),
                    (4, "getting-started.md".to_string()),
                    (5, "api/client.md".to_string()),
                    (6, "other.md".to_string()),
                ]
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module: llm_index_validation
// ─────────────────────────────────────────────────────────────────────────────

mod llm_index_validation {
    use super::*;

    /// Verify that `.llm/skills/index.md` does not use underscore emphasis
    /// (`_text_`), which violates markdownlint rule MD049. The project
    /// convention is to use asterisk emphasis (`*text*`) exclusively.
    #[test]
    fn index_md_uses_asterisk_emphasis_not_underscore() {
        let contents = read_project_file(".llm/skills/index.md");

        for (line_num, line) in contents.lines().enumerate() {
            let trimmed = line.trim();

            // Check for full-line underscore emphasis (the original bug pattern):
            // e.g. `_Generated by scripts/pre-commit-llm.py ..._`
            if trimmed.starts_with('_') && trimmed.ends_with('_') && trimmed.len() > 2 {
                panic!(
                    ".llm/skills/index.md line {} uses underscore emphasis: `{}`\n\
                     Underscore emphasis violates markdownlint MD049. \
                     Use asterisk emphasis (*text*) instead.",
                    line_num + 1,
                    trimmed
                );
            }

            // Check for inline underscore emphasis by splitting on whitespace.
            // A word-level token that starts and ends with `_` (e.g. `_word_`)
            // is underscore emphasis. Skip tokens inside backtick spans.
            let mut in_backtick = false;
            for token in trimmed.split_whitespace() {
                if token.contains('`') {
                    // Count backticks to track code span state.
                    let count = token.chars().filter(|&c| c == '`').count();
                    if count % 2 != 0 {
                        in_backtick = !in_backtick;
                    }
                }
                if in_backtick {
                    continue;
                }
                // Strip trailing punctuation for cleaner matching.
                let word = token.trim_end_matches(['.', ',', ')', ';', ':']);
                let word = word.trim_start_matches('(');
                if word.starts_with('_')
                    && !word.starts_with("__")
                    && word.ends_with('_')
                    && !word.ends_with("__")
                    && word.len() > 2
                {
                    panic!(
                        ".llm/skills/index.md line {} contains underscore \
                         emphasis: `{}`\nUnderscore emphasis violates \
                         markdownlint MD049. Use asterisk emphasis \
                         (*text*) instead.",
                        line_num + 1,
                        word
                    );
                }
            }
        }
    }

    /// Verify that `scripts/pre-commit-llm.py` does not contain string
    /// literals with underscore emphasis that would violate markdownlint MD049
    /// in the generated output. The `generate_index` function builds markdown
    /// via multi-line `lines.append(...)` calls, so we scan ALL quoted string
    /// literals within the function body (not just lines containing
    /// `lines.append`).
    ///
    /// To handle Python's implicit string concatenation across lines (e.g.
    /// `"_start..."` on one line and `"...end_"` on the next), we concatenate
    /// all string literals found within each `lines.append(...)` call before
    /// checking for underscore emphasis.
    #[test]
    fn pre_commit_script_footer_uses_asterisk_emphasis() {
        let contents = read_project_file("scripts/pre-commit-llm.py");

        // Find the `generate_index` function body and collect string literals
        // from each multi-line `lines.append(...)` call.
        let mut in_generate_index = false;
        let mut in_append = false;
        let mut append_strings: Vec<(usize, String)> = Vec::new();
        let mut append_start_line: usize = 0;

        for (line_num, line) in contents.lines().enumerate() {
            let trimmed = line.trim();

            // Detect entry into the generate_index function.
            if trimmed.starts_with("def generate_index(") {
                in_generate_index = true;
                continue;
            }

            // Detect exit: next top-level `def` or `class` ends the function.
            if in_generate_index
                && (trimmed.starts_with("def ") || trimmed.starts_with("class "))
                && !line.starts_with(' ')
            {
                break;
            }

            if !in_generate_index {
                continue;
            }

            // Track entry into lines.append(...) calls.
            if trimmed.contains("lines.append(") {
                in_append = true;
                append_start_line = line_num + 1;
                append_strings.clear();
            }

            if !in_append {
                continue;
            }

            // Extract all quoted string literals from this line.
            for quote in ['"', '\''] {
                let chars: Vec<char> = trimmed.chars().collect();
                let mut i = 0;
                while i < chars.len() {
                    if chars[i] == quote {
                        if let Some(close) = chars[i + 1..].iter().position(|&c| c == quote) {
                            let close_idx = i + 1 + close;
                            let sc: String = chars[i + 1..close_idx].iter().collect();
                            append_strings.push((line_num + 1, sc));
                            i = close_idx + 1;
                            continue;
                        }
                    }
                    i += 1;
                }
            }

            // Detect end of the append call.
            if trimmed.ends_with(')') {
                // Concatenate all string literals from this append call and
                // check the combined result for underscore emphasis.
                let combined: String = append_strings.iter().map(|(_, s)| s.as_str()).collect();
                let combined = combined.trim();

                // Skip Python dunder patterns (`__name__`).
                if !(combined.starts_with("__") && combined.ends_with("__"))
                    && combined.starts_with('_')
                    && !combined.starts_with("__")
                    && combined.ends_with('_')
                    && !combined.ends_with("__")
                    && combined.len() > 2
                {
                    panic!(
                        "scripts/pre-commit-llm.py lines {}-{} contain string \
                         literals that concatenate to underscore emphasis: \
                         `{}`\nThe generated index.md must use asterisk \
                         emphasis (*text*) to comply with markdownlint MD049.",
                        append_start_line,
                        line_num + 1,
                        combined
                    );
                }

                in_append = false;
                append_strings.clear();
            }
        }

        assert!(
            in_generate_index,
            "Could not find `def generate_index(` in scripts/pre-commit-llm.py"
        );
    }
}
