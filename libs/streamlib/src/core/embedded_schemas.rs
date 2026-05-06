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

/// List every embedded schema's canonical identifier (unversioned). Sorted
/// alphabetically so consumers (API server) get diff-stable output.
pub fn list_embedded_schema_names() -> Vec<&'static str> {
    EMBEDDED_SCHEMAS.iter().map(|(name, _)| *name).collect()
}

/// Structured segments for a schema identifier, derived at build time from
/// the resolver's package metadata. Returned by [`lookup_schema_ident_segments`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaIdentSegments {
    pub org: &'static str,
    pub package: &'static str,
    pub type_name: &'static str,
    pub version_major: u32,
    pub version_minor: u32,
    pub version_patch: u32,
}

/// Look up the structured segments for a schema, given its joined-versioned
/// identifier (e.g. `"@tatolab/core/VideoFrame@1.0.0"`).
///
/// This is the wire-boundary helper that lets producers convert a joined
/// `String` into a `SchemaIdentWire` without ever invoking a parser
/// (#401 phase 2 — Path C). The lookup table is populated at build time
/// from the resolver's structured `(org, package, type, version)` records,
/// so no string-splitting logic exists anywhere in the runtime path.
///
/// Returns `None` for:
/// - Legacy reverse-DNS schemas (e.g. `com.streamlib.h264_encoder.config@1.0.0`)
///   — they have no structured-segment representation by design.
/// - Unknown identifiers — the schema wasn't in `streamlib.yaml`'s dep graph.
///
/// The accepted input form is the **versioned** joined string. The unversioned
/// form is rejected (returns `None`) because the version is part of the wire
/// identity — a producer that doesn't know the version isn't ready to write
/// a fully-qualified wire frame.
pub fn lookup_schema_ident_segments(joined_versioned: &str) -> Option<SchemaIdentSegments> {
    EMBEDDED_SCHEMA_IDENT_SEGMENTS
        .binary_search_by(|(k, ..)| (*k).cmp(joined_versioned))
        .ok()
        .map(|i| {
            let (_key, org, package, type_name, major, minor, patch) =
                EMBEDDED_SCHEMA_IDENT_SEGMENTS[i];
            SchemaIdentSegments {
                org,
                package,
                type_name,
                version_major: major,
                version_minor: minor,
                version_patch: patch,
            }
        })
}

/// Errors returned when materializing a [`SchemaIdentWire`] from a joined
/// string at the wire boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaIdentWireBuildError {
    /// The joined identifier wasn't in the build-time embedded segment table.
    /// Either it's a legacy reverse-DNS schema (no structured representation
    /// by design) or it isn't part of `streamlib.yaml`'s dep graph at all.
    UnknownSchema { joined: String },
    /// The structured segments resolved cleanly from the table but exceeded
    /// the wire format's per-segment length bounds — propagated from
    /// [`SchemaIdentWire::from_segments`].
    Wire(crate::iceoryx2::SchemaIdentWireError),
}

impl std::fmt::Display for SchemaIdentWireBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownSchema { joined } => write!(
                f,
                "no structured segments registered for schema {joined:?} (legacy reverse-DNS or not in streamlib.yaml dep graph)"
            ),
            Self::Wire(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SchemaIdentWireBuildError {}

/// Materialize a [`SchemaIdentWire`] from a joined-versioned identifier, in
/// one step: looks up the structured segments, then constructs the wire
/// struct. The host-side adapter that wire-format producers in `streamlib`
/// use to bridge the `String`-shaped `PortDescriptor.schema` to the
/// structured wire bytes.
///
/// No parser runs anywhere — the segments come from the build-time table
/// populated by `build.rs` walking the resolver's structured metadata.
///
/// This adapter intentionally lives in the `streamlib` host crate (not on
/// `SchemaIdentWire` itself) so the cdylib dep graph (`streamlib-python-native`,
/// `streamlib-deno-native`) stays minimal — those crates receive structured
/// segments directly from the Surface 2 IPC envelope and call
/// `SchemaIdentWire::from_segments` without needing this lookup or
/// `streamlib-idents` as a dep.
pub fn schema_ident_wire_from_joined(
    joined_versioned: &str,
) -> Result<crate::iceoryx2::SchemaIdentWire, SchemaIdentWireBuildError> {
    let segs = lookup_schema_ident_segments(joined_versioned).ok_or_else(|| {
        SchemaIdentWireBuildError::UnknownSchema {
            joined: joined_versioned.to_string(),
        }
    })?;
    crate::iceoryx2::SchemaIdentWire::from_segments(
        segs.org,
        segs.package,
        segs.type_name,
        segs.version_major,
        segs.version_minor,
        segs.version_patch,
    )
    .map_err(SchemaIdentWireBuildError::Wire)
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

    #[test]
    fn segments_table_is_populated() {
        // Sanity: the resolver dep graph includes @tatolab/core, so the
        // segments table must contain at least the four wire types.
        assert!(
            !EMBEDDED_SCHEMA_IDENT_SEGMENTS.is_empty(),
            "build.rs must populate the segments table from new-shape schemas"
        );
    }

    #[test]
    fn segments_table_is_sorted() {
        // Binary search in lookup_schema_ident_segments depends on this
        // ordering; build.rs sorts before emit.
        let keys: Vec<&str> = EMBEDDED_SCHEMA_IDENT_SEGMENTS
            .iter()
            .map(|(k, ..)| *k)
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "build.rs sorts segment entries — diff stability");
    }

    #[test]
    fn segments_table_no_duplicates() {
        let keys: Vec<&str> = EMBEDDED_SCHEMA_IDENT_SEGMENTS
            .iter()
            .map(|(k, ..)| *k)
            .collect();
        let unique: std::collections::HashSet<&&str> = keys.iter().collect();
        assert_eq!(
            keys.len(),
            unique.len(),
            "duplicate joined-versioned key in segments table"
        );
    }

    #[test]
    fn lookup_schema_ident_segments_resolves_video_frame() {
        let segs = lookup_schema_ident_segments("@tatolab/core/VideoFrame@1.0.0");
        let segs = segs.expect("@tatolab/core/VideoFrame@1.0.0 must be in the segments table");
        assert_eq!(segs.org, "tatolab");
        assert_eq!(segs.package, "core");
        assert_eq!(segs.type_name, "VideoFrame");
        assert_eq!(segs.version_major, 1);
        assert_eq!(segs.version_minor, 0);
        assert_eq!(segs.version_patch, 0);
    }

    #[test]
    fn lookup_schema_ident_segments_resolves_all_core_wire_types() {
        // The four wire vocabulary types must all round-trip cleanly. If
        // any of these fail, the producer-side wire boundary will fall back
        // to default-zero segments and ship malformed frames.
        for (joined, expected_type) in [
            ("@tatolab/core/VideoFrame@1.0.0", "VideoFrame"),
            ("@tatolab/core/AudioFrame@1.0.0", "AudioFrame"),
            ("@tatolab/core/EncodedVideoFrame@1.0.0", "EncodedVideoFrame"),
            ("@tatolab/core/EncodedAudioFrame@1.0.0", "EncodedAudioFrame"),
        ] {
            let segs = lookup_schema_ident_segments(joined)
                .unwrap_or_else(|| panic!("missing segments for {joined}"));
            assert_eq!(segs.org, "tatolab", "{joined}");
            assert_eq!(segs.package, "core", "{joined}");
            assert_eq!(segs.type_name, expected_type, "{joined}");
        }
    }

    #[test]
    fn lookup_schema_ident_segments_returns_none_for_unversioned() {
        // Unversioned form is deliberately rejected — the wire boundary
        // requires a fully-qualified joined-versioned identifier.
        assert!(lookup_schema_ident_segments("@tatolab/core/VideoFrame").is_none());
    }

    #[test]
    fn lookup_schema_ident_segments_returns_none_for_legacy_reverse_dns() {
        // Legacy reverse-DNS schemas have no structured segment representation.
        assert!(
            lookup_schema_ident_segments("com.streamlib.h264_encoder.config@1.0.0").is_none()
        );
    }

    #[test]
    fn lookup_schema_ident_segments_returns_none_for_unknown() {
        assert!(lookup_schema_ident_segments("@nonexistent/pkg/Type@1.0.0").is_none());
        assert!(lookup_schema_ident_segments("garbage").is_none());
    }
}
