// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Deterministic property-ordering pass (Decision 7 of milestone-10's
//! `docs/architecture/schema-identity-and-packaging.md`).
//!
//! Backend codegen historically produced subtly different field orderings
//! across runs and across backends. The fix is a normalization pass that
//! stable-sorts every `properties` / `optionalProperties` / `definitions`
//! map by key before passing to `jtd-codegen`.
//!
//! `serde_json::Map` preserves insertion order; sorting recursively
//! eliminates the source of cross-run drift.

use serde_json::{Map, Value};

/// Recursively sort every JSON object's keys alphabetically.
///
/// Applied to a parsed JTD schema, this guarantees `properties`,
/// `optionalProperties`, `definitions`, and `metadata` all enumerate in the
/// same order across runs / backends. The `jtd-codegen` invocation that
/// follows produces output whose declaration order tracks key sort order.
pub fn sort_object_keys_recursively(value: &mut Value) {
    match value {
        Value::Object(map) => {
            // Sort children first so the recursion is deterministic top-down.
            for (_, v) in map.iter_mut() {
                sort_object_keys_recursively(v);
            }
            // Reorder the map by key.
            let mut sorted = Map::new();
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort();
            for k in keys {
                if let Some(v) = map.remove(&k) {
                    sorted.insert(k, v);
                }
            }
            *map = sorted;
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                sort_object_keys_recursively(v);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sort_top_level_keys() {
        let mut value = json!({
            "z": 1,
            "a": 2,
            "m": 3,
        });
        sort_object_keys_recursively(&mut value);
        let serialized = serde_json::to_string(&value).unwrap();
        // serde_json::Map preserves insertion order; check the serialized
        // form starts with "a".
        assert!(
            serialized.starts_with("{\"a\""),
            "expected sorted order, got: {}",
            serialized
        );
    }

    #[test]
    fn sort_nested_keys() {
        let mut value = json!({
            "outer": {
                "z": 1,
                "a": 2,
            }
        });
        sort_object_keys_recursively(&mut value);
        let serialized = serde_json::to_string(&value).unwrap();
        let inner = serialized.find("\"outer\":").unwrap();
        let after = &serialized[inner..];
        assert!(after.contains("\"a\":2"), "expected nested sort: {}", after);
        assert!(
            after.find("\"a\"").unwrap() < after.find("\"z\"").unwrap(),
            "nested keys not sorted: {}",
            serialized
        );
    }

    #[test]
    fn sort_inside_arrays() {
        let mut value = json!({
            "items": [
                { "z": 1, "a": 2 },
                { "y": 3, "b": 4 },
            ]
        });
        sort_object_keys_recursively(&mut value);
        let serialized = serde_json::to_string(&value).unwrap();
        // Both array elements should have keys in sorted order.
        assert!(serialized.contains("\"a\":2"));
        assert!(serialized.contains("\"b\":4"));
        // First object's `a` must come before its `z`.
        let first_a = serialized.find("\"a\":2").unwrap();
        let first_z = serialized.find("\"z\":1").unwrap();
        assert!(first_a < first_z);
    }

    #[test]
    fn ordering_is_idempotent() {
        let mut a = json!({
            "z": 1,
            "a": 2,
            "m": { "y": 3, "b": 4 }
        });
        sort_object_keys_recursively(&mut a);
        let mut b = a.clone();
        sort_object_keys_recursively(&mut b);
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }

    #[test]
    fn ordering_changes_serialization_for_unsorted_input() {
        let mut a = json!({ "z": 1, "a": 2 });
        let before = serde_json::to_string(&a).unwrap();
        sort_object_keys_recursively(&mut a);
        let after = serde_json::to_string(&a).unwrap();
        assert_ne!(before, after, "test invariant: input is unsorted");
    }
}
