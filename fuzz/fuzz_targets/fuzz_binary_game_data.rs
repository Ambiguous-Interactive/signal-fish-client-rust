#![no_main]

use libfuzzer_sys::fuzz_target;
use signal_fish_client::protocol::decode_v3_binary_game_data;

fuzz_target!(|wire: &[u8]| {
    let _ = decode_v3_binary_game_data(wire);
});
