// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Schema registry, looked up by canonical identifier.
//!
//! The registry starts empty. Every entry arrives at runtime via
//! [`register_schema`], invoked by `Runner::load_project` for each
//! `schemas:` entry in a loaded package's `streamlib.yaml`. Apps wire
//! the packages they need (`@tatolab/core` for the wire vocabulary,
//! `@tatolab/audio` / `@tatolab/h264` / etc. for domain processors);
//! `load_project` walks the dependency graph and registers each
//! package's schemas as it traverses.
//!
//! Canonical identifiers are stored unversioned:
//! - `@tatolab/core/VideoFrame`
//! - `@tatolab/audio/AudioMixerConfig`
//!
//! Lookups tolerate either the unversioned form or the versioned suffix
//! `@MAJOR.MINOR.PATCH`; the version is stripped before comparing.

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

#[cfg(test)]
mod integration_tests;

/// Runtime schema registry. Populated by [`register_schema`] (called
/// from `Runner::load_project` for each loaded package's `schemas:`
/// entries). Empty until something registers.
static SCHEMA_REGISTRY: LazyLock<RwLock<HashMap<String, Arc<str>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Register a schema's YAML body under its canonical identifier. Last
/// write wins. Idempotent for identical bodies.
pub fn register_schema(canonical_id: impl Into<String>, body: impl Into<Arc<str>>) {
    let canonical = canonical_id.into();
    let body = body.into();
    let mut guard = SCHEMA_REGISTRY.write();
    guard.insert(canonical, body);
}

/// Get the schema's YAML body for a canonical identifier.
///
/// Accepts both unversioned (`@tatolab/core/VideoFrame`) and versioned
/// (`@tatolab/core/VideoFrame@1.0.0`) forms; the version suffix is
/// stripped before lookup.
pub fn get_embedded_schema_definition(name: &str) -> Option<Arc<str>> {
    let canonical = strip_semver_suffix(name);
    SCHEMA_REGISTRY.read().get(canonical).cloned()
}

/// Resolve `max_payload_bytes` from a structured port-schema spec.
/// Renders the spec to its canonical lookup form, reads the embedded
/// schema YAML, and returns the declared bound. Returns the iceoryx2
/// default for `Any` and for unknown schemas.
pub fn max_payload_bytes_for_port_spec(
    schema_spec: &streamlib_processor_schema::PortSchemaSpec,
) -> usize {
    use crate::iceoryx2::MAX_PAYLOAD_SIZE;
    let canonical = schema_spec.to_string();
    if let Some(yaml) = get_embedded_schema_definition(&canonical) {
        if let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(&yaml) {
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

/// List every registered schema's canonical identifier (unversioned).
/// Sorted alphabetically so consumers (api-server) get diff-stable
/// output across processes.
pub fn list_embedded_schema_names() -> Vec<String> {
    let mut names: Vec<String> = SCHEMA_REGISTRY.read().keys().cloned().collect();
    names.sort();
    names
}

/// Strip a trailing `@MAJOR.MINOR.PATCH` suffix from an identifier. The
/// leading `@` of `@org/...` identifiers is *not* stripped — this only
/// fires when the last `@` is followed by a dotted-digits semver.
///
/// Examples:
/// - `@tatolab/core/VideoFrame@1.0.0` → `@tatolab/core/VideoFrame`
/// - `@tatolab/core/VideoFrame` → unchanged
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
pub(crate) mod test_support {
    //! Test helpers: register the `@tatolab/core` wire vocabulary
    //! against the live registry so consumers exercising VideoFrame /
    //! AudioFrame / EncodedVideoFrame lookups can run without spinning
    //! up a full `Runner::load_project` chain. Mirrors what
    //! `Runner::load_project(packages/core)` would do in production.
    //!
    //! Schemas are embedded via `include_str!` at compile time — the
    //! engine doesn't depend on `@tatolab/core` as a Cargo crate, the
    //! linkage is through `streamlib.yaml` and the schemas live on disk
    //! at the workspace-relative path below.

    use super::register_schema;
    use std::sync::Once;

    const AUDIO_FRAME_YAML: &str =
        include_str!("../../../../../packages/core/schemas/audio_frame.yaml");
    const ENCODED_AUDIO_FRAME_YAML: &str =
        include_str!("../../../../../packages/core/schemas/encoded_audio_frame.yaml");
    const ENCODED_VIDEO_FRAME_YAML: &str =
        include_str!("../../../../../packages/core/schemas/encoded_video_frame.yaml");
    const VIDEO_FRAME_YAML: &str =
        include_str!("../../../../../packages/core/schemas/video_frame.yaml");

    /// Register the four wire-vocabulary schemas. Idempotent across
    /// tests in the same process.
    pub fn register_core_wire_vocabulary() {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            register_schema("@tatolab/core/AudioFrame", AUDIO_FRAME_YAML);
            register_schema("@tatolab/core/EncodedAudioFrame", ENCODED_AUDIO_FRAME_YAML);
            register_schema(
                "@tatolab/core/EncodedVideoFrame",
                ENCODED_VIDEO_FRAME_YAML,
            );
            register_schema("@tatolab/core/VideoFrame", VIDEO_FRAME_YAML);
        });
    }
}

#[cfg(test)]
mod tests {
    //! Tests mutate the global `SCHEMA_REGISTRY` and do NOT clean up
    //! after themselves — every registration uses a canonical id
    //! unique to its test. The `register_core_wire_vocabulary()` setup
    //! helper is `Once`-guarded, so wire-vocabulary entries are
    //! registered at most once per test process.
    use super::*;
    use streamlib_idents::{Org, Package, SchemaIdent, SemVer, TypeName};
    use streamlib_processor_schema::PortSchemaSpec;

    /// Construct a `PortSchemaSpec::Specific` for a `@tatolab/core/<Type>` lookup.
    fn core_spec(type_name: &str) -> PortSchemaSpec {
        PortSchemaSpec::Specific(SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("core").unwrap(),
            TypeName::new(type_name).unwrap(),
            SemVer::new(1, 0, 0),
        ))
    }

    #[test]
    fn lookup_finds_wire_vocabulary_via_new_identifier() {
        test_support::register_core_wire_vocabulary();
        let yaml = get_embedded_schema_definition("@tatolab/core/VideoFrame");
        assert!(
            yaml.is_some(),
            "wire vocabulary VideoFrame must be registered before lookup"
        );
        assert!(
            yaml.unwrap().contains("metadata"),
            "registered schema body should contain its metadata block"
        );
    }

    #[test]
    fn lookup_strips_version_suffix() {
        test_support::register_core_wire_vocabulary();
        let unversioned = get_embedded_schema_definition("@tatolab/core/AudioFrame");
        let versioned = get_embedded_schema_definition("@tatolab/core/AudioFrame@1.0.0");
        assert!(unversioned.is_some());
        assert_eq!(unversioned.as_deref(), versioned.as_deref());
    }

    #[test]
    fn lookup_returns_none_for_unknown_schema() {
        assert!(get_embedded_schema_definition("@nonexistent/pkg/Type").is_none());
        assert!(get_embedded_schema_definition("@nonexistent/pkg/Type@1.0.0").is_none());
    }

    #[test]
    fn registry_starts_empty_until_explicit_registration() {
        // The engine's `streamlib.yaml` declares `@tatolab/escalate/*`
        // as an External dep, so a resurrected `EMBEDDED_SCHEMAS` const
        // walking the manifest at build time would seed
        // `@tatolab/escalate/EscalateRequest` into the registry.
        // No test in this crate's test binary registers it (the
        // wire-vocabulary setup helper only covers `@tatolab/core/*`,
        // and the `load_project` regression tests build fresh tempdir
        // packages with no `@tatolab/escalate` dep). So if this lookup
        // ever returns Some, someone has reintroduced a build-time
        // seeding path.
        assert!(
            get_embedded_schema_definition("@tatolab/escalate/EscalateRequest").is_none(),
            "registry must start empty — `@tatolab/escalate/EscalateRequest` would only \
             be present if a build-time seed path were reintroduced. Reverting the \
             `EMBEDDED_SCHEMAS` const + `generate_embedded_schemas_table` deletion \
             would make this test fail."
        );
    }

    #[test]
    fn list_is_sorted() {
        // Register at least one schema so the list has content; the
        // sort property must hold regardless of registration order.
        register_schema(
            "@tatolab/test_list_sorted_b/Beta",
            "metadata:\n  type: Beta\n",
        );
        register_schema(
            "@tatolab/test_list_sorted_a/Alpha",
            "metadata:\n  type: Alpha\n",
        );
        let names = list_embedded_schema_names();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "list_embedded_schema_names must return sorted output");
    }

    #[test]
    fn no_duplicate_names() {
        let names = list_embedded_schema_names();
        let unique: std::collections::HashSet<&String> = names.iter().collect();
        assert_eq!(
            names.len(),
            unique.len(),
            "duplicate canonical identifier — fix the upstream registration"
        );
    }

    #[test]
    fn max_payload_bytes_returns_default_for_unknown() {
        use crate::iceoryx2::MAX_PAYLOAD_SIZE;
        // A PortSchemaSpec that won't resolve in the registry — Specific
        // form, fully structured, but the package isn't registered.
        let spec = PortSchemaSpec::Specific(SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("does-not-exist").unwrap(),
            TypeName::new("Nothing").unwrap(),
            SemVer::new(1, 0, 0),
        ));
        assert_eq!(max_payload_bytes_for_port_spec(&spec), MAX_PAYLOAD_SIZE as usize);
    }

    #[test]
    fn max_payload_bytes_resolves_video_frame() {
        test_support::register_core_wire_vocabulary();
        let spec = core_spec("VideoFrame");
        let bytes = max_payload_bytes_for_port_spec(&spec);
        assert!(bytes >= 65536, "VideoFrame should declare a generous payload bound");
    }

    #[test]
    fn max_payload_bytes_any_returns_default() {
        use crate::iceoryx2::MAX_PAYLOAD_SIZE;
        assert_eq!(
            max_payload_bytes_for_port_spec(&PortSchemaSpec::Any),
            MAX_PAYLOAD_SIZE as usize
        );
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
    fn register_schema_inserts_runtime_entry() {
        // Use a unique key the test owns end-to-end.
        let canonical = "@tatolab/test-register-inserts/RuntimeOnlyType";
        let body = "metadata:\n  type: RuntimeOnlyType\n  max_payload_bytes: 4096\n";

        assert!(get_embedded_schema_definition(canonical).is_none());

        register_schema(canonical, body);

        let got = get_embedded_schema_definition(canonical)
            .expect("registered schema must resolve");
        assert!(got.contains("RuntimeOnlyType"));

        let spec = PortSchemaSpec::Specific(SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("test-register-inserts").unwrap(),
            TypeName::new("RuntimeOnlyType").unwrap(),
            SemVer::new(1, 0, 0),
        ));
        assert_eq!(max_payload_bytes_for_port_spec(&spec), 4096);

        assert!(list_embedded_schema_names().iter().any(|n| n == canonical));
    }

    #[test]
    fn register_schema_overwrite_is_last_write_wins() {
        let canonical = "@tatolab/test-register-overwrite/RewrittenType";
        let spec = PortSchemaSpec::Specific(SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("test-register-overwrite").unwrap(),
            TypeName::new("RewrittenType").unwrap(),
            SemVer::new(1, 0, 0),
        ));
        register_schema(
            canonical,
            "metadata:\n  type: RewrittenType\n  max_payload_bytes: 1024\n",
        );
        assert_eq!(max_payload_bytes_for_port_spec(&spec), 1024);
        register_schema(
            canonical,
            "metadata:\n  type: RewrittenType\n  max_payload_bytes: 2048\n",
        );
        assert_eq!(max_payload_bytes_for_port_spec(&spec), 2048);
    }

    #[test]
    fn register_schema_versioned_lookup_strips_to_canonical() {
        let canonical = "@tatolab/test-register-versioned/StripsVersion";
        register_schema(canonical, "metadata:\n  type: StripsVersion\n");
        assert!(get_embedded_schema_definition(&format!("{canonical}@1.0.0")).is_some());
    }
}
