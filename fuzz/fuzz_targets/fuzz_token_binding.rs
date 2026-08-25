#![no_main]

use libfuzzer_sys::fuzz_target;
use signal_fish_client::token_binding::__internal_fuzz_facade as facade;
use signal_fish_client::transport::TransportFrame;

// A valid 32-byte nonce (bytes 0x00..=0x1f) and a valid 16-byte RFC 6455
// handshake key keep the fuzzer past the shape/type/length frontier from its
// first iteration. Perturbed inputs then explore the strict validation
// branches of every parse path.
const CHALLENGE: &[u8] =
    br#"{"type":"TokenBindingChallenge","data":{"version":2,"scheme":"server_nonce_hkdf_sha256","nonce":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=","first_sequence":1}}"#;
const HANDSHAKE_KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";
const FINGERPRINT: &str =
    "aa:bb:cc:dd:ee:ff:00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff:00:11:22:33:44:55:66:77:88:99";

fn perturb(canonical: &[u8], input: &[u8]) -> Vec<u8> {
    let mut candidate = canonical.to_vec();
    for (index, byte) in input.iter().enumerate() {
        let slot = index % candidate.len();
        candidate[slot] ^= byte;
    }
    candidate
}

fn exercise_challenge(text: &str) -> Option<signal_fish_client::token_binding::TokenBindingChallenge> {
    match facade::parse_challenge(text) {
        Ok(challenge) => Some(challenge),
        Err(_) => None,
    }
}

fn exercise_session(challenge_text: &str, payload: &[u8], fingerprint: Option<&str>) {
    let Some(challenge) = exercise_challenge(challenge_text) else {
        return;
    };
    let Ok(mut session) = facade::FuzzSession::from_challenge(
        HANDSHAKE_KEY,
        challenge,
        fingerprint,
    ) else {
        return;
    };
    let text_frame = TransportFrame::Text(String::from_utf8_lossy(payload).into_owned());
    let binary_frame = TransportFrame::Binary(payload.to_vec());
    for frame in [text_frame, binary_frame] {
        if session.prepare(&frame).is_ok() {
            let _ = session.commit();
        }
        // A Pending or failed send must leave the sequence untouched; both
        // retries here reuse the same sequence by construction.
        let _ = session.prepare(&frame);
    }
}

fuzz_target!(|input: &[u8]| {
    let lossy = String::from_utf8_lossy(input);

    // Raw bytes as challenge text plus perturbed valid challenges.
    let _ = exercise_challenge(&lossy);
    let perturbed_bytes = perturb(CHALLENGE, input);
    let perturbed = String::from_utf8_lossy(&perturbed_bytes).into_owned();
    let _ = exercise_challenge(&perturbed);

    // Canonical JSON rendering over arbitrary parsed values.
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(input) {
        let _ = facade::canonical_json(&value);
    }

    // Session derivation plus prepare/commit cycles with and without an
    // optional client-certificate fingerprint claim. The unperturbed
    // challenge guarantees derived sessions; the perturbed one lets hostile
    // challenges reach session construction whenever validation permits.
    let canonical_challenge = String::from_utf8_lossy(CHALLENGE).into_owned();
    exercise_session(&canonical_challenge, input, None);
    exercise_session(&perturbed, input, Some(FINGERPRINT));
});
