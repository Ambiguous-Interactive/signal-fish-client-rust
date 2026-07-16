#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing
)]
//! Golden-wire conformance tests against the Signal Fish **server's** published
//! protocol samples (vendored under `tests/wire-samples/`).
//!
//! For every sample line this asserts BOTH directions of the wire contract:
//!   1. the literal server JSON **deserializes** into our typed enum, and
//!   2. re-serializing our typed value reproduces a **semantically identical**
//!      JSON object (compared as `serde_json::Value`, so key order / whitespace
//!      are ignored — only the actual wire content is checked).
//!
//! The server's v3 samples are complete (real UUIDs, all fields), so they get
//! full round-trip conformance. The v2 samples are illustrative documentation
//! and use `"..."` placeholders for ids / partial payloads; such lines cannot be
//! strictly deserialized, so they are only checked to be valid JSON carrying a
//! `type`. A complete (non-placeholder) line that fails to deserialize is a real
//! conformance break and fails the test.
//!
//! See `.llm/skills/protocol-wire-conformance/SKILL.md` for the refresh procedure when
//! the server protocol changes.

use serde::{de::DeserializeOwned, Serialize};
use signal_fish_client::protocol::{ClientMessage, ServerMessage};

const V2_CLIENT: &str = include_str!("wire-samples/v2-client-messages.jsonl");
const V2_SERVER: &str = include_str!("wire-samples/v2-server-messages.jsonl");
const V3_CLIENT: &str = include_str!("wire-samples/v3-client-messages.jsonl");
const V3_SERVER: &str = include_str!("wire-samples/v3-server-messages.jsonl");

/// Strictly check every line of a vendored sample file for round-trip conformance.
///
/// Used only for the COMPLETE v3 samples: every line must deserialize into our
/// typed enum AND re-serialize to a semantically identical `Value`. There is no
/// placeholder escape — any deserialize failure is a real conformance drift.
/// (The illustrative v2 samples use [`assert_structural`] instead.)
fn assert_conformance<T: Serialize + DeserializeOwned>(name: &str, content: &str) {
    let mut checked = 0usize;

    for (idx, raw) in content.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        checked += 1;
        let lineno = idx + 1;

        // (0) Every line must be valid JSON carrying a `type` tag.
        let want: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("{name}:{lineno}: invalid JSON: {e}\n  line: {line}"));
        assert!(
            want.get("type")
                .and_then(serde_json::Value::as_str)
                .is_some(),
            "{name}:{lineno}: sample is missing a string `type` tag\n  line: {line}"
        );

        // (1) Deserialize into our typed enum (no tolerance — these are complete).
        let typed: T = serde_json::from_str(line).unwrap_or_else(|e| {
            panic!(
                "{name}:{lineno}: sample line failed to deserialize into our typed enum: \
                 {e}\n  line: {line}\n\n  The client types have drifted from the server \
                 wire format. See .llm/skills/protocol-wire-conformance/SKILL.md."
            )
        });
        // (2) Re-serialize and compare semantically (order-independent).
        let reserialized = serde_json::to_string(&typed)
            .unwrap_or_else(|e| panic!("{name}:{lineno}: re-serialize failed: {e}"));
        let got: serde_json::Value = serde_json::from_str(&reserialized)
            .expect("our own serialized output must be valid JSON");
        assert_eq!(
            got, want,
            "{name}:{lineno}: re-serialized JSON differs from the server sample\n  \
             sample: {want}\n  ours:   {got}"
        );
    }

    assert!(
        checked > 0,
        "{name}: no sample lines were checked — the vendored file is empty or missing"
    );
}

#[test]
fn v3_client_messages_conform() {
    // All v3 client samples are complete and must fully round-trip.
    assert_conformance::<ClientMessage>("v3-client-messages", V3_CLIENT);
}

#[test]
fn v3_server_messages_conform() {
    // All v3 server samples are complete and must fully round-trip.
    assert_conformance::<ServerMessage>("v3-server-messages", V3_SERVER);
}

/// Structural-only check: every line is valid JSON carrying a string `type`.
///
/// Used for the v2 samples, which are illustrative documentation (they elide
/// optional fields and use `"..."` placeholders for ids), so they cannot be
/// strictly round-tripped. The v2 wire format is byte-tested directly in
/// `tests/protocol_tests.rs` with complete messages; this just guards the
/// vendored corpus against gross format drift.
fn assert_structural(name: &str, content: &str) {
    let mut checked = 0usize;
    for (idx, raw) in content.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        checked += 1;
        let value: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("{name}:{}: invalid JSON: {e}\n  line: {line}", idx + 1));
        assert!(
            value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .is_some(),
            "{name}:{}: sample is missing a string `type` tag\n  line: {line}",
            idx + 1
        );
    }
    assert!(checked > 0, "{name}: no sample lines were checked");
}

#[test]
fn v2_client_messages_are_structurally_valid() {
    assert_structural("v2-client-messages", V2_CLIENT);
}

#[test]
fn v2_server_messages_are_structurally_valid() {
    assert_structural("v2-server-messages", V2_SERVER);
}

#[test]
fn v2_authenticate_sample_carries_no_v3_fields() {
    // The relay-floor guarantee, pinned against the REAL server v2 sample: a v2
    // Authenticate has none of the v3 negotiation keys, and our round-trip must
    // not inject them.
    for raw in V2_CLIENT.lines() {
        let line = raw.trim();
        if !line.contains("\"Authenticate\"") {
            continue;
        }
        let typed: ClientMessage =
            serde_json::from_str(line).expect("v2 Authenticate deserializes");
        let json = serde_json::to_string(&typed).expect("serialize");
        assert!(
            !json.contains("protocol_version"),
            "v2 round-trip injected v3 key: {json}"
        );
        assert!(!json.contains("supported_transports"), "{json}");
        assert!(!json.contains("supported_topologies"), "{json}");
        return;
    }
    panic!("expected an Authenticate line in the v2 client samples");
}

#[test]
fn v3_signal_payload_is_externally_tagged_in_samples() {
    // The real server samples carry signals as externally-tagged objects
    // (`{"Offer": …}` / `{"Answer": …}` / `{"IceCandidate": …}`), matching our
    // PeerSignal. Spot-check that a v3 Signal line's `signal` is such an object.
    let mut found = false;
    for raw in V3_SERVER.lines().chain(V3_CLIENT.lines()) {
        let line = raw.trim();
        if !line.contains("\"Signal\"") {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        let signal = &value["data"]["signal"];
        let obj = signal.as_object().expect("signal must be an object");
        assert_eq!(
            obj.len(),
            1,
            "externally-tagged signal has exactly one key: {signal}"
        );
        let tag = obj.keys().next().unwrap();
        assert!(
            matches!(tag.as_str(), "Offer" | "Answer" | "IceCandidate"),
            "unexpected signal tag in sample: {tag}"
        );
        found = true;
    }
    assert!(found, "expected at least one Signal line in the v3 samples");
}
