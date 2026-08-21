#![cfg(feature = "token-binding")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing
)]

use sha2::{Digest as _, Sha256};

#[test]
fn pinned_server_070_token_binding_provenance_is_complete() {
    let provenance_path = "tests/token-binding/PROVENANCE.toml";
    let vectors_path = "tests/token-binding/vectors.toml";
    let provenance_text =
        std::fs::read_to_string(provenance_path).expect("read token-binding provenance");
    let provenance: toml::Value =
        toml::from_str(&provenance_text).expect("parse token-binding provenance");
    assert_eq!(
        provenance["server_commit"].as_str(),
        Some("3f7f43d4cd4b3cc7f8fb893220dc35c9b1fad333")
    );
    assert_eq!(provenance["server_release"].as_str(), Some("0.7.0"));
    let upstream = provenance["upstream_sha256"]
        .as_table()
        .expect("upstream hashes must be a table");
    let expected_upstream = [
        (
            "src/security/token_binding.rs",
            "63ab131c47f1a61bc64217530e0bbb46e4d138e154178faf668c5d17f2277f52",
        ),
        (
            "src/websocket/token_binding.rs",
            "2eacb6f0a92bbe189fc4a9e8e8df312b3c434ec6bf2737147b62b8083d7b2fe3",
        ),
        (
            "docs/configuration-recipes.md",
            "cefa5380830b575f8b7f195afd08759a4a08a09f96aba0b76f2fe34b5c7e2796",
        ),
        (
            "tests/mtls_token_binding_e2e.rs",
            "b8158e517c52d4433a58fba6ecfb5cdbb718d312be82199de5749a48225e4617",
        ),
    ];
    assert_eq!(upstream.len(), expected_upstream.len());
    for (path, expected_hash) in expected_upstream {
        let actual = upstream[path]
            .as_str()
            .unwrap_or_else(|| panic!("missing upstream hash for {path}"));
        assert_eq!(actual, expected_hash, "upstream hash drifted for {path}");
        assert_eq!(actual.len(), 64);
        assert!(actual.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    let vectors = std::fs::read(vectors_path).expect("read token-binding vectors");
    let checksum: String = Sha256::digest(&vectors)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(
        provenance["vectors_sha256"].as_str(),
        Some(checksum.as_str())
    );

    let vectors_text =
        std::str::from_utf8(&vectors).expect("token-binding vectors must be UTF-8 TOML");
    let vectors: toml::Value = toml::from_str(vectors_text).expect("parse token-binding vectors");
    assert_eq!(
        vectors["hkdf_info"].as_str(),
        Some("signalfish.tokenbinding.v2/session-key")
    );
    assert_eq!(vectors["json_sequence"].as_integer(), Some(1));
    assert_eq!(vectors["binary_sequence"].as_integer(), Some(2));
}
