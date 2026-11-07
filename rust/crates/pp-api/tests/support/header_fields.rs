//! Shared by the Build route parity tests.

/// GRE-1100 adds the header's `customer` and `due_date` to Rust's Plan
/// summary, from `build_profile_details`. Node's summary has no such keys
/// and Node isn't grown, so each Rust summary must carry both as null (no
/// case writes a header) and they're removed before the Node comparison.
/// Returns how many summaries carried them.
pub fn strip_header_fields(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(map) if map.contains_key("part_count") => {
            for key in ["customer", "due_date"] {
                assert_eq!(map.remove(key), Some(serde_json::Value::Null), "{key}");
            }
            map.values_mut().map(strip_header_fields).sum::<usize>() + 1
        }
        serde_json::Value::Object(map) => map.values_mut().map(strip_header_fields).sum(),
        serde_json::Value::Array(items) => items.iter_mut().map(strip_header_fields).sum(),
        _ => 0,
    }
}
