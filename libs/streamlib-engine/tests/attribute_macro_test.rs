// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Test for YAML-based #[streamlib_engine::processor] attribute macro syntax
//!
//! This test verifies the module-based processor generation works correctly
//! with YAML schema definitions.

use streamlib_engine::core::GeneratedProcessor;
use streamlib_engine::core::{EmptyConfig, Result, RuntimeContextFullAccess, RuntimeContextLimitedAccess};

// Define a simple processor using YAML schema
#[streamlib::processor("TestProcessor")]
pub struct TestProcessor;

// User implements the Processor trait on the generated Processor struct
impl streamlib_engine::ManualProcessor for TestProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    fn teardown(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
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
fn test_processor_schema_ident_resolves_from_package_block() {
    // The macro emits `Processor::schema_ident()` returning the structured
    // SchemaIdent composed from `streamlib.yaml`'s `package:` block plus
    // the processor's PascalCase short name. This locks the codegen
    // contract end-to-end: package metadata read, structured const emit,
    // SchemaIdent::new + segment validators all wired through the
    // generated module.
    //
    // Reverting the macro's `package:` resolution (e.g. hardcoding org)
    // would flip this assertion — that's the regression the test guards.
    let ident = TestProcessor::schema_ident();
    assert_eq!(ident.org.as_str(), "tatolab");
    assert_eq!(ident.package.as_str(), "streamlib-engine");
    assert_eq!(ident.r#type.as_str(), "TestProcessor");
    assert_eq!(ident.version.major, 0);
    assert_eq!(ident.version.minor, 4);
    assert_eq!(ident.version.patch, 28);
}

#[test]
fn test_processor_schema_ident_renders_canonical_joined_form() {
    // The structured SchemaIdent's Display impl produces the canonical
    // `@<org>/<package>/<Type>@<major.minor.patch>` joined form used by
    // `max_payload_bytes_for_schema` and other lookup paths.
    //
    // Package name is `streamlib-engine` post-#731 (was `streamlib` before
    // the SDK extraction). Reverse-DNS schema names in `core/streaming/`,
    // `linux/processors/`, etc. retain the `com.streamlib.*` namespace —
    // those are independent of the package's `name:` field.
    assert_eq!(
        TestProcessor::schema_ident().to_string(),
        "@tatolab/streamlib-engine/TestProcessor@0.4.28"
    );
}

// Test with config field
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    streamlib_engine::ConfigDescriptor,
)]
pub struct ConfiguredProcessorConfig {
    pub threshold: f32,
}

// The processor macro generates `crate::_generated_::ConfiguredProcessorConfig`.
// In integration tests, `crate` refers to this test binary, so we need a bridge module.
mod _generated_ {
    pub use super::ConfiguredProcessorConfig;
}

#[streamlib::processor("TestConfiguredProcessor")]
pub struct ConfiguredProcessor;

impl streamlib_engine::ContinuousProcessor for ConfiguredProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    fn teardown(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        // Access config
        let _threshold = self.config.threshold;
        Ok(())
    }
}

#[test]
fn test_config_field_access() {
    let config = ConfiguredProcessorConfig { threshold: 0.5 };
    let processor = ConfiguredProcessor::Processor::from_config(config).unwrap();

    // Config should be stored
    assert_eq!(processor.config.threshold, 0.5);
}

#[test]
fn test_config_update() {
    let config = ConfiguredProcessorConfig { threshold: 0.5 };
    let mut processor = ConfiguredProcessor::Processor::from_config(config).unwrap();

    // Update config
    let new_config = ConfiguredProcessorConfig { threshold: 0.8 };
    processor.update_config(new_config).unwrap();

    assert_eq!(processor.config.threshold, 0.8);
}
