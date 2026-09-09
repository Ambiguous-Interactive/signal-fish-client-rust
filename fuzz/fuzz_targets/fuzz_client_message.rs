#![no_main]

mod json_oracle;

use libfuzzer_sys::fuzz_target;
use signal_fish_client::protocol::ClientMessage;

fuzz_target!(|data: &[u8]| {
    // Exercise the raw-byte deserialization path (includes serde_json's
    // own UTF-8 validation and error handling for invalid sequences).
    if let Ok(message) = serde_json::from_slice::<ClientMessage>(data) {
        json_oracle::assert_render_stable(&message, "ClientMessage");
    }

    // Also exercise the str-based path for valid UTF-8 input, and pin that
    // both entry points classify identical text identically.
    if let Ok(text) = std::str::from_utf8(data) {
        if let Ok(message) = serde_json::from_str::<ClientMessage>(text) {
            json_oracle::assert_render_stable(&message, "ClientMessage");
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
