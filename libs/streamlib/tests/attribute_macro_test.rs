// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Test for YAML-based #[streamlib::processor] attribute macro syntax
//!
//! This test verifies the module-based processor generation works correctly
//! with YAML schema definitions.

use streamlib::core::GeneratedProcessor;
use streamlib::core::{EmptyConfig, Result, RuntimeContext};

// Define a simple processor using YAML schema
#[streamlib::processor("schemas/processors/test/test_processor.yaml")]
pub struct TestProcessor;

// User implements the Processor trait on the generated Processor struct
impl streamlib::ManualProcessor for TestProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    fn start(&mut self) -> Result<()> {
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
    fn assert_processor_type<T: streamlib::core::ManualProcessor>() {}
    assert_processor_type::<TestProcessor::Processor>();
}

#[test]
fn test_input_link_module_exists() {
    // Verify InputLink module has the expected port marker
    fn assert_input_marker<T: streamlib::core::InputPortMarker>() {}
    assert_input_marker::<TestProcessor::InputLink::video_in>();
}

#[test]
fn test_output_link_module_exists() {
    // Verify OutputLink module has the expected port marker
    fn assert_output_marker<T: streamlib::core::OutputPortMarker>() {}
    assert_output_marker::<TestProcessor::OutputLink::video_out>();
}

#[test]
fn test_port_marker_names() {
    // Verify port names are correct
    use streamlib::core::{InputPortMarker, OutputPortMarker};

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
    assert_eq!(processor.name(), "com.streamlib.test.processor");
}

// Test with config field
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    streamlib::ConfigDescriptor,
)]
pub struct MyConfig {
    pub threshold: f32,
}

#[streamlib::processor("schemas/processors/test/configured_processor.yaml")]
pub struct ConfiguredProcessor;

impl streamlib::ContinuousProcessor for ConfiguredProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    fn process(&mut self) -> Result<()> {
        // Access config
        let _threshold = self.config.threshold;
        Ok(())
    }
}

#[test]
fn test_config_field_access() {
    let config = MyConfig { threshold: 0.5 };
    let processor = ConfiguredProcessor::Processor::from_config(config).unwrap();

    // Config should be stored
    assert_eq!(processor.config.threshold, 0.5);
}

#[test]
fn test_config_update() {
    let config = MyConfig { threshold: 0.5 };
    let mut processor = ConfiguredProcessor::Processor::from_config(config).unwrap();

    // Update config
    let new_config = MyConfig { threshold: 0.8 };
    processor.update_config(new_config).unwrap();

    assert_eq!(processor.config.threshold, 0.8);
}
