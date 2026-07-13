#![allow(clippy::indexing_slicing, clippy::panic)]

use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

fn sha256(path: &Path) -> String {
    let bytes = fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn compatibility_manifest_binds_exact_server_040_artifacts() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest: toml::Value = toml::from_str(
        &fs::read_to_string(root.join("tests/compatibility.toml"))
            .unwrap_or_else(|error| panic!("read compatibility manifest: {error}")),
    )
    .unwrap_or_else(|error| panic!("parse compatibility manifest: {error}"));

    let client_version = manifest["client_version"]
        .as_str()
        .unwrap_or_else(|| panic!("client_version must be a string"));
    let mut version_parts = client_version.split('.');
    for _ in 0..3 {
        let part = version_parts
            .next()
            .unwrap_or_else(|| panic!("client_version must be strict X.Y.Z"));
        assert!(
            !part.is_empty()
                && part.chars().all(|character| character.is_ascii_digit())
                && (part == "0" || !part.starts_with('0')),
            "client_version must be strict X.Y.Z"
        );
    }
    assert!(
        version_parts.next().is_none(),
        "client_version must be strict X.Y.Z"
    );
    assert_eq!(manifest["server_version"].as_str(), Some("0.4.0"));
    assert_eq!(manifest["server_tag"].as_str(), Some("v0.4.0"));
    let commit = manifest["server_commit"]
        .as_str()
        .unwrap_or_else(|| panic!("server_commit must be a string"));
    assert_eq!(commit, "50b28a9a13dc2b99d301bfb2482c5fd6f768a2e8");

    let wire_provenance: toml::Value = toml::from_str(
        &fs::read_to_string(root.join("tests/wire-samples/PROVENANCE.toml"))
            .unwrap_or_else(|error| panic!("read wire provenance: {error}")),
    )
    .unwrap_or_else(|error| panic!("parse wire provenance: {error}"));
    let spec_provenance: toml::Value = toml::from_str(
        &fs::read_to_string(root.join("tests/server-spec/PROVENANCE.toml"))
            .unwrap_or_else(|error| panic!("read spec provenance: {error}")),
    )
    .unwrap_or_else(|error| panic!("parse spec provenance: {error}"));

    assert_eq!(wire_provenance["upstream"]["commit"].as_str(), Some(commit));
    assert_eq!(spec_provenance["upstream"]["commit"].as_str(), Some(commit));
    assert_eq!(
        wire_provenance["upstream"]["synced"].as_str(),
        manifest["synced"].as_str()
    );
    assert_eq!(
        spec_provenance["upstream"]["synced"].as_str(),
        manifest["synced"].as_str()
    );

    for (name, expected) in manifest["wire_samples"]
        .as_table()
        .unwrap_or_else(|| panic!("wire_samples must be a table"))
    {
        let expected = expected
            .as_str()
            .unwrap_or_else(|| panic!("{name} hash must be a string"));
        assert_eq!(
            sha256(&root.join("tests/wire-samples").join(name)),
            expected,
            "{name} must remain byte-identical to server v0.4.0"
        );
        assert_eq!(
            wire_provenance["files"][name].as_str(),
            Some(expected),
            "{name} legacy provenance must agree"
        );
    }

    for (name, expected) in manifest["server_spec"]
        .as_table()
        .unwrap_or_else(|| panic!("server_spec must be a table"))
    {
        let expected = expected
            .as_str()
            .unwrap_or_else(|| panic!("{name} hash must be a string"));
        assert_eq!(
            sha256(&root.join("tests/server-spec").join(name)),
            expected,
            "{name} must remain byte-identical to server v0.4.0"
        );
        assert_eq!(
            spec_provenance["files"][name].as_str(),
            Some(expected),
            "{name} legacy provenance must agree"
        );
    }
}
