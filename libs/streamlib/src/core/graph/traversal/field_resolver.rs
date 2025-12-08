// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Field resolution for unified property graph + ECS queries.
//!
//! Provides the ability to resolve field paths (e.g., "metrics.throughput_fps")
//! to JSON values, combining data from both the property graph and ECS components.

use serde_json::Value as JsonValue;

use crate::core::graph::ProcessorId;
use crate::core::links::LinkUniqueId;

/// Resolves field paths to JSON values for processors and links.
///
/// Field paths use dot notation matching the `to_json()` output structure.
/// For example:
/// - Processor: `"type"`, `"state"`, `"metrics.throughput_fps"`, `"config.bitrate"`
/// - Link: `"from.processor"`, `"type_info.capacity"`, `"buffer.fill_level"`
pub trait FieldResolver {
    /// Resolve a field path for a processor.
    ///
    /// Returns `None` if the processor doesn't exist or the field path is invalid.
    fn resolve_processor_field(&self, processor_id: &ProcessorId, path: &str) -> Option<JsonValue>;

    /// Resolve a field path for a link.
    ///
    /// Returns `None` if the link doesn't exist or the field path is invalid.
    fn resolve_link_field(&self, link_id: &LinkUniqueId, path: &str) -> Option<JsonValue>;

    /// Get the full JSON representation of a processor.
    ///
    /// Used for complex predicates that need access to multiple fields.
    fn processor_to_json(&self, processor_id: &ProcessorId) -> Option<JsonValue>;

    /// Get the full JSON representation of a link.
    ///
    /// Used for complex predicates that need access to multiple fields.
    fn link_to_json(&self, link_id: &LinkUniqueId) -> Option<JsonValue>;
}

/// Navigate a JSON value by dot-separated path.
///
/// Supports both object field access and array indexing:
/// - `"foo.bar"` -> `value["foo"]["bar"]`
/// - `"items.0.name"` -> `value["items"][0]["name"]`
pub fn resolve_json_path(value: &JsonValue, path: &str) -> Option<JsonValue> {
    if path.is_empty() {
        return Some(value.clone());
    }

    let mut current = value;

    for segment in path.split('.') {
        current = if let Ok(index) = segment.parse::<usize>() {
            // Array index
            current.get(index)?
        } else {
            // Object field
            current.get(segment)?
        };
    }

    Some(current.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_resolve_json_path_simple() {
        let value = json!({
            "type": "CameraProcessor",
            "state": "Running"
        });

        assert_eq!(
            resolve_json_path(&value, "type"),
            Some(json!("CameraProcessor"))
        );
        assert_eq!(resolve_json_path(&value, "state"), Some(json!("Running")));
        assert_eq!(resolve_json_path(&value, "missing"), None);
    }

    #[test]
    fn test_resolve_json_path_nested() {
        let value = json!({
            "metrics": {
                "throughput_fps": 60.0,
                "latency_p50_ms": 5.2
            }
        });

        assert_eq!(
            resolve_json_path(&value, "metrics.throughput_fps"),
            Some(json!(60.0))
        );
        assert_eq!(
            resolve_json_path(&value, "metrics.latency_p50_ms"),
            Some(json!(5.2))
        );
        assert_eq!(resolve_json_path(&value, "metrics.missing"), None);
    }

    #[test]
    fn test_resolve_json_path_array() {
        let value = json!({
            "items": [
                {"name": "first"},
                {"name": "second"}
            ]
        });

        assert_eq!(
            resolve_json_path(&value, "items.0.name"),
            Some(json!("first"))
        );
        assert_eq!(
            resolve_json_path(&value, "items.1.name"),
            Some(json!("second"))
        );
        assert_eq!(resolve_json_path(&value, "items.2.name"), None);
    }

    #[test]
    fn test_resolve_json_path_empty() {
        let value = json!({"foo": "bar"});
        assert_eq!(resolve_json_path(&value, ""), Some(value.clone()));
    }
}
