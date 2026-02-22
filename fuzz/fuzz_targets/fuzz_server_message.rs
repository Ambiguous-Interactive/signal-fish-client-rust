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
});
