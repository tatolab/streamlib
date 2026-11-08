// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::Serialize;
use serde_json::{Map, Value};

/// A project file's bytes in canonical form: keys sorted, two-space indent, one trailing newline, so a diff reads well.
pub fn canonical_project_json_bytes(value: &impl Serialize) -> serde_json::Result<Vec<u8>> {
    let sorted = value_with_every_object_sorted_by_key(serde_json::to_value(value)?);
    let mut bytes = serde_json::to_vec_pretty(&sorted)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn value_with_every_object_sorted_by_key(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries: Vec<(String, Value)> = object.into_iter().collect();
            entries.sort_by(|(left_key, _), (right_key, _)| left_key.cmp(right_key));
            let mut sorted = Map::with_capacity(entries.len());
            for (key, entry) in entries {
                sorted.insert(key, value_with_every_object_sorted_by_key(entry));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(value_with_every_object_sorted_by_key)
                .collect(),
        ),
        scalar => scalar,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_are_sorted_at_every_depth_with_two_space_indent_and_a_trailing_newline() {
        let bytes =
            canonical_project_json_bytes(&json!({"b": 1, "a": {"d": [{"z": 1, "y": 2}], "c": 3}}))
                .unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "{\n  \"a\": {\n    \"c\": 3,\n    \"d\": [\n      {\n        \"y\": 2,\n        \"z\": 1\n      }\n    ]\n  },\n  \"b\": 1\n}\n"
        );
    }
}
