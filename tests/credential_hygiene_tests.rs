#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
//! Guard for the cloud-auth credential story (issue #222).
//!
//! The SDK's credential surfaces are deliberate and secret-shaped: the
//! `app_id` is a public label, the legacy cloud control-plane key is an
//! `sfk_…` string that never belongs in this repository, and the tenant
//! connect token (`sfct_v1.…`, upstream issue #517) is configured through
//! `SignalFishConfig::with_connect_token`. No example, doc page, or README
//! may normalize pasting credential-shaped material into the SDK — the
//! hygiene failure the credential API must never reintroduce.
//!
//! The scan is repository-relative and fails loudly when the documentation
//! tree cannot be found: layout drift must fix the guard, not skip it. (The
//! published packages exclude `tests/`, so crate-packaging test runs never
//! execute this file.) Known limitation: a credential wrapped across a line
//! break stays under the per-line run threshold.

use std::path::{Path, PathBuf};

/// Credential-shaped prefixes this repository must never carry as literals:
/// the legacy cloud `sfk_` application keys and the `sfct_v1.` tenant
/// connect tokens (their signed wire form).
const CREDENTIAL_PREFIXES: &[(&str, CredentialShape)] = &[
    ("sfk_", CredentialShape::Hex),
    ("sfct_v1.", CredentialShape::Base64UrlSegment),
];

/// The character run that must follow a prefix for the line to count as a
/// credential literal. Generic mentions of the prefix (the docs page that
/// defines the credential story) do not match.
#[derive(Clone, Copy)]
enum CredentialShape {
    /// Legacy `sfk_` keys are hex runs.
    Hex,
    /// A `sfct_v1.` token's payload segment is a base64url run followed by
    /// the segment separator.
    Base64UrlSegment,
}

impl CredentialShape {
    fn is_run_char(self, c: char) -> bool {
        match self {
            Self::Hex => c.is_ascii_hexdigit(),
            Self::Base64UrlSegment => c.is_ascii_alphanumeric() || c == '_' || c == '-',
        }
    }
}

/// A credential-shaped literal carries at least this many run characters
/// after the prefix.
const MIN_RUN: usize = 8;

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
    for (prefix, shape) in CREDENTIAL_PREFIXES {
        let mut rest = line;
        while let Some(position) = rest.find(prefix) {
            let after = &rest[position + prefix.len()..];
            let run = after.chars().take_while(|c| shape.is_run_char(*c)).count();
            if run >= MIN_RUN {
                return true;
            }
            rest = after;
        }
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
    // Credential-shaped: token prefix plus a base64url payload segment.
    assert!(is_credential_literal(
        "connect_token: sfct_v1.cGF5bG9hZEJ5dGVz.sig"
    ));
    assert!(is_credential_literal(
        ".with_connect_token(\"sfct_v1.AAAA-BBBB_CCCC-DDDD.signature\")"
    ));
    // Generic mentions stay legal.
    assert!(!is_credential_literal("secret keys (`sfk_…`)"));
    assert!(!is_credential_literal("never paste sfk_ keys here"));
    assert!(!is_credential_literal("no prefix at all"));
    // The documented token format is prose, not a literal.
    assert!(!is_credential_literal("`sfct_v1.<base64url(payload)>`"));
    // Short hex runs are not credentials.
    assert!(!is_credential_literal("sfk_1234"));
    // A lone `sfct_v1.` mention without a payload run is not a credential.
    assert!(!is_credential_literal("the sfct_v1. token format"));
}
