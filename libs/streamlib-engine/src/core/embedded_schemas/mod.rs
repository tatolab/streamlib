// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Schema registry, looked up by canonical identifier.
//!
//! The registry starts empty. Every entry arrives at runtime via
//! [`register_schema`], invoked by `Runner::add_module` for each
//! `schemas:` entry in a loaded package's `streamlib.yaml`. Apps wire
//! the packages they need (`@tatolab/core` for the wire vocabulary,
//! `@tatolab/audio` / `@tatolab/h264` / etc. for domain processors);
//! the module loader walks the dependency graph and registers each
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
use std::ffi::c_void;
use std::sync::{Arc, LazyLock};

#[cfg(test)]
mod integration_tests;

/// Canonical-id → YAML-body store backing the runtime schema registry.
pub type SchemaRegistryStorage = RwLock<HashMap<String, Arc<str>>>;

/// Process-wide schema registry.
///
/// **Per-loaded-artifact instance.** Same shape as [`crate::core::pubsub::PUBSUB`]
/// — each linked copy of streamlib-engine has its own
/// `LazyLock<SchemaRegistryStorage>`. Plugin ABI bridging happens
/// inside the public functions below: when this artifact is a plugin
/// cdylib whose `install_host_services` has run,
/// [`register_schema`] and [`get_embedded_schema_definition`] route
/// through the host's `schema_register` / `schema_lookup` fn
/// pointers; otherwise they read/write the local artifact's instance
/// directly.
static SCHEMA_REGISTRY: LazyLock<SchemaRegistryStorage> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Register a schema's YAML body under its canonical identifier. Last
/// write wins. Idempotent for identical bodies.
///
/// In a plugin cdylib whose `install_host_services` has run, forwards
/// to the host's `schema_register` callback so registrations from
/// cdylib code land in the host's registry (where every consumer
/// reads).
pub fn register_schema(canonical_id: impl Into<String>, body: impl Into<Arc<str>>) {
    let canonical = canonical_id.into();
    let body = body.into();
    if let Some(cbs) = crate::core::plugin::host_services::host_callbacks() {
        let yaml: &str = &body;
        unsafe {
            (cbs.schema_register)(
                cbs.host,
                canonical.as_ptr(),
                canonical.len(),
                yaml.as_ptr(),
                yaml.len(),
            );
        }
        return;
    }
    let mut guard = SCHEMA_REGISTRY.write();
    guard.insert(canonical, body);
}

/// Get the schema's YAML body for a canonical identifier.
///
/// Accepts both unversioned (`@tatolab/core/VideoFrame`) and versioned
/// (`@tatolab/core/VideoFrame@1.0.0`) forms; the version suffix is
/// stripped before lookup.
///
/// In a plugin cdylib whose `install_host_services` has run, routes
/// through the host's `schema_lookup` callback — the host invokes a
/// cdylib-provided result callback with the yaml bytes (or null on
/// miss). The bytes are copied into an owned `Arc<str>` before the
/// host's call returns; the borrow doesn't outlive the call.
pub fn get_embedded_schema_definition(name: &str) -> Option<Arc<str>> {
    let canonical = strip_semver_suffix(name);
    if let Some(cbs) = crate::core::plugin::host_services::host_callbacks() {
        let mut captured: Option<Arc<str>> = None;
        extern "C" fn capture(userdata: *mut c_void, yaml_ptr: *const u8, yaml_len: usize) {
            if yaml_ptr.is_null() || yaml_len == 0 {
                return;
            }
            // SAFETY: caller (host) guarantees the bytes are valid
            // UTF-8 for the duration of this call. We copy into an
            // owned String before returning, so the borrow doesn't
            // outlive the call.
            let bytes = unsafe { std::slice::from_raw_parts(yaml_ptr, yaml_len) };
            let s: Arc<str> = std::str::from_utf8(bytes)
                .map(Into::into)
                .unwrap_or_else(|_| Arc::<str>::from(""));
            let target = unsafe { &mut *(userdata as *mut Option<Arc<str>>) };
            *target = Some(s);
        }
        unsafe {
            (cbs.schema_lookup)(
                cbs.host,
                canonical.as_ptr(),
                canonical.len(),
                capture,
                &mut captured as *mut _ as *mut c_void,
            );
        }
        return captured;
    }
    SCHEMA_REGISTRY.read().get(canonical).cloned()
}

/// Resolve `max_payload_bytes` from a structured port-schema spec.
///
/// Returns the iceoryx2 default for `Any` (legitimate wildcard) and for
/// registered schemas that don't declare `metadata.max_payload_bytes`.
/// Returns [`Error::Configuration`] when a [`PortSchemaSpec::Specific`]
/// (or [`PortSchemaSpec::Named`]) refers to a schema absent from the
/// runtime registry — the actionable shape catches the "forgot
/// `runtime.add_module(...)`" footgun at wire time rather than at
/// first-frame `ExceedsMaxLoanSize`.
///
/// [`Error::Configuration`]: crate::core::error::Error::Configuration
/// [`PortSchemaSpec::Specific`]: streamlib_processor_schema::PortSchemaSpec::Specific
/// [`PortSchemaSpec::Named`]: streamlib_processor_schema::PortSchemaSpec::Named
pub fn max_payload_bytes_for_port_spec(
    schema_spec: &streamlib_processor_schema::PortSchemaSpec,
) -> crate::core::error::Result<usize> {
    use crate::iceoryx2::MAX_PAYLOAD_SIZE;
    resolve_metadata_u64_for_port_spec(schema_spec, "max_payload_bytes")
        .map(|opt| opt.unwrap_or(MAX_PAYLOAD_SIZE as usize))
}

/// Resolve the iceoryx2 ring depth (slot count) for a port's wire schema
/// from `metadata.max_queued_messages`.
///
/// Returns [`DEFAULT_MAX_QUEUED_MESSAGES`] for `Any` (legitimate wildcard)
/// and for registered schemas that don't declare the field. Returns
/// [`Error::Configuration`] when the spec refers to a schema absent from
/// the runtime registry — the actionable shape catches the "forgot
/// `runtime.add_module(...)`" footgun at wire time rather than as a
/// silently undersized ring dropping messages under burst load.
///
/// [`DEFAULT_MAX_QUEUED_MESSAGES`]: crate::iceoryx2::DEFAULT_MAX_QUEUED_MESSAGES
/// [`Error::Configuration`]: crate::core::error::Error::Configuration
pub fn max_queued_messages_for_port_spec(
    schema_spec: &streamlib_processor_schema::PortSchemaSpec,
) -> crate::core::error::Result<usize> {
    use crate::iceoryx2::DEFAULT_MAX_QUEUED_MESSAGES;
    resolve_metadata_u64_for_port_spec(schema_spec, "max_queued_messages")
        .map(|opt| opt.unwrap_or(DEFAULT_MAX_QUEUED_MESSAGES))
}

/// Resolve the producer-side overflow policy for a destination input port.
///
/// Looks up the destination processor in [`PROCESSOR_REGISTRY`] and reads
/// the named input port's declared `overflow` field. Falls back to
/// [`Overflow::default()`] (`DropOldest` — the engine-wide realtime
/// invariant) when:
///
/// - The destination processor type isn't registered (e.g. an in-tree
///   compiler-op site that fires before registration completes — should
///   never happen for a Wired link but the conservative fallback is
///   correct).
/// - The named port doesn't exist on the destination (shape-mismatched
///   wiring; the compiler's other validators surface this distinctly).
/// - The port exists but no `overflow:` is declared (the common case).
///
/// Returns [`Error::Configuration`] when the declared string is non-empty
/// but unrecognized — typo at the manifest level is a wire-time error,
/// not a silent default substitution.
///
/// [`PROCESSOR_REGISTRY`]: crate::core::processors::PROCESSOR_REGISTRY
/// [`Overflow::default()`]: crate::iceoryx2::Overflow::default
/// [`Error::Configuration`]: crate::core::error::Error::Configuration
pub fn overflow_for_input_port(
    processor_type: &streamlib_idents::SchemaIdent,
    port_name: &str,
) -> crate::core::error::Result<crate::iceoryx2::Overflow> {
    use crate::iceoryx2::Overflow;

    let Some((inputs, _outputs)) =
        crate::core::processors::PROCESSOR_REGISTRY.port_info(processor_type)
    else {
        return Ok(Overflow::default());
    };
    let Some(port) = inputs.iter().find(|p| p.name == port_name) else {
        return Ok(Overflow::default());
    };
    let Some(declared) = port.overflow.as_deref() else {
        return Ok(Overflow::default());
    };
    Overflow::from_manifest_str(declared).map_err(|err| {
        crate::core::error::Error::Configuration(format!(
            "input port '{}' on '{}' declared {} — manifest must use one of \
             'drop_oldest' or 'block'.",
            port_name, processor_type, err
        ))
    })
}

/// Shared lookup helper for both port-spec metadata resolvers.
///
/// `Any` → `Ok(None)` (caller substitutes default).
/// `Specific` / `Named` with registry hit → `Ok(Some(value))` if the
/// declared `metadata.<field>` parses as a `u64`, else `Ok(None)`
/// (caller substitutes default — registered schema chose not to
/// constrain).
/// `Specific` / `Named` with registry miss → `Err(Configuration(...))`
/// naming the missing canonical id and pointing the developer at
/// `runtime.add_module(...)`.
fn resolve_metadata_u64_for_port_spec(
    schema_spec: &streamlib_processor_schema::PortSchemaSpec,
    field: &str,
) -> crate::core::error::Result<Option<usize>> {
    if matches!(schema_spec, streamlib_processor_schema::PortSchemaSpec::Any) {
        return Ok(None);
    }
    let canonical = schema_spec.to_string();
    let yaml = get_embedded_schema_definition(&canonical).ok_or_else(|| {
        crate::core::error::Error::Configuration(format!(
            "schema '{canonical}' referenced by a port spec but not in the \
             runtime schema registry — did you forget to call \
             `runtime.add_module(...)` for the package providing it? \
             (Use `list_embedded_schema_names()` to inspect what's currently \
             registered.)"
        ))
    })?;
    let value: serde_yaml::Value = match serde_yaml::from_str(&yaml) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let declared = value
        .get("metadata")
        .and_then(|m| m.get(field))
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    Ok(declared)
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

/// Whether `s` is a canonical `SemVer` string (`X.Y.Z`, optionally with a
/// `-dev.N` / `-rc.N` prerelease). Delegates to the canonical parser so the
/// suffix-stripping grammar stays in lock-step with `streamlib_idents::SemVer`.
fn is_semver(s: &str) -> bool {
    s.parse::<streamlib_idents::SemVer>().is_ok()
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Synthetic in-test schema fixtures for exercising the registry +
    //! metadata-resolver mechanics.
    //!
    //! The engine registry starts empty and learns every real schema only
    //! at runtime via `Runner::add_module`. These tests therefore register
    //! their own synthetic schemas under a neutral `@test/wire/*` namespace
    //! rather than reaching into any package's real schema files — the
    //! engine has no compile-time knowledge of `@tatolab/core`,
    //! `@tatolab/mavlink`, or any other package's wire vocabulary. Locking a
    //! package's own declarations (e.g. core's payload bounds, mavlink's
    //! queue depth) belongs in that package's tests, not the engine's.

    use super::register_schema;
    use std::sync::Once;

    /// Synthetic "small" wire schema — 128 KiB payload bound. Deliberately
    /// NOT the iceoryx2 default ([`MAX_PAYLOAD_SIZE`] = 64 KiB) so that
    /// asserting the resolved value distinguishes "read the declared
    /// metadata" from "fell back to the default".
    ///
    /// [`MAX_PAYLOAD_SIZE`]: crate::iceoryx2::MAX_PAYLOAD_SIZE
    pub const SMALL_FRAME_ID: &str = "@test/wire/SmallFrame";
    /// Declared `max_payload_bytes` for [`SMALL_FRAME_ID`] (128 KiB).
    pub const SMALL_FRAME_MAX_PAYLOAD_BYTES: usize = 131072;
    /// Synthetic "large" wire schema — 16 MiB payload bound, sized for the
    /// 256 KiB publish/subscribe roundtrip tests.
    pub const LARGE_FRAME_ID: &str = "@test/wire/LargeFrame";
    /// Declared `max_payload_bytes` for [`LARGE_FRAME_ID`] (16 MiB).
    pub const LARGE_FRAME_MAX_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;

    /// Register the synthetic wire schemas. Idempotent across tests in the
    /// same process.
    pub fn register_test_wire_vocabulary() {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            register_schema(
                SMALL_FRAME_ID,
                "metadata:\n  type: SmallFrame\n  max_payload_bytes: 131072\n  max_queued_messages: 32\n",
            );
            register_schema(
                LARGE_FRAME_ID,
                "metadata:\n  type: LargeFrame\n  max_payload_bytes: 16777216\n  max_queued_messages: 16\n",
            );
        });
    }
}

#[cfg(test)]
mod tests {
    //! Tests mutate the global `SCHEMA_REGISTRY` and do NOT clean up
    //! after themselves — every registration uses a canonical id
    //! unique to its test. The `register_test_wire_vocabulary()` setup
    //! helper is `Once`-guarded, so the synthetic fixtures are registered
    //! at most once per test process.
    use super::*;
    use streamlib_idents::{Org, Package, SchemaIdent, SemVer, TypeName};
    use streamlib_processor_schema::PortSchemaSpec;

    /// Construct a `PortSchemaSpec::Specific` for a synthetic
    /// `@test/wire/<Type>` lookup.
    fn test_wire_spec(type_name: &str) -> PortSchemaSpec {
        PortSchemaSpec::Specific(SchemaIdent::new(
            Org::new("test").unwrap(),
            Package::new("wire").unwrap(),
            TypeName::new(type_name).unwrap(),
            SemVer::new(1, 0, 0),
        ))
    }

    #[test]
    fn lookup_finds_wire_vocabulary_via_new_identifier() {
        test_support::register_test_wire_vocabulary();
        let yaml = get_embedded_schema_definition(test_support::SMALL_FRAME_ID);
        assert!(
            yaml.is_some(),
            "registered schema must be found before lookup"
        );
        assert!(
            yaml.unwrap().contains("metadata"),
            "registered schema body should contain its metadata block"
        );
    }

    #[test]
    fn lookup_strips_version_suffix() {
        test_support::register_test_wire_vocabulary();
        let unversioned = get_embedded_schema_definition(test_support::SMALL_FRAME_ID);
        let versioned =
            get_embedded_schema_definition(&format!("{}@1.0.0", test_support::SMALL_FRAME_ID));
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
        // synthetic wire-vocabulary setup helper only registers
        // `@test/wire/*` fixtures, and the `add_module` regression tests
        // build fresh tempdir packages with no `@tatolab/escalate` dep).
        // So if this lookup ever returns Some, someone has reintroduced a
        // build-time seeding path.
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
        assert_eq!(
            names, sorted,
            "list_embedded_schema_names must return sorted output"
        );
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

    /// A `Specific` spec referencing a schema absent from the registry
    /// must return a typed configuration error naming the missing
    /// canonical id and pointing at `runtime.add_module(...)`. Mentally
    /// reverting the resolver to silently fall back to `MAX_PAYLOAD_SIZE`
    /// will make this test fail.
    #[test]
    fn max_payload_bytes_errors_on_registry_miss_with_add_module_hint() {
        let spec = PortSchemaSpec::Specific(SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("does-not-exist-payload").unwrap(),
            TypeName::new("Nothing").unwrap(),
            SemVer::new(1, 0, 0),
        ));
        let err =
            max_payload_bytes_for_port_spec(&spec).expect_err("registry miss must surface as Err");
        let msg = err.to_string();
        assert!(
            msg.contains("@tatolab/does-not-exist-payload/Nothing"),
            "error must name the missing canonical id; got: {msg}"
        );
        assert!(
            msg.contains("add_module"),
            "error must point at `runtime.add_module(...)` as the fix; got: {msg}"
        );
        assert!(
            matches!(err, crate::core::error::Error::Configuration(_)),
            "registry miss must be Error::Configuration; got: {err:?}"
        );
    }

    #[test]
    fn max_payload_bytes_resolves_declared_value() {
        use crate::iceoryx2::MAX_PAYLOAD_SIZE;
        test_support::register_test_wire_vocabulary();
        let bytes = max_payload_bytes_for_port_spec(&test_wire_spec("SmallFrame")).unwrap();
        assert_eq!(
            bytes,
            test_support::SMALL_FRAME_MAX_PAYLOAD_BYTES,
            "resolver must return the schema's declared metadata.max_payload_bytes"
        );
        // Guard against a reverted resolver that ignores metadata and always
        // returns the default: the declared value is deliberately not the default.
        assert_ne!(
            test_support::SMALL_FRAME_MAX_PAYLOAD_BYTES,
            MAX_PAYLOAD_SIZE as usize,
            "test fixture must declare a non-default payload bound to be meaningful"
        );
    }

    #[test]
    fn max_payload_bytes_any_returns_default() {
        use crate::iceoryx2::MAX_PAYLOAD_SIZE;
        assert_eq!(
            max_payload_bytes_for_port_spec(&PortSchemaSpec::Any).unwrap(),
            MAX_PAYLOAD_SIZE as usize
        );
    }

    /// Symmetric registry-miss test for `max_queued_messages_for_port_spec`
    /// — the more insidious half of the helper pair (an undersized ring
    /// silently drops messages under burst load with no error visible to
    /// the application).
    #[test]
    fn max_queued_messages_errors_on_registry_miss_with_add_module_hint() {
        let spec = PortSchemaSpec::Specific(SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("does-not-exist-mqm").unwrap(),
            TypeName::new("Nothing").unwrap(),
            SemVer::new(1, 0, 0),
        ));
        let err = max_queued_messages_for_port_spec(&spec)
            .expect_err("registry miss must surface as Err");
        let msg = err.to_string();
        assert!(
            msg.contains("@tatolab/does-not-exist-mqm/Nothing"),
            "error must name the missing canonical id; got: {msg}"
        );
        assert!(
            msg.contains("add_module"),
            "error must point at `runtime.add_module(...)` as the fix; got: {msg}"
        );
        assert!(
            matches!(err, crate::core::error::Error::Configuration(_)),
            "registry miss must be Error::Configuration; got: {err:?}"
        );
    }

    #[test]
    fn max_queued_messages_any_returns_default() {
        use crate::iceoryx2::DEFAULT_MAX_QUEUED_MESSAGES;
        assert_eq!(
            max_queued_messages_for_port_spec(&PortSchemaSpec::Any).unwrap(),
            DEFAULT_MAX_QUEUED_MESSAGES
        );
    }

    /// A schema declaring `metadata.max_queued_messages` is honored by the
    /// resolver. Reverting the resolver's `metadata.max_queued_messages`
    /// branch to return the default will fail this test.
    #[test]
    fn max_queued_messages_resolves_declared_value() {
        let canonical = "@tatolab/test-mqm-resolves/HighRateStream";
        register_schema(
            canonical,
            "metadata:\n  type: HighRateStream\n  max_queued_messages: 128\n",
        );
        let spec = PortSchemaSpec::Specific(SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("test-mqm-resolves").unwrap(),
            TypeName::new("HighRateStream").unwrap(),
            SemVer::new(1, 0, 0),
        ));
        assert_eq!(max_queued_messages_for_port_spec(&spec).unwrap(), 128);
    }

    /// A registered schema without `metadata.max_queued_messages` falls
    /// back to the default — proves the resolver doesn't accidentally
    /// read some adjacent field, and proves the registry-miss-vs-field-
    /// absent split: a schema that IS registered but doesn't declare the
    /// field is a legitimate "use default" case (no error), whereas a
    /// registry miss is a configuration error.
    #[test]
    fn max_queued_messages_falls_back_when_field_absent() {
        use crate::iceoryx2::DEFAULT_MAX_QUEUED_MESSAGES;
        let canonical = "@tatolab/test-mqm-fallback/NoMqm";
        register_schema(
            canonical,
            "metadata:\n  type: NoMqm\n  max_payload_bytes: 4096\n",
        );
        let spec = PortSchemaSpec::Specific(SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("test-mqm-fallback").unwrap(),
            TypeName::new("NoMqm").unwrap(),
            SemVer::new(1, 0, 0),
        ));
        assert_eq!(
            max_queued_messages_for_port_spec(&spec).unwrap(),
            DEFAULT_MAX_QUEUED_MESSAGES
        );
    }

    /// Locks the default-fallback path: a processor type that isn't
    /// registered yields `Overflow::DropOldest` (the engine-wide
    /// realtime invariant). Mentally reverting the fallback to
    /// `Block` here would silently re-introduce producer-blocking
    /// for unregistered (defensively-handled) cases — fail loudly
    /// rather than ship that quietly.
    #[test]
    fn overflow_for_input_port_defaults_when_processor_unregistered() {
        use crate::iceoryx2::Overflow;
        let unknown = SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("does-not-exist-overflow").unwrap(),
            TypeName::new("Nothing").unwrap(),
            SemVer::new(1, 0, 0),
        );
        assert_eq!(
            overflow_for_input_port(&unknown, "video_in").unwrap(),
            Overflow::DropOldest
        );
    }

    /// Manifest-declared `overflow: "block"` on an input port is
    /// honored by the registry-side resolver. Built directly against
    /// the registry helpers used by `register_descriptor_only` — the
    /// subprocess registration path — so the assertion covers both
    /// host and subprocess wirings.
    #[test]
    fn overflow_for_input_port_resolves_block_declaration() {
        use crate::core::descriptors::{PortDescriptor, ProcessorDescriptor};
        use crate::core::processors::PROCESSOR_REGISTRY;
        use crate::iceoryx2::Overflow;

        let processor_type = SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("test-overflow-block").unwrap(),
            TypeName::new("BlockSink").unwrap(),
            SemVer::new(1, 0, 0),
        );
        let video_schema = PortSchemaSpec::Specific(SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("core").unwrap(),
            TypeName::new("VideoFrame").unwrap(),
            SemVer::new(1, 0, 0),
        ));
        let mut desc = ProcessorDescriptor::new(processor_type.clone(), "block-sink");
        desc.inputs.push(
            PortDescriptor::iceoryx2("video_in", "input", video_schema).with_overflow("block"),
        );
        PROCESSOR_REGISTRY
            .register_descriptor_only(desc)
            .expect("descriptor registration");

        assert_eq!(
            overflow_for_input_port(&processor_type, "video_in").unwrap(),
            Overflow::Block
        );
    }

    /// A registered input port without an explicit `overflow:`
    /// declaration falls back to the engine-wide default. Symmetric to
    /// the registry-miss test above — distinguishes "field absent" from
    /// "processor absent" so a future refactor can't conflate them.
    #[test]
    fn overflow_for_input_port_defaults_when_field_absent() {
        use crate::core::descriptors::{PortDescriptor, ProcessorDescriptor};
        use crate::core::processors::PROCESSOR_REGISTRY;
        use crate::iceoryx2::Overflow;

        let processor_type = SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("test-overflow-default").unwrap(),
            TypeName::new("DefaultSink").unwrap(),
            SemVer::new(1, 0, 0),
        );
        let video_schema = PortSchemaSpec::Specific(SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("core").unwrap(),
            TypeName::new("VideoFrame").unwrap(),
            SemVer::new(1, 0, 0),
        ));
        let mut desc = ProcessorDescriptor::new(processor_type.clone(), "default-sink");
        desc.inputs
            .push(PortDescriptor::iceoryx2("video_in", "input", video_schema));
        PROCESSOR_REGISTRY
            .register_descriptor_only(desc)
            .expect("descriptor registration");

        assert_eq!(
            overflow_for_input_port(&processor_type, "video_in").unwrap(),
            Overflow::DropOldest
        );
    }

    /// A typo at the manifest level surfaces as a typed configuration
    /// error rather than a silent default fallback — wire-time rejection
    /// of bad declarations is the actionable shape.
    #[test]
    fn overflow_for_input_port_rejects_unknown_string() {
        use crate::core::descriptors::{PortDescriptor, ProcessorDescriptor};
        use crate::core::processors::PROCESSOR_REGISTRY;

        let processor_type = SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("test-overflow-typo").unwrap(),
            TypeName::new("TypoSink").unwrap(),
            SemVer::new(1, 0, 0),
        );
        let video_schema = PortSchemaSpec::Specific(SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("core").unwrap(),
            TypeName::new("VideoFrame").unwrap(),
            SemVer::new(1, 0, 0),
        ));
        let mut desc = ProcessorDescriptor::new(processor_type.clone(), "typo-sink");
        desc.inputs.push(
            PortDescriptor::iceoryx2("video_in", "input", video_schema)
                .with_overflow("drop-oldest"), // hyphen instead of underscore
        );
        PROCESSOR_REGISTRY
            .register_descriptor_only(desc)
            .expect("descriptor registration");

        let err = overflow_for_input_port(&processor_type, "video_in")
            .expect_err("unknown overflow string must error");
        let msg = err.to_string();
        assert!(
            msg.contains("drop_oldest"),
            "error must list valid values: {msg}"
        );
        assert!(msg.contains("block"), "error must list valid values: {msg}");
        assert!(
            matches!(err, crate::core::error::Error::Configuration(_)),
            "must be Configuration error: {err:?}"
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
        // Prerelease-suffixed idents strip cleanly now that `is_semver`
        // delegates to the canonical parser. Schema idents are
        // release-only by invariant, but the stripper stays robust regardless.
        assert_eq!(
            strip_semver_suffix("@tatolab/core/VideoFrame@0.4.33-dev.2"),
            "@tatolab/core/VideoFrame"
        );
        assert_eq!(
            strip_semver_suffix("@tatolab/core/VideoFrame@1.0.0-rc.1"),
            "@tatolab/core/VideoFrame"
        );
        // A non-semver trailing `@segment` is left intact.
        assert_eq!(
            strip_semver_suffix("@tatolab/core/VideoFrame@latest"),
            "@tatolab/core/VideoFrame@latest"
        );
    }

    #[test]
    fn register_schema_inserts_runtime_entry() {
        // Use a unique key the test owns end-to-end.
        let canonical = "@tatolab/test-register-inserts/RuntimeOnlyType";
        let body = "metadata:\n  type: RuntimeOnlyType\n  max_payload_bytes: 4096\n";

        assert!(get_embedded_schema_definition(canonical).is_none());

        register_schema(canonical, body);

        let got =
            get_embedded_schema_definition(canonical).expect("registered schema must resolve");
        assert!(got.contains("RuntimeOnlyType"));

        let spec = PortSchemaSpec::Specific(SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("test-register-inserts").unwrap(),
            TypeName::new("RuntimeOnlyType").unwrap(),
            SemVer::new(1, 0, 0),
        ));
        assert_eq!(max_payload_bytes_for_port_spec(&spec).unwrap(), 4096);

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
        assert_eq!(max_payload_bytes_for_port_spec(&spec).unwrap(), 1024);
        register_schema(
            canonical,
            "metadata:\n  type: RewrittenType\n  max_payload_bytes: 2048\n",
        );
        assert_eq!(max_payload_bytes_for_port_spec(&spec).unwrap(), 2048);
    }

    #[test]
    fn register_schema_versioned_lookup_strips_to_canonical() {
        let canonical = "@tatolab/test-register-versioned/StripsVersion";
        register_schema(canonical, "metadata:\n  type: StripsVersion\n");
        assert!(get_embedded_schema_definition(&format!("{canonical}@1.0.0")).is_some());
    }
}
