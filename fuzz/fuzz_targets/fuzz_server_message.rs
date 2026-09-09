#![no_main]

mod json_oracle;

use libfuzzer_sys::fuzz_target;
use signal_fish_client::protocol::ServerMessage;

fuzz_target!(|data: &[u8]| {
    // Exercise the raw-byte deserialization path (includes serde_json's
    // own UTF-8 validation and error handling for invalid sequences).
    if let Ok(message) = serde_json::from_slice::<ServerMessage>(data) {
        json_oracle::assert_render_stable(&message, "ServerMessage");
    }

    // Also exercise the str-based path for valid UTF-8 input, and pin that
    // both entry points classify identical text identically.
    if let Ok(text) = std::str::from_utf8(data) {
        if let Ok(message) = serde_json::from_str::<ServerMessage>(text) {
            json_oracle::assert_render_stable(&message, "ServerMessage");
            let from_bytes = serde_json::from_slice::<ServerMessage>(data)
                .expect("slice/str deserialization disagree on identical bytes");
            let rendered =
                serde_json::to_string(&from_bytes).expect("re-render slice-parsed ServerMessage");
            assert_eq!(
                rendered,
                serde_json::to_string(&message).expect("re-render str-parsed ServerMessage"),
                "slice and str deserialization produced different messages"
            );
        }
    }

    // Peer-authored signal shapes are a separate inbound parse boundary
    // (PeerSignal::try_from rejects everything but the three externally
    // tagged Offer/Answer/IceCandidate objects). A hostile `signal` value
    // inside a server envelope reaches it through the mesh choreography, so
    // drive the parser directly on the raw input as well, and pin the
    // accepted set as a fixpoint of its own serializer: whatever
    // `try_from` accepts must re-render to a value that parses to the
    // identical signal.
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(data) {
        if let Ok(signal) = signal_fish_client::PeerSignal::try_from(&value) {
            let rendered = serde_json::to_value(&signal).expect("re-render accepted PeerSignal");
            let reparsed = signal_fish_client::PeerSignal::try_from(&rendered)
                .expect("re-parse rendered PeerSignal");
            assert_eq!(
                reparsed, signal,
                "PeerSignal parse/render identity broken"
            );
        }
    }
});
