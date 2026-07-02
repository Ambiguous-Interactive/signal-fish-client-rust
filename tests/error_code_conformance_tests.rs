#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing
)]
//! Cross-repo error-code conformance tests.
//!
//! The server's AsyncAPI spec (vendored verbatim at
//! `tests/server-spec/signal-fish-protocol.asyncapi.yaml`, provenance pinned
//! in `PROVENANCE.toml`) declares the full set of wire error-code tokens the
//! server may send. The client's [`ErrorCode`] enum is deliberately
//! exhaustive, so an unknown token fails deserialization of the *entire*
//! enclosing `ServerMessage`. These tests keep the two value spaces in
//! lockstep in both directions, closing the drift blind spot where a
//! server-side code addition passes the wire-sample golden tests (which pin
//! message *shapes*, not the error-code value space).
//!
//! A weekly CI job (`.github/workflows/protocol-sync.yml`) diffs the vendored
//! spec against the server's `main`, so upstream additions surface here as a
//! red `every_server_error_code_token_deserializes_into_a_client_variant`.

use signal_fish_client::error_codes::ErrorCode;

const SPEC: &str = include_str!("server-spec/signal-fish-protocol.asyncapi.yaml");

/// Client-only codes intentionally absent from the server spec.
///
/// Kept empty on purpose: every client variant is expected to exist because
/// the server can send it. Add a token here only with a comment explaining
/// why the client models a code the server does not declare.
const CLIENT_ONLY_ALLOWLIST: &[&str] = &[];

/// Extracts the wire tokens of the spec's `ErrorCode` enum with a plain line
/// scan (no YAML dependency).
///
/// Anchors on the `ErrorCode:` schema key, then its `enum:` list, and
/// collects `- SCREAMING_SNAKE` items until the first line that is not one
/// (dedent or blank line ends the block). Restructuring the spec breaks the
/// scan loudly via `spec_error_code_extraction_finds_a_plausible_count`.
fn extract_spec_error_tokens() -> Vec<String> {
    let mut lines = SPEC.lines();

    // Locate the `ErrorCode:` schema key (indented mapping key, exact name).
    for line in lines.by_ref() {
        if line.trim() == "ErrorCode:" {
            break;
        }
    }

    // Locate the `enum:` key within the schema body.
    for line in lines.by_ref() {
        if line.trim() == "enum:" {
            break;
        }
    }

    // Collect `- TOKEN` items until the block ends.
    let mut tokens = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        let Some(item) = trimmed.strip_prefix("- ") else {
            break;
        };
        let is_screaming_snake = !item.is_empty()
            && item.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && item
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        if !is_screaming_snake {
            break;
        }
        tokens.push(item.to_string());
    }
    tokens
}

/// Every client `ErrorCode` variant, exactly once.
///
/// Kept honest by [`exhaustiveness_guard`]: adding an enum variant fails to
/// compile there, forcing an edit to this file — extend BOTH the guard match
/// and this list.
fn all_client_error_codes() -> Vec<ErrorCode> {
    vec![
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
        ErrorCode::StorageError,
        ErrorCode::ServiceUnavailable,
        ErrorCode::GameStartNotReady,
        ErrorCode::GameStartForbidden,
        ErrorCode::CrossRoomSignal,
        ErrorCode::UnsupportedTransport,
        ErrorCode::SignalTargetNotFound,
        ErrorCode::SignalRateLimited,
        ErrorCode::SignalTooLarge,
        ErrorCode::ConnectionIdleTimeout,
        ErrorCode::SlowConsumer,
        ErrorCode::ActivityTimeout,
    ]
}

/// Compile-time exhaustiveness guard: one arm per variant, no wildcard.
///
/// A new `ErrorCode` variant fails compilation here until both this match and
/// [`all_client_error_codes`] are extended.
fn exhaustiveness_guard(code: &ErrorCode) {
    match code {
        ErrorCode::Unauthorized
        | ErrorCode::InvalidToken
        | ErrorCode::AuthenticationRequired
        | ErrorCode::InvalidAppId
        | ErrorCode::AppIdExpired
        | ErrorCode::AppIdRevoked
        | ErrorCode::AppIdSuspended
        | ErrorCode::MissingAppId
        | ErrorCode::AuthenticationTimeout
        | ErrorCode::SdkVersionUnsupported
        | ErrorCode::UnsupportedGameDataFormat
        | ErrorCode::InvalidInput
        | ErrorCode::InvalidGameName
        | ErrorCode::InvalidRoomCode
        | ErrorCode::InvalidPlayerName
        | ErrorCode::InvalidMaxPlayers
        | ErrorCode::MessageTooLarge
        | ErrorCode::RoomNotFound
        | ErrorCode::RoomFull
        | ErrorCode::AlreadyInRoom
        | ErrorCode::NotInRoom
        | ErrorCode::RoomCreationFailed
        | ErrorCode::MaxRoomsPerGameExceeded
        | ErrorCode::InvalidRoomState
        | ErrorCode::AuthorityNotSupported
        | ErrorCode::AuthorityConflict
        | ErrorCode::AuthorityDenied
        | ErrorCode::RateLimitExceeded
        | ErrorCode::TooManyConnections
        | ErrorCode::ReconnectionFailed
        | ErrorCode::ReconnectionTokenInvalid
        | ErrorCode::ReconnectionExpired
        | ErrorCode::PlayerAlreadyConnected
        | ErrorCode::SpectatorNotAllowed
        | ErrorCode::TooManySpectators
        | ErrorCode::NotASpectator
        | ErrorCode::SpectatorJoinFailed
        | ErrorCode::InternalError
        | ErrorCode::StorageError
        | ErrorCode::ServiceUnavailable
        | ErrorCode::GameStartNotReady
        | ErrorCode::GameStartForbidden
        | ErrorCode::CrossRoomSignal
        | ErrorCode::UnsupportedTransport
        | ErrorCode::SignalTargetNotFound
        | ErrorCode::SignalRateLimited
        | ErrorCode::SignalTooLarge
        | ErrorCode::ConnectionIdleTimeout
        | ErrorCode::SlowConsumer
        | ErrorCode::ActivityTimeout => {}
    }
}

fn wire_token(code: &ErrorCode) -> String {
    let json = serde_json::to_string(code).expect("ErrorCode must serialize");
    json.trim_matches('"').to_string()
}

#[test]
fn every_server_error_code_token_deserializes_into_a_client_variant() {
    let tokens = extract_spec_error_tokens();
    let unknown: Vec<&String> = tokens
        .iter()
        .filter(|token| serde_json::from_str::<ErrorCode>(&format!("\"{token}\"")).is_err())
        .collect();
    assert!(
        unknown.is_empty(),
        "server spec declares error-code tokens the client ErrorCode enum cannot \
         deserialize (an Error frame carrying one is dropped undecoded): {unknown:?}. \
         Add the missing variants to src/error_codes.rs."
    );
}

#[test]
fn every_client_error_code_appears_in_server_spec() {
    let tokens = extract_spec_error_tokens();
    let stray: Vec<String> = all_client_error_codes()
        .iter()
        .map(wire_token)
        .filter(|token| {
            !tokens.iter().any(|t| t == token) && !CLIENT_ONLY_ALLOWLIST.contains(&token.as_str())
        })
        .collect();
    assert!(
        stray.is_empty(),
        "client ErrorCode variants missing from the server spec (either remove them, \
         update the vendored spec, or add to CLIENT_ONLY_ALLOWLIST with justification): \
         {stray:?}"
    );
}

#[test]
fn client_and_spec_error_code_counts_match() {
    let tokens = extract_spec_error_tokens();
    let codes = all_client_error_codes();
    for code in &codes {
        exhaustiveness_guard(code);
    }
    assert_eq!(
        codes.len(),
        tokens.len() + CLIENT_ONLY_ALLOWLIST.len(),
        "client ErrorCode variant count must equal the spec token count plus the \
         allowlist (spec: {}, client: {}, allowlist: {})",
        tokens.len(),
        codes.len(),
        CLIENT_ONLY_ALLOWLIST.len()
    );
}

#[test]
fn spec_error_code_extraction_finds_a_plausible_count() {
    let tokens = extract_spec_error_tokens();
    assert!(
        tokens.len() >= 50,
        "expected at least 50 error-code tokens in the vendored spec, found {} — \
         the line-scan extractor likely no longer matches the spec's structure",
        tokens.len()
    );
    let mut deduped = tokens.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(
        deduped.len(),
        tokens.len(),
        "spec error-code enum contains duplicate tokens"
    );
}
