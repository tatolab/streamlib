// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Test for the ports-in-code `#[processor(...)]` attribute macro.
//!
//! The attribute is the single source of truth for identity, execution mode,
//! and ports — nothing is read from any file at expansion. This test exercises
//! the macro's typed-API surface (descriptor + ident + port markers) and
//! intentionally does not register the processor in the global
//! `PROCESSOR_REGISTRY`.

use streamlib_engine::core::GeneratedProcessor;
use streamlib_engine::core::{EmptyConfig, Result, RuntimeContextFullAccess};

// Define a simple processor. The macro emits the type, port markers,
// descriptor, and `schema_ident()` accessor — it never auto-registers.
#[streamlib::sdk::processor(
    "@tatolab/streamlib-engine/TestProcessor",
    execution = manual,
    input("video_in", any, delivery_profile = "latest"),
    output("video_out", any),
)]
pub struct TestProcessor;

// User implements the Processor trait on the generated Processor struct
impl streamlib_engine::ManualProcessor for TestProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // With iceoryx2 IPC, data is read via self.inputs.read("port_name")
        // and written via self.outputs.write("port_name", &data)
        // For this test, we just verify the structure exists
        Ok(())
    }
}

#[test]
fn test_module_structure_generated() {
    // Verify the module structure was generated correctly
    // TestProcessor::Processor should exist
    fn assert_processor_type<T: streamlib_engine::core::ManualProcessor>() {}
    assert_processor_type::<TestProcessor::Processor>();
}

#[test]
fn test_input_link_module_exists() {
    // Verify InputLink module has the expected port marker
    fn assert_input_marker<T: streamlib_engine::core::InputPortMarker>() {}
    assert_input_marker::<TestProcessor::InputLink::video_in>();
}

#[test]
fn test_output_link_module_exists() {
    // Verify OutputLink module has the expected port marker
    fn assert_output_marker<T: streamlib_engine::core::OutputPortMarker>() {}
    assert_output_marker::<TestProcessor::OutputLink::video_out>();
}

#[test]
fn test_port_marker_names() {
    // Verify port names are correct
    use streamlib_engine::core::{InputPortMarker, OutputPortMarker};

    assert_eq!(
        <TestProcessor::InputLink::video_in as InputPortMarker>::PORT_NAME,
        "video_in"
    );
    assert_eq!(
        <TestProcessor::OutputLink::video_out as OutputPortMarker>::PORT_NAME,
        "video_out"
    );
}

#[test]
fn test_processor_instantiation() {
    // Create processor from empty config
    let processor = TestProcessor::Processor::from_config(EmptyConfig).unwrap();

    // Verify it has the expected name from YAML schema
    assert_eq!(processor.name(), "TestProcessor");
}

#[test]
fn empty_config_is_a_tolerant_bag() {
    // config-as-bag: a no-config processor's `EmptyConfig` deserializes from
    // any named map, discarding unknown / forward-compat keys, and serializes
    // back as an empty named map. Mentally revert the custom EmptyConfig serde
    // impls and this fails (a unit struct rejects a map).
    let from_populated: EmptyConfig =
        serde_json::from_value(serde_json::json!({ "leftover": 1, "future": true })).unwrap();
    let processor = TestProcessor::Processor::from_config(from_populated).unwrap();
    assert_eq!(processor.name(), "TestProcessor");

    let from_empty: EmptyConfig = serde_json::from_value(serde_json::json!({})).unwrap();
    assert_eq!(
        serde_json::to_value(from_empty).unwrap(),
        serde_json::json!({})
    );
}

#[test]
fn test_processor_schema_ident_declared_in_attribute() {
    // The macro emits `Processor::schema_ident()` returning the structured
    // SchemaIdent parsed from the attribute's identity string. This locks the
    // codegen contract end-to-end: identity parse, structured const emit,
    // SchemaIdent::new + segment validators all wired through the generated
    // module.
    //
    // Reverting the attribute's identity parse (e.g. hardcoding org) would
    // flip this assertion — that's the regression the test guards.
    let ident = TestProcessor::schema_ident();
    assert_eq!(ident.org.as_str(), "tatolab");
    assert_eq!(ident.package.as_str(), "streamlib-engine");
    assert_eq!(ident.r#type.as_str(), "TestProcessor");
    // The version-free attribute grammar (#1409) synthesizes the 0.0.0
    // version-free sentinel — versions are derived at package-build time.
    assert_eq!(ident.version.major, 0);
    assert_eq!(ident.version.minor, 0);
    assert_eq!(ident.version.patch, 0);
}

// A bare processor with NO identity string. This test crate has no sibling
// streamlib.yaml and the macro reads no file — the identity is synthesized
// purely from the struct name as `@app/local/<StructName>@0.0.0`. This is the
// full macro-expansion proof of the named acceptance criterion (#1409): a bare
// crate with no streamlib.yaml compiles under @app/local.
#[streamlib::sdk::processor(execution = manual)]
pub struct BareAppLocalProcessor;

impl streamlib_engine::ManualProcessor for BareAppLocalProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

#[test]
fn test_bare_processor_synthesizes_app_local_identity() {
    // No identity string in the attribute and no file read: the emitted
    // `schema_ident()` is `@app/local/<StructName>@0.0.0`, the struct name
    // supplying the Type segment and the 0.0.0 version-free sentinel the
    // version. Reverting the @app/local synthesis (or making it read a file)
    // flips this assertion — that is the regression it guards.
    let ident = BareAppLocalProcessor::schema_ident();
    assert_eq!(ident.org.as_str(), "app");
    assert_eq!(ident.package.as_str(), "local");
    assert_eq!(ident.r#type.as_str(), "BareAppLocalProcessor");
    assert_eq!(ident.version.major, 0);
    assert_eq!(ident.version.minor, 0);
    assert_eq!(ident.version.patch, 0);
    assert_eq!(
        BareAppLocalProcessor::schema_ident().to_string(),
        "@app/local/BareAppLocalProcessor@0.0.0"
    );
}

/// A field type that only exists on Linux, so the processor below can gate a
/// field on the real `target_os` rather than only on always-true / always-false
/// predicates.
#[cfg(target_os = "linux")]
#[derive(Default)]
pub struct CfgGatedFieldLinuxOnlyState {
    pub linux_only_marker: u32,
}

/// The non-Linux half of the same pair.
#[cfg(not(target_os = "linux"))]
#[derive(Default)]
pub struct CfgGatedFieldNonLinuxOnlyState {
    pub non_linux_only_marker: u32,
}

/// A field type compiled on every target, gated by an always-true `cfg`.
#[derive(Default)]
pub struct CfgGatedFieldAlwaysCompiledState {
    pub always_compiled_marker: u32,
}

// #1588: a `#[cfg]` authored on a processor struct field must be re-emitted at
// BOTH generated sites — the `Processor` field definition and its `from_config`
// struct-literal initializer. This is a compile-level proof: `any()` is false on
// every target, so `never_compiled_state`'s deliberately-undeclared type only
// resolves if the macro stripped that field from both sites; `all()` is true on
// every target, so `always_compiled_state` only compiles if the surviving arm
// still gets its initializer. The `target_os` pair exercises the real-world
// shape on top of the target-independent halves.
#[streamlib::sdk::processor(
    "@tatolab/streamlib-engine/CfgGatedFieldProcessor",
    execution = manual,
    input("video_in", any, delivery_profile = "latest"),
    output("video_out", any),
)]
pub struct CfgGatedFieldProcessor {
    #[cfg(any())]
    never_compiled_state: ThisTypeIsDeliberatelyUndeclared,
    // The forwarded `allow` is load-bearing twice over: it silences
    // `non_minimal_cfg` on the deliberately-degenerate always-true predicate,
    // and it only reaches the generated field at all if the macro forwards lint
    // controls — so this line also exercises that half of the filter in-tree.
    #[allow(clippy::non_minimal_cfg)]
    #[cfg(all())]
    always_compiled_state: CfgGatedFieldAlwaysCompiledState,
    #[cfg(target_os = "linux")]
    linux_only_state: CfgGatedFieldLinuxOnlyState,
    #[cfg(not(target_os = "linux"))]
    non_linux_only_state: CfgGatedFieldNonLinuxOnlyState,
}

impl streamlib_engine::ManualProcessor for CfgGatedFieldProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

#[test]
fn cfg_gated_processor_field_survives_onto_both_generated_sites() {
    let processor = CfgGatedFieldProcessor::Processor::from_config(EmptyConfig).unwrap();
    assert_eq!(processor.always_compiled_state.always_compiled_marker, 0);
    #[cfg(target_os = "linux")]
    assert_eq!(processor.linux_only_state.linux_only_marker, 0);
    #[cfg(not(target_os = "linux"))]
    assert_eq!(processor.non_linux_only_state.non_linux_only_marker, 0);
}

#[test]
fn cfg_gated_processor_still_declares_its_port_fields_unconditionally() {
    // The `inputs` / `outputs` handle fields are emitted ahead of the
    // custom fields and are patched by name in `set_iceoryx2_resources` — the
    // field-attribute filter must never reach them. Naming both fields here is
    // the compile-level half; `is_configured()` is false until the host wires
    // them after `from_config` returns.
    let processor = CfgGatedFieldProcessor::Processor::from_config(EmptyConfig).unwrap();
    assert!(!processor.inputs.is_configured());
    assert!(!processor.outputs.is_configured());
}

#[test]
fn test_processor_schema_ident_renders_canonical_joined_form() {
    // The structured SchemaIdent's Display impl produces the canonical
    // `@<org>/<package>/<Type>@<major.minor.patch>` joined form used by
    // `expected_payload_bytes_for_port_spec` and other lookup paths. The
    // version-free grammar renders the 0.0.0 sentinel.
    assert_eq!(
        TestProcessor::schema_ident().to_string(),
        "@tatolab/streamlib-engine/TestProcessor@0.0.0"
    );
}
