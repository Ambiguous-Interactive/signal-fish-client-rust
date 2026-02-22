#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing
)]
//! Protocol serialization tests for the Signal Fish Client.
//!
//! Verifies round-trip serialization of every protocol type, including all
//! `ClientMessage` and `ServerMessage` variants, `ErrorCode` SCREAMING_SNAKE_CASE
//! encoding, and JSON fixtures that match real server output.

use signal_fish_client::error_codes::ErrorCode;
use signal_fish_client::protocol::{
    ClientMessage, ConnectionInfo, GameDataEncoding, LobbyState, PeerConnectionInfo, PlayerInfo,
    PlayerNameRulesPayload, ProtocolInfoPayload, RateLimitInfo, ReconnectedPayload, RelayTransport,
    RoomJoinedPayload, ServerMessage, SpectatorInfo, SpectatorJoinedPayload,
    SpectatorStateChangeReason,
};

// ════════════════════════════════════════════════════════════════════
// Helper
// ════════════════════════════════════════════════════════════════════

/// Serialize `val` to JSON, then deserialize back to `T` and return it.
fn round_trip<T: serde::Serialize + serde::de::DeserializeOwned>(val: &T) -> T {
    let json = serde_json::to_string(val).expect("serialize");
    serde_json::from_str(&json).expect("deserialize")
}

fn nil_uuid() -> uuid::Uuid {
    uuid::Uuid::nil()
}

fn test_uuid(n: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128(n)
}

// ════════════════════════════════════════════════════════════════════
// ClientMessage round-trip tests (11 variants)
// ════════════════════════════════════════════════════════════════════

#[test]
fn client_message_authenticate_round_trip() {
    let msg = ClientMessage::Authenticate {
        app_id: "mb_app_test".into(),
        sdk_version: Some("0.1.0".into()),
        platform: Some("rust".into()),
        game_data_format: Some(GameDataEncoding::Json),
    };
    let json = serde_json::to_string(&msg).expect("serialize");
    let deser: ClientMessage = serde_json::from_str(&json).expect("deserialize");
    if let ClientMessage::Authenticate {
        app_id,
        sdk_version,
        platform,
        game_data_format,
    } = deser
    {
        assert_eq!(app_id, "mb_app_test");
        assert_eq!(sdk_version.as_deref(), Some("0.1.0"));
        assert_eq!(platform.as_deref(), Some("rust"));
        assert!(matches!(game_data_format, Some(GameDataEncoding::Json)));
    } else {
        panic!("expected Authenticate variant");
    }
}

#[test]
fn client_message_join_room_round_trip() {
    let msg = ClientMessage::JoinRoom {
        game_name: "my-game".into(),
        room_code: Some("ABC123".into()),
        player_name: "Alice".into(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: Some(RelayTransport::Udp),
    };
    let deser = round_trip(&msg);
    if let ClientMessage::JoinRoom {
        game_name,
        room_code,
        player_name,
        max_players,
        supports_authority,
        relay_transport,
    } = deser
    {
        assert_eq!(game_name, "my-game");
        assert_eq!(room_code.as_deref(), Some("ABC123"));
        assert_eq!(player_name, "Alice");
        assert_eq!(max_players, Some(4));
        assert_eq!(supports_authority, Some(true));
        assert!(matches!(relay_transport, Some(RelayTransport::Udp)));
    } else {
        panic!("expected JoinRoom variant");
    }
}

#[test]
fn client_message_leave_room_round_trip() {
    let msg = ClientMessage::LeaveRoom;
    let json = serde_json::to_string(&msg).expect("serialize");
    let deser: ClientMessage = serde_json::from_str(&json).expect("deserialize");
    assert!(matches!(deser, ClientMessage::LeaveRoom));
}

#[test]
fn client_message_game_data_round_trip() {
    let data = serde_json::json!({ "action": "move", "x": 10 });
    let msg = ClientMessage::GameData { data: data.clone() };
    let deser = round_trip(&msg);
    if let ClientMessage::GameData { data: d } = deser {
        assert_eq!(d, data);
    } else {
        panic!("expected GameData variant");
    }
}

#[test]
fn client_message_authority_request_round_trip() {
    let msg = ClientMessage::AuthorityRequest {
        become_authority: true,
    };
    let deser = round_trip(&msg);
    if let ClientMessage::AuthorityRequest { become_authority } = deser {
        assert!(become_authority);
    } else {
        panic!("expected AuthorityRequest variant");
    }
}

#[test]
fn client_message_player_ready_round_trip() {
    let msg = ClientMessage::PlayerReady;
    let json = serde_json::to_string(&msg).expect("serialize");
    let deser: ClientMessage = serde_json::from_str(&json).expect("deserialize");
    assert!(matches!(deser, ClientMessage::PlayerReady));
}

#[test]
fn client_message_provide_connection_info_round_trip() {
    let msg = ClientMessage::ProvideConnectionInfo {
        connection_info: ConnectionInfo::Direct {
            host: "127.0.0.1".into(),
            port: 7777,
        },
    };
    let deser = round_trip(&msg);
    if let ClientMessage::ProvideConnectionInfo { connection_info } = deser {
        if let ConnectionInfo::Direct { host, port } = connection_info {
            assert_eq!(host, "127.0.0.1");
            assert_eq!(port, 7777);
        } else {
            panic!("expected Direct connection info");
        }
    } else {
        panic!("expected ProvideConnectionInfo variant");
    }
}

#[test]
fn client_message_ping_round_trip() {
    let msg = ClientMessage::Ping;
    let json = serde_json::to_string(&msg).expect("serialize");
    let deser: ClientMessage = serde_json::from_str(&json).expect("deserialize");
    assert!(matches!(deser, ClientMessage::Ping));
}

#[test]
fn client_message_reconnect_round_trip() {
    let player_id = test_uuid(1);
    let room_id = test_uuid(2);
    let msg = ClientMessage::Reconnect {
        player_id,
        room_id,
        auth_token: "tok-123".into(),
    };
    let deser = round_trip(&msg);
    if let ClientMessage::Reconnect {
        player_id: pid,
        room_id: rid,
        auth_token,
    } = deser
    {
        assert_eq!(pid, player_id);
        assert_eq!(rid, room_id);
        assert_eq!(auth_token, "tok-123");
    } else {
        panic!("expected Reconnect variant");
    }
}

#[test]
fn client_message_join_as_spectator_round_trip() {
    let msg = ClientMessage::JoinAsSpectator {
        game_name: "game1".into(),
        room_code: "ROOM1".into(),
        spectator_name: "Watcher".into(),
    };
    let deser = round_trip(&msg);
    if let ClientMessage::JoinAsSpectator {
        game_name,
        room_code,
        spectator_name,
    } = deser
    {
        assert_eq!(game_name, "game1");
        assert_eq!(room_code, "ROOM1");
        assert_eq!(spectator_name, "Watcher");
    } else {
        panic!("expected JoinAsSpectator variant");
    }
}

#[test]
fn client_message_leave_spectator_round_trip() {
    let msg = ClientMessage::LeaveSpectator;
    let json = serde_json::to_string(&msg).expect("serialize");
    let deser: ClientMessage = serde_json::from_str(&json).expect("deserialize");
    assert!(matches!(deser, ClientMessage::LeaveSpectator));
}

// ════════════════════════════════════════════════════════════════════
// ServerMessage round-trip tests (24 variants)
// ════════════════════════════════════════════════════════════════════

#[test]
fn server_message_authenticated_round_trip() {
    let msg = ServerMessage::Authenticated {
        app_name: "My App".into(),
        organization: Some("Org".into()),
        rate_limits: RateLimitInfo {
            per_minute: 60,
            per_hour: 1000,
            per_day: 10000,
        },
    };
    let deser = round_trip(&msg);
    if let ServerMessage::Authenticated {
        app_name,
        organization,
        rate_limits,
    } = deser
    {
        assert_eq!(app_name, "My App");
        assert_eq!(organization.as_deref(), Some("Org"));
        assert_eq!(rate_limits.per_minute, 60);
        assert_eq!(rate_limits.per_hour, 1000);
        assert_eq!(rate_limits.per_day, 10000);
    } else {
        panic!("expected Authenticated variant");
    }
}

#[test]
fn server_message_protocol_info_round_trip() {
    let msg = ServerMessage::ProtocolInfo(ProtocolInfoPayload {
        platform: Some("rust".into()),
        sdk_version: Some("0.1.0".into()),
        minimum_version: None,
        recommended_version: None,
        capabilities: vec!["authority".into()],
        notes: None,
        game_data_formats: vec![GameDataEncoding::Json, GameDataEncoding::MessagePack],
        player_name_rules: Some(PlayerNameRulesPayload {
            max_length: 32,
            min_length: 1,
            allow_unicode_alphanumeric: true,
            allow_spaces: true,
            allow_leading_trailing_whitespace: false,
            allowed_symbols: vec!['-', '_'],
            additional_allowed_characters: None,
        }),
    });
    let deser = round_trip(&msg);
    if let ServerMessage::ProtocolInfo(payload) = deser {
        assert_eq!(payload.platform.as_deref(), Some("rust"));
        assert_eq!(payload.capabilities.len(), 1);
        assert_eq!(payload.game_data_formats.len(), 2);
        let rules = payload.player_name_rules.expect("rules present");
        assert_eq!(rules.max_length, 32);
        assert_eq!(rules.allowed_symbols, vec!['-', '_']);
    } else {
        panic!("expected ProtocolInfo variant");
    }
}

#[test]
fn server_message_authentication_error_round_trip() {
    let msg = ServerMessage::AuthenticationError {
        error: "invalid app id".into(),
        error_code: ErrorCode::InvalidAppId,
    };
    let deser = round_trip(&msg);
    if let ServerMessage::AuthenticationError { error, error_code } = deser {
        assert_eq!(error, "invalid app id");
        assert_eq!(error_code, ErrorCode::InvalidAppId);
    } else {
        panic!("expected AuthenticationError variant");
    }
}

#[test]
fn server_message_room_joined_round_trip() {
    let payload = RoomJoinedPayload {
        room_id: nil_uuid(),
        room_code: "XYZ789".into(),
        player_id: test_uuid(5),
        game_name: "puzzle".into(),
        max_players: 2,
        supports_authority: false,
        current_players: vec![PlayerInfo {
            id: test_uuid(5),
            name: "Bob".into(),
            is_authority: false,
            is_ready: true,
            connected_at: "2026-01-01T00:00:00Z".into(),
            connection_info: None,
        }],
        is_authority: false,
        lobby_state: LobbyState::Lobby,
        ready_players: vec![test_uuid(5)],
        relay_type: "tcp".into(),
        current_spectators: vec![],
    };
    let msg = ServerMessage::RoomJoined(Box::new(payload));
    let deser = round_trip(&msg);
    if let ServerMessage::RoomJoined(p) = deser {
        assert_eq!(p.room_code, "XYZ789");
        assert_eq!(p.current_players.len(), 1);
        assert_eq!(p.current_players[0].name, "Bob");
        assert!(p.current_players[0].is_ready);
        assert!(matches!(p.lobby_state, LobbyState::Lobby));
    } else {
        panic!("expected RoomJoined variant");
    }
}

#[test]
fn server_message_room_join_failed_round_trip() {
    let msg = ServerMessage::RoomJoinFailed {
        reason: "room full".into(),
        error_code: Some(ErrorCode::RoomFull),
    };
    let deser = round_trip(&msg);
    if let ServerMessage::RoomJoinFailed { reason, error_code } = deser {
        assert_eq!(reason, "room full");
        assert_eq!(error_code, Some(ErrorCode::RoomFull));
    } else {
        panic!("expected RoomJoinFailed variant");
    }
}

#[test]
fn server_message_room_left_round_trip() {
    let msg = ServerMessage::RoomLeft;
    let deser = round_trip(&msg);
    assert!(matches!(deser, ServerMessage::RoomLeft));
}

#[test]
fn server_message_player_joined_round_trip() {
    let msg = ServerMessage::PlayerJoined {
        player: PlayerInfo {
            id: test_uuid(10),
            name: "Charlie".into(),
            is_authority: true,
            is_ready: false,
            connected_at: "2026-02-15T10:30:00Z".into(),
            connection_info: None,
        },
    };
    let deser = round_trip(&msg);
    if let ServerMessage::PlayerJoined { player } = deser {
        assert_eq!(player.name, "Charlie");
        assert!(player.is_authority);
        assert!(!player.is_ready);
    } else {
        panic!("expected PlayerJoined variant");
    }
}

#[test]
fn server_message_player_left_round_trip() {
    let msg = ServerMessage::PlayerLeft {
        player_id: test_uuid(99),
    };
    let deser = round_trip(&msg);
    if let ServerMessage::PlayerLeft { player_id } = deser {
        assert_eq!(player_id, test_uuid(99));
    } else {
        panic!("expected PlayerLeft variant");
    }
}

#[test]
fn server_message_game_data_round_trip() {
    let msg = ServerMessage::GameData {
        from_player: test_uuid(7),
        data: serde_json::json!({"hp": 100}),
    };
    let deser = round_trip(&msg);
    if let ServerMessage::GameData { from_player, data } = deser {
        assert_eq!(from_player, test_uuid(7));
        assert_eq!(data["hp"], 100);
    } else {
        panic!("expected GameData variant");
    }
}

#[test]
fn server_message_game_data_binary_round_trip() {
    let msg = ServerMessage::GameDataBinary {
        from_player: test_uuid(8),
        encoding: GameDataEncoding::MessagePack,
        payload: vec![0xDE, 0xAD, 0xBE, 0xEF],
    };
    let deser = round_trip(&msg);
    if let ServerMessage::GameDataBinary {
        from_player,
        encoding,
        payload,
    } = deser
    {
        assert_eq!(from_player, test_uuid(8));
        assert!(matches!(encoding, GameDataEncoding::MessagePack));
        assert_eq!(payload, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    } else {
        panic!("expected GameDataBinary variant");
    }
}

#[test]
fn server_message_authority_changed_round_trip() {
    let msg = ServerMessage::AuthorityChanged {
        authority_player: Some(test_uuid(11)),
        you_are_authority: true,
    };
    let deser = round_trip(&msg);
    if let ServerMessage::AuthorityChanged {
        authority_player,
        you_are_authority,
    } = deser
    {
        assert_eq!(authority_player, Some(test_uuid(11)));
        assert!(you_are_authority);
    } else {
        panic!("expected AuthorityChanged variant");
    }
}

#[test]
fn server_message_authority_response_round_trip() {
    let msg = ServerMessage::AuthorityResponse {
        granted: false,
        reason: Some("already claimed".into()),
        error_code: Some(ErrorCode::AuthorityConflict),
    };
    let deser = round_trip(&msg);
    if let ServerMessage::AuthorityResponse {
        granted,
        reason,
        error_code,
    } = deser
    {
        assert!(!granted);
        assert_eq!(reason.as_deref(), Some("already claimed"));
        assert_eq!(error_code, Some(ErrorCode::AuthorityConflict));
    } else {
        panic!("expected AuthorityResponse variant");
    }
}

#[test]
fn server_message_lobby_state_changed_round_trip() {
    let p1 = test_uuid(1);
    let p2 = test_uuid(2);
    let msg = ServerMessage::LobbyStateChanged {
        lobby_state: LobbyState::Finalized,
        ready_players: vec![p1, p2],
        all_ready: true,
    };
    let deser = round_trip(&msg);
    if let ServerMessage::LobbyStateChanged {
        lobby_state,
        ready_players,
        all_ready,
    } = deser
    {
        assert!(matches!(lobby_state, LobbyState::Finalized));
        assert_eq!(ready_players.len(), 2);
        assert!(all_ready);
    } else {
        panic!("expected LobbyStateChanged variant");
    }
}

#[test]
fn server_message_game_starting_round_trip() {
    let msg = ServerMessage::GameStarting {
        peer_connections: vec![PeerConnectionInfo {
            player_id: test_uuid(20),
            player_name: "Player1".into(),
            is_authority: true,
            relay_type: "auto".into(),
            connection_info: Some(ConnectionInfo::Direct {
                host: "10.0.0.1".into(),
                port: 8080,
            }),
        }],
    };
    let deser = round_trip(&msg);
    if let ServerMessage::GameStarting { peer_connections } = deser {
        assert_eq!(peer_connections.len(), 1);
        assert_eq!(peer_connections[0].player_name, "Player1");
        assert!(peer_connections[0].is_authority);
    } else {
        panic!("expected GameStarting variant");
    }
}

#[test]
fn server_message_pong_round_trip() {
    let msg = ServerMessage::Pong;
    let deser = round_trip(&msg);
    assert!(matches!(deser, ServerMessage::Pong));
}

#[test]
fn server_message_reconnected_round_trip() {
    let payload = ReconnectedPayload {
        room_id: test_uuid(50),
        room_code: "RECON2".into(),
        player_id: test_uuid(51),
        game_name: "reconnect-game".into(),
        max_players: 8,
        supports_authority: true,
        current_players: vec![],
        is_authority: false,
        lobby_state: LobbyState::Waiting,
        ready_players: vec![],
        relay_type: "udp".into(),
        current_spectators: vec![],
        missed_events: vec![ServerMessage::Pong],
    };
    let msg = ServerMessage::Reconnected(Box::new(payload));
    let deser = round_trip(&msg);
    if let ServerMessage::Reconnected(p) = deser {
        assert_eq!(p.room_code, "RECON2");
        assert_eq!(p.max_players, 8);
        assert_eq!(p.missed_events.len(), 1);
        assert!(matches!(p.missed_events[0], ServerMessage::Pong));
    } else {
        panic!("expected Reconnected variant");
    }
}

#[test]
fn server_message_reconnection_failed_round_trip() {
    let msg = ServerMessage::ReconnectionFailed {
        reason: "session expired".into(),
        error_code: ErrorCode::ReconnectionExpired,
    };
    let deser = round_trip(&msg);
    if let ServerMessage::ReconnectionFailed { reason, error_code } = deser {
        assert_eq!(reason, "session expired");
        assert_eq!(error_code, ErrorCode::ReconnectionExpired);
    } else {
        panic!("expected ReconnectionFailed variant");
    }
}

#[test]
fn server_message_player_reconnected_round_trip() {
    let msg = ServerMessage::PlayerReconnected {
        player_id: test_uuid(60),
    };
    let deser = round_trip(&msg);
    if let ServerMessage::PlayerReconnected { player_id } = deser {
        assert_eq!(player_id, test_uuid(60));
    } else {
        panic!("expected PlayerReconnected variant");
    }
}

#[test]
fn server_message_spectator_joined_round_trip() {
    let payload = SpectatorJoinedPayload {
        room_id: test_uuid(70),
        room_code: "SPEC2".into(),
        spectator_id: test_uuid(71),
        game_name: "spectated-game".into(),
        current_players: vec![],
        current_spectators: vec![SpectatorInfo {
            id: test_uuid(71),
            name: "Spectator1".into(),
            connected_at: "2026-01-01T00:00:00Z".into(),
        }],
        lobby_state: LobbyState::Waiting,
        reason: Some(SpectatorStateChangeReason::Joined),
    };
    let msg = ServerMessage::SpectatorJoined(Box::new(payload));
    let deser = round_trip(&msg);
    if let ServerMessage::SpectatorJoined(p) = deser {
        assert_eq!(p.room_code, "SPEC2");
        assert_eq!(p.current_spectators.len(), 1);
        assert_eq!(p.current_spectators[0].name, "Spectator1");
        assert!(matches!(p.reason, Some(SpectatorStateChangeReason::Joined)));
    } else {
        panic!("expected SpectatorJoined variant");
    }
}

#[test]
fn server_message_spectator_join_failed_round_trip() {
    let msg = ServerMessage::SpectatorJoinFailed {
        reason: "not allowed".into(),
        error_code: Some(ErrorCode::SpectatorNotAllowed),
    };
    let deser = round_trip(&msg);
    if let ServerMessage::SpectatorJoinFailed { reason, error_code } = deser {
        assert_eq!(reason, "not allowed");
        assert_eq!(error_code, Some(ErrorCode::SpectatorNotAllowed));
    } else {
        panic!("expected SpectatorJoinFailed variant");
    }
}

#[test]
fn server_message_spectator_left_round_trip() {
    let msg = ServerMessage::SpectatorLeft {
        room_id: Some(test_uuid(80)),
        room_code: Some("RM1".into()),
        reason: Some(SpectatorStateChangeReason::VoluntaryLeave),
        current_spectators: vec![],
    };
    let deser = round_trip(&msg);
    if let ServerMessage::SpectatorLeft {
        room_id,
        room_code,
        reason,
        current_spectators,
    } = deser
    {
        assert_eq!(room_id, Some(test_uuid(80)));
        assert_eq!(room_code.as_deref(), Some("RM1"));
        assert!(matches!(
            reason,
            Some(SpectatorStateChangeReason::VoluntaryLeave)
        ));
        assert!(current_spectators.is_empty());
    } else {
        panic!("expected SpectatorLeft variant");
    }
}

#[test]
fn server_message_new_spectator_joined_round_trip() {
    let msg = ServerMessage::NewSpectatorJoined {
        spectator: SpectatorInfo {
            id: test_uuid(90),
            name: "NewSpec".into(),
            connected_at: "2026-06-01T12:00:00Z".into(),
        },
        current_spectators: vec![],
        reason: None,
    };
    let deser = round_trip(&msg);
    if let ServerMessage::NewSpectatorJoined {
        spectator,
        current_spectators,
        reason,
    } = deser
    {
        assert_eq!(spectator.name, "NewSpec");
        assert!(current_spectators.is_empty());
        assert!(reason.is_none());
    } else {
        panic!("expected NewSpectatorJoined variant");
    }
}

#[test]
fn server_message_spectator_disconnected_round_trip() {
    let msg = ServerMessage::SpectatorDisconnected {
        spectator_id: test_uuid(95),
        reason: Some(SpectatorStateChangeReason::Disconnected),
        current_spectators: vec![],
    };
    let deser = round_trip(&msg);
    if let ServerMessage::SpectatorDisconnected {
        spectator_id,
        reason,
        current_spectators,
    } = deser
    {
        assert_eq!(spectator_id, test_uuid(95));
        assert!(matches!(
            reason,
            Some(SpectatorStateChangeReason::Disconnected)
        ));
        assert!(current_spectators.is_empty());
    } else {
        panic!("expected SpectatorDisconnected variant");
    }
}

#[test]
fn server_message_error_round_trip() {
    let msg = ServerMessage::Error {
        message: "internal failure".into(),
        error_code: Some(ErrorCode::InternalError),
    };
    let deser = round_trip(&msg);
    if let ServerMessage::Error {
        message,
        error_code,
    } = deser
    {
        assert_eq!(message, "internal failure");
        assert_eq!(error_code, Some(ErrorCode::InternalError));
    } else {
        panic!("expected Error variant");
    }
}

// ════════════════════════════════════════════════════════════════════
// ErrorCode serialization (SCREAMING_SNAKE_CASE)
// ════════════════════════════════════════════════════════════════════

#[test]
fn error_code_serialize_screaming_snake_case() {
    let code = ErrorCode::RoomNotFound;
    let json = serde_json::to_string(&code).expect("serialize");
    assert_eq!(json, "\"ROOM_NOT_FOUND\"");
}

#[test]
fn error_code_deserialize_screaming_snake_case() {
    let code: ErrorCode = serde_json::from_str("\"RATE_LIMIT_EXCEEDED\"").expect("deserialize");
    assert_eq!(code, ErrorCode::RateLimitExceeded);
}

#[test]
fn error_code_round_trip_all_variants() {
    let variants = [
        ErrorCode::Unauthorized,
        ErrorCode::InvalidToken,
        ErrorCode::AuthenticationRequired,
        ErrorCode::InvalidAppId,
        ErrorCode::AppIdExpired,
        ErrorCode::AppIdRevoked,
        ErrorCode::AppIdSuspended,
        ErrorCode::MissingAppId,
        ErrorCode::AuthenticationTimeout,
        ErrorCode::SdkVersionUnsupported,
        ErrorCode::UnsupportedGameDataFormat,
        ErrorCode::InvalidInput,
        ErrorCode::InvalidGameName,
        ErrorCode::InvalidRoomCode,
        ErrorCode::InvalidPlayerName,
        ErrorCode::InvalidMaxPlayers,
        ErrorCode::MessageTooLarge,
        ErrorCode::RoomNotFound,
        ErrorCode::RoomFull,
        ErrorCode::AlreadyInRoom,
        ErrorCode::NotInRoom,
        ErrorCode::RoomCreationFailed,
        ErrorCode::MaxRoomsPerGameExceeded,
        ErrorCode::InvalidRoomState,
        ErrorCode::AuthorityNotSupported,
        ErrorCode::AuthorityConflict,
        ErrorCode::AuthorityDenied,
        ErrorCode::RateLimitExceeded,
        ErrorCode::TooManyConnections,
        ErrorCode::ReconnectionFailed,
        ErrorCode::ReconnectionTokenInvalid,
        ErrorCode::ReconnectionExpired,
        ErrorCode::PlayerAlreadyConnected,
        ErrorCode::SpectatorNotAllowed,
        ErrorCode::TooManySpectators,
        ErrorCode::NotASpectator,
        ErrorCode::SpectatorJoinFailed,
        ErrorCode::InternalError,
        ErrorCode::DatabaseError,
        ErrorCode::ServiceUnavailable,
    ];
    for variant in &variants {
        let json = serde_json::to_string(variant).expect("serialize");
        // Must be a string in SCREAMING_SNAKE_CASE.
        assert!(
            json.starts_with('"') && json.ends_with('"'),
            "expected JSON string for {variant:?}, got {json}"
        );
        let inner = &json[1..json.len() - 1];
        // Every character should be uppercase letter or underscore.
        assert!(
            inner.chars().all(|c| c.is_ascii_uppercase() || c == '_'),
            "expected SCREAMING_SNAKE_CASE for {variant:?}, got {inner}"
        );
        // Round-trip.
        let deser: ErrorCode = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(&deser, variant);
    }
}

// ════════════════════════════════════════════════════════════════════
// Server JSON fixture tests (simulate real server JSON)
// ════════════════════════════════════════════════════════════════════

#[test]
fn fixture_authenticated_from_server() {
    let json = r#"{
        "type": "Authenticated",
        "data": {
            "app_name": "My Game",
            "organization": "Acme Corp",
            "rate_limits": {
                "per_minute": 120,
                "per_hour": 5000,
                "per_day": 50000
            }
        }
    }"#;
    let msg: ServerMessage = serde_json::from_str(json).expect("deserialize");
    if let ServerMessage::Authenticated {
        app_name,
        organization,
        rate_limits,
    } = msg
    {
        assert_eq!(app_name, "My Game");
        assert_eq!(organization.as_deref(), Some("Acme Corp"));
        assert_eq!(rate_limits.per_minute, 120);
    } else {
        panic!("expected Authenticated");
    }
}

#[test]
fn fixture_room_joined_from_server() {
    let room_id = uuid::Uuid::new_v4();
    let player_id = uuid::Uuid::new_v4();
    let json = format!(
        r#"{{
            "type": "RoomJoined",
            "data": {{
                "room_id": "{room_id}",
                "room_code": "ABCD12",
                "player_id": "{player_id}",
                "game_name": "battle-royale",
                "max_players": 8,
                "supports_authority": true,
                "current_players": [],
                "is_authority": true,
                "lobby_state": "waiting",
                "ready_players": [],
                "relay_type": "auto",
                "current_spectators": []
            }}
        }}"#
    );
    let msg: ServerMessage = serde_json::from_str(&json).expect("deserialize");
    if let ServerMessage::RoomJoined(p) = msg {
        assert_eq!(p.room_code, "ABCD12");
        assert_eq!(p.player_id, player_id);
        assert!(p.is_authority);
        assert!(matches!(p.lobby_state, LobbyState::Waiting));
    } else {
        panic!("expected RoomJoined");
    }
}

#[test]
fn fixture_error_from_server() {
    let json = r#"{
        "type": "Error",
        "data": {
            "message": "Rate limit exceeded",
            "error_code": "RATE_LIMIT_EXCEEDED"
        }
    }"#;
    let msg: ServerMessage = serde_json::from_str(json).expect("deserialize");
    if let ServerMessage::Error {
        message,
        error_code,
    } = msg
    {
        assert_eq!(message, "Rate limit exceeded");
        assert_eq!(error_code, Some(ErrorCode::RateLimitExceeded));
    } else {
        panic!("expected Error");
    }
}

#[test]
fn fixture_pong_from_server() {
    let json = r#"{"type": "Pong"}"#;
    let msg: ServerMessage = serde_json::from_str(json).expect("deserialize");
    assert!(matches!(msg, ServerMessage::Pong));
}

#[test]
fn fixture_room_left_from_server() {
    let json = r#"{"type": "RoomLeft"}"#;
    let msg: ServerMessage = serde_json::from_str(json).expect("deserialize");
    assert!(matches!(msg, ServerMessage::RoomLeft));
}

#[test]
fn fixture_authentication_error_from_server() {
    let json = r#"{
        "type": "AuthenticationError",
        "data": {
            "error": "Invalid application credentials",
            "error_code": "INVALID_APP_ID"
        }
    }"#;
    let msg: ServerMessage = serde_json::from_str(json).expect("deserialize");
    if let ServerMessage::AuthenticationError { error, error_code } = msg {
        assert_eq!(error, "Invalid application credentials");
        assert_eq!(error_code, ErrorCode::InvalidAppId);
    } else {
        panic!("expected AuthenticationError");
    }
}

#[test]
fn fixture_game_data_from_server() {
    let pid = uuid::Uuid::new_v4();
    let json = format!(
        r#"{{
            "type": "GameData",
            "data": {{
                "from_player": "{pid}",
                "data": {{"action": "fire", "target": [1, 2, 3]}}
            }}
        }}"#
    );
    let msg: ServerMessage = serde_json::from_str(&json).expect("deserialize");
    if let ServerMessage::GameData { from_player, data } = msg {
        assert_eq!(from_player, pid);
        assert_eq!(data["action"], "fire");
    } else {
        panic!("expected GameData");
    }
}

#[test]
fn fixture_player_joined_with_connection_info() {
    let pid = uuid::Uuid::new_v4();
    let json = format!(
        r#"{{
            "type": "PlayerJoined",
            "data": {{
                "player": {{
                    "id": "{pid}",
                    "name": "Bob",
                    "is_authority": false,
                    "is_ready": true,
                    "connected_at": "2026-02-20T12:00:00Z",
                    "connection_info": {{
                        "type": "direct",
                        "host": "192.168.1.10",
                        "port": 9999
                    }}
                }}
            }}
        }}"#
    );
    let msg: ServerMessage = serde_json::from_str(&json).expect("deserialize");
    if let ServerMessage::PlayerJoined { player } = msg {
        assert_eq!(player.name, "Bob");
        assert!(player.is_ready);
        if let Some(ConnectionInfo::Direct { host, port }) = player.connection_info {
            assert_eq!(host, "192.168.1.10");
            assert_eq!(port, 9999);
        } else {
            panic!("expected Direct connection_info");
        }
    } else {
        panic!("expected PlayerJoined");
    }
}

#[test]
fn fixture_reconnected_from_server() {
    let room_id = uuid::Uuid::new_v4();
    let player_id = uuid::Uuid::new_v4();
    let missed_player_id = uuid::Uuid::new_v4();
    let json = format!(
        r#"{{
            "type": "Reconnected",
            "data": {{
                "room_id": "{room_id}",
                "room_code": "RECON1",
                "player_id": "{player_id}",
                "game_name": "recon-game",
                "max_players": 6,
                "supports_authority": false,
                "current_players": [],
                "is_authority": true,
                "lobby_state": "lobby",
                "ready_players": ["{player_id}"],
                "relay_type": "tcp",
                "current_spectators": [],
                "missed_events": [
                    {{
                        "type": "PlayerLeft",
                        "data": {{
                            "player_id": "{missed_player_id}"
                        }}
                    }}
                ]
            }}
        }}"#
    );
    let msg: ServerMessage = serde_json::from_str(&json).expect("deserialize");
    if let ServerMessage::Reconnected(p) = msg {
        assert_eq!(p.room_code, "RECON1");
        assert_eq!(p.player_id, player_id);
        assert!(p.is_authority);
        assert!(matches!(p.lobby_state, LobbyState::Lobby));
        assert_eq!(p.ready_players.len(), 1);
        assert_eq!(p.missed_events.len(), 1);
        assert!(matches!(
            &p.missed_events[0],
            ServerMessage::PlayerLeft { player_id: pid } if *pid == missed_player_id
        ));
    } else {
        panic!("expected Reconnected");
    }
}

#[test]
fn fixture_lobby_state_changed_from_server() {
    let pid1 = uuid::Uuid::new_v4();
    let pid2 = uuid::Uuid::new_v4();
    let json = format!(
        r#"{{
            "type": "LobbyStateChanged",
            "data": {{
                "lobby_state": "finalized",
                "ready_players": ["{pid1}", "{pid2}"],
                "all_ready": true
            }}
        }}"#
    );
    let msg: ServerMessage = serde_json::from_str(&json).expect("deserialize");
    if let ServerMessage::LobbyStateChanged {
        lobby_state,
        ready_players,
        all_ready,
    } = msg
    {
        assert!(matches!(lobby_state, LobbyState::Finalized));
        assert_eq!(ready_players.len(), 2);
        assert!(all_ready);
    } else {
        panic!("expected LobbyStateChanged");
    }
}

#[test]
fn fixture_game_starting_from_server() {
    let pid = uuid::Uuid::new_v4();
    let json = format!(
        r#"{{
            "type": "GameStarting",
            "data": {{
                "peer_connections": [
                    {{
                        "player_id": "{pid}",
                        "player_name": "Alice",
                        "is_authority": true,
                        "relay_type": "udp",
                        "connection_info": {{
                            "type": "direct",
                            "host": "192.168.1.1",
                            "port": 7777
                        }}
                    }}
                ]
            }}
        }}"#
    );
    let msg: ServerMessage = serde_json::from_str(&json).expect("deserialize");
    if let ServerMessage::GameStarting { peer_connections } = msg {
        assert_eq!(peer_connections.len(), 1);
        let pc = &peer_connections[0];
        assert_eq!(pc.player_id, pid);
        assert_eq!(pc.player_name, "Alice");
        assert!(pc.is_authority);
        assert_eq!(pc.relay_type, "udp");
        if let Some(ConnectionInfo::Direct { host, port }) = &pc.connection_info {
            assert_eq!(host, "192.168.1.1");
            assert_eq!(*port, 7777);
        } else {
            panic!("expected Direct connection_info in peer_connections");
        }
    } else {
        panic!("expected GameStarting");
    }
}

#[test]
fn fixture_spectator_joined_from_server() {
    let room_id = uuid::Uuid::new_v4();
    let spectator_id = uuid::Uuid::new_v4();
    let json = format!(
        r#"{{
            "type": "SpectatorJoined",
            "data": {{
                "room_id": "{room_id}",
                "room_code": "SPEC42",
                "spectator_id": "{spectator_id}",
                "game_name": "spectator-game",
                "current_players": [],
                "current_spectators": [
                    {{
                        "id": "{spectator_id}",
                        "name": "Watcher1",
                        "connected_at": "2026-02-20T10:00:00Z"
                    }}
                ],
                "lobby_state": "waiting",
                "reason": "joined"
            }}
        }}"#
    );
    let msg: ServerMessage = serde_json::from_str(&json).expect("deserialize");
    if let ServerMessage::SpectatorJoined(p) = msg {
        assert_eq!(p.room_code, "SPEC42");
        assert_eq!(p.spectator_id, spectator_id);
        assert_eq!(p.game_name, "spectator-game");
        assert_eq!(p.current_spectators.len(), 1);
        assert_eq!(p.current_spectators[0].name, "Watcher1");
        assert!(matches!(p.lobby_state, LobbyState::Waiting));
        assert!(matches!(p.reason, Some(SpectatorStateChangeReason::Joined)));
    } else {
        panic!("expected SpectatorJoined");
    }
}

#[test]
fn fixture_reconnection_failed_from_server() {
    let json = r#"{
        "type": "ReconnectionFailed",
        "data": {
            "reason": "Session expired",
            "error_code": "RECONNECTION_EXPIRED"
        }
    }"#;
    let msg: ServerMessage = serde_json::from_str(json).expect("deserialize");
    if let ServerMessage::ReconnectionFailed { reason, error_code } = msg {
        assert_eq!(reason, "Session expired");
        assert_eq!(error_code, ErrorCode::ReconnectionExpired);
    } else {
        panic!("expected ReconnectionFailed");
    }
}

#[test]
fn fixture_authority_response_from_server() {
    let json = r#"{
        "type": "AuthorityResponse",
        "data": {
            "granted": false,
            "reason": "Another player is authority",
            "error_code": "AUTHORITY_DENIED"
        }
    }"#;
    let msg: ServerMessage = serde_json::from_str(json).expect("deserialize");
    if let ServerMessage::AuthorityResponse {
        granted,
        reason,
        error_code,
    } = msg
    {
        assert!(!granted);
        assert_eq!(reason.as_deref(), Some("Another player is authority"));
        assert_eq!(error_code, Some(ErrorCode::AuthorityDenied));
    } else {
        panic!("expected AuthorityResponse");
    }
}

// ════════════════════════════════════════════════════════════════════
// ConnectionInfo tag format verification
// ════════════════════════════════════════════════════════════════════

#[test]
fn connection_info_direct_tag() {
    let info = ConnectionInfo::Direct {
        host: "1.2.3.4".into(),
        port: 1234,
    };
    let json = serde_json::to_string(&info).expect("serialize");
    let val: serde_json::Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(val["type"], "direct");
}

#[test]
fn connection_info_unity_relay_tag() {
    let info = ConnectionInfo::UnityRelay {
        allocation_id: "alloc1".into(),
        connection_data: "conndata".into(),
        key: "key1".into(),
    };
    let json = serde_json::to_string(&info).expect("serialize");
    let val: serde_json::Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(val["type"], "unity_relay");
}

#[test]
fn connection_info_relay_tag() {
    let info = ConnectionInfo::Relay {
        host: "relay.example.com".into(),
        port: 3000,
        transport: RelayTransport::Tcp,
        allocation_id: "room-1".into(),
        token: "tok1".into(),
        client_id: Some(42),
    };
    let json = serde_json::to_string(&info).expect("serialize");
    let val: serde_json::Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(val["type"], "relay");
    assert_eq!(val["client_id"], 42);
}

#[test]
fn connection_info_webrtc_tag() {
    let info = ConnectionInfo::WebRTC {
        sdp: Some("v=0\r\n".into()),
        ice_candidates: vec!["candidate:1".into()],
    };
    let json = serde_json::to_string(&info).expect("serialize");
    let val: serde_json::Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(val["type"], "webrtc");
}

#[test]
fn connection_info_custom_tag() {
    let info = ConnectionInfo::Custom {
        data: serde_json::json!({"custom_key": "custom_value"}),
    };
    let json = serde_json::to_string(&info).expect("serialize");
    let val: serde_json::Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(val["type"], "custom");
    assert_eq!(val["data"]["custom_key"], "custom_value");
}

#[test]
fn connection_info_relay_round_trip_with_defaults() {
    let info = ConnectionInfo::Relay {
        host: "relay.example.com".into(),
        port: 3000,
        transport: RelayTransport::Auto,
        allocation_id: "alloc-2".into(),
        token: "tok2".into(),
        client_id: None,
    };
    let deser = round_trip(&info);
    if let ConnectionInfo::Relay {
        host,
        port,
        transport,
        allocation_id,
        token,
        client_id,
    } = deser
    {
        assert_eq!(host, "relay.example.com");
        assert_eq!(port, 3000);
        assert!(matches!(transport, RelayTransport::Auto));
        assert_eq!(allocation_id, "alloc-2");
        assert_eq!(token, "tok2");
        assert!(client_id.is_none());
    } else {
        panic!("expected Relay variant");
    }
}

// ════════════════════════════════════════════════════════════════════
// GameDataBinary serde_bytes verification
// ════════════════════════════════════════════════════════════════════

#[test]
fn game_data_binary_payload_serde_bytes_round_trip() {
    let original_payload: Vec<u8> = (0u8..=255).collect();
    let msg = ServerMessage::GameDataBinary {
        from_player: test_uuid(42),
        encoding: GameDataEncoding::Rkyv,
        payload: original_payload.clone(),
    };
    let json = serde_json::to_string(&msg).expect("serialize");
    let deser: ServerMessage = serde_json::from_str(&json).expect("deserialize");
    if let ServerMessage::GameDataBinary {
        payload, encoding, ..
    } = deser
    {
        assert_eq!(payload, original_payload);
        assert!(matches!(encoding, GameDataEncoding::Rkyv));
    } else {
        panic!("expected GameDataBinary variant");
    }
}

#[test]
fn game_data_binary_empty_payload() {
    let msg = ServerMessage::GameDataBinary {
        from_player: nil_uuid(),
        encoding: GameDataEncoding::Json,
        payload: vec![],
    };
    let deser = round_trip(&msg);
    if let ServerMessage::GameDataBinary { payload, .. } = deser {
        assert!(payload.is_empty());
    } else {
        panic!("expected GameDataBinary variant");
    }
}

// ════════════════════════════════════════════════════════════════════
// Enum serde tests: LobbyState, RelayTransport, GameDataEncoding
// ════════════════════════════════════════════════════════════════════

#[test]
fn lobby_state_serde_waiting() {
    let val = LobbyState::Waiting;
    let json = serde_json::to_string(&val).expect("serialize");
    assert_eq!(json, "\"waiting\"");
    let deser: LobbyState = serde_json::from_str(&json).expect("deserialize");
    assert!(matches!(deser, LobbyState::Waiting));
}

#[test]
fn lobby_state_serde_lobby() {
    let val = LobbyState::Lobby;
    let json = serde_json::to_string(&val).expect("serialize");
    assert_eq!(json, "\"lobby\"");
    let deser: LobbyState = serde_json::from_str(&json).expect("deserialize");
    assert!(matches!(deser, LobbyState::Lobby));
}

#[test]
fn lobby_state_serde_finalized() {
    let val = LobbyState::Finalized;
    let json = serde_json::to_string(&val).expect("serialize");
    assert_eq!(json, "\"finalized\"");
    let deser: LobbyState = serde_json::from_str(&json).expect("deserialize");
    assert!(matches!(deser, LobbyState::Finalized));
}

#[test]
fn relay_transport_serde_tcp() {
    let val = RelayTransport::Tcp;
    let json = serde_json::to_string(&val).expect("serialize");
    assert_eq!(json, "\"tcp\"");
    let deser: RelayTransport = serde_json::from_str(&json).expect("deserialize");
    assert!(matches!(deser, RelayTransport::Tcp));
}

#[test]
fn relay_transport_serde_udp() {
    let val = RelayTransport::Udp;
    let json = serde_json::to_string(&val).expect("serialize");
    assert_eq!(json, "\"udp\"");
    let deser: RelayTransport = serde_json::from_str(&json).expect("deserialize");
    assert!(matches!(deser, RelayTransport::Udp));
}

#[test]
fn relay_transport_serde_websocket() {
    let val = RelayTransport::Websocket;
    let json = serde_json::to_string(&val).expect("serialize");
    assert_eq!(json, "\"websocket\"");
    let deser: RelayTransport = serde_json::from_str(&json).expect("deserialize");
    assert!(matches!(deser, RelayTransport::Websocket));
}

#[test]
fn relay_transport_serde_auto() {
    let val = RelayTransport::Auto;
    let json = serde_json::to_string(&val).expect("serialize");
    assert_eq!(json, "\"auto\"");
    let deser: RelayTransport = serde_json::from_str(&json).expect("deserialize");
    assert!(matches!(deser, RelayTransport::Auto));
}

#[test]
fn game_data_encoding_serde_json() {
    let val = GameDataEncoding::Json;
    let json = serde_json::to_string(&val).expect("serialize");
    assert_eq!(json, "\"json\"");
    let deser: GameDataEncoding = serde_json::from_str(&json).expect("deserialize");
    assert!(matches!(deser, GameDataEncoding::Json));
}

#[test]
fn game_data_encoding_serde_message_pack() {
    let val = GameDataEncoding::MessagePack;
    let json = serde_json::to_string(&val).expect("serialize");
    assert_eq!(json, "\"message_pack\"");
    let deser: GameDataEncoding = serde_json::from_str(&json).expect("deserialize");
    assert!(matches!(deser, GameDataEncoding::MessagePack));
}

#[test]
fn game_data_encoding_serde_rkyv() {
    let val = GameDataEncoding::Rkyv;
    let json = serde_json::to_string(&val).expect("serialize");
    assert_eq!(json, "\"rkyv\"");
    let deser: GameDataEncoding = serde_json::from_str(&json).expect("deserialize");
    assert!(matches!(deser, GameDataEncoding::Rkyv));
}

// ════════════════════════════════════════════════════════════════════
// SpectatorStateChangeReason serde tests
// ════════════════════════════════════════════════════════════════════

#[test]
fn spectator_state_change_reason_serde_all() {
    let variants = [
        (SpectatorStateChangeReason::Joined, "\"joined\""),
        (
            SpectatorStateChangeReason::VoluntaryLeave,
            "\"voluntary_leave\"",
        ),
        (SpectatorStateChangeReason::Disconnected, "\"disconnected\""),
        (SpectatorStateChangeReason::Removed, "\"removed\""),
        (SpectatorStateChangeReason::RoomClosed, "\"room_closed\""),
    ];
    for (variant, expected_json) in &variants {
        let json = serde_json::to_string(variant).expect("serialize");
        assert_eq!(&json, expected_json, "for variant {variant:?}");
        let deser: SpectatorStateChangeReason = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(&deser, variant);
    }
}

// ════════════════════════════════════════════════════════════════════
// Struct serde tests: PlayerInfo, SpectatorInfo, PeerConnectionInfo
// ════════════════════════════════════════════════════════════════════

#[test]
fn player_info_round_trip() {
    let info = PlayerInfo {
        id: test_uuid(100),
        name: "TestPlayer".into(),
        is_authority: true,
        is_ready: false,
        connected_at: "2026-02-20T08:00:00Z".into(),
        connection_info: Some(ConnectionInfo::Direct {
            host: "10.0.0.5".into(),
            port: 5555,
        }),
    };
    let deser = round_trip(&info);
    assert_eq!(deser.name, "TestPlayer");
    assert!(deser.is_authority);
    assert!(!deser.is_ready);
    assert!(deser.connection_info.is_some());
}

#[test]
fn player_info_no_connection_info_skipped() {
    let info = PlayerInfo {
        id: nil_uuid(),
        name: "NoConn".into(),
        is_authority: false,
        is_ready: true,
        connected_at: "2026-01-01T00:00:00Z".into(),
        connection_info: None,
    };
    let json = serde_json::to_string(&info).expect("serialize");
    // The "connection_info" field should be absent (skip_serializing_if).
    assert!(
        !json.contains("connection_info"),
        "expected connection_info to be skipped, got {json}"
    );
    let deser: PlayerInfo = serde_json::from_str(&json).expect("deserialize");
    assert!(deser.connection_info.is_none());
}

#[test]
fn spectator_info_round_trip() {
    let info = SpectatorInfo {
        id: test_uuid(200),
        name: "Watcher".into(),
        connected_at: "2026-03-01T15:00:00Z".into(),
    };
    let deser = round_trip(&info);
    assert_eq!(deser.id, test_uuid(200));
    assert_eq!(deser.name, "Watcher");
    assert_eq!(deser.connected_at, "2026-03-01T15:00:00Z");
}

#[test]
fn peer_connection_info_round_trip() {
    let info = PeerConnectionInfo {
        player_id: test_uuid(300),
        player_name: "Peer1".into(),
        is_authority: false,
        relay_type: "udp".into(),
        connection_info: Some(ConnectionInfo::WebRTC {
            sdp: None,
            ice_candidates: vec![],
        }),
    };
    let deser = round_trip(&info);
    assert_eq!(deser.player_name, "Peer1");
    assert_eq!(deser.relay_type, "udp");
    if let Some(ConnectionInfo::WebRTC {
        sdp,
        ice_candidates,
    }) = deser.connection_info
    {
        assert!(sdp.is_none());
        assert!(ice_candidates.is_empty());
    } else {
        panic!("expected WebRTC connection info");
    }
}

#[test]
fn peer_connection_info_no_connection_info() {
    let info = PeerConnectionInfo {
        player_id: nil_uuid(),
        player_name: "Peer2".into(),
        is_authority: true,
        relay_type: "tcp".into(),
        connection_info: None,
    };
    let json = serde_json::to_string(&info).expect("serialize");
    assert!(
        !json.contains("connection_info"),
        "expected connection_info to be skipped"
    );
    let deser: PeerConnectionInfo = serde_json::from_str(&json).expect("deserialize");
    assert!(deser.connection_info.is_none());
}

#[test]
fn rate_limit_info_round_trip() {
    let info = RateLimitInfo {
        per_minute: 100,
        per_hour: 2000,
        per_day: 20000,
    };
    let deser = round_trip(&info);
    assert_eq!(deser.per_minute, 100);
    assert_eq!(deser.per_hour, 2000);
    assert_eq!(deser.per_day, 20000);
}

// ════════════════════════════════════════════════════════════════════
// Tag format verification for ClientMessage and ServerMessage
// ════════════════════════════════════════════════════════════════════

#[test]
fn client_message_uses_type_and_content_tags() {
    let msg = ClientMessage::Authenticate {
        app_id: "test".into(),
        sdk_version: None,
        platform: None,
        game_data_format: None,
    };
    let json = serde_json::to_string(&msg).expect("serialize");
    let val: serde_json::Value = serde_json::from_str(&json).expect("parse");
    // Must have a "type" field.
    assert!(
        val.get("type").is_some(),
        "ClientMessage should have 'type' tag"
    );
    assert_eq!(val["type"], "Authenticate");
    // Must have a "data" field for variants with content.
    assert!(
        val.get("data").is_some(),
        "Authenticate should have 'data' content"
    );
}

#[test]
fn client_message_unit_variant_has_no_data() {
    let msg = ClientMessage::Ping;
    let json = serde_json::to_string(&msg).expect("serialize");
    let val: serde_json::Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(val["type"], "Ping");
    // Unit variants don't have a "data" field.
    assert!(
        val.get("data").is_none(),
        "Ping should have no 'data' field"
    );
    let obj = val.as_object().expect("object");
    assert_eq!(
        obj.len(),
        1,
        "Ping should serialize with only the 'type' field"
    );
}

#[test]
fn client_message_leave_room_unit_variant_has_no_data() {
    let msg = ClientMessage::LeaveRoom;
    let json = serde_json::to_string(&msg).expect("serialize");
    let val: serde_json::Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(val["type"], "LeaveRoom");
    assert!(val.get("data").is_none());
    let obj = val.as_object().expect("object");
    assert_eq!(
        obj.len(),
        1,
        "LeaveRoom should serialize with only the 'type' field"
    );
}

#[test]
fn client_message_leave_spectator_unit_variant_has_no_data() {
    let msg = ClientMessage::LeaveSpectator;
    let json = serde_json::to_string(&msg).expect("serialize");
    let val: serde_json::Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(val["type"], "LeaveSpectator");
    assert!(val.get("data").is_none());
    let obj = val.as_object().expect("object");
    assert_eq!(
        obj.len(),
        1,
        "LeaveSpectator should serialize with only the 'type' field"
    );
}

#[test]
fn server_message_uses_type_and_content_tags() {
    let msg = ServerMessage::Authenticated {
        app_name: "app".into(),
        organization: None,
        rate_limits: RateLimitInfo {
            per_minute: 1,
            per_hour: 1,
            per_day: 1,
        },
    };
    let json = serde_json::to_string(&msg).expect("serialize");
    let val: serde_json::Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(val["type"], "Authenticated");
    assert!(val.get("data").is_some());
}

#[test]
fn server_message_unit_variant_has_no_data() {
    let msg = ServerMessage::Pong;
    let json = serde_json::to_string(&msg).expect("serialize");
    let val: serde_json::Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(val["type"], "Pong");
    assert!(val.get("data").is_none());
    let obj = val.as_object().expect("object");
    assert_eq!(
        obj.len(),
        1,
        "Pong should serialize with only the 'type' field"
    );
}

#[test]
fn server_message_room_left_unit_variant_has_no_data() {
    let msg = ServerMessage::RoomLeft;
    let json = serde_json::to_string(&msg).expect("serialize");
    let val: serde_json::Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(val["type"], "RoomLeft");
    assert!(val.get("data").is_none());
    let obj = val.as_object().expect("object");
    assert_eq!(
        obj.len(),
        1,
        "RoomLeft should serialize with only the 'type' field"
    );
}

// ════════════════════════════════════════════════════════════════════
// Default trait implementations
// ════════════════════════════════════════════════════════════════════

#[test]
fn relay_transport_default_is_auto() {
    assert!(matches!(RelayTransport::default(), RelayTransport::Auto));
}

#[test]
fn game_data_encoding_default_is_json() {
    assert!(matches!(
        GameDataEncoding::default(),
        GameDataEncoding::Json
    ));
}

#[test]
fn lobby_state_default_is_waiting() {
    assert!(matches!(LobbyState::default(), LobbyState::Waiting));
}

#[test]
fn spectator_state_change_reason_default_is_joined() {
    assert!(matches!(
        SpectatorStateChangeReason::default(),
        SpectatorStateChangeReason::Joined
    ));
}

// ════════════════════════════════════════════════════════════════════
// ProtocolInfoPayload and PlayerNameRulesPayload
// ════════════════════════════════════════════════════════════════════

#[test]
fn protocol_info_payload_round_trip_minimal() {
    let payload = ProtocolInfoPayload {
        platform: None,
        sdk_version: None,
        minimum_version: None,
        recommended_version: None,
        capabilities: vec![],
        notes: None,
        game_data_formats: vec![],
        player_name_rules: None,
    };
    let deser = round_trip(&payload);
    assert!(deser.platform.is_none());
    assert!(deser.capabilities.is_empty());
    assert!(deser.player_name_rules.is_none());
}

#[test]
fn player_name_rules_payload_round_trip() {
    let rules = PlayerNameRulesPayload {
        max_length: 20,
        min_length: 3,
        allow_unicode_alphanumeric: false,
        allow_spaces: false,
        allow_leading_trailing_whitespace: false,
        allowed_symbols: vec!['_'],
        additional_allowed_characters: Some("àé".into()),
    };
    let deser = round_trip(&rules);
    assert_eq!(deser.max_length, 20);
    assert_eq!(deser.min_length, 3);
    assert!(!deser.allow_unicode_alphanumeric);
    assert_eq!(deser.allowed_symbols, vec!['_']);
    assert_eq!(deser.additional_allowed_characters.as_deref(), Some("àé"));
}

// ════════════════════════════════════════════════════════════════════
// ErrorCode::description smoke test
// ════════════════════════════════════════════════════════════════════

#[test]
fn error_code_description_not_empty() {
    let code = ErrorCode::Unauthorized;
    let desc = code.description();
    assert!(!desc.is_empty());
}

#[test]
fn error_code_display_returns_description() {
    let code = ErrorCode::RoomNotFound;
    let display = format!("{code}");
    assert_eq!(display, code.description());
}
