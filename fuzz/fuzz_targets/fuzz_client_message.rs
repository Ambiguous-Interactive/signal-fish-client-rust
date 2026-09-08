#![no_main]

use libfuzzer_sys::fuzz_target;
use signal_fish_client::protocol::ClientMessage;

// Anything the JSON deserializer accepts must be byte-stable across a
// re-encode/re-decode cycle: `to_string(m)` reparsed and re-rendered must
// produce the identical text. This is the oracle for the Serialize side of
// `ClientMessage` (adjacent tagging, payload omission, payload `Value`
// re-rendering); a one-way decode that emits bytes the server or the client
// itself could not re-parse would fail here. Canonical bytes are what
// `WebSocketTransport` actually puts on the wire.
fn assert_stable(message: &ClientMessage) {
    let once = serde_json::to_string(message).expect("re-render decoded ClientMessage");
    let reparsed: ClientMessage =
        serde_json::from_str(&once).expect("re-parse rendered ClientMessage");
    let twice = serde_json::to_string(&reparsed).expect("re-render reparsed ClientMessage");
    assert_eq!(twice, once, "ClientMessage render/re-parse not idempotent");
}

fuzz_target!(|data: &[u8]| {
    // Exercise the raw-byte deserialization path (includes serde_json's
    // own UTF-8 validation and error handling for invalid sequences).
    if let Ok(message) = serde_json::from_slice::<ClientMessage>(data) {
        assert_stable(&message);
    }

    // Also exercise the str-based path for valid UTF-8 input, and pin that
    // both entry points classify identical text identically.
    if let Ok(text) = std::str::from_utf8(data) {
        if let Ok(message) = serde_json::from_str::<ClientMessage>(text) {
            assert_stable(&message);
            let from_bytes = serde_json::from_slice::<ClientMessage>(data)
                .expect("slice/str deserialization disagree on identical bytes");
            let rendered =
                serde_json::to_string(&from_bytes).expect("re-render slice-parsed ClientMessage");
            assert_eq!(
                rendered,
                serde_json::to_string(&message).expect("re-render str-parsed ClientMessage"),
                "slice and str deserialization produced different messages"
            );
        }
    }
});
