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
        let cargo = read_project_file("Cargo.toml");

        // Extract rust-version from Cargo.toml.
        let cargo_msrv = cargo
            .lines()
            .find(|line| line.starts_with("rust-version"))
            .expect("Cargo.toml must declare a rust-version");
        // Extract the version string between quotes.
        let version = cargo_msrv
            .split('"')
            .nth(1)
            .expect("rust-version must be quoted in Cargo.toml");

        assert!(
            ci.contains(version),
            "ci.yml does not reference the MSRV '{version}' from Cargo.toml. \
             The CI MSRV job must test the exact version declared in Cargo.toml \
             to prevent silent breakage for downstream users."
        );
    }

    /// Verify that key documentation and config files reference the same MSRV
    /// as Cargo.toml. Prevents drift where Cargo.toml is bumped but docs or
    /// scripts are left with the old version.
    #[test]
    #[allow(clippy::indexing_slicing)]
    fn msrv_consistent_across_key_files() {
        let cargo = read_project_file("Cargo.toml");
        let version = cargo
            .lines()
            .find(|line| line.starts_with("rust-version"))
            .and_then(|line| line.split('"').nth(1))
            .expect("Cargo.toml must declare a rust-version");

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
        ];

        for path in files_to_check {
            let contents = read_project_file(path);
            assert!(
                contents.contains(version),
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
    // We allow `dtolnay/rust-toolchain@<channel>` because that action is
    // designed to be referenced by channel name (stable, nightly, 1.85.0).
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
