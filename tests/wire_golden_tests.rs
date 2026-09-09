#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
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
use signal_fish_client::protocol::{ClientMessage, RoomOperationRequest, ServerMessage};

/// Fixed identifier for the pin inventory (a wire token, not a semantic value).
fn probe_player_id() -> signal_fish_client::protocol::PlayerId {
    uuid::Uuid::from_u128(0x0b0b)
}

fn probe_operation_id() -> signal_fish_client::protocol::RoomOperationId {
    uuid::Uuid::from_u128(0x0d0d)
}

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
fn v2_join_room_sample_omission_convention_is_preserved() {
    // The schema marks room_code/max_players/supports_authority/relay_transport
    // as optional members that are OMITTED when unset ("Omit to create a new
    // room"), and the real server sample carries only the set ones. Our
    // round-trip must preserve that omission convention instead of emitting
    // explicit nulls, which fail strict JSON-Schema type validation of these
    // properties.
    for raw in V2_CLIENT.lines() {
        let line = raw.trim();
        if !line.contains("\"JoinRoom\"") {
            continue;
        }
        let typed: ClientMessage = serde_json::from_str(line).expect("v2 JoinRoom deserializes");
        let json = serde_json::to_string(&typed).expect("serialize");
        assert!(
            !json.contains("max_players"),
            "round-trip injected an unset-optional member: {json}"
        );
        assert!(!json.contains("supports_authority"), "{json}");
        assert!(!json.contains("relay_transport"), "{json}");
        return;
    }
    panic!("expected a JoinRoom line in the v2 client samples");
}

#[test]
fn quick_match_join_room_omits_every_unset_optional_member() {
    // Quick match (no builder options) must produce exactly the
    // schema-required members in both the legacy and negotiated forms.
    let plain = ClientMessage::JoinRoom {
        game_name: "game".to_string(),
        room_code: None,
        player_name: "Alice".to_string(),
        max_players: None,
        supports_authority: None,
        relay_transport: None,
        password: None,
    };
    let value = serde_json::to_value(&plain).expect("plain join serializes");
    let data = value
        .get("data")
        .and_then(serde_json::Value::as_object)
        .expect("adjacent tagging keeps members under data");
    assert_eq!(
        data.keys().collect::<Vec<_>>(),
        ["game_name", "player_name"],
        "unset optionals must be omitted, not null: {value}"
    );

    let negotiated = ClientMessage::RoomOperation {
        operation_id: uuid::Uuid::from_u128(0xaaaa_aaaa_aaaa_aaaa_aaaa_aaaa_aaaa_aaaa),
        operation: Box::new(RoomOperationRequest::JoinRoom {
            game_name: "game".to_string(),
            room_code: None,
            player_name: "Alice".to_string(),
            max_players: None,
            supports_authority: None,
            relay_transport: None,
            password: None,
        }),
    };
    let value = serde_json::to_value(&negotiated).expect("negotiated join serializes");
    let operation = value
        .pointer("/data/operation/data")
        .and_then(serde_json::Value::as_object)
        .expect("nested join keeps members under data/operation/data");
    assert_eq!(
        operation.keys().collect::<Vec<_>>(),
        ["game_name", "player_name"],
        "unset optionals must be omitted inside the envelope too: {value}"
    );

    // A set password serializes into both forms; the omission contract above
    // keeps unset joins byte-identical to the pre-access-control wire.
    let sealed = ClientMessage::JoinRoom {
        game_name: "game".to_string(),
        room_code: None,
        player_name: "Alice".to_string(),
        max_players: None,
        supports_authority: None,
        relay_transport: None,
        password: Some("secret".to_string()),
    };
    let value = serde_json::to_value(&sealed).expect("sealed join serializes");
    assert_eq!(
        value
            .pointer("/data/password")
            .and_then(serde_json::Value::as_str),
        Some("secret"),
        "a set password must serialize under data: {value}"
    );
    let sealed_negotiated = ClientMessage::RoomOperation {
        operation_id: uuid::Uuid::from_u128(0xaaaa_aaaa_aaaa_aaaa_aaaa_aaaa_aaaa_aaaa),
        operation: Box::new(RoomOperationRequest::JoinRoom {
            game_name: "game".to_string(),
            room_code: None,
            player_name: "Alice".to_string(),
            max_players: None,
            supports_authority: None,
            relay_transport: None,
            password: Some("secret".to_string()),
        }),
    };
    let value =
        serde_json::to_value(&sealed_negotiated).expect("sealed negotiated join serializes");
    assert_eq!(
        value
            .pointer("/data/operation/data/password")
            .and_then(serde_json::Value::as_str),
        Some("secret"),
        "a set password must serialize inside the negotiated envelope: {value}"
    );
}

#[test]
fn sealed_spectator_join_serializes_password_in_both_forms() {
    // The spectator-join password face (spec: JoinAsSpectator.password) must
    // serialize in both the legacy and negotiated forms, and the plain
    // spectator join must keep omitting the member entirely.
    let plain = ClientMessage::JoinAsSpectator {
        game_name: "game".to_string(),
        room_code: "ABC123".to_string(),
        spectator_name: "Watcher".to_string(),
        password: None,
    };
    let value = serde_json::to_value(&plain).expect("plain spectator join serializes");
    assert!(
        !value
            .pointer("/data")
            .expect("data object")
            .as_object()
            .expect("object")
            .contains_key("password"),
        "unset spectator password must be omitted, not null: {value}"
    );

    let sealed = ClientMessage::JoinAsSpectator {
        game_name: "game".to_string(),
        room_code: "ABC123".to_string(),
        spectator_name: "Watcher".to_string(),
        password: Some("hunter2".to_string()),
    };
    let value = serde_json::to_value(&sealed).expect("sealed spectator join serializes");
    assert_eq!(
        value
            .pointer("/data/password")
            .and_then(serde_json::Value::as_str),
        Some("hunter2"),
        "a set spectator password must serialize under data: {value}"
    );

    let sealed_negotiated = ClientMessage::RoomOperation {
        operation_id: uuid::Uuid::from_u128(0xaaaa_aaaa_aaaa_aaaa_aaaa_aaaa_aaaa_aaaa),
        operation: Box::new(RoomOperationRequest::JoinAsSpectator {
            game_name: "game".to_string(),
            room_code: "ABC123".to_string(),
            spectator_name: "Watcher".to_string(),
            password: Some("hunter2".to_string()),
        }),
    };
    let value = serde_json::to_value(&sealed_negotiated)
        .expect("sealed negotiated spectator join serializes");
    assert_eq!(
        value
            .pointer("/data/operation/data/password")
            .and_then(serde_json::Value::as_str),
        Some("hunter2"),
        "a set spectator password must serialize inside the negotiated envelope: {value}"
    );
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

/// Hand-built spectator server-message wire fixtures.
///
/// The upstream server publishes no `SpectatorJoined`/`SpectatorLeft` (or
/// related) sample lines, so the vendored `.jsonl` corpus is blind to the
/// spectator lifecycle: a serde-shape drift there could never fail the golden
/// conformance layer (round-20 finding F1 — a schema-optional `room_id`
/// quarantining clients — is exactly the class this blindness allowed).
///
/// These fixtures are hand-built to the **vendored AsyncAPI authority**
/// (`tests/server-spec/signal-fish-protocol.asyncapi.yaml`, checksum-pinned):
/// `SpectatorJoined` oneOf branches (V2/V3/empty-room), `SpectatorJoinFailed`,
/// `SpectatorLeft` (including the schema-optional `room_id` omission),
/// `NewSpectatorJoined`, and `SpectatorDisconnected`. Every line must
/// deserialize into `ServerMessage` and round-trip to a semantically
/// identical JSON object, exactly like the complete v3 samples.
#[test]
fn spectator_server_wire_fixtures_conform() {
    const SPECTATOR_SERVER_MESSAGES: &str = r#"
{"type": "SpectatorJoined", "data": {"room_id": "11111111-1111-1111-1111-111111111111", "room_code": "ABC123", "spectator_id": "00000000-0000-0000-0000-0000000000c1", "game_name": "test_game", "current_players": [{"id": "00000000-0000-0000-0000-00000000000a", "name": "Alice", "is_authority": true, "is_ready": false, "connected_at": "2024-01-02T03:04:05Z", "epoch": 1, "seq": 42}], "current_spectators": [{"id": "00000000-0000-0000-0000-0000000000c1", "name": "Observer", "connected_at": "2024-01-02T03:06:07Z"}], "lobby_state": "waiting", "reason": "joined"}}
{"type": "SpectatorJoined", "data": {"room_id": "11111111-1111-1111-1111-111111111111", "room_code": "ABC123", "spectator_id": "00000000-0000-0000-0000-0000000000c1", "game_name": "test_game", "current_players": [{"id": "00000000-0000-0000-0000-00000000000a", "name": "Alice", "is_authority": true, "is_ready": false, "connected_at": "2024-01-02T03:04:05Z"}], "current_spectators": [{"id": "00000000-0000-0000-0000-0000000000c1", "name": "Observer", "connected_at": "2024-01-02T03:06:07Z"}], "lobby_state": "lobby"}}
{"type": "SpectatorJoined", "data": {"room_id": "11111111-1111-1111-1111-111111111111", "room_code": "ABC123", "spectator_id": "00000000-0000-0000-0000-0000000000c1", "game_name": "test_game", "current_players": [], "current_spectators": [{"id": "00000000-0000-0000-0000-0000000000c1", "name": "Observer", "connected_at": "2024-01-02T03:06:07Z"}], "lobby_state": "waiting", "reason": "joined"}}
{"type": "SpectatorJoinFailed", "data": {"reason": "room is full", "error_code": "TOO_MANY_SPECTATORS"}}
{"type": "SpectatorJoinFailed", "data": {"reason": "spectators are not allowed in this room"}}
{"type": "SpectatorLeft", "data": {"room_id": "11111111-1111-1111-1111-111111111111", "room_code": "ABC123", "reason": "voluntary_leave", "current_spectators": [{"id": "00000000-0000-0000-0000-0000000000c2", "name": "Second", "connected_at": "2024-01-02T03:06:08Z"}]}}
{"type": "SpectatorLeft", "data": {"reason": "removed", "current_spectators": []}}
{"type": "NewSpectatorJoined", "data": {"spectator": {"id": "00000000-0000-0000-0000-0000000000c2", "name": "Second", "connected_at": "2024-01-02T03:06:08Z"}, "current_spectators": [{"id": "00000000-0000-0000-0000-0000000000c1", "name": "Observer", "connected_at": "2024-01-02T03:06:07Z"}, {"id": "00000000-0000-0000-0000-0000000000c2", "name": "Second", "connected_at": "2024-01-02T03:06:08Z"}], "reason": "joined"}}
{"type": "SpectatorDisconnected", "data": {"spectator_id": "00000000-0000-0000-0000-0000000000c2", "reason": "disconnected", "current_spectators": [{"id": "00000000-0000-0000-0000-0000000000c1", "name": "Observer", "connected_at": "2024-01-02T03:06:07Z"}]}}
"#;
    assert_conformance::<ServerMessage>("spectator-server-fixtures", SPECTATOR_SERVER_MESSAGES);
}

/// Hand-built authority/relay/reconnect server-message wire fixtures.
///
/// The upstream server publishes no `AuthorityChanged`, `RelayStats`,
/// `PlayerReconnected`, `RoomJoinFailed`, or v3 `PlayerLeft` sample lines, so
/// the vendored `.jsonl` corpus is blind to every one of those frames: a serde
/// shape drift there could only ever surface on a live server (network-gated)
/// or through mock frames the test suite built itself from the same types
/// under test — the exact blindness class the spectator fixtures above closed
/// for the spectator lifecycle.
///
/// These fixtures are hand-built to the **vendored AsyncAPI authority**
/// (`tests/server-spec/signal-fish-protocol.asyncapi.yaml`, checksum-pinned):
/// `AuthorityChanged` (peer/you/`null`-vacated authority), `RelayStats`
/// (cumulative counters), `PlayerReconnected` (v2 epoch-less and v3 `epoch`
/// faces), `PlayerLeft` (v3 terminal-watermark face and the bare v2
/// top-level face), `RoomJoinFailed` (with and without the schema-optional
/// `error_code`), the spectator fan-out-slimming v3 faces
/// (`current_spectators: []` + `spectator_count`), the granted
/// `AuthorityResponse` face (`reason` schema-required, nullable), and the
/// `GoingAway` advisory with the optional `retry_after_secs` omitted.
/// Every line must deserialize into `ServerMessage` and round-trip to a
/// semantically identical JSON object, exactly like the complete v3 samples.
#[test]
fn authority_relay_reconnect_server_wire_fixtures_conform() {
    const SERVER_MESSAGES: &str = r#"
{"type": "AuthorityChanged", "data": {"authority_player": "00000000-0000-0000-0000-00000000000b", "you_are_authority": false}}
{"type": "AuthorityChanged", "data": {"authority_player": "00000000-0000-0000-0000-00000000000a", "you_are_authority": true}}
{"type": "AuthorityChanged", "data": {"authority_player": null, "you_are_authority": false}}
{"type": "RelayStats", "data": {"interval_ms": 5000, "sent_to_you": 1234, "dropped_for_you": 0, "backpressure_events": 2}}
{"type": "PlayerReconnected", "data": {"player_id": "00000000-0000-0000-0000-00000000000a"}}
{"type": "PlayerReconnected", "data": {"player_id": "00000000-0000-0000-0000-00000000000a", "epoch": 2}}
{"type": "PlayerLeft", "data": {"player_id": "00000000-0000-0000-0000-00000000000a", "epoch": 1, "final_seq": 42}}
{"type": "RoomJoinFailed", "data": {"reason": "room is full", "error_code": "ROOM_FULL"}}
{"type": "RoomJoinFailed", "data": {"reason": "room not found"}}
{"type": "NewSpectatorJoined", "data": {"spectator": {"id": "00000000-0000-0000-0000-0000000000c2", "name": "Second", "connected_at": "2024-01-02T03:06:08Z"}, "current_spectators": [], "reason": "joined", "spectator_count": 3}}
{"type": "SpectatorDisconnected", "data": {"spectator_id": "00000000-0000-0000-0000-0000000000c2", "reason": "disconnected", "current_spectators": [], "spectator_count": 0}}
{"type": "PlayerLeft", "data": {"player_id": "00000000-0000-0000-0000-00000000000a"}}
{"type": "AuthorityResponse", "data": {"granted": true, "reason": null}}
{"type": "GoingAway", "data": {"deadline_ms": 1730000000000}}
"#;
    assert_conformance::<ServerMessage>("authority-relay-reconnect-fixtures", SERVER_MESSAGES);
}

/// Hand-built `GameDataBinary` JSON wire fixtures.
///
/// The vendored `.jsonl` corpus carries **no** `"type": "GameDataBinary"`
/// line: the AsyncAPI authority models the message's transport face as the
/// raw MessagePack envelope (`contentType: application/msgpack`), so no
/// sample exercises the typed variant's serde shape. The only prior
/// coverage was self-referential `serde_bytes` round-trips — a payload
/// face, encoding-token, or optional-`seq`/`epoch` drift passed the entire
/// suite. Both faces the authority's envelope schemas define are pinned: the
/// v3 face with delivery stamps and the minimal (pre-v3, stamp-less) face.
/// (The authority's transport face for this message is msgpack-only; these
/// lines pin the typed variant's JSON serde face, whose field vocabulary
/// the schemas prescribe.)
#[test]
fn binary_game_data_server_wire_fixtures_conform() {
    const GAME_DATA_BINARY_SERVER_MESSAGES: &str = r#"
{"type": "GameDataBinary", "data": {"from_player": "00000000-0000-0000-0000-00000000000a", "encoding": "json", "payload": [104, 105], "seq": 42, "epoch": 1}}
{"type": "GameDataBinary", "data": {"from_player": "00000000-0000-0000-0000-00000000000a", "encoding": "message_pack", "payload": [104, 105]}}
"#;
    assert_conformance::<ServerMessage>(
        "binary-game-data-fixtures",
        GAME_DATA_BINARY_SERVER_MESSAGES,
    );
}

/// The v2-dialect client-message floor must parse from hand-built wire
/// lines.
///
/// The vendored v2 client samples are illustrative placeholders that the
/// corpus tests only check for JSON validity (`assert_structural`), so
/// `AuthorityRequest`, `PlayerReady`, `Ping`, `LeaveRoom`, `LeaveSpectator`,
/// `JoinAsSpectator`, and `ProvideConnectionInfo` had no wire-shaped
/// deserialize coverage — only struct round-trips built from the types under
/// test. These lines are the authority's complete v2 shapes: the no-payload
/// schemas (`Ping`, `PlayerReady`, `LeaveRoom`, `LeaveSpectator`) require
/// only the `type` tag (our serializer likewise omits `data`; the tolerant
/// `"data": null` input face is a serde detail, not the pinned wire form),
/// the `JoinAsSpectator` line pins the full open-room spectator face, and
/// `ProvideConnectionInfo` wraps an internally-tagged `direct` info object.
#[test]
fn v2_client_message_wire_fixtures_conform() {
    const V2_CLIENT_MESSAGES: &str = r#"
{"type": "AuthorityRequest", "data": {"become_authority": true}}
{"type": "PlayerReady"}
{"type": "Ping"}
{"type": "LeaveRoom"}
{"type": "LeaveSpectator"}
{"type": "JoinAsSpectator", "data": {"game_name": "my-game", "room_code": "ABC123", "spectator_name": "Watcher"}}
{"type": "ProvideConnectionInfo", "data": {"connection_info": {"type": "direct", "host": "127.0.0.1", "port": 7777}}}
"#;
    assert_conformance::<ClientMessage>("v2-client-fixtures", V2_CLIENT_MESSAGES);
}

/// Every outbound wire face must be pinned by a non-co-drifting fixture or
/// sample pin.
///
/// Round-trip tests are self-referential (a serde-attribute rename moves both
/// faces together), and perf-lab ledger digests only fail on byte drift until
/// deliberately refreshed, so the pins named here are the durable nets for
/// each outbound face: hand-built, authority-derived fixtures that fail on a
/// one-sided outbound drift, or byte-exact ledgers checked on every CI run.
/// Both matches below are deliberately exhaustive and wildcard-free:
/// adding a `ClientMessage` or `RoomOperationRequest` variant fails to
/// compile until this inventory (and a real wire pin) covers its outbound
/// face.
fn client_message_outbound_pin(message: &ClientMessage) -> &'static str {
    match message {
        ClientMessage::Authenticate { .. } => {
            "v3-client corpus `Authenticate` line (both faces) plus the perf-lab lobby ledgers"
        }
        ClientMessage::JoinRoom { .. } => {
            "`quick_match_join_room_omits_every_unset_optional_member` (omission and \
             password faces, both forms) plus the perf-lab lobby ledgers"
        }
        ClientMessage::LeaveRoom => "`v2_client_message_wire_fixtures_conform` (`LeaveRoom` line)",
        ClientMessage::GameData { .. } => {
            "the perf-lab json/out ledgers (plain face, byte-exact) plus the v3-client \
             corpus `GameData` lines (classified faces, both directions)"
        }
        ClientMessage::AuthorityRequest { .. } => {
            "`v2_client_message_wire_fixtures_conform` (`AuthorityRequest` line)"
        }
        ClientMessage::PlayerReady => {
            "`v2_client_message_wire_fixtures_conform` (`PlayerReady` line)"
        }
        ClientMessage::ProvideConnectionInfo { .. } => {
            "`v2_client_message_wire_fixtures_conform` (`ProvideConnectionInfo` line)"
        }
        ClientMessage::Ping => "`v2_client_message_wire_fixtures_conform` (`Ping` line)",
        ClientMessage::Reconnect { .. } => {
            "the perf-lab reconnect ledgers (byte-exact, checked on every CI run)"
        }
        ClientMessage::JoinAsSpectator { .. } => {
            "`v2_client_message_wire_fixtures_conform` (`JoinAsSpectator` line, full \
             shape) plus `sealed_spectator_join_serializes_password_in_both_forms` \
             (password faces)"
        }
        ClientMessage::LeaveSpectator => {
            "`v2_client_message_wire_fixtures_conform` (`LeaveSpectator` line)"
        }
        ClientMessage::RoomOperation { operation, .. } => room_operation_outbound_pin(operation),
        ClientMessage::StartGame => "`client_message_start_game_unit_variant_has_no_data`",
        ClientMessage::Signal { .. } => "v3-client corpus `Signal` lines (both faces)",
        ClientMessage::TransportStatus { .. } => {
            "v3-client corpus `TransportStatus` line (both faces)"
        }
    }
}

fn room_operation_outbound_pin(operation: &RoomOperationRequest) -> &'static str {
    match operation {
        RoomOperationRequest::JoinRoom { .. } => {
            "`correlated_client_operations_have_exact_nested_shapes` (join_room case)"
        }
        RoomOperationRequest::LeaveRoom => {
            "`correlated_client_operations_have_exact_nested_shapes` (leave_room case)"
        }
        RoomOperationRequest::Reconnect { .. } => {
            "`correlated_client_operations_have_exact_nested_shapes` (reconnect case)"
        }
        RoomOperationRequest::JoinAsSpectator { .. } => {
            "`correlated_client_operations_have_exact_nested_shapes` (open and sealed cases)"
        }
        RoomOperationRequest::LeaveSpectator => {
            "`correlated_client_operations_have_exact_nested_shapes` (leave_spectator case)"
        }
        RoomOperationRequest::KickPlayer { .. } => {
            "v3-client corpus `RoomOperation`/`KickPlayer` line (both faces)"
        }
        RoomOperationRequest::RegenerateRoomCode => {
            "v3-client corpus `RoomOperation`/`RegenerateRoomCode` line (both faces)"
        }
        RoomOperationRequest::SetRoomAccess { .. } => {
            "`access_control_surface_round_trips_with_exact_wire_tokens` (SetRoomAccess cases)"
        }
        RoomOperationRequest::BanPlayer { .. } => {
            "`access_control_surface_round_trips_with_exact_wire_tokens` (BanPlayer case)"
        }
        RoomOperationRequest::UnbanPlayer { .. } => {
            "`access_control_surface_round_trips_with_exact_wire_tokens` (UnbanPlayer case)"
        }
        RoomOperationRequest::TransferAuthority { .. } => {
            "`access_control_surface_round_trips_with_exact_wire_tokens` (TransferAuthority case)"
        }
    }
}

#[test]
fn every_outbound_wire_face_has_a_named_pin() {
    // The enforcement is the wildcard-free matches above (a new variant fails
    // to compile until it names a pin); exercising representative arms here
    // keeps both functions live and documents the inventory in runnable form.
    let kick_player_id = probe_player_id();
    let messages = [
        ClientMessage::Ping,
        ClientMessage::LeaveRoom,
        ClientMessage::LeaveSpectator,
        ClientMessage::PlayerReady,
        ClientMessage::StartGame,
        ClientMessage::RoomOperation {
            operation_id: probe_operation_id(),
            operation: Box::new(RoomOperationRequest::LeaveRoom),
        },
        ClientMessage::RoomOperation {
            operation_id: probe_operation_id(),
            operation: Box::new(RoomOperationRequest::KickPlayer {
                player_id: kick_player_id,
            }),
        },
        ClientMessage::RoomOperation {
            operation_id: probe_operation_id(),
            operation: Box::new(RoomOperationRequest::RegenerateRoomCode),
        },
    ];
    for message in &messages {
        let _ = client_message_outbound_pin(message);
    }

    let operations = [
        RoomOperationRequest::LeaveRoom,
        RoomOperationRequest::LeaveSpectator,
        RoomOperationRequest::RegenerateRoomCode,
    ];
    for operation in &operations {
        let _ = room_operation_outbound_pin(operation);
    }
}

/// The binary game-data envelopes must decode from hand-assembled bytes.
///
/// The fuzz seed corpus and every prior test built envelopes with this
/// repository's own encoder, so a decoder/encoder co-drift (both faces
/// moving together) was invisible. These goldens are assembled by hand from
/// the checksum-pinned authority's envelope schemas
/// (`V2BinaryGameDataEnvelope` / `V3BinaryGameDataEnvelope`): fixmap
/// headers, `bin8` UUID/payload fields, the string encoding tokens, and
/// minimal-width integer stamps. The third face deliberately uses
/// non-minimal encodings (`map16`, `bin16`, `uint32`, `uint16`) to pin the
/// decoder's documented "type-level shape, not minimal-length" tolerance.
#[test]
fn binary_game_data_envelopes_decode_from_hand_assembled_bytes() {
    use signal_fish_client::protocol::{
        decode_v2_binary_game_data, decode_v3_binary_game_data, GameDataEncoding,
    };

    fn from_hex(segments: &[&str]) -> Vec<u8> {
        segments
            .concat()
            .as_bytes()
            .chunks(2)
            .map(|pair| {
                assert_eq!(pair.len(), 2, "golden hex must have paired digits");
                let high = (pair[0] as char).to_digit(16).expect("hex digit");
                let low = (pair[1] as char).to_digit(16).expect("hex digit");
                (high * 16 + low) as u8
            })
            .collect()
    }

    let player = "00000000-0000-0000-0000-00000000000a";

    // fixmap(3) { "from_player": bin8(uuid), "encoding": "message_pack",
    //             "payload": bin8("hi") }
    let v2 = from_hex(&[
        "83",                                   // fixmap(3)
        "AB66726F6D5F706C61796572",             // "from_player"
        "C4100000000000000000000000000000000A", // bin8 uuid
        "A8656E636F64696E67",                   // "encoding"
        "AC6D6573736167655F7061636B",           // "message_pack"
        "A77061796C6F6164",                     // "payload"
        "C4026869",                             // bin8("hi")
    ]);
    let v2_frame = decode_v2_binary_game_data(&v2).expect("hand-assembled v2 envelope must decode");
    assert_eq!(v2_frame.from_player.to_string(), player);
    assert_eq!(v2_frame.encoding, GameDataEncoding::MessagePack);
    assert_eq!(v2_frame.payload, b"hi");

    // fixmap(5) { "from_player": bin8(uuid), "encoding": "json",
    //             "payload": bin8("hi"), "seq": 42, "epoch": 1 }
    let v3 = from_hex(&[
        "85",                                   // fixmap(5)
        "AB66726F6D5F706C61796572",             // "from_player"
        "C4100000000000000000000000000000000A", // bin8 uuid
        "A8656E636F64696E67",                   // "encoding"
        "A46A736F6E",                           // "json"
        "A77061796C6F6164",                     // "payload"
        "C4026869",                             // bin8("hi")
        "A3736571",                             // "seq"
        "2A",                                   // 42
        "A565706F6368",                         // "epoch"
        "01",                                   // 1
    ]);
    let v3_frame = decode_v3_binary_game_data(&v3).expect("hand-assembled v3 envelope must decode");
    assert_eq!(v3_frame.from_player.to_string(), player);
    assert_eq!(v3_frame.encoding, GameDataEncoding::Json);
    assert_eq!(v3_frame.payload, b"hi");
    assert_eq!(v3_frame.seq, 42);
    assert_eq!(v3_frame.epoch, 1);

    // The same v3 envelope in deliberately non-minimal widths:
    // map16(5), bin16 UUID, uint32 seq, uint16 epoch.
    let v3_wide = from_hex(&[
        "DE0005",                                 // map16(5)
        "AB66726F6D5F706C61796572",               // "from_player"
        "C500100000000000000000000000000000000A", // bin16 uuid
        "A8656E636F64696E67",                     // "encoding"
        "A46A736F6E",                             // "json"
        "A77061796C6F6164",                       // "payload"
        "C4026869",                               // bin8("hi")
        "A3736571",                               // "seq"
        "CE0000002A",                             // uint32 42
        "A565706F6368",                           // "epoch"
        "CD0001",                                 // uint16 1
    ]);
    let wide_frame =
        decode_v3_binary_game_data(&v3_wide).expect("non-minimal-width v3 envelope must decode");
    assert_eq!(wide_frame, v3_frame);
}

/// The `fuzz_binary_game_data` seed corpus must stay decodable.
///
/// The fuzz target keeps two canonical envelopes inline; the seed files under
/// `fuzz/seeds/` are a second, independently consumed copy (the Deep Safety
/// lane feeds the directory to libFuzzer). If the MessagePack wire shape ever
/// drifts, this pin forces the corpus to move with it instead of silently
/// degrading to error-string coverage past the map-header frontier.
#[test]
fn fuzz_binary_game_data_seed_corpus_decodes() {
    use signal_fish_client::protocol::{decode_v2_binary_game_data, decode_v3_binary_game_data};

    let v2 = std::fs::read("fuzz/seeds/fuzz_binary_game_data/v2_canonical.msgpack")
        .unwrap_or_else(|e| panic!("missing fuzz seed corpus v2_canonical.msgpack: {e}"));
    let v3 = std::fs::read("fuzz/seeds/fuzz_binary_game_data/v3_canonical.msgpack")
        .unwrap_or_else(|e| panic!("missing fuzz seed corpus v3_canonical.msgpack: {e}"));
    assert!(
        decode_v2_binary_game_data(&v2).is_ok(),
        "v2_canonical.msgpack must remain a decodable canonical envelope; refresh the fuzz seed corpus"
    );
    assert!(
        decode_v3_binary_game_data(&v3).is_ok(),
        "v3_canonical.msgpack must remain a decodable canonical envelope; refresh the fuzz seed corpus"
    );
}
