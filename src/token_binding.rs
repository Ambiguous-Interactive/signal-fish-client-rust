//! Signal Fish token-binding-v2 negotiation and challenge types.
//!
//! The built-in native WebSocket transport implements the extension when the
//! `token-binding` feature is enabled. The default remains disabled.

use serde::{Deserialize, Serialize};

#[cfg(feature = "token-binding")]
use crate::error::{SignalFishError, TokenBindingFailure};
#[cfg(feature = "token-binding")]
use crate::transport::TransportFrame;

/// Exact RFC 6455 subprotocol token for the pinned v2 extension.
pub const TOKEN_BINDING_SUBPROTOCOL: &str = "signalfish.tokenbinding.v2";

/// Whether a native WebSocket connection offers and requires token binding.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum TokenBindingMode {
    /// Do not offer token binding. This is the byte-compatible default.
    #[default]
    Disabled,
    /// Offer v2 and protect frames only when the server selects it.
    Optional,
    /// Offer v2 and reject the connection unless the server selects it.
    Required,
}

/// Observable token-binding state of an established native WebSocket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenBindingStatus {
    /// The connection did not offer token binding.
    Disabled,
    /// Optional mode was offered and the server permitted an unsigned fallback.
    NotNegotiated,
    /// The server selected v2 and outbound frames are protected.
    Active,
}

/// Token-binding proof scheme defined by Signal Fish Server 0.7.0.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TokenBindingScheme {
    /// HKDF-SHA-256 over the RFC 6455 key and a fresh server nonce.
    ServerNonceHkdfSha256,
}

/// First application message on a token-bound WebSocket.
///
/// The nonce is public server-fresh input, not a credential, but `Debug`
/// deliberately reports only its length so transitive transport diagnostics do
/// not accumulate handshake material. Explicit field access is unchanged.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenBindingChallenge {
    /// Wire-contract version. Server 0.7.0 emits `2`.
    pub version: u8,
    /// Selected key-derivation and proof scheme.
    pub scheme: TokenBindingScheme,
    /// Standard-base64 server nonce, which decodes to exactly 32 bytes.
    pub nonce: String,
    /// First shared JSON/binary sequence number. Server 0.7.0 emits `1`.
    pub first_sequence: u64,
}

impl std::fmt::Debug for TokenBindingChallenge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenBindingChallenge")
            .field("version", &self.version)
            .field("scheme", &self.scheme)
            .field("nonce_len", &self.nonce.len())
            .field("first_sequence", &self.first_sequence)
            .finish()
    }
}

#[cfg(feature = "token-binding")]
const VERSION: u8 = 2;
#[cfg(feature = "token-binding")]
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
#[cfg(feature = "token-binding")]
const JSON_DOMAIN: &[u8] = b"signalfish.tokenbinding.v2\0json\0";
#[cfg(feature = "token-binding")]
const BINARY_DOMAIN: &[u8] = b"signalfish.tokenbinding.v2\0binary\0";
#[cfg(feature = "token-binding")]
const HKDF_INFO: &[u8] = b"signalfish.tokenbinding.v2/session-key";

#[cfg(feature = "token-binding")]
#[derive(Serialize)]
struct TokenBindingProof {
    version: u8,
    scheme: TokenBindingScheme,
    sequence: u64,
    signature: String,
    fingerprint: Option<String>,
}

#[cfg(feature = "token-binding")]
#[derive(Serialize)]
struct TokenBoundBinaryFrame<'a> {
    token_binding: TokenBindingProof,
    #[serde(with = "serde_bytes")]
    payload: &'a [u8],
}

/// Secret-bearing per-connection signer. Its custom `Debug` exposes only state.
#[cfg(feature = "token-binding")]
pub(crate) struct TokenBindingSession {
    secret: zeroize::Zeroizing<[u8; 32]>,
    next_sequence: u64,
    challenge: TokenBindingChallenge,
}

#[cfg(feature = "token-binding")]
impl std::fmt::Debug for TokenBindingSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenBindingSession")
            .field("next_sequence", &self.next_sequence)
            .field("challenge", &self.challenge)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "token-binding")]
impl TokenBindingSession {
    pub(crate) fn from_challenge(
        handshake_key: &str,
        challenge: TokenBindingChallenge,
    ) -> Result<Self, SignalFishError> {
        use base64::{engine::general_purpose::STANDARD, Engine as _};

        if challenge.version != VERSION {
            return Err(SignalFishError::TokenBinding(
                TokenBindingFailure::UnsupportedVersion,
            ));
        }
        if challenge.scheme != TokenBindingScheme::ServerNonceHkdfSha256 {
            return Err(SignalFishError::TokenBinding(
                TokenBindingFailure::UnsupportedScheme,
            ));
        }
        if challenge.first_sequence != 1 {
            return Err(SignalFishError::TokenBinding(
                TokenBindingFailure::InvalidFirstSequence,
            ));
        }

        let client_key =
            zeroize::Zeroizing::new(STANDARD.decode(handshake_key.as_bytes()).map_err(|_| {
                SignalFishError::TokenBinding(TokenBindingFailure::InvalidHandshakeKey)
            })?);
        if client_key.len() != 16 {
            return Err(SignalFishError::TokenBinding(
                TokenBindingFailure::InvalidHandshakeKey,
            ));
        }
        let nonce = zeroize::Zeroizing::new(
            STANDARD
                .decode(challenge.nonce.as_bytes())
                .map_err(|_| SignalFishError::TokenBinding(TokenBindingFailure::InvalidNonce))?,
        );
        if nonce.len() != 32 {
            return Err(SignalFishError::TokenBinding(
                TokenBindingFailure::InvalidNonce,
            ));
        }

        let hkdf = hkdf::Hkdf::<sha2::Sha256>::new(Some(nonce.as_slice()), client_key.as_slice());
        let mut secret = zeroize::Zeroizing::new([0_u8; 32]);
        hkdf.expand(HKDF_INFO, secret.as_mut())
            .map_err(|_| SignalFishError::TokenBinding(TokenBindingFailure::KeyDerivation))?;

        Ok(Self {
            secret,
            next_sequence: challenge.first_sequence,
            challenge,
        })
    }

    pub(crate) fn challenge(&self) -> &TokenBindingChallenge {
        &self.challenge
    }

    pub(crate) fn prepare(
        &self,
        frame: &TransportFrame,
    ) -> Result<TransportFrame, SignalFishError> {
        if self.next_sequence > MAX_SAFE_INTEGER {
            return Err(SignalFishError::TokenBinding(
                TokenBindingFailure::SequenceExhausted,
            ));
        }
        match frame {
            TransportFrame::Text(text) => self.prepare_text(text),
            TransportFrame::Binary(payload) => self.prepare_binary(payload),
        }
    }

    pub(crate) fn commit(&mut self) -> Result<(), SignalFishError> {
        self.next_sequence =
            self.next_sequence
                .checked_add(1)
                .ok_or(SignalFishError::TokenBinding(
                    TokenBindingFailure::SequenceExhausted,
                ))?;
        Ok(())
    }

    fn proof(&self, domain: &[u8], payload: &[u8]) -> Result<TokenBindingProof, SignalFishError> {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        use hmac::Mac as _;

        type HmacSha256 = hmac::Hmac<sha2::Sha256>;
        let mut mac = <HmacSha256 as hmac::KeyInit>::new_from_slice(self.secret.as_ref())
            .map_err(|_| SignalFishError::TokenBinding(TokenBindingFailure::KeyDerivation))?;
        mac.update(domain);
        mac.update(&self.next_sequence.to_be_bytes());
        mac.update(payload);
        Ok(TokenBindingProof {
            version: VERSION,
            scheme: TokenBindingScheme::ServerNonceHkdfSha256,
            sequence: self.next_sequence,
            signature: STANDARD.encode(mac.finalize().into_bytes()),
            fingerprint: None,
        })
    }

    fn prepare_text(&self, text: &str) -> Result<TransportFrame, SignalFishError> {
        let UniqueJsonValue(mut value) = serde_json::from_str(text)
            .map_err(|_| SignalFishError::TokenBinding(TokenBindingFailure::UnsupportedJson))?;
        if value
            .as_object()
            .is_some_and(|object| object.contains_key("token_binding"))
        {
            return Err(SignalFishError::TokenBinding(
                TokenBindingFailure::UnsupportedJson,
            ));
        }
        let canonical = canonical_json(&value)?;
        let proof = serde_json::to_value(self.proof(JSON_DOMAIN, &canonical)?)
            .map_err(|_| SignalFishError::TokenBinding(TokenBindingFailure::MessageEncoding))?;
        value
            .as_object_mut()
            .ok_or(SignalFishError::TokenBinding(
                TokenBindingFailure::UnsupportedJson,
            ))?
            .insert("token_binding".to_string(), proof);
        serde_json::to_string(&value)
            .map(TransportFrame::Text)
            .map_err(|_| SignalFishError::TokenBinding(TokenBindingFailure::MessageEncoding))
    }

    fn prepare_binary(&self, payload: &[u8]) -> Result<TransportFrame, SignalFishError> {
        let envelope = TokenBoundBinaryFrame {
            token_binding: self.proof(BINARY_DOMAIN, payload)?,
            payload,
        };
        rmp_serde::to_vec_named(&envelope)
            .map(TransportFrame::Binary)
            .map_err(|_| SignalFishError::TokenBinding(TokenBindingFailure::MessageEncoding))
    }
}

#[cfg(feature = "token-binding")]
pub(crate) fn parse_challenge(text: &str) -> Result<TokenBindingChallenge, SignalFishError> {
    let UniqueJsonValue(value) = serde_json::from_str(text)
        .map_err(|_| SignalFishError::TokenBinding(TokenBindingFailure::MalformedChallenge))?;
    let envelope = value.as_object().ok_or(SignalFishError::TokenBinding(
        TokenBindingFailure::MalformedChallenge,
    ))?;
    if envelope.len() != 2 || !envelope.contains_key("type") || !envelope.contains_key("data") {
        return Err(SignalFishError::TokenBinding(
            TokenBindingFailure::MalformedChallenge,
        ));
    }
    if envelope.get("type").and_then(serde_json::Value::as_str) != Some("TokenBindingChallenge") {
        return Err(SignalFishError::TokenBinding(
            TokenBindingFailure::MalformedChallenge,
        ));
    }
    let data = envelope
        .get("data")
        .and_then(serde_json::Value::as_object)
        .ok_or(SignalFishError::TokenBinding(
            TokenBindingFailure::MalformedChallenge,
        ))?;
    if data.len() != 4
        || !["version", "scheme", "nonce", "first_sequence"]
            .iter()
            .all(|field| data.contains_key(*field))
    {
        return Err(SignalFishError::TokenBinding(
            TokenBindingFailure::MalformedChallenge,
        ));
    }
    let version = data
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|version| u8::try_from(version).ok())
        .ok_or(SignalFishError::TokenBinding(
            TokenBindingFailure::MalformedChallenge,
        ))?;
    if version != VERSION {
        return Err(SignalFishError::TokenBinding(
            TokenBindingFailure::UnsupportedVersion,
        ));
    }
    let scheme = match data.get("scheme").and_then(serde_json::Value::as_str) {
        Some("server_nonce_hkdf_sha256") => TokenBindingScheme::ServerNonceHkdfSha256,
        Some(_) => {
            return Err(SignalFishError::TokenBinding(
                TokenBindingFailure::UnsupportedScheme,
            ))
        }
        None => {
            return Err(SignalFishError::TokenBinding(
                TokenBindingFailure::MalformedChallenge,
            ))
        }
    };
    let nonce = data
        .get("nonce")
        .and_then(serde_json::Value::as_str)
        .ok_or(SignalFishError::TokenBinding(
            TokenBindingFailure::MalformedChallenge,
        ))?
        .to_owned();
    let first_sequence = data
        .get("first_sequence")
        .and_then(serde_json::Value::as_u64)
        .ok_or(SignalFishError::TokenBinding(
            TokenBindingFailure::MalformedChallenge,
        ))?;
    if first_sequence != 1 {
        return Err(SignalFishError::TokenBinding(
            TokenBindingFailure::InvalidFirstSequence,
        ));
    }
    Ok(TokenBindingChallenge {
        version,
        scheme,
        nonce,
        first_sequence,
    })
}

#[cfg(feature = "token-binding")]
fn canonical_json(value: &serde_json::Value) -> Result<Vec<u8>, SignalFishError> {
    fn write(value: &serde_json::Value, output: &mut Vec<u8>) -> serde_json::Result<()> {
        use serde::ser::Error as _;
        use serde_json::Value;

        match value {
            Value::Null => output.extend_from_slice(b"null"),
            Value::Bool(true) => output.extend_from_slice(b"true"),
            Value::Bool(false) => output.extend_from_slice(b"false"),
            Value::Number(number) => {
                let rendered = number
                    .as_i64()
                    .filter(|value| value.unsigned_abs() <= MAX_SAFE_INTEGER)
                    .map(|value| value.to_string())
                    .or_else(|| {
                        number
                            .as_u64()
                            .filter(|value| *value <= MAX_SAFE_INTEGER)
                            .map(|value| value.to_string())
                    })
                    .ok_or_else(|| serde_json::Error::custom("unsupported JSON number"))?;
                output.extend_from_slice(rendered.as_bytes());
            }
            Value::String(string) => serde_json::to_writer(output, string)?,
            Value::Array(values) => {
                output.push(b'[');
                for (index, item) in values.iter().enumerate() {
                    if index > 0 {
                        output.push(b',');
                    }
                    write(item, output)?;
                }
                output.push(b']');
            }
            Value::Object(values) => {
                let mut properties: Vec<_> = values.iter().collect();
                properties.sort_by_cached_key(|(key, _)| key.encode_utf16().collect::<Vec<_>>());
                output.push(b'{');
                for (index, (key, item)) in properties.into_iter().enumerate() {
                    if index > 0 {
                        output.push(b',');
                    }
                    serde_json::to_writer(&mut *output, key)?;
                    output.push(b':');
                    write(item, output)?;
                }
                output.push(b'}');
            }
        }
        Ok(())
    }

    let mut output = Vec::with_capacity(128);
    write(value, &mut output)
        .map_err(|_| SignalFishError::TokenBinding(TokenBindingFailure::UnsupportedJson))?;
    Ok(output)
}

#[cfg(feature = "token-binding")]
struct UniqueJsonValue(serde_json::Value);

#[cfg(feature = "token-binding")]
impl<'de> Deserialize<'de> for UniqueJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueJsonVisitor)
    }
}

#[cfg(feature = "token-binding")]
struct UniqueJsonVisitor;

#[cfg(feature = "token-binding")]
impl<'de> serde::de::Visitor<'de> for UniqueJsonVisitor {
    type Value = UniqueJsonValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value without duplicate object members")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(UniqueJsonValue(serde_json::Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if value.unsigned_abs() > MAX_SAFE_INTEGER {
            return Err(E::custom(
                "JSON integer exceeds the interoperable safe range",
            ));
        }
        Ok(UniqueJsonValue(serde_json::Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if value > MAX_SAFE_INTEGER {
            return Err(E::custom(
                "JSON integer exceeds the interoperable safe range",
            ));
        }
        Ok(UniqueJsonValue(serde_json::Value::Number(value.into())))
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Err(E::custom("floating-point JSON numbers are unsupported"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.visit_string(value.to_owned())
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(UniqueJsonValue(serde_json::Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueJsonValue(serde_json::Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueJsonValue(serde_json::Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0));
        while let Some(UniqueJsonValue(value)) = sequence.next_element()? {
            values.push(value);
        }
        Ok(UniqueJsonValue(serde_json::Value::Array(values)))
    }

    fn visit_map<A>(self, mut entries: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        let mut values = serde_json::Map::new();
        while let Some((key, UniqueJsonValue(value))) = entries.next_entry::<String, _>()? {
            if values.contains_key(&key) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate JSON object member: {key}"
                )));
            }
            values.insert(key, value);
        }
        Ok(UniqueJsonValue(serde_json::Value::Object(values)))
    }
}

#[cfg(all(test, feature = "token-binding"))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    const HANDSHAKE_KEY: &str = "MDEyMzQ1Njc4OWFiY2RlZg==";
    const NONCE: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

    #[derive(Deserialize)]
    struct GoldenVectors {
        handshake_key_base64: String,
        nonce_base64: String,
        derived_key_hex: String,
        hkdf_info: String,
        json_domain_hex: String,
        json_input: String,
        json_canonical: String,
        json_sequence: u64,
        json_signature_base64: String,
        binary_domain_hex: String,
        binary_payload_hex: String,
        binary_sequence: u64,
        binary_signature_base64: String,
        binary_envelope_hex: String,
    }

    fn golden_vectors() -> GoldenVectors {
        toml::from_str(include_str!("../tests/token-binding/vectors.toml"))
            .expect("pinned token-binding vectors must parse")
    }

    fn challenge() -> TokenBindingChallenge {
        TokenBindingChallenge {
            version: 2,
            scheme: TokenBindingScheme::ServerNonceHkdfSha256,
            nonce: NONCE.to_string(),
            first_sequence: 1,
        }
    }

    fn decode_hex(raw: &str) -> Vec<u8> {
        raw.as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let digit = |byte: u8| match byte {
                    b'0'..=b'9' => byte - b'0',
                    b'a'..=b'f' => byte - b'a' + 10,
                    _ => 0xff,
                };
                let high = digit(pair[0]);
                let low = digit(pair[1]);
                assert!(high < 16 && low < 16, "golden hex must be valid");
                high * 16 + low
            })
            .collect()
    }

    fn encode_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn server_070_json_and_binary_goldens_match_exactly() {
        let vectors = golden_vectors();
        let mut challenge = challenge();
        challenge.nonce.clone_from(&vectors.nonce_base64);
        let mut session =
            TokenBindingSession::from_challenge(&vectors.handshake_key_base64, challenge)
                .expect("pinned Server 0.7 challenge must derive a session");
        assert_eq!(encode_hex(session.secret.as_ref()), vectors.derived_key_hex);
        assert_eq!(HKDF_INFO, vectors.hkdf_info.as_bytes());
        assert_eq!(encode_hex(JSON_DOMAIN), vectors.json_domain_hex);
        assert_eq!(encode_hex(BINARY_DOMAIN), vectors.binary_domain_hex);
        let UniqueJsonValue(unsigned_json) = serde_json::from_str(&vectors.json_input)
            .expect("golden JSON must be unique and strict");
        assert_eq!(
            canonical_json(&unsigned_json).expect("golden JSON must canonicalize"),
            vectors.json_canonical.as_bytes()
        );

        let input = TransportFrame::Text(vectors.json_input.clone());
        let TransportFrame::Text(signed) = session
            .prepare(&input)
            .expect("golden JSON must be token-binding compatible")
        else {
            panic!("text input must produce text output");
        };
        let signed: serde_json::Value =
            serde_json::from_str(&signed).expect("signed JSON must parse");
        assert_eq!(
            signed["token_binding"]["signature"],
            vectors.json_signature_base64
        );
        assert_eq!(signed["token_binding"]["sequence"], vectors.json_sequence);
        session
            .commit()
            .expect("accepting the JSON golden must advance sequence");

        let payload = decode_hex(&vectors.binary_payload_hex);
        let TransportFrame::Binary(signed) = session
            .prepare(&TransportFrame::Binary(payload))
            .expect("golden binary must be token-binding compatible")
        else {
            panic!("binary input must produce binary output");
        };
        assert_eq!(vectors.binary_sequence, vectors.json_sequence + 1);
        assert!(
            signed
                .windows(vectors.binary_signature_base64.len())
                .any(|window| window == vectors.binary_signature_base64.as_bytes()),
            "binary envelope must contain the independently pinned signature"
        );
        assert_eq!(signed, decode_hex(&vectors.binary_envelope_hex));
    }

    #[test]
    fn canonical_json_rejects_the_server_forbidden_input_class() {
        let session = TokenBindingSession::from_challenge(HANDSHAKE_KEY, challenge())
            .expect("test challenge must derive a session");
        for raw in [
            r#"{"type":"Ping","n":1.0}"#,
            r#"{"type":"Ping","n":1e0}"#,
            r#"{"type":"Ping","n":-0}"#,
            r#"{"type":"Ping","n":9007199254740992}"#,
            r#"{"type":"Ping","type":"Pong"}"#,
            r#"{"type":"Ping","token_binding":{}}"#,
            r#"["Ping"]"#,
        ] {
            assert!(matches!(
                session.prepare(&TransportFrame::Text(raw.to_string())),
                Err(SignalFishError::TokenBinding(
                    TokenBindingFailure::UnsupportedJson
                ))
            ));
        }
    }

    #[test]
    fn malformed_challenges_fail_with_specific_non_secret_reasons() {
        let cases = [
            (
                r#"{"type":"Other","data":{}}"#,
                TokenBindingFailure::MalformedChallenge,
            ),
            (
                r#"{"type":"TokenBindingChallenge","data":{"version":3,"scheme":"server_nonce_hkdf_sha256","nonce":"AA==","first_sequence":1}}"#,
                TokenBindingFailure::UnsupportedVersion,
            ),
            (
                r#"{"type":"TokenBindingChallenge","data":{"version":2,"scheme":"future","nonce":"AA==","first_sequence":1}}"#,
                TokenBindingFailure::UnsupportedScheme,
            ),
            (
                r#"{"type":"TokenBindingChallenge","data":{"version":2,"scheme":"server_nonce_hkdf_sha256","nonce":"AA==","first_sequence":2}}"#,
                TokenBindingFailure::InvalidFirstSequence,
            ),
            (
                r#"{"type":"TokenBindingChallenge","type":"TokenBindingChallenge","data":{}}"#,
                TokenBindingFailure::MalformedChallenge,
            ),
        ];
        for (raw, expected) in cases {
            assert!(matches!(
                parse_challenge(raw).expect_err("malformed challenge must fail"),
                SignalFishError::TokenBinding(actual) if actual == expected
            ));
        }
    }

    #[test]
    fn debug_and_errors_omit_handshake_and_proof_material() {
        let challenge = challenge();
        let session = TokenBindingSession::from_challenge(HANDSHAKE_KEY, challenge.clone())
            .expect("test challenge must derive a session");
        let signed = session
            .prepare(&TransportFrame::Text(r#"{"type":"Ping"}"#.to_string()))
            .expect("Ping must be signable");
        let TransportFrame::Text(signed) = signed else {
            panic!("Ping must stay a text frame");
        };
        let proof: serde_json::Value =
            serde_json::from_str(&signed).expect("signed Ping must parse");
        let signature = proof["token_binding"]["signature"]
            .as_str()
            .expect("proof must contain a signature");
        for output in [
            format!("{challenge:?}"),
            format!("{session:?}"),
            format!(
                "{:?}",
                SignalFishError::TokenBinding(TokenBindingFailure::InvalidHandshakeKey)
            ),
            SignalFishError::TokenBinding(TokenBindingFailure::InvalidHandshakeKey).to_string(),
        ] {
            assert!(
                !output.contains(HANDSHAKE_KEY),
                "handshake key leaked: {output}"
            );
            assert!(!output.contains(NONCE), "challenge nonce leaked: {output}");
            assert!(
                !output.contains(signature),
                "proof signature leaked: {output}"
            );
            assert!(
                !output.contains("Ping"),
                "application payload leaked: {output}"
            );
        }
    }

    #[test]
    fn sequence_exhaustion_is_fail_closed() {
        let mut session = TokenBindingSession::from_challenge(HANDSHAKE_KEY, challenge())
            .expect("test challenge must derive a session");
        session.next_sequence = MAX_SAFE_INTEGER;
        assert!(session
            .prepare(&TransportFrame::Text(r#"{"type":"Ping"}"#.to_string()))
            .is_ok());
        session
            .commit()
            .expect("the final safe sequence may be accepted");
        assert!(matches!(
            session.prepare(&TransportFrame::Text(r#"{"type":"Ping"}"#.to_string())),
            Err(SignalFishError::TokenBinding(
                TokenBindingFailure::SequenceExhausted
            ))
        ));
    }
}
