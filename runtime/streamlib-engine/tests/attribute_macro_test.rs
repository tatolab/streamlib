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
// descriptor, and the class-path accessor — it never auto-registers.
#[streamlib::sdk::processor(
    execution = manual,
    input("video_in", delivery_profile = "newest"),
    output("video_out"),
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
fn the_class_short_name_is_the_authored_struct_ident() {
    // `NAME` is the display-name default's only carrier. It comes off the item
    // the attribute is attached to — never a string in the attribute, and never
    // recovered by splitting the import path.
    assert_eq!(TestProcessor::Processor::NAME, "TestProcessor");
    assert_eq!(
        BareProcessor::Processor::NAME,
        "BareProcessor",
        "a bare `#[processor]` still names its class"
    );
}

// A bare `#[processor]` — the attribute takes no identity in any spelling, so
// this is the only spelling there is. The class is named by its import path,
// captured at the expansion site.
#[streamlib::sdk::processor(execution = manual)]
pub struct BareProcessor;

impl streamlib_engine::ManualProcessor for BareProcessor::Processor {
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
fn a_bare_processor_is_named_by_its_import_path() {
    // The org/package synthesis is gone with the grammar. The class path is
    // the whole identity, and it names the module the macro expanded in —
    // nothing is synthesized and no file is read.
    assert_eq!(
        BareProcessor::processor_class_import_path().as_str(),
        "attribute_macro_test::BareProcessor"
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
    execution = manual,
    input("video_in", delivery_profile = "newest"),
    output("video_out"),
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
fn the_descriptor_carries_the_short_name_and_the_import_path_apart() {
    // Two fields, two jobs: the path is identity, the short name is only the
    // display default. Collapsing either into the other is the regression.
    let descriptor = <TestProcessor::Processor as GeneratedProcessor>::descriptor()
        .expect("the macro emits a descriptor");
    assert_eq!(
        descriptor.processor_class_short_name.as_str(),
        "TestProcessor"
    );
    assert_eq!(
        descriptor.processor_class_import_path.as_str(),
        "attribute_macro_test::TestProcessor"
    );
}

// A windowed audio consumer: the declaration the macro must carry all the way
// into the emitted descriptor, since that descriptor is what the engine reads
// at `rt.add` time.
#[streamlib::sdk::processor(
    execution = reactive,
    input(
        "audio",
        delivery_profile = "ordered",
        audio_window(
            sample_rate = 16_000,
            channels = 1,
            dtype = "f32",
            window_size = 512,
            hop = 160
        )
    ),
    output("windows"),
)]
pub struct WindowedAudioConsumer;

impl streamlib_engine::ReactiveProcessor for WindowedAudioConsumer::Processor {
    fn process(
        &mut self,
        _ctx: &streamlib_engine::core::RuntimeContextLimitedAccess<'_>,
    ) -> Result<()> {
        Ok(())
    }
}

#[test]
fn the_descriptor_carries_the_window_contract_its_port_declared() {
    let descriptor = <WindowedAudioConsumer::Processor as GeneratedProcessor>::descriptor()
        .expect("the macro emits a descriptor");
    let audio = descriptor
        .inputs
        .iter()
        .find(|port| port.name == "audio")
        .expect("the audio port is in the descriptor");

    assert_eq!(
        audio.audio_window,
        Some(
            streamlib_engine::core::descriptors::AudioWindowContract::Declaration(
                streamlib_engine::core::descriptors::AudioWindowContractDeclaredValues {
                    sample_rate: 16_000,
                    channels: Some(1),
                    dtype: "f32".to_string(),
                    window_size: 512,
                    hop: 160,
                }
            )
        )
    );
}

#[test]
fn a_port_declaring_no_window_contract_carries_none_into_the_descriptor() {
    let descriptor = <WindowedAudioConsumer::Processor as GeneratedProcessor>::descriptor()
        .expect("the macro emits a descriptor");

    assert!(
        descriptor
            .outputs
            .iter()
            .all(|port| port.audio_window.is_none()),
        "an output port declares no contract"
    );
    let unwindowed = <TestProcessor::Processor as GeneratedProcessor>::descriptor()
        .expect("the macro emits a descriptor");
    assert!(
        unwindowed
            .inputs
            .iter()
            .all(|port| port.audio_window.is_none()),
        "a port that declared no contract carries none"
    );
}
