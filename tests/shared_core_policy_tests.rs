#![allow(clippy::panic)]

use std::fs;

#[test]
fn shared_core_guide_pins_driver_ownership_and_object_safety() {
    let path = ".llm/skills/shared-core-drivers/SKILL.md";
    let guide = fs::read_to_string(path).unwrap_or_else(|error| {
        panic!("failed to read {path}: {error}");
    });
    for required in [
        "ClientCore",
        "SignalFishClientApi",
        "object-safe",
        "Common synchronous commands take",
        "Waiting sends and `shutdown`",
        "`poll`, `close`, and",
        "parity matrix",
        "Release it before",
    ] {
        assert!(
            guide.contains(required),
            "{path} must document shared-core invariant {required:?}"
        );
    }
}

#[test]
fn both_drivers_delegate_inbound_frames_to_client_core() {
    for path in ["src/client.rs", "src/polling_client.rs"] {
        let source = fs::read_to_string(path).unwrap_or_else(|error| {
            panic!("failed to read {path}: {error}");
        });
        assert!(
            source.contains(".process_frame(frame)"),
            "{path} must delegate inbound frames to ClientCore"
        );
        for forbidden in [
            "from_str::<ServerMessage>",
            "validate_server_frame",
            "DeliveryAccountability::new",
            "#[cfg(any())]",
        ] {
            assert!(
                !source.contains(forbidden),
                "{path} must not reintroduce driver-owned protocol logic {forbidden:?}"
            );
        }
    }
}
