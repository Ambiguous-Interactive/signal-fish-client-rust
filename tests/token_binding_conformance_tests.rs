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
fn pinned_server_080_token_binding_provenance_is_complete() {
    let provenance_path = "tests/token-binding/PROVENANCE.toml";
    let vectors_path = "src/testdata/token-binding-vectors.toml";
    let provenance_text =
        std::fs::read_to_string(provenance_path).expect("read token-binding provenance");
    let provenance: toml::Value =
        toml::from_str(&provenance_text).expect("parse token-binding provenance");
    assert_eq!(
        provenance["server_commit"].as_str(),
        Some("1975db5d900221331a2abffb9fa6762fe9c6e502")
    );
    assert_eq!(provenance["server_release"].as_str(), Some("0.8.0"));
    let upstream = provenance["upstream_sha256"]
        .as_table()
        .expect("upstream hashes must be a table");
    let expected_upstream = [
        (
            "src/security/tls.rs",
            "2d729abca710064b888982adbec94316c60fa9e7bfcec05e0cb9ca94ba2171d3",
        ),
        (
            "src/security/token_binding.rs",
            "63ab131c47f1a61bc64217530e0bbb46e4d138e154178faf668c5d17f2277f52",
        ),
        (
            "src/websocket/token_binding.rs",
            "2ddf2af30f758eda333026422b88743f042a098170525922078f0899d54e3f2f",
        ),
        (
            "docs/configuration-recipes.md",
            "d144c945ffe0306460b4f486f651791104dc1c9d8cc5c9e50e720368c414e66a",
        ),
        (
            "tests/fixtures/tls/client-101-cert.pem",
            "73559903c2783614576e2b5a87c8778b111aaffba5ba0de54bebc02ff0e20282",
        ),
        (
            "tests/mtls_token_binding_e2e.rs",
            "9d25e8a92c7d1ec98c5ef5a3d590f9c152eb1450686e06a517b4d3ee2b162351",
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
    let fingerprint = vectors["client_fingerprint"]
        .as_str()
        .expect("fingerprint vector must be a string");
    assert_eq!(fingerprint.len(), 64);
    assert!(
        fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "fingerprint must be lowercase hexadecimal"
    );
    let pinned_pem = include_str!("token-binding/client-101-cert.pem");
    assert_eq!(
        Sha256::digest(pinned_pem.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        upstream["tests/fixtures/tls/client-101-cert.pem"]
            .as_str()
            .expect("pinned certificate source hash must exist")
    );
    let pinned_der = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        pinned_pem
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .collect::<String>(),
    )
    .expect("pinned client certificate must be valid PEM");
    assert_eq!(
        Sha256::digest(pinned_der)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        fingerprint
    );
    for field in [
        "fingerprint_json_signature_base64",
        "fingerprint_binary_signature_base64",
        "fingerprint_json_mac_input_hex",
        "fingerprint_binary_mac_input_hex",
    ] {
        assert!(
            vectors[field]
                .as_str()
                .is_some_and(|value| !value.is_empty()),
            "missing fingerprint-bound vector field {field}"
        );
    }
}
