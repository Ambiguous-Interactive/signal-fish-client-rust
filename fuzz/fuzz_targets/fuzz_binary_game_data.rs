#![no_main]

use libfuzzer_sys::fuzz_target;
use signal_fish_client::protocol::{decode_v2_binary_game_data, decode_v3_binary_game_data};

// Valid envelopes keep the fuzzer past the map-header frontier from its first
// iteration. Input bytes then perturb those envelopes so it explores both the
// successful decoders and their strict validation branches.
const V2_ENVELOPE: &[u8] = b"\x83\xabfrom_player\xc4\x10\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb\xcc\xdd\xee\xff\xa8encoding\xacmessage_pack\xa7payload\xc4\x04\x00\x01\x02\xff";
const V3_ENVELOPE: &[u8] = b"\x85\xabfrom_player\xc4\x10\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb\xcc\xdd\xee\xff\xa8encoding\xa4json\xa7payload\xc4\x04\x00\x01\x02\xff\xa3seq\x09\xa5epoch\x03";

fn exercise(wire: &[u8]) {
    let _ = decode_v2_binary_game_data(wire);
    let _ = decode_v3_binary_game_data(wire);
}

fn perturb(canonical: &[u8], input: &[u8]) -> Vec<u8> {
    let mut candidate = canonical.to_vec();
    for (index, byte) in input.iter().enumerate() {
        let slot = index % candidate.len();
        candidate[slot] ^= byte;
    }
    candidate
}

fuzz_target!(|wire: &[u8]| {
    exercise(wire);
    exercise(V2_ENVELOPE);
    exercise(V3_ENVELOPE);
    exercise(&perturb(V2_ENVELOPE, wire));
    exercise(&perturb(V3_ENVELOPE, wire));
});
