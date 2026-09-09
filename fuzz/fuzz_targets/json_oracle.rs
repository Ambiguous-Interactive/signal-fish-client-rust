//! Shared render/re-parse stability oracle for the two JSON fuzz targets.
//!
//! serde_json without its `float_roundtrip` feature documents that float
//! parsing is fast but not always correctly rounded: one decimal text can
//! parse to an f64 whose shortest re-rendering parses to a neighboring
//! value, so byte-level render idempotence is unachievable for payloads
//! containing floats (observed directly by CI's fuzz build; the crash
//! regression seed pins the exact shape). This module asserts the
//! strongest invariant that holds in every build:
//!
//! - float-free messages are byte-idempotent end to end;
//! - any other message keeps every byte and every structural element
//!   stable, with each float allowed to drift by at most one ULP per
//!   re-parse cycle (integer faces stay exact).
//!
//! Everything else failing = a real decode/encode defect worth crashing
//! over: shape changes, vanished keys, integer drift, >1-ULP float damage.

use serde_json::Value;

/// Render `message`, re-parse it, re-render it, and assert the stability
/// contract documented above. `type_name` only sharpens failure messages.
pub fn assert_render_stable<T>(message: &T, type_name: &'static str)
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let once = serde_json::to_string(message)
        .unwrap_or_else(|error| panic!("{type_name} re-render failed: {error}"));
    let reparsed: T = serde_json::from_str(&once)
        .unwrap_or_else(|error| panic!("{type_name} re-parse of its own render failed: {error}"));
    let twice = serde_json::to_string(&reparsed)
        .unwrap_or_else(|error| panic!("{type_name} second render failed: {error}"));

    let value_once = serde_json::to_value(message)
        .unwrap_or_else(|error| panic!("{type_name} value render failed: {error}"));
    if contains_float(&value_once) {
        let value_twice = serde_json::to_value(&reparsed)
            .unwrap_or_else(|error| panic!("{type_name} second value render failed: {error}"));
        assert_structurally_equal(&value_once, &value_twice, "$");
    } else {
        assert_eq!(
            twice, once,
            "{type_name} render/re-parse not idempotent"
        );
    }
}

fn contains_float(value: &Value) -> bool {
    match value {
        Value::Number(number) => number.as_f64().is_some() && !number.is_u64() && !number.is_i64(),
        Value::Array(items) => items.iter().any(contains_float),
        Value::Object(fields) => fields.values().any(contains_float),
        _ => false,
    }
}

fn assert_structurally_equal(expected: &Value, found: &Value, path: &str) {
    match (expected, found) {
        (Value::Number(expected_number), Value::Number(found_number)) => {
            if let (Some(expected_float), Some(found_float)) =
                (expected_number.as_f64(), found_number.as_f64())
            {
                if !expected_number.is_u64()
                    && !expected_number.is_i64()
                    && !found_number.is_u64()
                    && !found_number.is_i64()
                {
                    // Float faces: serde_json's fast (non `float_roundtrip`)
                    // parser may land one ULP from the exactly rounded value.
                    let drift = expected_float
                        .to_bits()
                        .abs_diff(found_float.to_bits());
                    assert!(
                        drift <= 1,
                        "float drifted {drift} ULP at {path}: {expected_float} vs {found_float}"
                    );
                    return;
                }
            }
            assert_eq!(
                expected_number, found_number,
                "number changed at {path}: {expected_number} vs {found_number}"
            );
        }
        (Value::Array(expected_items), Value::Array(found_items)) => {
            assert_eq!(
                expected_items.len(),
                found_items.len(),
                "array length changed at {path}"
            );
            for (index, (expected_item, found_item)) in
                expected_items.iter().zip(found_items).enumerate()
            {
                assert_structurally_equal(
                    expected_item,
                    found_item,
                    &format!("{path}/{index}"),
                );
            }
        }
        (Value::Object(expected_fields), Value::Object(found_fields)) => {
            assert_eq!(
                expected_fields.len(),
                found_fields.len(),
                "object field count changed at {path}"
            );
            for (key, expected_field) in expected_fields {
                let found_field = found_fields.get(key).unwrap_or_else(|| {
                    panic!("key {key:?} vanished at {path}")
                });
                assert_structurally_equal(
                    expected_field,
                    found_field,
                    &format!("{path}/{key}"),
                );
            }
        }
        _ => assert_eq!(
            expected, found,
            "value changed at {path}: {expected} vs {found}"
        ),
    }
}
