#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
//! Guard for the cloud-auth credential story (issue #222).
//!
//! The SDK has no surface for secret-form application keys (`sfk_…`): the
//! `app_id` is a public label and the documented credential slot is reserved
//! for a future wire contract. Until that surface exists, no example, doc
//! page, or README may normalize pasting a credential-shaped `sfk_` key into
//! the SDK — the hygiene failure the future credential API must never
//! reintroduce.
//!
//! The scan is repository-relative and fails loudly when the documentation
//! tree cannot be found: layout drift must fix the guard, not skip it. (The
//! published packages exclude `tests/`, so crate-packaging test runs never
//! execute this file.) Known limitation: a credential wrapped across a line
//! break stays under the per-line hex-run threshold.

use std::path::{Path, PathBuf};

const CREDENTIAL_PREFIX: &str = "sfk_";
/// A credential-shaped literal carries at least this many hex characters
/// after the prefix. Generic mentions of the prefix (the docs page that
/// defines the credential story) do not match.
const MIN_HEX_RUN: usize = 8;

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut candidate = Some(manifest);
    while let Some(root) = candidate {
        if root.join("docs").is_dir() && root.join("examples").is_dir() {
            return root;
        }
        candidate = root.parent().map(Path::to_path_buf);
    }
    panic!(
        "credential scan could not locate the repository tree: layout drift \
         must be fixed, not silently skipped"
    );
}

fn is_credential_literal(line: &str) -> bool {
    let mut rest = line;
    while let Some(position) = rest.find(CREDENTIAL_PREFIX) {
        let after = &rest[position + CREDENTIAL_PREFIX.len()..];
        let hex_run = after.chars().take_while(char::is_ascii_hexdigit).count();
        if hex_run >= MIN_HEX_RUN {
            return true;
        }
        rest = after;
    }
    false
}

fn collect_files(root: &Path, relative: &str, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root.join(relative)) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => {
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                // Vendored upstream authorities are copies of external
                // content; site assets are generated.
                if matches!(name, "server-spec" | "assets" | "includes" | "javascripts") {
                    continue;
                }
                let Ok(stripped) = path.strip_prefix(root) else {
                    continue;
                };
                collect_files(root, &stripped.to_string_lossy(), files);
            }
            Ok(_) => files.push(path),
            Err(_) => continue,
        }
    }
}

fn scanned_targets(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_files(root, "docs", &mut files);
    collect_files(root, "examples", &mut files);
    for readme in ["README.md", "CHANGELOG.md", "llms.txt"] {
        let path = root.join(readme);
        if path.is_file() {
            files.push(path);
        }
    }
    files.retain(|path| {
        matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some(
                "md" | "rs"
                    | "toml"
                    | "yml"
                    | "yaml"
                    | "html"
                    | "js"
                    | "sh"
                    | "py"
                    | "json"
                    | "txt"
            )
        )
    });
    files
}

#[test]
fn no_secret_credential_literals_in_examples_or_docs() {
    let root = repo_root();
    let files = scanned_targets(&root);
    assert!(
        !files.is_empty(),
        "the documentation tree must be discoverable for the credential scan"
    );

    let mut violations = Vec::new();
    for file in &files {
        let Ok(content) = std::fs::read_to_string(file) else {
            continue;
        };
        for (index, line) in content.lines().enumerate() {
            if is_credential_literal(line) {
                violations.push(format!(
                    "{}:{}: credential-shaped literal",
                    file.display(),
                    index + 1
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "secret-form credentials must never appear in examples or docs:\n{}",
        violations.join("\n")
    );
}

#[test]
fn credential_shape_detection_is_precise() {
    // Credential-shaped: prefix plus a hex run.
    assert!(is_credential_literal("app_id = \"sfk_0123456789abcdef\""));
    assert!(is_credential_literal("token: sfk_DEADBEEF"));
    // Generic mentions stay legal.
    assert!(!is_credential_literal("secret keys (`sfk_…`)"));
    assert!(!is_credential_literal("never paste sfk_ keys here"));
    assert!(!is_credential_literal("no prefix at all"));
    // Short hex runs are not credentials.
    assert!(!is_credential_literal("sfk_1234"));
}
