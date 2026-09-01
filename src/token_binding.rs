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

/// Token-binding proof scheme, introduced by Signal Fish Server 0.7.0 and
/// unchanged through 0.8.0.
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
    /// Wire-contract version. Server 0.7.0+ emits `2`.
    pub version: u8,
    /// Selected key-derivation and proof scheme.
    pub scheme: TokenBindingScheme,
    /// Standard-base64 server nonce, which decodes to exactly 32 bytes.
    pub nonce: String,
    /// First shared JSON/binary sequence number. Server 0.7.0+ emits `1`.
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
    client_fingerprint: Option<zeroize::Zeroizing<String>>,
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
        client_fingerprint: Option<String>,
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
            client_fingerprint: client_fingerprint.map(zeroize::Zeroizing::new),
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
        if let Some(fingerprint) = self.client_fingerprint.as_deref() {
            mac.update(fingerprint.as_bytes());
        }
        Ok(TokenBindingProof {
            version: VERSION,
            scheme: TokenBindingScheme::ServerNonceHkdfSha256,
            sequence: self.next_sequence,
            signature: STANDARD.encode(mac.finalize().into_bytes()),
            fingerprint: self
                .client_fingerprint
                .as_deref()
                .map(std::string::ToString::to_string),
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
    // Recursion depth is bounded transitively: every `Value` reaching here is
    // parsed by serde_json's default 128-level recursion limit (the
    // `unbounded_depth` feature is not enabled anywhere), so `write` cannot
    // drive the stack to exhaustion regardless of input size. Keep that true:
    // never feed hand-built deeply nested `Value`s into this function.
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
        // A consumer crate can silently enable serde_json's `arbitrary_precision`
        // feature through Cargo feature unification; that feature delivers every
        // number that fails its u64/i64 parse to `deserialize_any` visitors as a
        // single-member marker map instead of calling the scalar visitors.
        // Classify the marker unconditionally (the key is a serde_json-internal
        // name no protocol payload legitimately carries) so a consumer's feature
        // flags cannot flip accept/reject verdicts or the canonical bytes. The
        // one deliberate divergence: serde_json classifies the literal `-0` as
        // the integer `0` before any marker exists, so unified builds accept
        // `-0` as `0` — as the bare literal or via a hand-crafted marker raw
        // text — where default builds reject it as a negative-zero float;
        // canonical `0` is exactly the server's RFC 8785 form for `-0`.
        if values.len() == 1 {
            if let Some(serde_json::Value::String(raw)) =
                values.get(ARBITRARY_PRECISION_NUMBER_MARKER)
            {
                return classify_marker_number(raw)
                    .map(UniqueJsonValue)
                    .map_err(serde::de::Error::custom);
            }
        }
        Ok(UniqueJsonValue(serde_json::Value::Object(values)))
    }
}

#[cfg(feature = "token-binding")]
const ARBITRARY_PRECISION_NUMBER_MARKER: &str = "$serde_json::private::Number";

/// Classifies a serde_json `arbitrary_precision` marker payload exactly like
/// the scalar visitors classify a directly parsed number: integers within the
/// interoperable safe range are kept, and everything else (floats, exponents,
/// out-of-range integers) is forbidden input. The literal `-0` never reaches
/// this helper from serde_json itself — neither as the bare literal nor inside
/// a genuine marker raw text: serde_json classifies it as the integer `0`
/// before any marker exists, so unified builds accept it as `0` where default
/// builds reject it as a negative-zero float (canonical `0` is exactly the
/// server's RFC 8785 form for `-0`). A hand-crafted marker `"-0"` follows the
/// same split verdicts.
#[cfg(feature = "token-binding")]
fn classify_marker_number(raw: &str) -> Result<serde_json::Value, String> {
    // serde_json marker raw texts are exact number tokens with no surrounding
    // whitespace; reject anything else instead of relying on `from_str`
    // leniency.
    if raw.trim() != raw {
        return Err("malformed JSON number".to_string());
    }
    let parsed: serde_json::Value =
        serde_json::from_str(raw).map_err(|_| "malformed JSON number".to_string())?;
    if let Some(value) = parsed.as_u64() {
        if value > MAX_SAFE_INTEGER {
            return Err("JSON integer exceeds the interoperable safe range".to_string());
        }
        return Ok(serde_json::Value::Number(value.into()));
    }
    if let Some(value) = parsed.as_i64() {
        if value.unsigned_abs() > MAX_SAFE_INTEGER {
            return Err("JSON integer exceeds the interoperable safe range".to_string());
        }
        return Ok(serde_json::Value::Number(value.into()));
    }
    if parsed.as_f64().is_some() {
        return Err("floating-point JSON numbers are unsupported".to_string());
    }
    Err("malformed JSON number".to_string())
}

#[cfg(all(test, feature = "token-binding"))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
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
        client_fingerprint: String,
        fingerprint_json_signature_base64: String,
        fingerprint_binary_signature_base64: String,
        fingerprint_json_mac_input_hex: String,
        fingerprint_binary_mac_input_hex: String,
    }

    #[derive(Deserialize)]
    struct SignedBinaryEnvelope {
        token_binding: SignedProof,
        #[serde(rename = "payload")]
        #[serde(with = "serde_bytes")]
        _payload: Vec<u8>,
    }

    #[derive(Deserialize)]
    struct SignedProof {
        signature: String,
        fingerprint: Option<String>,
    }

    fn golden_vectors() -> GoldenVectors {
        toml::from_str(include_str!("testdata/token-binding-vectors.toml"))
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
    fn server_080_json_and_binary_goldens_match_exactly() {
        let vectors = golden_vectors();
        let mut challenge = challenge();
        challenge.nonce.clone_from(&vectors.nonce_base64);
        let mut session =
            TokenBindingSession::from_challenge(&vectors.handshake_key_base64, challenge, None)
                .expect("pinned Server 0.8 challenge must derive a session");
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
        let session = TokenBindingSession::from_challenge(HANDSHAKE_KEY, challenge(), None)
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

    /// A marker-shaped single-member string object is classified as a number
    /// in every build (see `visit_map`): a safe-integer raw text must produce
    /// the exact same signed envelope as the bare literal, and a non-string
    /// marker value must keep the plain-object fall-through byte-stable.
    /// (serde_json itself only ever produces marker raw texts for forbidden
    /// classes — floats, exponents, out-of-range integers — so the `"5"`
    /// case pins the deliberate default-build reclassification, the same
    /// verdict a unified build would reach for that input.)
    #[test]
    fn canonical_json_treats_arbitrary_precision_marker_maps_like_numbers() {
        // Two sessions from the same challenge share the derived key and
        // start at the same sequence, so differing envelopes would prove the
        // marker path changed the canonical bytes.
        let marker = r#"{"type":"Ping","n":{"$serde_json::private::Number":"5"}}"#;
        let plain = r#"{"type":"Ping","n":5}"#;
        let signed_marker = TokenBindingSession::from_challenge(HANDSHAKE_KEY, challenge(), None)
            .expect("test challenge must derive a session")
            .prepare(&TransportFrame::Text(marker.to_string()))
            .expect("marker-encoded safe integer must prepare");
        let signed_plain = TokenBindingSession::from_challenge(HANDSHAKE_KEY, challenge(), None)
            .expect("test challenge must derive a session")
            .prepare(&TransportFrame::Text(plain.to_string()))
            .expect("plain safe integer must prepare");
        assert_eq!(signed_marker, signed_plain);

        // A marker key with a non-string value is an ordinary object: sign it
        // and prove the envelope differs from the number-reclassified form.
        let object = r#"{"type":"Ping","w":{"$serde_json::private::Number":5}}"#;
        let signed_object = TokenBindingSession::from_challenge(HANDSHAKE_KEY, challenge(), None)
            .expect("test challenge must derive a session")
            .prepare(&TransportFrame::Text(object.to_string()))
            .expect("non-string marker value must stay an ordinary object");
        let reclassified = TokenBindingSession::from_challenge(HANDSHAKE_KEY, challenge(), None)
            .expect("test challenge must derive a session")
            .prepare(&TransportFrame::Text(
                r#"{"type":"Ping","w":5}"#.to_string(),
            ))
            .expect("plain nested number must prepare");
        assert_ne!(
            signed_object, reclassified,
            "a non-string marker value must not be reclassified as its number"
        );
    }

    #[test]
    fn canonical_json_rejects_marker_encoded_forbidden_numbers() {
        let session = TokenBindingSession::from_challenge(HANDSHAKE_KEY, challenge(), None)
            .expect("test challenge must derive a session");
        for raw in [
            r#"{"type":"Ping","n":{"$serde_json::private::Number":"1.0"}}"#,
            r#"{"type":"Ping","n":{"$serde_json::private::Number":"1e0"}}"#,
            r#"{"type":"Ping","n":{"$serde_json::private::Number":"-0.0"}}"#,
            r#"{"type":"Ping","n":{"$serde_json::private::Number":"9007199254740992"}}"#,
            r#"{"type":"Ping","n":{"$serde_json::private::Number":"-9007199254740992"}}"#,
            r#"{"type":"Ping","n":{"$serde_json::private::Number":"bogus"}}"#,
            r#"{"type":"Ping","n":{"$serde_json::private::Number":""}}"#,
        ] {
            assert!(
                matches!(
                    session.prepare(&TransportFrame::Text(raw.to_string())),
                    Err(SignalFishError::TokenBinding(
                        TokenBindingFailure::UnsupportedJson
                    ))
                ),
                "marker payload must be classified like the scalar visitors: {raw}"
            );
        }
    }

    /// Object keys are ordered by UTF-16 code units (JCS order), matching the
    /// server's canonicalizer byte-for-byte. UTF-8 byte order disagrees for
    /// keys above U+E000 versus astral keys; this pin fails if the sort is
    /// ever "simplified" to plain string ordering.
    #[test]
    fn canonical_json_orders_object_keys_by_utf16_code_units() {
        let value = serde_json::from_str::<serde_json::Value>(r#"{"":1,"😀":2,"a":3}"#)
            .expect("ordering fixture must parse");
        let canonical = canonical_json(&value).expect("ordering fixture must canonicalize");
        assert_eq!(
            std::str::from_utf8(&canonical).expect("canonical output is UTF-8"),
            r#"{"a":3,"😀":2,"":1}"#,
        );
        // serde_json's BTreeMap wire order genuinely differs, which is why
        // proofs are computed over the canonical bytes rather than a
        // re-serialization of the parsed map.
        let wire_order = serde_json::to_string(&value).expect("re-serialization");
        assert_ne!(wire_order, std::str::from_utf8(&canonical).unwrap());
    }

    /// Pin string-value escaping to the ECMAScript/JCS set the server's
    /// canonicalizer implements: short escapes for `\b\t\n\f\r`, lowercase
    /// `\u00xx` for the remaining control characters below 0x20, and raw
    /// UTF-8 for DEL and all non-ASCII scalar values. The golden vectors only
    /// exercise ASCII values, so this test is the parity pin for the rest.
    #[test]
    fn canonical_json_escapes_control_and_non_ascii_values_like_the_server() {
        let value = serde_json::json!({
            "z": "\u{01}\u{1F600}",
            "a": "q\"b\\c\u{08}\u{7F}",
        });
        let canonical = canonical_json(&value).expect("escaping fixture must canonicalize");
        assert_eq!(
            std::str::from_utf8(&canonical).expect("canonical output is UTF-8"),
            "{\"a\":\"q\\\"b\\\\c\\b\u{7F}\",\"z\":\"\\u0001\u{1F600}\"}",
        );
    }

    #[test]
    fn server_080_fingerprint_goldens_bind_json_and_binary_proofs() {
        let vectors = golden_vectors();
        let mut challenge = challenge();
        challenge.nonce.clone_from(&vectors.nonce_base64);
        let mut session = TokenBindingSession::from_challenge(
            &vectors.handshake_key_base64,
            challenge,
            Some(vectors.client_fingerprint.clone()),
        )
        .expect("pinned Server 0.8 fingerprint challenge must derive a session");
        let UniqueJsonValue(unsigned_json) = serde_json::from_str(&vectors.json_input)
            .expect("fingerprint JSON golden must be unique and strict");
        let mut expected_json_mac_input = JSON_DOMAIN.to_vec();
        expected_json_mac_input.extend_from_slice(&vectors.json_sequence.to_be_bytes());
        expected_json_mac_input.extend_from_slice(
            &canonical_json(&unsigned_json).expect("fingerprint JSON golden must canonicalize"),
        );
        expected_json_mac_input.extend_from_slice(vectors.client_fingerprint.as_bytes());
        assert_eq!(
            encode_hex(&expected_json_mac_input),
            vectors.fingerprint_json_mac_input_hex
        );

        let TransportFrame::Text(signed_json) = session
            .prepare(&TransportFrame::Text(vectors.json_input.clone()))
            .expect("fingerprint-bound JSON golden must be signable")
        else {
            panic!("text input must produce text output");
        };
        let signed_json: serde_json::Value =
            serde_json::from_str(&signed_json).expect("signed JSON must parse");
        assert_eq!(
            signed_json["token_binding"]["fingerprint"],
            vectors.client_fingerprint
        );
        assert_eq!(
            signed_json["token_binding"]["signature"],
            vectors.fingerprint_json_signature_base64
        );
        session
            .commit()
            .expect("accepting the JSON golden must advance sequence");

        let binary_payload = decode_hex(&vectors.binary_payload_hex);
        let mut expected_binary_mac_input = BINARY_DOMAIN.to_vec();
        expected_binary_mac_input.extend_from_slice(&vectors.binary_sequence.to_be_bytes());
        expected_binary_mac_input.extend_from_slice(&binary_payload);
        expected_binary_mac_input.extend_from_slice(vectors.client_fingerprint.as_bytes());
        assert_eq!(
            encode_hex(&expected_binary_mac_input),
            vectors.fingerprint_binary_mac_input_hex
        );
        let TransportFrame::Binary(signed_binary) = session
            .prepare(&TransportFrame::Binary(binary_payload))
            .expect("fingerprint-bound binary golden must be signable")
        else {
            panic!("binary input must produce binary output");
        };
        let signed_binary: SignedBinaryEnvelope =
            rmp_serde::from_slice(&signed_binary).expect("binary envelope must parse");
        assert_eq!(
            signed_binary.token_binding.fingerprint.as_deref(),
            Some(vectors.client_fingerprint.as_str())
        );
        assert_eq!(
            signed_binary.token_binding.signature,
            vectors.fingerprint_binary_signature_base64
        );
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
        let fingerprint = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
        let session = TokenBindingSession::from_challenge(
            HANDSHAKE_KEY,
            challenge.clone(),
            Some(fingerprint.to_string()),
        )
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
                !output.contains(fingerprint),
                "fingerprint leaked: {output}"
            );
            assert!(
                !output.contains("Ping"),
                "application payload leaked: {output}"
            );
        }
    }

    #[test]
    fn sequence_exhaustion_is_fail_closed() {
        let mut session = TokenBindingSession::from_challenge(HANDSHAKE_KEY, challenge(), None)
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

/// Internal entry points for the repository's `fuzz_token_binding` cargo-fuzz
/// target.
///
/// This module is `#[doc(hidden)]`, gated behind the non-default
/// `internal-fuzz-facade` feature (which itself requires `token-binding`),
/// and is explicitly **not part of the public API**: no semver guarantee
/// covers anything here, the surface may change or vanish at any time, and
/// production code must never enable the feature. It exists so the fuzz
/// target can drive `pub(crate)` parsing/profiling paths without widening
/// the supported API (issue #163).
#[cfg(all(feature = "token-binding", feature = "internal-fuzz-facade"))]
#[doc(hidden)]
pub mod __internal_fuzz_facade {
    use super::{
        canonical_json as internal_canonical_json, parse_challenge as internal_parse_challenge,
        SignalFishError, TokenBindingChallenge, TokenBindingSession,
    };
    use crate::transport::TransportFrame;

    /// Parse raw challenge text through the strict internal validator.
    pub fn parse_challenge(text: &str) -> Result<TokenBindingChallenge, SignalFishError> {
        internal_parse_challenge(text)
    }

    /// Render already-parsed JSON through the internal canonicalizer.
    pub fn canonical_json(value: &serde_json::Value) -> Result<Vec<u8>, SignalFishError> {
        internal_canonical_json(value)
    }

    /// Opaque handle driving a secret-bearing session's prepare/commit cycle.
    pub struct FuzzSession(TokenBindingSession);

    impl FuzzSession {
        /// Build a session exactly as the transport would after selection.
        pub fn from_challenge(
            handshake_key: &str,
            challenge: TokenBindingChallenge,
            client_fingerprint: Option<&str>,
        ) -> Result<Self, SignalFishError> {
            TokenBindingSession::from_challenge(
                handshake_key,
                challenge,
                client_fingerprint.map(std::borrow::ToOwned::to_owned),
            )
            .map(Self)
        }

        /// Protect one frame with the current sequence without committing it.
        pub fn prepare(&self, frame: &TransportFrame) -> Result<TransportFrame, SignalFishError> {
            self.0.prepare(frame)
        }

        /// Advance the sequence exactly as a successful send would.
        pub fn commit(&mut self) -> Result<(), SignalFishError> {
            self.0.commit()
        }
    }
}
