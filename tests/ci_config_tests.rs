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

/// Reads Cargo.toml and returns package version.
fn cargo_package_version() -> String {
    let cargo = read_project_file("Cargo.toml");
    let parsed: toml::Value = toml::from_str(&cargo).expect("Cargo.toml must be valid TOML");
    parsed
        .get("package")
        .and_then(|v| v.get("version"))
        .and_then(toml::Value::as_str)
        .map(std::string::ToString::to_string)
        .expect("Cargo.toml must define [package].version as a string")
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
            "scripts/check-docsrs.sh",
            "The docs.rs check script verifies nightly/docsrs rustdoc compatibility before release.",
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
// Module: docsrs_policy
// ─────────────────────────────────────────────────────────────────────────────

mod docsrs_policy {
    use super::*;

    #[test]
    fn removed_doc_auto_cfg_feature_is_not_used() {
        let lib_rs = read_project_file("src/lib.rs");
        let banned_patterns = ["feature(doc_auto_cfg)"];

        let found: Vec<&str> = banned_patterns
            .iter()
            .copied()
            .filter(|pattern| lib_rs.contains(pattern))
            .collect();

        assert!(
            found.is_empty(),
            "src/lib.rs contains removed rustdoc feature gates: {found:?}. \
             The `doc_auto_cfg` feature is removed from rustdoc; use docs.rs-compatible \
             configuration without that gate."
        );
    }

    #[test]
    fn doc_auto_cfg_guidance_avoids_release_specific_removal_versions() {
        let files = [".llm/skills/crate-publishing.md"];

        for path in files {
            let contents = read_project_file(path);
            if contents.contains("doc_auto_cfg") {
                assert!(
                    !contents.contains("removed in Rust "),
                    "{path} includes release-specific wording ('removed in Rust ...') \
                     for `doc_auto_cfg`. Prefer stable wording like 'removed from rustdoc' \
                     to avoid stale guidance."
                );
            }
        }
    }

    #[test]
    fn cargo_toml_docs_rs_metadata_is_present() {
        let cargo_content = read_project_file("Cargo.toml");
        let parsed: toml::Value =
            toml::from_str(&cargo_content).expect("Cargo.toml must be valid TOML");

        let docs_rs = parsed
            .get("package")
            .and_then(|package| package.get("metadata"))
            .and_then(|metadata| metadata.get("docs"))
            .and_then(|docs| docs.get("rs"))
            .expect("Cargo.toml must define [package.metadata.docs.rs]");

        let all_features = docs_rs
            .get("all-features")
            .and_then(toml::Value::as_bool)
            .or_else(|| docs_rs.get("all_features").and_then(toml::Value::as_bool))
            .expect("[package.metadata.docs.rs] must set all-features = true");
        assert!(
            all_features,
            "[package.metadata.docs.rs] must set all-features = true"
        );

        let rustdoc_args = docs_rs
            .get("rustdoc-args")
            .or_else(|| docs_rs.get("rustdoc_args"))
            .and_then(toml::Value::as_array)
            .expect("[package.metadata.docs.rs] must set rustdoc-args");

        let has_docsrs_cfg = rustdoc_args.windows(2).any(|pair| {
            pair.first().and_then(toml::Value::as_str) == Some("--cfg")
                && pair.get(1).and_then(toml::Value::as_str) == Some("docsrs")
        });

        assert!(
            has_docsrs_cfg,
            "[package.metadata.docs.rs].rustdoc-args must include [\"--cfg\", \"docsrs\"]"
        );
    }

    #[test]
    fn ci_and_publish_workflows_run_docsrs_check_script() {
        struct Case {
            workflow_path: &'static str,
            required_snippet: &'static str,
        }

        let cases = [
            Case {
                workflow_path: ".github/workflows/ci.yml",
                required_snippet: "bash scripts/check-docsrs.sh",
            },
            Case {
                workflow_path: ".github/workflows/publish.yml",
                required_snippet: "bash scripts/check-docsrs.sh",
            },
        ];

        for case in cases {
            let contents = read_project_file(case.workflow_path);
            assert!(
                contents.contains(case.required_snippet),
                "{} must run '{}'. This prevents docs.rs-only nightly breakage from reaching releases.",
                case.workflow_path,
                case.required_snippet
            );
        }
    }

    #[test]
    fn check_all_docsrs_failure_does_not_double_count_phase_4_failures() {
        let contents = read_project_file("scripts/check-all.sh");

        assert!(
            contents.contains("mark_phase_fail()"),
            "scripts/check-all.sh must define mark_phase_fail() so repeated \
             sub-check failures within the same phase do not inflate the \
             overall FAILURES count."
        );

        let docsrs_fail_pos = contents.find("docs.rs simulation: FAIL").expect(
            "scripts/check-all.sh must report docs.rs simulation failures in the Phase 4 block",
        );
        let docsrs_tail = &contents[docsrs_fail_pos..];

        assert!(
            docsrs_tail.contains("mark_phase_fail 4"),
            "scripts/check-all.sh must use mark_phase_fail 4 when docs.rs \
             simulation fails, so Phase 4 remains a single failed phase \
             in the final summary."
        );
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

    fn is_semver_like_dtolnay_ref(reference: &str) -> bool {
        !reference.is_empty()
            && reference.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
            && reference.chars().any(|ch| ch.is_ascii_digit())
    }

    fn validate_msrv_toolchain_step(msrv_job_block: &str, version: &str) -> Result<(), String> {
        let has_semver_like_dtolnay_ref = msrv_job_block.lines().any(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("- uses: dtolnay/rust-toolchain@")
                .or_else(|| trimmed.strip_prefix("uses: dtolnay/rust-toolchain@"))
                .map(|reference| {
                    reference
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .trim_matches('"')
                        .trim_matches('\'')
                })
                .is_some_and(is_semver_like_dtolnay_ref)
        });

        if has_semver_like_dtolnay_ref {
            return Err(
                "MSRV job uses a semver-like dtolnay/rust-toolchain ref (digits/dots only). Use @stable with explicit with.toolchain instead."
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
            expected_error_contains: Option<&'static str>,
        }

        let cases = [
            Case {
                name: "valid_stable_with_explicit_toolchain",
                job_block: "  msrv:\n    steps:\n      - uses: dtolnay/rust-toolchain@stable\n        with:\n          toolchain: 1.85.0",
                expected_ok: true,
                expected_error_contains: None,
            },
            Case {
                name: "valid_stable_with_quoted_toolchain",
                job_block: "  msrv:\n    steps:\n      - uses: dtolnay/rust-toolchain@stable\n        with:\n          toolchain: \"1.85.0\"",
                expected_ok: true,
                expected_error_contains: None,
            },
            Case {
                name: "semver_like_ref_without_with_toolchain",
                job_block: "  msrv:\n    steps:\n      - uses: dtolnay/rust-toolchain@1.85.0",
                expected_ok: false,
                expected_error_contains: Some("semver-like dtolnay/rust-toolchain ref"),
            },
            Case {
                name: "digit_leading_sha_ref_is_not_semver_like",
                job_block: "  msrv:\n    steps:\n      - uses: dtolnay/rust-toolchain@1a2b3c4d5e6f77889900aabbccddeeff00112233\n        with:\n          toolchain: 1.85.0",
                expected_ok: false,
                expected_error_contains: Some("missing 'uses: dtolnay/rust-toolchain@stable'"),
            },
            Case {
                name: "stable_without_explicit_with_toolchain",
                job_block: "  msrv:\n    steps:\n      - uses: dtolnay/rust-toolchain@stable",
                expected_ok: false,
                expected_error_contains: Some("missing explicit 'toolchain: 1.85.0'"),
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

            if let Some(expected_error_fragment) = case.expected_error_contains {
                let error = result.expect_err("case must fail when expected_error_contains is set");
                assert!(
                    error.contains(expected_error_fragment),
                    "MSRV validator regression for case '{}': expected error to contain '{}', got '{}'.",
                    case.name,
                    expected_error_fragment,
                    error
                );
            }
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
// Module: crate_version_consistency
// ─────────────────────────────────────────────────────────────────────────────

mod crate_version_consistency {
    use super::*;

    #[test]
    fn llm_context_version_matches_cargo_package_version() {
        let cargo_version = cargo_package_version();
        let context = read_project_file(".llm/context.md");
        let expected_line = format!("- **Version:** {cargo_version}");
        assert!(
            context.contains(&expected_line),
            ".llm/context.md must contain `{expected_line}` so the project context \
             stays synchronized with Cargo.toml package version."
        );
    }

    #[test]
    fn dependency_snippets_use_cargo_package_version() {
        let cargo_version = cargo_package_version();
        let files = ["README.md", "docs/getting-started.md", "docs/index.md"];

        for path in files {
            let contents = read_project_file(path);
            for (line_num, line) in contents.lines().enumerate() {
                let trimmed = line.trim();
                if !trimmed.starts_with("signal-fish-client =") {
                    continue;
                }

                if trimmed.contains('{') {
                    let expected = format!("version = \"{cargo_version}\"");
                    assert!(
                        trimmed.contains(&expected),
                        "{path}:{} has signal-fish-client inline table without canonical \
                         crate version.\nLine: `{trimmed}`\nExpected to contain `{expected}`.",
                        line_num + 1
                    );
                } else {
                    let expected = format!("signal-fish-client = \"{cargo_version}\"");
                    assert!(
                        trimmed.contains(&expected),
                        "{path}:{} has non-canonical signal-fish-client dependency line.\n\
                         Line: `{trimmed}`\nExpected `{expected}`.",
                        line_num + 1
                    );
                }
            }
        }
    }

    #[test]
    fn crate_publishing_skill_package_snippet_matches_cargo_package_version() {
        let cargo_version = cargo_package_version();
        let contents = read_project_file(".llm/skills/crate-publishing.md");
        let expected = format!("version = \"{cargo_version}\"");
        assert!(
            contents.contains(&expected),
            ".llm/skills/crate-publishing.md must include `{expected}` in the \
             Cargo.toml metadata snippet."
        );
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
            contents.contains("@[0-9]+(\\.[0-9]+)*"),
            "scripts/check-workflows.sh must detect only semver-like dtolnay refs (digits and dots only)."
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

    /// Verify that the CARGO_MSRV empty-check properly guards subsequent
    /// comparisons. If CARGO_MSRV extraction fails and produces an empty
    /// string, the script must NOT fall through to compare the empty value
    /// against CI_MSRV_TOOLCHAIN, which would produce a confusing error
    /// message like "Cargo.toml rust-version is '' but ci.yml msrv
    /// toolchain is '1.85.0'".
    ///
    /// The fix: the `if [ -z "$CARGO_MSRV" ]` block must use an `else`
    /// (or early return) so that CI_MSRV_BLOCK extraction and the
    /// CARGO_MSRV vs CI_MSRV_TOOLCHAIN comparison only run when
    /// CARGO_MSRV is non-empty.
    #[test]
    fn check_workflows_script_guards_empty_cargo_msrv() {
        let contents = read_project_file("scripts/check-workflows.sh");

        // Find the line with `[ -z "$CARGO_MSRV" ]` and the line with
        // `CI_MSRV_BLOCK=`. Between them there must be an `else` keyword
        // (indicating the subsequent logic is inside the else branch),
        // not just a bare `fi` (which would allow fall-through).
        let msrv_empty_check_pos = contents
            .find("[ -z \"$CARGO_MSRV\" ]")
            .expect("scripts/check-workflows.sh must check for empty CARGO_MSRV");

        let ci_block_extraction_pos = contents[msrv_empty_check_pos..]
            .find("CI_MSRV_BLOCK=")
            .map(|offset| msrv_empty_check_pos + offset)
            .expect("scripts/check-workflows.sh must extract CI_MSRV_BLOCK after CARGO_MSRV check");

        let between = &contents[msrv_empty_check_pos..ci_block_extraction_pos];

        assert!(
            between.contains("else"),
            "scripts/check-workflows.sh: The CI_MSRV_BLOCK extraction must be \
             inside an `else` branch of the CARGO_MSRV empty check. Without \
             this guard, a failed CARGO_MSRV extraction falls through and \
             produces confusing mismatch errors with an empty version string.\n\
             Content between empty check and CI_MSRV_BLOCK extraction:\n{between}"
        );

        // Also verify there is no bare `fi` without an `else` between the
        // empty check and CI_MSRV_BLOCK extraction. The `else` must come
        // before any `fi` that would close the empty check's if-block.
        let else_pos = between.find("else").unwrap();
        let fi_before_else = between[..else_pos].lines().any(|line| line.trim() == "fi");

        assert!(
            !fi_before_else,
            "scripts/check-workflows.sh: Found a bare `fi` before the `else` \
             in the CARGO_MSRV empty check. This means the if-block closes \
             before the else branch, causing fall-through on empty CARGO_MSRV."
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

    fn validate_sc2317_directive_line(
        path: &str,
        line_number: usize,
        directive_line: &str,
    ) -> Result<(), String> {
        let prefix = "# shellcheck disable=SC2317";

        if !directive_line.starts_with(prefix) {
            return Err(format!(
                "{path}:{line_number}: directive must start with '{prefix}'; got: {directive_line}"
            ));
        }

        for forbidden in [" -- ", " — ", " – "] {
            if directive_line.contains(forbidden) {
                return Err(format!(
                    "{path}:{line_number}: directive must not contain '{forbidden}'; got: {directive_line}"
                ));
            }
        }

        let trailing = directive_line.strip_prefix(prefix).unwrap_or("").trim();
        if !trailing.is_empty() && !directive_line.contains("  # ") {
            return Err(format!(
                "{path}:{line_number}: directive rationale must use '  # '; got: {directive_line}"
            ));
        }

        Ok(())
    }

    fn validate_sc2317_directive_in_script(path: &str) -> Result<(), String> {
        let contents = read_project_file(path);

        if !contents.contains("shellcheck disable=SC2317") {
            return Err(format!(
                "{path}: missing 'shellcheck disable=SC2317' directive"
            ));
        }

        if !contents.contains("trap ") {
            return Err(format!(
                "{path}: missing 'trap ' usage required for trap-handler SC2317 suppression"
            ));
        }

        let (directive_line_index, directive_line) = contents
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("shellcheck disable=SC2317"))
            .ok_or_else(|| {
                format!(
                    "{path}: could not locate directive line containing 'shellcheck disable=SC2317'"
                )
            })?;

        validate_sc2317_directive_line(path, directive_line_index + 1, directive_line)
    }

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

    #[test]
    fn msrv_badge_links_use_stable_rust_release_notes() {
        struct Case {
            path: &'static str,
            marker: &'static str,
            required_url: &'static str,
        }

        let cases = [
            Case {
                path: "README.md",
                marker: "MSRV",
                required_url: "https://doc.rust-lang.org/stable/releases.html",
            },
            Case {
                path: "docs/index.md",
                marker: "[![MSRV]",
                required_url: "https://doc.rust-lang.org/stable/releases.html",
            },
        ];

        for case in cases {
            let contents = read_project_file(case.path);
            let marker_line = contents
                .lines()
                .find(|line| line.contains(case.marker))
                .map(std::string::ToString::to_string)
                .unwrap_or_else(|| {
                    panic!(
                        "{} does not contain an MSRV badge/link marker '{}'.",
                        case.path, case.marker
                    )
                });

            assert!(
                contents.contains(case.required_url),
                "{} MSRV link must target stable Rust release notes ({}) \
                 to avoid flaky blog.rust-lang.org availability in CI.\n\
                 Marker line: {}",
                case.path,
                case.required_url,
                marker_line
            );
            assert!(
                !contents.contains("https://blog.rust-lang.org/"),
                "{} MSRV link must not target blog.rust-lang.org due to \
                 intermittent 503 responses in CI.\n\
                 Marker line: {}",
                case.path,
                marker_line
            );
        }
    }

    /// Verify that trap-handler scripts use a parse-safe SC2317 directive style.
    /// The directive line must start with `# shellcheck disable=SC2317`, avoid
    /// inline dash separators, and if a rationale is present it must be added via
    /// a second comment marker (`  # rationale`).
    #[test]
    fn trap_handler_scripts_use_parse_safe_sc2317_disable_directive() {
        for path in ["scripts/verify-sccache.sh", "scripts/check-workflows.sh"] {
            let validation = validate_sc2317_directive_in_script(path);
            assert!(validation.is_ok(), "{}", validation.unwrap_err());
        }
    }

    #[test]
    fn sc2317_disable_directive_rejects_dash_separators() {
        struct Case {
            name: &'static str,
            directive_line: &'static str,
        }

        let cases = [
            Case {
                name: "double-hyphen separator",
                directive_line: "# shellcheck disable=SC2317 -- called indirectly via trap",
            },
            Case {
                name: "em-dash separator",
                directive_line: "# shellcheck disable=SC2317 — called indirectly via trap",
            },
            Case {
                name: "en-dash separator",
                directive_line: "# shellcheck disable=SC2317 – called indirectly via trap",
            },
        ];

        for case in cases {
            let result = validate_sc2317_directive_line("<case>", 1, case.directive_line);
            assert!(
                result.is_err(),
                "case '{}' should be rejected but passed: {}",
                case.name,
                case.directive_line
            );
        }
    }

    #[test]
    fn check_all_script_avoids_shellcheck_sc2004_array_index_style() {
        let path = "scripts/check-all.sh";
        let contents = read_project_file(path);

        let offenders: Vec<(usize, String)> = contents
            .lines()
            .enumerate()
            .filter_map(|(line_idx, line)| {
                let has_phase_results_dollar_index =
                    line.contains("PHASE_RESULTS[$") || line.contains("PHASE_RESULTS[${");
                let has_phase_names_dollar_index =
                    line.contains("PHASE_NAMES[$") || line.contains("PHASE_NAMES[${");

                if has_phase_results_dollar_index || has_phase_names_dollar_index {
                    Some((line_idx + 1, line.trim_end().to_string()))
                } else {
                    None
                }
            })
            .collect();

        assert!(
            offenders.is_empty(),
            "Found ShellCheck SC2004-prone array index style in {}.\n\
             Offending lines (use [name] without '$' in array indexes):\n{}",
            path,
            offenders
                .iter()
                .map(|(line_no, line)| format!("{line_no}: {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn install_hooks_hook_script_includes_optional_shellcheck() {
        let contents = read_project_file("scripts/install-hooks.sh");

        assert!(
            contents.contains("if command -v shellcheck &>/dev/null; then"),
            "scripts/install-hooks.sh must include an optional shellcheck block \
             in the generated pre-commit hook."
        );

        assert!(
            contents.contains("shellcheck \"${REPO_ROOT}\"/scripts/*.sh"),
            "scripts/install-hooks.sh must run shellcheck on scripts/*.sh \
             (repo-root resolved) in the generated pre-commit hook."
        );
    }

    #[test]
    fn ci_configuration_skill_documents_sc2004_for_reads_and_writes() {
        let contents = read_project_file(".llm/skills/ci-configuration.md");

        assert!(
            contents.contains("applies to both reads and writes"),
            ".llm/skills/ci-configuration.md must state that SC2004 guidance \
             applies to both reads and writes."
        );

        assert!(
            contents.contains("${PHASE_RESULTS[phase]}"),
            ".llm/skills/ci-configuration.md must include a read example using \
             $PHASE_RESULTS[phase] syntax (without '$' in the index)."
        );

        assert!(
            contents.contains("PHASE_RESULTS[phase]=\"FAIL\""),
            ".llm/skills/ci-configuration.md must include a write example using \
             `PHASE_RESULTS[phase]=\"FAIL\"` (without '$' in the index)."
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

// ─────────────────────────────────────────────────────────────────────────────
// Module: markdown_policy_validation
// ─────────────────────────────────────────────────────────────────────────────

mod markdown_policy_validation {
    use super::*;

    const LLM_MAX_LINES: usize = 300;

    fn is_heading_line(line: &str) -> bool {
        let trimmed = line.trim_start();
        let hash_count = trimmed.chars().take_while(|&ch| ch == '#').count();
        hash_count > 0 && hash_count <= 6 && trimmed.chars().nth(hash_count) == Some(' ')
    }

    fn is_list_item_line(line: &str) -> bool {
        let trimmed = line.trim_start();
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
            return true;
        }

        let Some((number, remainder)) = trimmed.split_once('.') else {
            return false;
        };
        !number.is_empty()
            && number.chars().all(|ch| ch.is_ascii_digit())
            && remainder.starts_with(' ')
    }

    #[test]
    fn llm_markdown_files_respect_line_limit() {
        let llm_dir = project_root().join(".llm");
        let mut stack = vec![llm_dir.clone()];
        let mut markdown_files: Vec<PathBuf> = Vec::new();

        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir)
                .unwrap_or_else(|e| panic!("Failed to read '{}': {e}", dir.display()))
            {
                let entry = entry
                    .unwrap_or_else(|e| panic!("Failed to read entry in '{}': {e}", dir.display()));
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_some_and(|ext| ext == "md") {
                    markdown_files.push(path);
                }
            }
        }

        markdown_files.sort();
        assert!(
            !markdown_files.is_empty(),
            "No markdown files found in '{}'.",
            llm_dir.display()
        );

        let mut violations: Vec<String> = Vec::new();

        for path in markdown_files {
            let line_count = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("Failed to read '{}': {e}", path.display()))
                .lines()
                .count();
            if line_count > LLM_MAX_LINES {
                let relative = path
                    .strip_prefix(project_root())
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                violations.push(format!(
                    "{relative}: {line_count} lines (limit is {LLM_MAX_LINES})"
                ));
            }
        }

        assert!(
            violations.is_empty(),
            ".llm/ markdown files exceed {LLM_MAX_LINES} lines:\n{}",
            violations.join("\n")
        );
    }

    #[test]
    fn llm_skills_headings_have_blank_lines_around_them() {
        let skills_dir = project_root().join(".llm/skills");
        let mut markdown_files: Vec<PathBuf> = std::fs::read_dir(&skills_dir)
            .unwrap_or_else(|e| panic!("Failed to read '{}': {e}", skills_dir.display()))
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
            .collect();

        markdown_files.sort();
        assert!(
            !markdown_files.is_empty(),
            "No markdown files found in '{}'.",
            skills_dir.display()
        );

        let mut violations: Vec<String> = Vec::new();

        for path in markdown_files {
            let relative = path
                .strip_prefix(project_root())
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            let contents = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("Failed to read '{}': {e}", path.display()));
            let lines: Vec<&str> = contents.lines().collect();
            let mut in_fenced_code_block = false;

            for (idx, line) in lines.iter().enumerate() {
                let trimmed = line.trim_start();

                if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                    in_fenced_code_block = !in_fenced_code_block;
                    continue;
                }

                if in_fenced_code_block {
                    continue;
                }

                if !is_heading_line(line) {
                    continue;
                }

                if idx > 0 && !lines[idx - 1].trim().is_empty() {
                    violations.push(format!(
                        "{relative}:{} heading is missing a blank line above: `{}`",
                        idx + 1,
                        line.trim()
                    ));
                }

                if idx + 1 < lines.len()
                    && !lines[idx + 1].trim().is_empty()
                    && !is_heading_line(lines[idx + 1])
                {
                    violations.push(format!(
                        "{relative}:{} heading is missing a blank line below: `{}`",
                        idx + 1,
                        line.trim()
                    ));
                }
            }
        }

        assert!(
            violations.is_empty(),
            "Markdown heading spacing policy violations detected:\n{}",
            violations.join("\n")
        );
    }

    #[test]
    fn list_introduction_lines_require_blank_spacing_before_list_items() {
        let cases = [
            (
                ".llm/skills/ci-configuration.md",
                "Keep comments in CI shell scripts behaviorally exact:",
            ),
            (
                ".llm/skills/ci-configuration.md",
                "version and must be updated in sync:",
            ),
        ];

        for (path, intro_line) in cases {
            let content = read_project_file(path);
            let lines: Vec<&str> = content.lines().collect();
            let intro_idx = lines
                .iter()
                .position(|line| line.trim() == intro_line)
                .unwrap_or_else(|| {
                    panic!("Could not find intro line `{intro_line}` in {path}");
                });

            let blank_line = lines.get(intro_idx + 1).unwrap_or_else(|| {
                panic!(
                    "{path}:{line} intro line `{intro_line}` must be followed by a blank line and list items",
                    line = intro_idx + 1
                )
            });
            assert!(
                blank_line.trim().is_empty(),
                "{path}:{line} intro line `{intro_line}` must be followed by a blank line before list items",
                line = intro_idx + 1
            );

            let first_non_empty_after_intro = lines
                .iter()
                .skip(intro_idx + 1)
                .position(|line| !line.trim().is_empty())
                .map(|offset| intro_idx + 1 + offset)
                .unwrap_or_else(|| {
                    panic!(
                        "{path}:{line} intro line `{intro_line}` must be followed by list items",
                        line = intro_idx + 1
                    )
                });

            assert!(
                is_list_item_line(lines[first_non_empty_after_intro]),
                "{path}:{line} expected first non-empty line after `{intro_line}` to be a list item, found `{found}`",
                line = first_non_empty_after_intro + 1,
                found = lines[first_non_empty_after_intro].trim()
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module: docs_nav_card_consistency
// ─────────────────────────────────────────────────────────────────────────────

mod docs_nav_card_consistency {
    use super::*;

    /// Extract the first H1 heading (`# Title`) from markdown content,
    /// skipping lines inside fenced code blocks.
    fn extract_h1(content: &str) -> Option<String> {
        let mut fence_char: Option<char> = None;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                let ch = trimmed.chars().next().unwrap();
                if let Some(fc) = fence_char {
                    if ch == fc {
                        fence_char = None;
                    }
                } else {
                    fence_char = Some(ch);
                }
                continue;
            }
            if fence_char.is_some() {
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("# ") {
                return Some(rest.trim().to_string());
            }
        }
        None
    }

    /// Extract navigation card links from `docs/index.md`.
    ///
    /// Matches the pattern `[:octicons-arrow-right-24: LABEL](FILENAME)`
    /// and returns `(label, filename)` pairs for local `.md` files only.
    fn extract_nav_card_links(content: &str) -> Vec<(String, String)> {
        let mut results = Vec::new();
        for line in content.lines() {
            let trimmed = line.trim();
            // Pattern: [:octicons-arrow-right-24: LABEL](FILENAME)
            let prefix = "[:octicons-arrow-right-24: ";
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                // rest = "LABEL](FILENAME)"
                if let Some(bracket_pos) = rest.find("](") {
                    let label = rest[..bracket_pos].to_string();
                    let after = &rest[bracket_pos + 2..];
                    if let Some(paren_pos) = after.find(')') {
                        let filename = after[..paren_pos].to_string();
                        // Only include local .md files (skip external URLs)
                        if filename.ends_with(".md") && !filename.starts_with("http") {
                            results.push((label, filename));
                        }
                    }
                }
            }
        }
        results
    }

    /// Verify that every navigation card link label in `docs/index.md`
    /// matches the H1 heading of the target page. This prevents drift
    /// between card labels and actual page titles.
    #[test]
    fn nav_card_labels_match_page_titles() {
        let index_content = read_project_file("docs/index.md");
        let cards = extract_nav_card_links(&index_content);

        assert!(
            !cards.is_empty(),
            "Expected to find navigation card links in docs/index.md"
        );

        let mut mismatches: Vec<String> = Vec::new();

        for (label, filename) in &cards {
            let rel_path = format!("docs/{filename}");
            let target_content = read_project_file(&rel_path);
            let h1 = extract_h1(&target_content).unwrap_or_else(|| {
                panic!(
                    "docs/{filename} has no H1 heading. \
                     Every docs page must start with a `# Title` heading."
                )
            });

            if *label != h1 {
                mismatches.push(format!(
                    "  Card label \"{label}\" does not match H1 \"{h1}\" in docs/{filename}"
                ));
            }
        }

        assert!(
            mismatches.is_empty(),
            "Navigation card labels in docs/index.md do not match page titles:\n{}\n\
             Update the card labels to match the H1 headings of the target pages.",
            mismatches.join("\n")
        );
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn extracts_local_md_links() {
            let content = r#"
    [:octicons-arrow-right-24: Getting Started](getting-started.md)
    [:octicons-arrow-right-24: docs.rs](https://docs.rs/signal-fish-client)
"#;
            let links = extract_nav_card_links(content);
            assert_eq!(links.len(), 1);
            assert_eq!(links[0].0, "Getting Started");
            assert_eq!(links[0].1, "getting-started.md");
        }

        #[test]
        fn extracts_h1_skipping_fenced_blocks() {
            let content = "```\n# Not a title\n```\n# Real Title\n";
            assert_eq!(extract_h1(content), Some("Real Title".to_string()));
        }

        #[test]
        fn no_h1_returns_none() {
            let content = "## Only H2\nSome text.\n";
            assert_eq!(extract_h1(content), None);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module: llm_context_urls_validation
// ─────────────────────────────────────────────────────────────────────────────

mod llm_context_urls_validation {
    use super::*;

    /// Verify that `.llm/context.md` lists both the `homepage` and
    /// `documentation` URLs from `Cargo.toml`. This prevents drift where
    /// `context.md` only mentions one URL while the crate metadata has both.
    #[test]
    fn context_md_contains_both_cargo_urls() {
        let cargo_content = read_project_file("Cargo.toml");
        let context_content = read_project_file(".llm/context.md");

        let parsed: toml::Value =
            toml::from_str(&cargo_content).expect("Cargo.toml must be valid TOML");
        let package = parsed
            .get("package")
            .expect("Cargo.toml must have a [package] section");

        let homepage = package
            .get("homepage")
            .and_then(|v| v.as_str())
            .expect("Cargo.toml must have a homepage field");

        let documentation = package
            .get("documentation")
            .and_then(|v| v.as_str())
            .expect("Cargo.toml must have a documentation field");

        assert!(
            context_content.contains(homepage),
            ".llm/context.md does not contain the Cargo.toml homepage URL: {homepage}\n\
             Both homepage and documentation URLs from Cargo.toml must appear in context.md."
        );

        assert!(
            context_content.contains(documentation),
            ".llm/context.md does not contain the Cargo.toml documentation URL: {documentation}\n\
             Both homepage and documentation URLs from Cargo.toml must appear in context.md."
        );
    }
}
