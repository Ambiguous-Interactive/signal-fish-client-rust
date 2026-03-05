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
    ".github/workflows/wasm.yml",
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
    fn check_all_docsrs_failure_does_not_double_count_phase_failures() {
        let contents = read_project_file("scripts/check-all.sh");

        assert!(
            contents.contains("mark_phase_fail()"),
            "scripts/check-all.sh must define mark_phase_fail() so repeated \
             sub-check failures within the same phase do not inflate the \
             overall FAILURES count."
        );

        let docsrs_fail_pos = contents.find("docs.rs simulation: FAIL").expect(
            "scripts/check-all.sh must report docs.rs simulation failures in the cargo doc phase block",
        );
        let docsrs_tail = &contents[docsrs_fail_pos..];

        assert!(
            docsrs_tail.contains("mark_phase_fail 5"),
            "scripts/check-all.sh must use mark_phase_fail 5 when docs.rs \
             simulation fails, so the cargo doc phase remains a single \
             failed phase in the final summary."
        );
    }

    #[test]
    fn transports_mod_does_not_use_intradoc_link_for_emscripten_transport() {
        let contents = read_project_file("src/transports/mod.rs");
        let forbidden = "[`EmscriptenWebSocketTransport`]";

        for (i, line) in contents.lines().enumerate() {
            assert!(
                !line.contains(forbidden),
                "src/transports/mod.rs line {} contains an intra-doc link to \
                 EmscriptenWebSocketTransport: {line:?}. \
                 This type is target-gated (only available on target_os = \"emscripten\"), \
                 so it can never resolve on non-emscripten hosts. Use plain backtick \
                 formatting (`EmscriptenWebSocketTransport`) instead of intra-doc link \
                 syntax ([`EmscriptenWebSocketTransport`]).",
                i + 1
            );
        }
    }

    #[test]
    fn no_source_file_uses_intradoc_link_for_target_gated_emscripten_type() {
        // EmscriptenWebSocketTransport is gated on target_os = "emscripten",
        // so intra-doc links to it will fail on any other host. Scan all Rust
        // source files to prevent this regression anywhere in the crate.
        //
        // Files inside the emscripten_websocket module are excluded because
        // they are themselves target-gated — rustdoc only processes them on
        // emscripten where the type IS in scope, so their intra-doc links
        // are valid.
        //
        // If new target-gated types are introduced, add their names here.
        //
        // The pattern omits the trailing `]` to also catch method-level
        // links like [`EmscriptenWebSocketTransport::connect`].
        let forbidden = "[`EmscriptenWebSocketTransport";
        let src_dir = project_root().join("src");
        let mut violations = Vec::new();

        fn visit_rs_files(dir: &std::path::Path, forbidden: &str, violations: &mut Vec<String>) {
            let entries = std::fs::read_dir(dir).unwrap_or_else(|e| {
                panic!("Failed to read directory '{}': {e}", dir.display());
            });
            for entry in entries {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.is_dir() {
                    visit_rs_files(&path, forbidden, violations);
                } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    // Skip files inside the emscripten_websocket module: they
                    // are compiled only on target_os = "emscripten" where the
                    // type is in scope, so intra-doc links there are correct.
                    //
                    // We check ALL path components, not just the final filename,
                    // so that files inside a potential `emscripten_websocket/`
                    // directory (e.g., `emscripten_websocket/connection.rs`) are
                    // also excluded. We match the exact stem "emscripten_websocket"
                    // (directory component or `.rs` file) rather than a prefix to
                    // avoid false-positive exclusions on unrelated files that
                    // happen to share the prefix.
                    if path.components().any(|c| {
                        c.as_os_str().to_str().is_some_and(|s| {
                            s == "emscripten_websocket" || s == "emscripten_websocket.rs"
                        })
                    }) {
                        continue;
                    }
                    let contents = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                        panic!("Failed to read '{}': {e}", path.display());
                    });
                    for (i, line) in contents.lines().enumerate() {
                        if line.contains(forbidden) {
                            violations.push(format!("{}:{}: {line}", path.display(), i + 1));
                        }
                    }
                }
            }
        }

        visit_rs_files(&src_dir, forbidden, &mut violations);

        assert!(
            violations.is_empty(),
            "Found intra-doc links to EmscriptenWebSocketTransport (or its methods) \
             in source files. This type is target-gated (target_os = \"emscripten\") \
             and cannot resolve on other hosts, causing rustdoc failures with \
             -D warnings. Use plain backtick formatting instead.\nViolations:\n{}",
            violations.join("\n")
        );
    }

    /// Regression test: verify that the emscripten_websocket exclusion logic
    /// in `no_source_file_uses_intradoc_link_for_target_gated_emscripten_type`
    /// checks path *components*, not just the final filename. This ensures
    /// files inside a potential `emscripten_websocket/` directory (e.g.,
    /// `emscripten_websocket/connection.rs`) are also correctly excluded.
    #[test]
    fn emscripten_exclusion_uses_component_based_path_matching() {
        /// Checks whether a path should be excluded from the intra-doc link
        /// scan. This duplicates the component-checking logic used in the
        /// production test above, allowing us to unit-test it in isolation.
        fn is_excluded_emscripten_path(path: &std::path::Path) -> bool {
            path.components().any(|c| {
                c.as_os_str()
                    .to_str()
                    .is_some_and(|s| s == "emscripten_websocket" || s == "emscripten_websocket.rs")
            })
        }

        // Current single-file layout — must be excluded.
        assert!(
            is_excluded_emscripten_path(std::path::Path::new(
                "src/transports/emscripten_websocket.rs"
            )),
            "emscripten_websocket.rs (current layout) must be excluded"
        );

        // Potential future directory layout — must also be excluded.
        assert!(
            is_excluded_emscripten_path(std::path::Path::new(
                "src/transports/emscripten_websocket/connection.rs"
            )),
            "emscripten_websocket/connection.rs (future directory layout) must be excluded"
        );

        // Deeply nested file inside the emscripten_websocket directory.
        assert!(
            is_excluded_emscripten_path(std::path::Path::new(
                "src/transports/emscripten_websocket/sub/helper.rs"
            )),
            "emscripten_websocket/sub/helper.rs must be excluded"
        );

        // Unrelated transport — must NOT be excluded.
        assert!(
            !is_excluded_emscripten_path(std::path::Path::new("src/transports/websocket.rs")),
            "websocket.rs must NOT be excluded"
        );

        // Unrelated source file — must NOT be excluded.
        assert!(
            !is_excluded_emscripten_path(std::path::Path::new("src/client.rs")),
            "client.rs must NOT be excluded"
        );

        // File whose name contains "emscripten" but not as a component prefix.
        assert!(
            !is_excluded_emscripten_path(std::path::Path::new("src/transports/not_emscripten.rs")),
            "not_emscripten.rs must NOT be excluded (emscripten_websocket prefix required)"
        );

        // File that SHARES the emscripten_websocket prefix but is a different
        // module — must NOT be excluded. This ensures exact-stem matching, not
        // prefix matching.
        assert!(
            !is_excluded_emscripten_path(std::path::Path::new(
                "src/transports/emscripten_websocket_notes.rs"
            )),
            "emscripten_websocket_notes.rs must NOT be excluded (different module)"
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

    /// Verify that the CI clippy job tests all feature combinations:
    /// default, `--all-features`, and `--no-default-features`.
    ///
    /// Regression: Without `--no-default-features`, dead_code warnings from
    /// items only used behind feature gates go undetected until a user builds
    /// the crate with a minimal feature set.
    #[test]
    fn ci_clippy_covers_no_default_features() {
        let contents = ci_contents();
        assert!(
            contents.contains("--no-default-features"),
            "ci.yml clippy job must include a '--no-default-features' matrix entry. \
             Without this check, dead_code and other warnings that only appear \
             when optional features are disabled will not be caught in CI."
        );
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

    fn is_semver(value: &str) -> bool {
        let mut parts = value.split('.');
        let Some(major) = parts.next() else {
            return false;
        };
        let Some(minor) = parts.next() else {
            return false;
        };
        let Some(patch) = parts.next() else {
            return false;
        };
        if parts.next().is_some() {
            return false;
        }
        !major.is_empty()
            && !minor.is_empty()
            && !patch.is_empty()
            && major.chars().all(|c| c.is_ascii_digit())
            && minor.chars().all(|c| c.is_ascii_digit())
            && patch.chars().all(|c| c.is_ascii_digit())
    }

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

    #[test]
    fn sdk_version_examples_use_cargo_package_version() {
        let cargo_version = cargo_package_version();

        let client_contents = read_project_file("docs/client.md");
        let client_expected = format!("sdk_version: Some(\"{cargo_version}\".into()),");
        assert!(
            client_contents.contains(&client_expected),
            "docs/client.md must keep its SignalFishConfig `sdk_version` example \
             synchronized with Cargo.toml package version.\nExpected line: `{client_expected}`"
        );

        let protocol_contents = read_project_file("docs/protocol.md");
        let protocol_expected = format!("\"sdk_version\": \"{cargo_version}\"");
        assert!(
            protocol_contents.contains(&protocol_expected),
            "docs/protocol.md must keep its Authenticate payload `sdk_version` \
             example synchronized with Cargo.toml package version.\nExpected fragment: `{protocol_expected}`"
        );
    }

    #[test]
    fn all_semver_sdk_version_literals_in_docs_match_cargo_package_version() {
        let cargo_version = cargo_package_version();
        let files = [
            "README.md",
            "docs/index.md",
            "docs/getting-started.md",
            "docs/client.md",
            "docs/protocol.md",
            "docs/examples.md",
            "docs/events.md",
            "docs/concepts.md",
            "docs/errors.md",
            "docs/transport.md",
        ];

        for path in files {
            let contents = read_project_file(path);
            for (line_num, line) in contents.lines().enumerate() {
                if !line.contains("sdk_version") {
                    continue;
                }

                for (idx, segment) in line.split('"').enumerate() {
                    if idx % 2 == 0 || !is_semver(segment) {
                        continue;
                    }

                    assert_eq!(
                        segment,
                        cargo_version,
                        "{path}:{} contains stale semver `sdk_version` literal `{segment}`. \
                         Expected `{cargo_version}` (from Cargo.toml) or a placeholder like `<version>`.",
                        line_num + 1
                    );
                }
            }
        }
    }

    #[test]
    fn changelog_current_version_header_is_dated_or_absent() {
        let cargo_version = cargo_package_version();
        let changelog = read_project_file("CHANGELOG.md");
        let plain_header = format!("## [{cargo_version}]");
        let dated_prefix = format!("## [{cargo_version}] - ");

        for (line_num, line) in changelog.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed == plain_header {
                panic!(
                    "CHANGELOG.md:{} has an undated current-version header `{plain_header}`. \
                     Keep feature PR entries under `## [Unreleased]`, or use \
                     `## [{cargo_version}] - YYYY-MM-DD` for release cutover PRs.",
                    line_num + 1
                );
            }

            if trimmed.starts_with(&plain_header) && !trimmed.starts_with(&dated_prefix) {
                panic!(
                    "CHANGELOG.md:{} has malformed current-version header `{trimmed}`. \
                     Expected either no current-version header, or `## [{cargo_version}] - YYYY-MM-DD`.",
                    line_num + 1
                );
            }
        }
    }

    fn collect_changelog_added_bullets(changelog: &str) -> Vec<String> {
        let mut in_added_section = false;
        let mut bullets = Vec::new();

        for line in changelog.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("## ") {
                in_added_section = false;
                continue;
            }
            if trimmed.starts_with("### ") {
                in_added_section = trimmed == "### Added";
                continue;
            }
            if in_added_section && trimmed.starts_with("- ") {
                bullets.push(trimmed.to_string());
            }
        }

        bullets
    }

    #[test]
    fn changelog_added_sections_include_signalfishconfig_public_api_additions() {
        let changelog = read_project_file("CHANGELOG.md");
        let added_bullets = collect_changelog_added_bullets(&changelog);

        let required_api_markers = [
            "`SignalFishConfig::event_channel_capacity`",
            "`SignalFishConfig::shutdown_timeout`",
            "`SignalFishConfig::with_event_channel_capacity(n)`",
            "`SignalFishConfig::with_shutdown_timeout(d)`",
        ];

        for marker in required_api_markers {
            assert!(
                added_bullets.iter().any(|bullet| bullet.contains(marker)),
                "CHANGELOG.md must document {marker} under a `### Added` section \
                 because it is a user-visible public API addition."
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

    // Verifies that action `uses:` references are version tags (v-prefixed)
    // rather than commit hashes.
    //
    // A valid reference looks like:
    //   `uses: actions/checkout@v6.0.2`
    //
    // We allow `dtolnay/rust-toolchain@<channel>` to use channels by design.
    // MSRV policy is validated separately: the `msrv` job must use
    // `dtolnay/rust-toolchain@stable` with explicit `with.toolchain` matching
    // Cargo.toml rust-version.
    #[test]
    fn action_references_use_version_tags() {
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
                     `@<version>`.",
                    line_num + 1,
                );

                let at_pos = at_pos.unwrap();
                let action_name = &reference[..at_pos];
                let after_at = &reference[at_pos + 1..];
                // Remove any trailing comments or whitespace.
                let version_ref = after_at.split_whitespace().next().unwrap_or("");

                if action_name == "dtolnay/rust-toolchain" {
                    let is_supported_channel = matches!(version_ref, "stable" | "nightly" | "beta");
                    assert!(
                        is_supported_channel,
                        "Action reference in '{workflow_path}' line {} uses unsupported \
                         dtolnay/rust-toolchain channel `{version_ref}`. Expected one of \
                         stable/nightly/beta.",
                        line_num + 1,
                    );
                    continue;
                }

                let is_hash_ref =
                    version_ref.len() == 40 && version_ref.chars().all(|c| c.is_ascii_hexdigit());
                let is_v_tag = version_ref.starts_with('v')
                    && version_ref
                        .chars()
                        .nth(1)
                        .is_some_and(|c| c.is_ascii_digit());

                assert!(
                    !is_hash_ref && is_v_tag,
                    "Action reference in '{workflow_path}' line {} violates version-tag policy: \
                     `{reference}`. Use v-prefixed version tags (e.g., `@v6` or `@v6.0.2`) \
                     and do not use commit hashes.",
                    line_num + 1,
                );
            }
        }
    }

    #[test]
    fn all_workflow_steps_have_explicit_names() {
        for workflow_path in REQUIRED_WORKFLOW_PATHS {
            let contents = read_project_file(workflow_path);

            for (line_num, line) in contents.lines().enumerate() {
                let trimmed = line.trim_start();
                let is_unnamed_step =
                    trimmed.starts_with("- uses:") || trimmed.starts_with("- run:");
                assert!(
                    !is_unnamed_step,
                    "Workflow '{workflow_path}' line {} defines a step without an explicit name: \
                     `{}`. Use `- name: ...` followed by `uses:`/`run:` for readability in the \
                     Actions UI and logs.",
                    line_num + 1,
                    trimmed
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
            contents.contains("mktemp -t signal-fish-action-ref-violations"),
            "scripts/check-workflows.sh must track action-ref violations in a temp file."
        );

        assert!(
            contents.contains("trap cleanup EXIT"),
            "scripts/check-workflows.sh must register trap cleanup for temp file removal."
        );

        assert!(
            contents.contains("hash-pinned action ref is not allowed"),
            "scripts/check-workflows.sh must explicitly reject hash-pinned action refs."
        );

        assert!(
            contents.contains("expected v-prefixed version tag"),
            "scripts/check-workflows.sh must enforce v-prefixed action tags."
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

        assert!(
            contents.contains("^[[:space:]]*-[[:space:]]+(uses|run):"),
            "scripts/check-workflows.sh must enforce explicit `name:` fields by detecting raw `- uses:` / `- run:` steps."
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

    /// Verify that scripts/check-workflows.sh contains Phase 7, which warns
    /// about major-only version tags (e.g. `@v2` instead of `@v2.8.2`).
    /// Major-only tags are mutable floating references that can silently
    /// pick up breaking changes; Phase 7 flags them as an informational
    /// warning so maintainers are aware.
    #[test]
    fn check_workflows_script_detects_major_only_version_tags() {
        let contents = read_project_file("scripts/check-workflows.sh");

        assert!(
            contents.contains("signal-fish-major-only-violations"),
            "scripts/check-workflows.sh must create a temp file for major-only \
             version tag violations (signal-fish-major-only-violations)."
        );

        assert!(
            contents.contains("MAJOR_ONLY_EXCEPTIONS"),
            "scripts/check-workflows.sh must declare a MAJOR_ONLY_EXCEPTIONS \
             list so that specific actions can be exempt from the major-only \
             version tag warning."
        );

        assert!(
            contents.contains("mymindstorm/setup-emsdk"),
            "scripts/check-workflows.sh must include mymindstorm/setup-emsdk \
             in the MAJOR_ONLY_EXCEPTIONS list as a known exception."
        );

        assert!(
            contents.contains("^v[0-9]+$"),
            "scripts/check-workflows.sh must use the regex pattern ^v[0-9]+$ \
             to detect major-only version tags (e.g. @v2, @v14)."
        );

        assert!(
            contents.contains("informational"),
            "scripts/check-workflows.sh Phase 7 must be marked as informational \
             to confirm it is a non-blocking warning rather than a hard failure."
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

    fn cargo_toml() -> toml::Value {
        let contents = read_project_file("Cargo.toml");
        toml::from_str(&contents).expect("Cargo.toml must be valid TOML")
    }

    fn dependency_features(dependency: &toml::Value) -> Vec<String> {
        dependency
            .get("features")
            .and_then(toml::Value::as_array)
            .map(|features| {
                features
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .map(std::string::ToString::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

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

    #[test]
    fn uuid_dependency_enables_v4_and_serde_features() {
        let parsed = cargo_toml();
        let uuid_dep = parsed
            .get("dependencies")
            .and_then(|deps| deps.get("uuid"))
            .expect("Cargo.toml must define [dependencies].uuid");

        let features = dependency_features(uuid_dep);
        assert!(
            features.iter().any(|feature| feature == "v4"),
            "[dependencies].uuid must enable the `v4` feature."
        );
        assert!(
            features.iter().any(|feature| feature == "serde"),
            "[dependencies].uuid must enable the `serde` feature."
        );
    }

    #[test]
    fn wasm_uuid_dependency_includes_js_v4_and_serde_features() {
        let parsed = cargo_toml();
        let uuid_dep = parsed
            .get("target")
            .and_then(|target| target.get("cfg(target_arch = \"wasm32\")"))
            .and_then(|cfg| cfg.get("dependencies"))
            .and_then(|deps| deps.get("uuid"))
            .expect(
                "Cargo.toml must define [target.'cfg(target_arch = \"wasm32\")'.dependencies].uuid",
            );

        let features = dependency_features(uuid_dep);
        for required in ["js", "v4", "serde"] {
            assert!(
                features.iter().any(|feature| feature == required),
                "[target.'cfg(target_arch = \"wasm32\")'.dependencies].uuid must enable the `{required}` feature."
            );
        }
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
    /// field is an array of strings. Lychee v0.18+ expects headers as a
    /// sequence of `"Name: value"` strings, not an inline table (map).
    /// A map-typed `header` was a real CI failure — lychee rejects it with
    /// "invalid type: map, expected a sequence".
    #[test]
    fn lychee_config_header_is_an_array() {
        let contents = read_project_file(".lychee.toml");
        let parsed: toml::Value =
            toml::from_str(&contents).expect(".lychee.toml must be valid TOML");

        let header = parsed.get("header").expect(
            ".lychee.toml must have a 'header' field to set a User-Agent \
             for link checking requests.",
        );

        let arr = header.as_array().unwrap_or_else(|| {
            panic!(
                ".lychee.toml 'header' field must be an array of strings, \
                 e.g.: header = [\"user-agent=...\"].\n\
                 lychee v0.18+ rejects map syntax. Found type: {}",
                header.type_str()
            )
        });

        assert!(
            !arr.is_empty(),
            ".lychee.toml 'header' array must not be empty — \
             at least a user-agent header is required."
        );

        for (i, entry) in arr.iter().enumerate() {
            let s = entry.as_str().unwrap_or_else(|| {
                panic!(".lychee.toml header[{i}] must be a string, found: {entry}")
            });
            assert!(
                s.contains('='),
                ".lychee.toml header[{i}] = {s:?} does not contain '=' — \
                 lychee v0.18+ requires headers in \"key=value\" format."
            );
        }
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

    /// Verify that the pre-push hook in `install-hooks.sh` runs the
    /// panic-free policy check and the markdown snippet compilation check.
    /// These are CI-critical scripts that should be caught before push.
    #[test]
    fn install_hooks_pre_push_runs_ci_scripts() {
        let contents = read_project_file("scripts/install-hooks.sh");

        assert!(
            contents.contains("scripts/check-no-panics.sh"),
            "scripts/install-hooks.sh pre-push hook must run \
             scripts/check-no-panics.sh to catch panic-free policy \
             violations before push."
        );

        assert!(
            contents.contains("scripts/extract-rust-snippets.sh"),
            "scripts/install-hooks.sh pre-push hook must run \
             scripts/extract-rust-snippets.sh to catch markdown snippet \
             compilation failures before push."
        );
    }

    /// Verify that `.pre-commit-config.yaml` includes push-stage hooks
    /// for the panic-free policy check and markdown snippet compilation.
    #[test]
    fn pre_commit_config_has_push_stage_ci_script_hooks() {
        let contents = read_project_file(".pre-commit-config.yaml");

        assert!(
            contents.contains("id: check-no-panics"),
            ".pre-commit-config.yaml must define a 'check-no-panics' hook \
             to run the panic-free policy check on push."
        );

        assert!(
            contents.contains("id: extract-rust-snippets"),
            ".pre-commit-config.yaml must define an 'extract-rust-snippets' \
             hook to run the markdown snippet compilation check on push."
        );

        // Both hooks must be push-only (too slow for every commit).
        // Verify by checking the hook blocks contain `stages: [push]`.
        let panics_block_start = contents
            .find("id: check-no-panics")
            .expect("check-no-panics hook must exist");
        let panics_block_end = contents[panics_block_start..]
            .find("\n  - repo:")
            .map(|offset| panics_block_start + offset)
            .unwrap_or(contents.len());
        let panics_block = &contents[panics_block_start..panics_block_end];
        assert!(
            panics_block.contains("stages: [push]"),
            "check-no-panics hook must be push-only (stages: [push])."
        );

        let snippets_block_start = contents
            .find("id: extract-rust-snippets")
            .expect("extract-rust-snippets hook must exist");
        let snippets_block_end = contents[snippets_block_start..]
            .find("\n  - repo:")
            .map(|offset| snippets_block_start + offset)
            .unwrap_or(contents.len());
        let snippets_block = &contents[snippets_block_start..snippets_block_end];
        assert!(
            snippets_block.contains("stages: [push]"),
            "extract-rust-snippets hook must be push-only (stages: [push])."
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

    /// Verify that `scripts/extract-rust-snippets.sh` is executed in a CI
    /// workflow. The script validates that Rust code blocks embedded in
    /// markdown files actually compile, catching stale or broken examples.
    ///
    /// The script is expected in the examples-validation workflow (which
    /// covers doc tests, example programs, and markdown snippet compilation).
    #[test]
    fn ci_runs_extract_rust_snippets_script() {
        let contents = read_project_file(".github/workflows/examples-validation.yml");
        assert!(
            contents.contains("bash scripts/extract-rust-snippets.sh"),
            ".github/workflows/examples-validation.yml must run \
             'bash scripts/extract-rust-snippets.sh'. This script validates \
             that Rust code blocks in markdown files compile, preventing \
             stale or broken documentation examples from reaching main."
        );
    }

    /// Verify that the pre-push hook in `.pre-commit-config.yaml` includes
    /// a `cargo clippy --no-default-features` check to catch dead_code
    /// warnings and other issues that only surface when optional features
    /// are disabled.
    #[test]
    fn pre_commit_config_has_no_default_features_clippy_hook() {
        let contents = read_project_file(".pre-commit-config.yaml");
        assert!(
            contents.contains("cargo clippy --all-targets --no-default-features -- -D warnings"),
            ".pre-commit-config.yaml must define a cargo clippy hook with \
             --no-default-features to catch dead_code warnings and other \
             issues that only surface when optional features are disabled."
        );
    }

    /// Collect all workflow YAML files under `.github/workflows/`.
    fn all_workflow_files() -> Vec<(String, String)> {
        let workflows_dir = project_root().join(".github/workflows");
        let mut results = Vec::new();
        if workflows_dir.is_dir() {
            for entry in std::fs::read_dir(&workflows_dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("yml") {
                    let relative = format!(
                        ".github/workflows/{}",
                        path.file_name().unwrap().to_string_lossy()
                    );
                    let contents = read_project_file(&relative);
                    results.push((relative, contents));
                }
            }
        }
        results
    }

    /// Extract all `uses: owner/repo@version` references from workflow YAML,
    /// returning `(action_name, version_ref, file_path, line_number)` tuples.
    fn extract_action_references(
        workflow_path: &str,
        contents: &str,
    ) -> Vec<(String, String, String, usize)> {
        let mut refs = Vec::new();
        for (line_num, line) in contents.lines().enumerate() {
            let trimmed = line.trim();

            // Only check `uses:` lines.
            let uses_value = if let Some(rest) = trimmed.strip_prefix("- uses:") {
                rest.trim()
            } else if let Some(rest) = trimmed.strip_prefix("uses:") {
                rest.trim()
            } else {
                continue;
            };
            let uses_value = uses_value.trim_matches('"');

            // Skip local and docker actions.
            if uses_value.starts_with("./") || uses_value.starts_with("docker://") {
                continue;
            }

            if let Some(at_pos) = uses_value.find('@') {
                let action_name = &uses_value[..at_pos];
                let version_ref = uses_value[at_pos + 1..]
                    .split_whitespace()
                    .next()
                    .unwrap_or("");
                refs.push((
                    action_name.to_string(),
                    version_ref.to_string(),
                    workflow_path.to_string(),
                    line_num + 1,
                ));
            }
        }
        refs
    }

    /// All uses of the same GitHub Action across all workflow files should use
    /// the same version tag. For example, if `actions/checkout@v6.0.2` appears
    /// in ci.yml, every other workflow must also use `@v6.0.2` and not an older
    /// or newer version. This prevents silent behavioral differences between
    /// workflows caused by version skew.
    ///
    /// `dtolnay/rust-toolchain` is excluded because it intentionally uses
    /// different channel refs (stable, nightly, beta) in different workflows.
    #[test]
    fn all_action_versions_are_consistent_across_workflows() {
        let workflows = all_workflow_files();
        assert!(
            !workflows.is_empty(),
            "No workflow files found under .github/workflows/. \
             Expected at least one .yml file."
        );

        // Collect all action references across all workflows.
        let mut action_versions: std::collections::HashMap<String, Vec<(String, String, usize)>> =
            std::collections::HashMap::new();

        for (path, contents) in &workflows {
            for (action_name, version_ref, file_path, line_num) in
                extract_action_references(path, contents)
            {
                action_versions.entry(action_name).or_default().push((
                    version_ref,
                    file_path,
                    line_num,
                ));
            }
        }

        let mut inconsistencies = Vec::new();

        for (action_name, usages) in &action_versions {
            // Skip dtolnay/rust-toolchain — it uses channels, not versions.
            if action_name == "dtolnay/rust-toolchain" {
                continue;
            }

            let first_version = &usages[0].0;
            for (version, file, line) in usages.iter().skip(1) {
                if version != first_version {
                    inconsistencies.push(format!(
                        "  {action_name}: '{first_version}' (in {}, line {}) vs \
                         '{version}' (in {file}, line {line})",
                        usages[0].1, usages[0].2
                    ));
                }
            }
        }

        assert!(
            inconsistencies.is_empty(),
            "Action version inconsistencies found across workflow files.\n\
             All uses of the same GitHub Action must use the same version tag \
             to prevent silent behavioral differences between workflows.\n\
             Fix by updating all references to use a single version:\n{}",
            inconsistencies.join("\n")
        );
    }

    /// When `taiki-e/install-action` is used with a `tool:` parameter, the
    /// tool should include an explicit version pin (e.g., `cargo-audit@0.22.1`
    /// not just `cargo-audit`). Without a version pin, CI silently installs
    /// whatever the latest version is, which can break when tools release
    /// breaking changes (e.g., cargo-audit adding CVSS 4.0 support that
    /// requires a newer advisory database format, or cargo-semver-checks
    /// requiring a newer rustdoc JSON format).
    #[test]
    fn install_action_tools_have_version_pins() {
        let workflows = all_workflow_files();

        let mut unpinned = Vec::new();

        for (path, contents) in &workflows {
            let lines: Vec<&str> = contents.lines().collect();

            for (i, line) in lines.iter().enumerate() {
                let trimmed = line.trim();

                // Detect `uses: taiki-e/install-action@...`
                let is_install_action =
                    trimmed.contains("uses:") && trimmed.contains("taiki-e/install-action@");

                if !is_install_action {
                    continue;
                }

                // Look ahead for a `tool:` line within the next few lines
                // (typically in a `with:` block immediately after).
                let lookahead_end = std::cmp::min(i + 5, lines.len());
                for (offset, lookahead_line) in lines[i + 1..lookahead_end].iter().enumerate() {
                    let tool_trimmed = lookahead_line.trim();
                    let line_num = i + 1 + offset + 1; // 1-based line number

                    if let Some(tool_value) = tool_trimmed.strip_prefix("tool:") {
                        let tool_value = tool_value.trim().trim_matches('"').trim_matches('\'');

                        // Check each comma-separated tool for version pin.
                        for tool in tool_value.split(',') {
                            let tool = tool.trim();
                            if tool.is_empty() {
                                continue;
                            }

                            if !tool.contains('@') {
                                unpinned.push(format!(
                                    "  {path}:{line_num}: tool '{tool}' has no version pin. \
                                     Use '{tool}@<version>' to prevent CI breakage \
                                     from upstream tool releases.",
                                ));
                            }
                        }
                        break;
                    }

                    // Stop looking if we hit a step boundary or unrelated key.
                    if tool_trimmed.starts_with("- name:")
                        || tool_trimmed.starts_with("- uses:")
                        || tool_trimmed.starts_with("- run:")
                    {
                        break;
                    }
                }
            }
        }

        assert!(
            unpinned.is_empty(),
            "Found taiki-e/install-action tool references without version pins.\n\
             Unpinned tools can break CI when upstream releases include breaking \
             changes (e.g., new CVSS format support, new rustdoc JSON version).\n\
             Add explicit version pins to each tool:\n{}",
            unpinned.join("\n")
        );
    }

    /// All `.toml` config files in the repo root (including hidden files like
    /// `.lychee.toml` and `.typos.toml`) must parse successfully as valid TOML.
    /// This catches format errors early — for example, a previous CI failure was
    /// caused by `.lychee.toml` using map syntax for `header` instead of array
    /// syntax, which only failed at lychee runtime.
    #[test]
    fn all_root_toml_files_parse_successfully() {
        let root = project_root();
        let mut failures = Vec::new();

        for entry in std::fs::read_dir(&root).unwrap() {
            let entry = entry.unwrap();
            let file_name = entry.file_name().to_string_lossy().to_string();

            if !file_name.ends_with(".toml") {
                continue;
            }
            if !entry.file_type().unwrap().is_file() {
                continue;
            }

            let contents = std::fs::read_to_string(entry.path()).unwrap_or_else(|e| {
                panic!("Failed to read '{}': {e}", entry.path().display());
            });

            if let Err(e) = toml::from_str::<toml::Value>(&contents) {
                failures.push(format!("  {file_name}: {e}"));
            }
        }

        assert!(
            failures.is_empty(),
            "Found TOML files in the repo root that fail to parse.\n\
             All .toml config files must be valid TOML to prevent CI \
             runtime failures from config format errors.\n\
             Parse errors:\n{}",
            failures.join("\n")
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

// ─────────────────────────────────────────────────────────────────────────────
// Module: ffi_safety
// ─────────────────────────────────────────────────────────────────────────────

mod ffi_safety {

    /// Verify that `#[repr(C)]` structs across the entire `src/` tree do not
    /// use bare `bool` fields.
    ///
    /// Rust `bool` is 1 byte, but C's boolean-like types (e.g., `EM_BOOL`,
    /// which is `int`) are typically 4 bytes. Using `bool` in a `#[repr(C)]`
    /// struct causes an ABI mismatch: the C side writes 4 bytes, but Rust
    /// only reads 1, leaving subsequent fields misaligned. This has caused
    /// real production bugs where `is_text` was always read as `0` (binary).
    ///
    /// The correct type is `c_int` or a type alias like `EM_BOOL`.
    #[test]
    fn ffi_repr_c_structs_do_not_use_bare_bool() {
        // Scan all .rs files under src/ to prevent this class of bug anywhere.
        let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut all_sources: Vec<(String, String)> = Vec::new();
        fn collect_rs_files(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        collect_rs_files(&path, out);
                    } else if path.extension().is_some_and(|e| e == "rs") {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            out.push((path.display().to_string(), content));
                        }
                    }
                }
            }
        }
        collect_rs_files(&src_dir, &mut all_sources);
        assert!(
            !all_sources.is_empty(),
            "Expected to find .rs files under src/"
        );

        let mut all_violations: Vec<String> = Vec::new();

        for (file_path, source) in &all_sources {
            let mut in_repr_c = false;
            let mut in_struct = false;
            let mut struct_name = String::new();
            let mut brace_depth: i32 = 0;

            for (lineno, line) in source.lines().enumerate() {
                let trimmed = line.trim();

                // Detect #[repr(C)] annotation.
                if trimmed == "#[repr(C)]" {
                    in_repr_c = true;
                    continue;
                }

                // Detect struct opening after #[repr(C)].
                if in_repr_c {
                    if trimmed.starts_with("struct ") || trimmed.starts_with("pub struct ") {
                        in_struct = true;
                        struct_name = trimmed
                            .split_whitespace()
                            .find(|w| *w != "struct" && *w != "pub")
                            .unwrap_or("unknown")
                            .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_')
                            .to_string();
                        brace_depth = line.chars().filter(|&c| c == '{').count() as i32
                            - line.chars().filter(|&c| c == '}').count() as i32;
                        in_repr_c = false;
                        continue;
                    }
                    if trimmed.is_empty() || trimmed.starts_with("#[") || trimmed.starts_with("///")
                    {
                        continue;
                    }
                    in_repr_c = false;
                }

                // Inside a #[repr(C)] struct body — check for bare bool fields.
                if in_struct {
                    brace_depth += line.chars().filter(|&c| c == '{').count() as i32;
                    brace_depth -= line.chars().filter(|&c| c == '}').count() as i32;

                    if !trimmed.starts_with("//") {
                        let field_parts: Vec<&str> = trimmed.splitn(2, ':').collect();
                        if field_parts.len() == 2 {
                            let type_part = field_parts[1].trim().trim_end_matches(',').trim();
                            if type_part == "bool" {
                                all_violations.push(format!(
                                    "  {file_path}:{}: field in struct '{}' uses bare 'bool'\n    {trimmed}",
                                    lineno + 1,
                                    struct_name,
                                ));
                            }
                        }
                    }

                    if brace_depth <= 0 {
                        in_struct = false;
                        struct_name.clear();
                        brace_depth = 0;
                    }
                }
            }
        }

        assert!(
            all_violations.is_empty(),
            "#[repr(C)] structs must not use bare 'bool' fields in FFI code.\n\
             Rust bool is 1 byte, but C uses int (4 bytes) for EM_BOOL.\n\
             Use c_int or EM_BOOL instead.\n\n\
             Violations found:\n{}",
            all_violations.join("\n")
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module: panic_script_cfg_handling
// ─────────────────────────────────────────────────────────────────────────────

mod panic_script_cfg_handling {
    use super::*;

    /// The grep pattern used by `check-no-panics.sh` to detect `#[cfg(..test..)]`
    /// module boundaries. This must match both simple `#[cfg(test)]` and compound
    /// forms like `#[cfg(all(test, feature = "tokio-runtime"))]`.
    ///
    /// Regression: The original script only matched `#[cfg(test)]` exactly,
    /// missing compound cfg attributes. This caused false violations for code
    /// inside `#[cfg(all(test, ...))]` modules (e.g., `src/client.rs`).
    #[test]
    fn check_no_panics_script_uses_compound_cfg_test_pattern() {
        let contents = read_project_file("scripts/check-no-panics.sh");

        // The script must use a grep pattern that matches `test` as a word
        // boundary inside any `#[cfg(...)]` attribute, not just exact
        // `#[cfg(test)]`. The current pattern is:
        //   grep -n '#\[cfg(.*\btest\b' "$file"
        assert!(
            contents.contains(r"#\[cfg(.*\btest\b"),
            "scripts/check-no-panics.sh must use a grep pattern that matches \
             compound cfg attributes containing `test` (e.g., \
             `#[cfg(all(test, feature = \"...\"))]`). \
             Expected pattern: `#\\[cfg(.*\\btest\\b`"
        );
    }

    /// Verify that the compound cfg pattern in `check-no-panics.sh` would match
    /// all real-world cfg(test) variants found in this project's source code.
    ///
    /// This is a data-driven test: it collects every `#[cfg(..test..)]` line
    /// from `src/` and verifies the script's grep pattern would match each one.
    #[test]
    fn check_no_panics_pattern_matches_all_src_cfg_test_attributes() {
        let src_dir = project_root().join("src");
        let mut cfg_test_lines: Vec<(String, String)> = Vec::new();

        fn collect_cfg_test_lines(
            dir: &std::path::Path,
            root: &std::path::Path,
            out: &mut Vec<(String, String)>,
        ) {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        collect_cfg_test_lines(&path, root, out);
                    } else if path.extension().is_some_and(|e| e == "rs") {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            let relative = path
                                .strip_prefix(root)
                                .unwrap_or(&path)
                                .to_string_lossy()
                                .to_string();
                            for line in content.lines() {
                                let trimmed = line.trim();
                                if trimmed.starts_with("#[cfg(") && trimmed.contains("test") {
                                    out.push((relative.clone(), trimmed.to_string()));
                                }
                            }
                        }
                    }
                }
            }
        }

        collect_cfg_test_lines(&src_dir, &project_root(), &mut cfg_test_lines);

        assert!(
            !cfg_test_lines.is_empty(),
            "Expected to find at least one #[cfg(..test..)] attribute in src/. \
             If all test modules have been removed, this test should be updated."
        );

        // The script's grep pattern is: #\[cfg(.*\btest\b
        // This translates to: the line must contain `#[cfg(` followed (possibly
        // with intervening characters) by the word `test` as a whole word.
        // We check this without a regex dependency by verifying:
        //   1. The line contains `#[cfg(`
        //   2. After `#[cfg(`, the word `test` appears as a standalone identifier
        //      (not part of a larger word like `testing`).
        fn matches_cfg_test_pattern(line: &str) -> bool {
            let Some(cfg_pos) = line.find("#[cfg(") else {
                return false;
            };
            let after_cfg = &line[cfg_pos + 6..]; // skip past "#[cfg("
                                                  // Check that `test` appears and is bounded by non-word characters.
            let mut search = after_cfg;
            while let Some(test_pos) = search.find("test") {
                let before_ok = test_pos == 0
                    || (!search.as_bytes()[test_pos - 1].is_ascii_alphanumeric()
                        && search.as_bytes()[test_pos - 1] != b'_');
                let after_pos = test_pos + 4;
                let after_ok = after_pos >= search.len()
                    || (!search.as_bytes()[after_pos].is_ascii_alphanumeric()
                        && search.as_bytes()[after_pos] != b'_');
                if before_ok && after_ok {
                    return true;
                }
                search = &search[test_pos + 4..];
            }
            false
        }

        let mut unmatched: Vec<String> = Vec::new();
        for (file, line) in &cfg_test_lines {
            if !matches_cfg_test_pattern(line) {
                unmatched.push(format!("  {file}: {line}"));
            }
        }

        assert!(
            unmatched.is_empty(),
            "The grep pattern in check-no-panics.sh (`#\\[cfg(.*\\btest\\b`) \
             does not match the following cfg(test) attributes found in src/:\n{}\n\
             Update the script's pattern to handle these variants.",
            unmatched.join("\n")
        );
    }

    /// Safety net: verify that no `src/` file uses `#[cfg(not(test))]`.
    ///
    /// The grep pattern in `check-no-panics.sh` (`#\[cfg(.*\btest\b`) would
    /// match `#[cfg(not(test))]`, incorrectly treating the code below it as
    /// "inside a test module" when it is actually production code. As long as
    /// no source file uses this attribute, the false positive cannot occur.
    #[test]
    fn no_src_file_uses_cfg_not_test() {
        let src_dir = project_root().join("src");
        let mut violations: Vec<(String, usize, String)> = Vec::new();

        fn scan_for_cfg_not_test(
            dir: &std::path::Path,
            root: &std::path::Path,
            out: &mut Vec<(String, usize, String)>,
        ) {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        scan_for_cfg_not_test(&path, root, out);
                    } else if path.extension().is_some_and(|e| e == "rs") {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            let relative = path
                                .strip_prefix(root)
                                .unwrap_or(&path)
                                .to_string_lossy()
                                .to_string();
                            for (i, line) in content.lines().enumerate() {
                                let trimmed = line.trim();
                                if trimmed.contains("#[cfg(not(test))]") {
                                    out.push((relative.clone(), i + 1, trimmed.to_string()));
                                }
                            }
                        }
                    }
                }
            }
        }

        scan_for_cfg_not_test(&src_dir, &project_root(), &mut violations);

        assert!(
            violations.is_empty(),
            "Found `#[cfg(not(test))]` in src/ files. This attribute causes a false \
             positive in check-no-panics.sh (the grep pattern `#\\[cfg(.*\\btest\\b` \
             matches it and incorrectly treats the code as inside a test module). \
             Use a different gating mechanism or update check-no-panics.sh to \
             exclude `not(test)` before adding this attribute.\n\
             Violations:\n{}",
            violations
                .iter()
                .map(|(f, line, text)| format!("  {f}:{line}: {text}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    /// Verify that the script explicitly documents the compound cfg handling
    /// in its inline comments. This prevents future maintainers from
    /// simplifying the pattern back to exact `#[cfg(test)]` matching.
    #[test]
    fn check_no_panics_script_documents_compound_cfg_handling() {
        let contents = read_project_file("scripts/check-no-panics.sh");

        assert!(
            contents.contains("cfg(all(test,"),
            "scripts/check-no-panics.sh must mention `cfg(all(test,` in a comment \
             to document that compound cfg attributes are supported."
        );

        assert!(
            contents.contains("#[cfg(..test..)]") || contents.contains("cfg(..test..)"),
            "scripts/check-no-panics.sh must reference the general pattern \
             `cfg(..test..)` to indicate it handles any cfg containing `test`."
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module: snippet_extraction_policy
// ─────────────────────────────────────────────────────────────────────────────

mod snippet_extraction_policy {
    use super::*;

    /// Verify that `extract-rust-snippets.sh` explicitly handles `rust,ignore`
    /// code blocks by skipping them during compilation checks.
    ///
    /// Regression: The original script extracted `rust,ignore` blocks and
    /// tried to compile them, causing CI failures for code that is
    /// intentionally marked as not compilable (e.g., platform-specific or
    /// external-crate snippets).
    #[test]
    fn extract_snippets_script_skips_rust_ignore_blocks() {
        let contents = read_project_file("scripts/extract-rust-snippets.sh");

        // The script must recognize `rust,ignore` as a language tag.
        assert!(
            contents.contains("rust,ignore"),
            "scripts/extract-rust-snippets.sh must handle the `rust,ignore` \
             language tag to skip blocks that are intentionally not compilable."
        );

        // The script must have explicit skip logic for rust,ignore blocks.
        // It should set the block_lang to "rust,ignore" and then skip when
        // closing the block.
        assert!(
            contents.contains(r#"block_lang = "rust,ignore""#)
                || contents.contains(r#"block_lang="rust,ignore""#),
            "scripts/extract-rust-snippets.sh must track `rust,ignore` blocks \
             via block_lang so they can be skipped at the end of the block."
        );

        // The script must increment the SKIPPED counter for rust,ignore blocks.
        assert!(
            contents.contains(r#"if [ "$block_lang" = "rust,ignore" ]"#),
            "scripts/extract-rust-snippets.sh must check block_lang = \"rust,ignore\" \
             and skip compilation for those blocks."
        );
    }

    /// Verify that the case statement in `extract-rust-snippets.sh` lists
    /// all supported language tags. The script must process `rust` and
    /// `rust,no_run` while skipping `rust,ignore`.
    #[test]
    fn extract_snippets_script_case_statement_covers_all_rust_tags() {
        let contents = read_project_file("scripts/extract-rust-snippets.sh");

        // Each recognized Rust code block tag must appear individually
        // somewhere in the script's case statement or handling logic.
        for tag in ["rust", "rust,no_run", "rust,ignore"] {
            assert!(
                contents.contains(tag),
                "scripts/extract-rust-snippets.sh must recognize the `{tag}` \
                 language tag to properly handle all Rust code block annotations."
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module: emscripten_target_guard
// ─────────────────────────────────────────────────────────────────────────────

mod emscripten_target_guard {
    use super::*;

    /// The `transport-websocket-emscripten` feature compiles FFI bindings that
    /// only link on `wasm32-unknown-emscripten`. A `compile_error!()` guard
    /// must be present so developers get a clear diagnostic instead of cryptic
    /// linker failures when the feature is accidentally enabled on another target.
    #[test]
    fn emscripten_websocket_has_compile_error_target_guard() {
        let contents = read_project_file("src/transports/emscripten_websocket.rs");

        assert!(
            contents.contains(r#"#[cfg(not(target_os = "emscripten"))]"#),
            "src/transports/emscripten_websocket.rs must contain a \
             `#[cfg(not(target_os = \"emscripten\"))]` guard to prevent \
             compilation on non-Emscripten targets."
        );

        assert!(
            contents.contains("compile_error!"),
            "src/transports/emscripten_websocket.rs must contain a \
             `compile_error!()` that fires when compiled on a \
             non-Emscripten target."
        );
    }

    /// The module declaration in `mod.rs` must gate the emscripten module
    /// on BOTH the feature AND `target_os = "emscripten"`. This dual gate
    /// ensures `--all-features` works on non-Emscripten hosts (features must
    /// be additive per Cargo convention). The `compile_error!()` inside the
    /// file serves as defense-in-depth.
    #[test]
    fn emscripten_module_gated_on_feature_and_target() {
        let mod_rs = read_project_file("src/transports/mod.rs");

        assert!(
            mod_rs.contains(
                r#"#[cfg(all(feature = "transport-websocket-emscripten", target_os = "emscripten"))]"#
            ),
            "src/transports/mod.rs must gate the emscripten_websocket module \
             on both the feature and target_os = \"emscripten\" so that \
             --all-features works on non-Emscripten hosts."
        );

        let lib_rs = read_project_file("src/lib.rs");

        assert!(
            lib_rs.contains(
                r#"#[cfg(all(feature = "transport-websocket-emscripten", target_os = "emscripten"))]"#
            ),
            "src/lib.rs must gate the EmscriptenWebSocketTransport re-export \
             on both the feature and target_os = \"emscripten\"."
        );
    }

    /// Cargo.toml must document the target restriction for the
    /// `transport-websocket-emscripten` feature so developers see the
    /// constraint before enabling it.
    #[test]
    fn cargo_toml_documents_emscripten_target_restriction() {
        let contents = read_project_file("Cargo.toml");

        // Find the line defining transport-websocket-emscripten and check
        // that there is a comment nearby mentioning the target restriction.
        let feature_line_idx = contents
            .lines()
            .position(|line| line.starts_with("transport-websocket-emscripten"))
            .expect("Cargo.toml must define the transport-websocket-emscripten feature");

        // Check the preceding line(s) for a target restriction comment.
        let lines: Vec<&str> = contents.lines().collect();
        let has_target_comment = (feature_line_idx.saturating_sub(3)..feature_line_idx).any(|i| {
            let line = lines[i].to_lowercase();
            line.contains("emscripten") || line.contains("target")
        });

        assert!(
            has_target_comment,
            "Cargo.toml must have a comment near the transport-websocket-emscripten \
             feature definition documenting the target restriction."
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module: check_all_documentation_accuracy
// ─────────────────────────────────────────────────────────────────────────────

mod check_all_documentation_accuracy {
    use super::*;

    /// Extract the TOTAL_PHASES value from check-all.sh.
    fn script_total_phases() -> u32 {
        let contents = read_project_file("scripts/check-all.sh");
        contents
            .lines()
            .find_map(|line| {
                let trimmed = line.trim();
                trimmed
                    .strip_prefix("TOTAL_PHASES=")
                    .and_then(|v| v.parse::<u32>().ok())
            })
            .expect("scripts/check-all.sh must define TOTAL_PHASES=<number>")
    }

    /// Extract the quick-mode phase count from check-all.sh.
    fn script_quick_phases() -> u32 {
        let contents = read_project_file("scripts/check-all.sh");
        let mut in_quick_block = false;

        for line in contents.lines() {
            let trimmed = line.trim();
            if trimmed.contains("QUICK") && trimmed.contains("true") && trimmed.contains("then") {
                in_quick_block = true;
                continue;
            }
            if in_quick_block {
                if let Some(val) = trimmed.strip_prefix("TOTAL_PHASES=") {
                    return val
                        .parse::<u32>()
                        .expect("Quick-mode TOTAL_PHASES must be a number");
                }
                if trimmed == "fi" {
                    break;
                }
            }
        }
        panic!("scripts/check-all.sh must set TOTAL_PHASES inside the --quick conditional");
    }

    /// `.llm/skills/ci-configuration.md` must reference the correct total
    /// phase count from `scripts/check-all.sh`. This prevents documentation
    /// drift when phases are added or removed.
    #[test]
    fn ci_configuration_md_references_correct_total_phase_count() {
        let total = script_total_phases();
        let doc = read_project_file(".llm/skills/ci-configuration.md");
        let expected_fragment = format!("{total}-phase");

        assert!(
            doc.contains(&expected_fragment),
            ".llm/skills/ci-configuration.md must reference '{expected_fragment}' \
             to match the TOTAL_PHASES={total} in scripts/check-all.sh. \
             Found TOTAL_PHASES={total} in the script but the documentation \
             does not contain '{expected_fragment}'."
        );
    }

    /// `.llm/skills/ci-configuration.md` must reference the correct
    /// `--quick` phase range. If the script runs phases 1-N in quick mode,
    /// the docs must say "phases 1-N".
    #[test]
    fn ci_configuration_md_references_correct_quick_phase_count() {
        let quick_phases = script_quick_phases();
        let doc = read_project_file(".llm/skills/ci-configuration.md");
        let expected_fragment = format!("phases 1-{quick_phases}");

        assert!(
            doc.contains(&expected_fragment),
            ".llm/skills/ci-configuration.md must reference '{expected_fragment}' \
             to match the --quick TOTAL_PHASES={quick_phases} in scripts/check-all.sh. \
             The documentation is stale."
        );
    }

    /// The check-all.sh header comment must list the correct number of phases.
    /// This prevents the script's own documentation from drifting.
    #[test]
    fn check_all_header_matches_total_phases() {
        let total = script_total_phases();
        let contents = read_project_file("scripts/check-all.sh");

        // Count PHASE_NAMES assignments in the script.
        let phase_name_count = contents
            .lines()
            .filter(|line| {
                let trimmed = line.trim();
                trimmed.starts_with("PHASE_NAMES[")
            })
            .count() as u32;

        assert_eq!(
            phase_name_count, total,
            "scripts/check-all.sh defines {phase_name_count} PHASE_NAMES entries \
             but TOTAL_PHASES={total}. These must match."
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module: ffi_safety_documentation
// ─────────────────────────────────────────────────────────────────────────────

mod ffi_safety_documentation {
    use super::*;

    /// Every `unsafe {` block in the Emscripten WebSocket transport must have
    /// a SAFETY comment within the preceding 15 lines. This ensures that all
    /// unsafe code has documented safety justification.
    #[test]
    fn emscripten_websocket_unsafe_blocks_have_safety_comments() {
        let contents = read_project_file("src/transports/emscripten_websocket.rs");
        let lines: Vec<&str> = contents.lines().collect();
        let mut violations: Vec<String> = Vec::new();

        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            // Match lines that open an unsafe block (not `unsafe impl`).
            if (trimmed.starts_with("unsafe {")
                || trimmed.contains("= unsafe {")
                || (trimmed.starts_with("let ") && trimmed.contains("unsafe {")))
                && !trimmed.contains("unsafe impl")
            {
                // Look backwards up to 15 lines for a SAFETY comment.
                let start = i.saturating_sub(15);
                let has_safety = lines[start..i].iter().any(|prev| prev.contains("SAFETY"));

                if !has_safety {
                    violations.push(format!("  line {}: `{}`", i + 1, trimmed));
                }
            }
        }

        assert!(
            violations.is_empty(),
            "All `unsafe` blocks in emscripten_websocket.rs must have a \
             SAFETY comment within the preceding 15 lines.\n\
             Violations:\n{}",
            violations.join("\n")
        );
    }

    /// The `connect()` error path and the `Drop` implementation must both
    /// follow the same cleanup sequence: close → delete → drop. This test
    /// verifies the ordering by checking that both code paths contain the
    /// three operations in the correct order.
    #[test]
    fn error_path_cleanup_matches_drop_cleanup_order() {
        let contents = read_project_file("src/transports/emscripten_websocket.rs");

        // Find the connect() error path (inside the `for (name, result)` loop).
        let error_block = contents
            .find("if result != EMSCRIPTEN_RESULT_SUCCESS {")
            .and_then(|start| {
                contents[start..]
                    .find("return Err(")
                    .map(|end| &contents[start..start + end + 30])
            })
            .expect("connect() must have an error path checking EMSCRIPTEN_RESULT_SUCCESS");

        assert!(
            error_block.contains("emscripten_websocket_close"),
            "connect() error path must call emscripten_websocket_close"
        );
        assert!(
            error_block.contains("emscripten_websocket_delete"),
            "connect() error path must call emscripten_websocket_delete"
        );
        assert!(
            error_block.contains("Box::from_raw"),
            "connect() error path must reclaim state_ptr via Box::from_raw"
        );

        // Verify ordering: close before delete before from_raw.
        let close_pos = error_block.find("emscripten_websocket_close").unwrap();
        let delete_pos = error_block.find("emscripten_websocket_delete").unwrap();
        let from_raw_pos = error_block.find("Box::from_raw").unwrap();

        assert!(
            close_pos < delete_pos,
            "connect() error path must call close BEFORE delete"
        );
        assert!(
            delete_pos < from_raw_pos,
            "connect() error path must call delete BEFORE Box::from_raw"
        );

        // Verify Drop follows the same order.
        let drop_block = contents
            .find("impl Drop for EmscriptenWebSocketTransport")
            .map(|start| &contents[start..])
            .expect("EmscriptenWebSocketTransport must implement Drop");

        let drop_close = drop_block
            .find("emscripten_websocket_close")
            .expect("Drop must call emscripten_websocket_close");
        let drop_delete = drop_block
            .find("emscripten_websocket_delete")
            .expect("Drop must call emscripten_websocket_delete");
        let drop_from_raw = drop_block
            .find("Box::from_raw")
            .expect("Drop must reclaim state via Box::from_raw");

        assert!(
            drop_close < drop_delete,
            "Drop must call close BEFORE delete"
        );
        assert!(
            drop_delete < drop_from_raw,
            "Drop must call delete BEFORE Box::from_raw"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module: pending_future_documentation
// ─────────────────────────────────────────────────────────────────────────────

mod pending_future_documentation {
    use super::*;

    /// Scan all `.rs` files for uses of `std::future::pending()` and verify
    /// that each usage has an explanatory comment within 5 lines above.
    ///
    /// `std::future::pending()` creates a future that never completes and
    /// never registers a waker. It is a dangerous pattern that can silently
    /// hang tasks if used incorrectly. Every usage must be accompanied by
    /// a nearby comment explaining why it is safe in context.
    ///
    /// The comment must contain at least one of these keywords/phrases:
    /// "never wake", "noop waker", "pending", "polling", "never completes".
    ///
    /// Lines inside doc comments (`///` or `//!`) that merely *mention*
    /// `std::future::pending()` (e.g., in module-level documentation) are
    /// not flagged — only actual `.await` call sites are checked.
    #[test]
    fn all_std_future_pending_usages_have_explanatory_comments() {
        let root = project_root();
        let mut violations: Vec<String> = Vec::new();

        fn visit_rs_files(
            dir: &std::path::Path,
            root: &std::path::Path,
            violations: &mut Vec<String>,
        ) {
            let entries = std::fs::read_dir(dir).unwrap_or_else(|e| {
                panic!("Failed to read directory '{}': {e}", dir.display());
            });
            for entry in entries {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.is_dir() {
                    visit_rs_files(&path, root, violations);
                } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    check_file(&path, root, violations);
                }
            }
        }

        fn check_file(
            path: &std::path::Path,
            root: &std::path::Path,
            violations: &mut Vec<String>,
        ) {
            let contents = std::fs::read_to_string(path).unwrap_or_else(|e| {
                panic!("Failed to read '{}': {e}", path.display());
            });
            let lines: Vec<&str> = contents.lines().collect();
            let relative = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();

            let required_keywords = [
                "never wake",
                "noop waker",
                "pending",
                "polling",
                "never completes",
            ];

            // Build the search needle by concatenation so this test file
            // does not self-match when scanned.
            let needle = format!("std::future::{}().await", "pending");

            for (i, line) in lines.iter().enumerate() {
                let trimmed = line.trim();

                // Only check actual `.await` call sites. This filters out:
                // - doc comments that merely discuss the pattern
                // - string literals that mention the function name
                // - comments referencing the function
                if !trimmed.contains(&needle) {
                    continue;
                }

                // Skip doc comments (/// and //!) — these describe the
                // pattern but are not actual usages.
                if trimmed.starts_with("///") || trimmed.starts_with("//!") {
                    continue;
                }

                // Skip comment lines that merely reference the call site.
                if trimmed.starts_with("//") {
                    continue;
                }

                // Look at up to 5 lines above for a comment containing
                // at least one required keyword.
                let window_start = i.saturating_sub(5);
                let has_keyword = lines[window_start..i].iter().any(|prev_line| {
                    let prev_trimmed = prev_line.trim();
                    // Only consider comment lines.
                    if !prev_trimmed.starts_with("//") {
                        return false;
                    }
                    let lower = prev_trimmed.to_lowercase();
                    required_keywords.iter().any(|kw| lower.contains(kw))
                });

                if !has_keyword {
                    violations.push(format!(
                        "{}:{}: `{needle}` usage lacks an explanatory comment \
                         within 5 lines above. Add a comment containing one \
                         of: {required_keywords:?}",
                        relative,
                        i + 1,
                    ));
                }
            }
        }

        // Scan both src/ and tests/ directories.
        let src_dir = root.join("src");
        let tests_dir = root.join("tests");
        visit_rs_files(&src_dir, &root, &mut violations);
        if tests_dir.is_dir() {
            visit_rs_files(&tests_dir, &root, &mut violations);
        }

        let needle_display = format!("std::future::{}().await", "pending");
        let joined = violations.join("\n");
        assert!(
            violations.is_empty(),
            "Found undocumented `{needle_display}` call sites. This pattern \
             creates a future that never completes and never registers a \
             waker, which can silently hang tasks. Every usage must have an \
             explanatory comment within 5 lines above.\n\nViolations:\n\
             {joined}"
        );
    }
}
