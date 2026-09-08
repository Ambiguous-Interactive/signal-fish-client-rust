#![no_main]

use libfuzzer_sys::fuzz_target;
use signal_fish_client::protocol::{decode_v2_binary_game_data, decode_v3_binary_game_data};

// Valid envelopes keep the fuzzer past the map-header frontier from its first
// iteration. Input bytes then perturb those envelopes so it explores both the
// successful decoders and their strict validation branches.
const V2_ENVELOPE: &[u8] = b"\x83\xabfrom_player\xc4\x10\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb\xcc\xdd\xee\xff\xa8encoding\xacmessage_pack\xa7payload\xc4\x04\x00\x01\x02\xff";
const V3_ENVELOPE: &[u8] = b"\x85\xabfrom_player\xc4\x10\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb\xcc\xdd\xee\xff\xa8encoding\xa4json\xa7payload\xc4\x04\x00\x01\x02\xff\xa3seq\x09\xa5epoch\x03";

// Anything the strict decoder accepts must survive its own canonical
// re-serialization unchanged. This is the oracle for the Serialize side of
// the envelope: a decoder/encoder asymmetry (field dropped, bin wrapper
// lost, map key renamed, order-dependent decode) would produce bytes the
// decoder rejects or a different frame, and the fuzz run fails.
fn exercise_2(wire: &[u8]) {
    if let Ok(frame) = decode_v2_binary_game_data(wire) {
        let encoded = rmp_serde::to_vec_named(&frame).expect("re-encode decoded v2 frame");
        assert_eq!(
            decode_v2_binary_game_data(&encoded).expect("decode re-encoded v2 frame"),
            frame,
            "v2 envelope decode/encode identity broken"
        );
    }
}

fn exercise_3(wire: &[u8]) {
    if let Ok(frame) = decode_v3_binary_game_data(wire) {
        let encoded = rmp_serde::to_vec_named(&frame).expect("re-encode decoded v3 frame");
        assert_eq!(
            decode_v3_binary_game_data(&encoded).expect("decode re-encoded v3 frame"),
            frame,
            "v3 envelope decode/encode identity broken"
        );
    }
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
    exercise_2(wire);
    exercise_3(wire);
    exercise_2(V2_ENVELOPE);
    exercise_3(V3_ENVELOPE);
    exercise_2(&perturb(V2_ENVELOPE, wire));
    exercise_3(&perturb(V3_ENVELOPE, wire));
});
