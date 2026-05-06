// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Embedded JTD schema definitions, looked up by canonical identifier.
//!
//! The lookup table is generated at build time by `build.rs` walking
//! `streamlib.yaml`'s `schemas:` list AND its resolved `dependencies:`
//! graph (#401). Canonical identifiers are stored unversioned:
//! - Legacy reverse-DNS: `com.streamlib.h264_encoder.config`
//! - New structured: `@tatolab/core/VideoFrame`
//!
//! Lookups tolerate either the unversioned form or the versioned suffix
//! `@MAJOR.MINOR.PATCH`; the version is stripped before comparing.

include!(concat!(env!("OUT_DIR"), "/embedded_schemas_table.rs"));

/// Get the embedded JTD YAML definition for a built-in schema.
///
/// Accepts both unversioned (`@tatolab/core/VideoFrame`) and versioned
/// (`@tatolab/core/VideoFrame@1.0.0`) forms; the version suffix is stripped
/// before lookup.
pub fn get_embedded_schema_definition(name: &str) -> Option<&'static str> {
    let canonical = strip_semver_suffix(name);
    EMBEDDED_SCHEMAS
        .iter()
        .find(|(n, _)| *n == canonical)
        .map(|(_, body)| *body)
}

/// Extract `max_payload_bytes` from a schema's metadata section.
///
/// Tolerates both unversioned and versioned forms. Returns the iceoryx2
/// default when the schema is unknown or doesn't declare a payload bound.
pub fn max_payload_bytes_for_schema(schema_name: &str) -> usize {
    use crate::iceoryx2::MAX_PAYLOAD_SIZE;
    if let Some(yaml) = get_embedded_schema_definition(schema_name) {
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

/// Resolve `max_payload_bytes` from a structured port-schema spec by
/// rendering it to its canonical lookup form via `Display`. Returns the
/// iceoryx2 default for `Any` and for unknown schemas.
pub fn max_payload_bytes_for_port_spec(
    schema_spec: &streamlib_processor_schema::PortSchemaSpec,
) -> usize {
    max_payload_bytes_for_schema(&schema_spec.to_string())
}

/// List every embedded schema's canonical identifier (unversioned). Sorted
/// alphabetically so consumers (API server) get diff-stable output.
pub fn list_embedded_schema_names() -> Vec<&'static str> {
    EMBEDDED_SCHEMAS.iter().map(|(name, _)| *name).collect()
}

/// Strip a trailing `@MAJOR.MINOR.PATCH` suffix from an identifier. The
/// leading `@` of `@org/...` identifiers is *not* stripped — this only
/// fires when the last `@` is followed by a dotted-digits semver.
///
/// Examples:
/// - `@tatolab/core/VideoFrame@1.0.0` → `@tatolab/core/VideoFrame`
/// - `@tatolab/core/VideoFrame` → unchanged
/// - `com.streamlib.h264_encoder.config@1.0.0` → `com.streamlib.h264_encoder.config`
/// - `com.streamlib.h264_encoder.config` → unchanged
pub(crate) fn strip_semver_suffix(name: &str) -> &str {
    if let Some(at_pos) = name.rfind('@') {
        let suffix = &name[at_pos + 1..];
        if is_semver(suffix) {
            return &name[..at_pos];
        }
    }
    name
}

fn is_semver(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_table_is_populated() {
        assert!(
            !EMBEDDED_SCHEMAS.is_empty(),
            "build.rs must populate the embedded schema table from streamlib.yaml"
        );
    }

    #[test]
    fn lookup_finds_wire_vocabulary_via_new_identifier() {
        let yaml = get_embedded_schema_definition("@tatolab/core/VideoFrame");
        assert!(
            yaml.is_some(),
            "wire vocabulary VideoFrame must be embedded under the new identifier"
        );
        assert!(
            yaml.unwrap().contains("metadata"),
            "embedded schema body should contain its metadata block"
        );
    }

    #[test]
    fn lookup_strips_version_suffix() {
        // Both unversioned and versioned forms must hit the same entry.
        let unversioned = get_embedded_schema_definition("@tatolab/core/AudioFrame");
        let versioned = get_embedded_schema_definition("@tatolab/core/AudioFrame@1.0.0");
        assert!(unversioned.is_some());
        assert_eq!(unversioned, versioned);
    }

    #[test]
    fn lookup_strips_legacy_version_suffix() {
        // Legacy reverse-DNS form still works.
        let unversioned = get_embedded_schema_definition("com.streamlib.h264_encoder.config");
        let versioned =
            get_embedded_schema_definition("com.streamlib.h264_encoder.config@1.0.0");
        assert!(unversioned.is_some());
        assert_eq!(unversioned, versioned);
    }

    #[test]
    fn lookup_returns_none_for_unknown_schema() {
        assert!(get_embedded_schema_definition("does.not.exist").is_none());
        assert!(get_embedded_schema_definition("@nonexistent/pkg/Type").is_none());
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
            "duplicate canonical identifier across schemas — fix streamlib.yaml or its deps"
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

    #[test]
    fn max_payload_bytes_resolves_video_frame_via_either_form() {
        // VideoFrame's schema declares max_payload_bytes: 65536. The
        // helper must return 65536 from both unversioned and versioned
        // forms (the bug class #401 fixed: leading `@` in the new
        // grammar broke `split('@').next()` parser).
        let unv = max_payload_bytes_for_schema("@tatolab/core/VideoFrame");
        let v = max_payload_bytes_for_schema("@tatolab/core/VideoFrame@1.0.0");
        assert_eq!(unv, v);
        assert!(unv >= 65536, "VideoFrame should declare a generous payload bound");
    }

    #[test]
    fn strip_semver_suffix_handles_new_grammar() {
        assert_eq!(
            strip_semver_suffix("@tatolab/core/VideoFrame@1.0.0"),
            "@tatolab/core/VideoFrame"
        );
        // The leading `@` of `@org/...` identifiers is NOT a version marker.
        assert_eq!(
            strip_semver_suffix("@tatolab/core/VideoFrame"),
            "@tatolab/core/VideoFrame"
        );
    }

    #[test]
    fn strip_semver_suffix_handles_legacy_grammar() {
        assert_eq!(
            strip_semver_suffix("com.tatolab.foo@1.0.0"),
            "com.tatolab.foo"
        );
        assert_eq!(strip_semver_suffix("com.tatolab.foo"), "com.tatolab.foo");
    }

}
