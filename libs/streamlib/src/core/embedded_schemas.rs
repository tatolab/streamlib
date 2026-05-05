// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Embedded JTD schema definitions, looked up by metadata name.
//!
//! The lookup table is generated at build time by `build.rs` from the
//! `schemas:` list in `streamlib.yaml` (replacing the hand-curated 21-arm
//! match that historically drifted against the on-disk schema set —
//! see #402).

include!(concat!(env!("OUT_DIR"), "/embedded_schemas_table.rs"));

/// Get the embedded JTD YAML definition for a built-in schema.
pub fn get_embedded_schema_definition(name: &str) -> Option<&'static str> {
    EMBEDDED_SCHEMAS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, body)| *body)
}

/// Extract `max_payload_bytes` from a schema's metadata section.
///
/// Strips any version suffix (e.g. `com.tatolab.audioframe@1.0.0` →
/// `com.tatolab.audioframe`) before lookup. Returns the iceoryx2 default if
/// the schema is unknown or has no declaration.
pub fn max_payload_bytes_for_schema(schema_name: &str) -> usize {
    use crate::iceoryx2::MAX_PAYLOAD_SIZE;
    let base_name = schema_name.split('@').next().unwrap_or(schema_name);
    if let Some(yaml) = get_embedded_schema_definition(base_name) {
        if let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(yaml) {
            if let Some(n) = value
                .get("metadata")
                .and_then(|m| m.get("max_payload_bytes"))
                .and_then(|v| v.as_u64())
            {
                return n as usize;
            }
        }
    }
    MAX_PAYLOAD_SIZE as usize
}

/// List all embedded schema names. Returns a fresh `Vec` keyed by
/// `metadata.name` for every schema declared in `streamlib.yaml`. Sorted
/// alphabetically so consumers (API server) get diff-stable output.
pub fn list_embedded_schema_names() -> Vec<&'static str> {
    EMBEDDED_SCHEMAS.iter().map(|(name, _)| *name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_table_is_populated() {
        // Build script must populate at least the wire vocabulary plus a
        // representative config schema. Specific count is intentionally
        // not asserted (volatile across schema additions, per
        // `feedback_no_brittle_numbers_in_docs`).
        assert!(
            !EMBEDDED_SCHEMAS.is_empty(),
            "build.rs must populate the embedded schema table from streamlib.yaml"
        );
    }

    #[test]
    fn lookup_finds_known_schema() {
        let yaml = get_embedded_schema_definition("com.tatolab.videoframe");
        assert!(
            yaml.is_some(),
            "videoframe wire vocabulary must be embedded"
        );
        assert!(
            yaml.unwrap().contains("metadata"),
            "embedded schema body should contain its metadata block"
        );
    }

    #[test]
    fn lookup_returns_none_for_unknown_schema() {
        assert!(get_embedded_schema_definition("does.not.exist").is_none());
    }

    #[test]
    fn list_is_sorted() {
        let names = list_embedded_schema_names();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "build.rs sorts entries — diff stability");
    }

    #[test]
    fn no_duplicate_names() {
        let names = list_embedded_schema_names();
        let unique: std::collections::HashSet<&&str> = names.iter().collect();
        assert_eq!(
            names.len(),
            unique.len(),
            "duplicate metadata.name across schemas — fix the streamlib.yaml `schemas:` list"
        );
    }

    #[test]
    fn max_payload_bytes_returns_default_for_unknown() {
        use crate::iceoryx2::MAX_PAYLOAD_SIZE;
        assert_eq!(
            max_payload_bytes_for_schema("does.not.exist"),
            MAX_PAYLOAD_SIZE as usize
        );
    }
}
