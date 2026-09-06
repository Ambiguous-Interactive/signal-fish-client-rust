#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Exercise the raw-byte deserialization path (includes serde_json's
    // own UTF-8 validation and error handling for invalid sequences).
    let _ = serde_json::from_slice::<signal_fish_client::protocol::ServerMessage>(data);

    // Also exercise the str-based path for valid UTF-8 input.
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = serde_json::from_str::<signal_fish_client::protocol::ServerMessage>(s);
    }

    // Peer-authored signal shapes are a separate inbound parse boundary
    // (PeerSignal::try_from rejects everything but the three externally
    // tagged Offer/Answer/IceCandidate objects). A hostile `signal` value
    // inside a server envelope reaches it through the mesh choreography, so
    // drive the parser directly on the raw input as well.
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(data) {
        let _ = signal_fish_client::PeerSignal::try_from(&value);
    }
});
