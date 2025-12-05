// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Test for new #[streamlib::processor] attribute macro syntax
//!
//! This test verifies the module-based processor generation works correctly.

use streamlib::core::{EmptyConfig, LinkInput, LinkOutput, Result, RuntimeContext, VideoFrame};

// Define a simple processor using the new attribute macro syntax
#[streamlib::processor(execution = Manual)]
pub struct TestProcessor {
    #[streamlib::input]
    video_in: LinkInput<VideoFrame>,

    #[streamlib::output]
    video_out: LinkOutput<VideoFrame>,
}

// User implements methods on the generated Processor struct
impl TestProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        // Simple passthrough - read from input and push to output
        while let Some(frame) = self.video_in.read() {
            self.video_out.push(frame);
        }
        Ok(())
    }
}

#[test]
fn test_module_structure_generated() {
    // Verify the module structure was generated correctly
    // TestProcessor::Processor should exist
    fn assert_processor_type<T: streamlib::core::Processor>() {}
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
    use streamlib::core::Processor;

    // Create processor from empty config
    let processor = TestProcessor::Processor::from_config(EmptyConfig).unwrap();

    // Verify it has the expected name
    use streamlib::core::BaseProcessor;
    assert_eq!(processor.name(), "TestProcessor");
}

// Test with config field
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MyConfig {
    pub threshold: f32,
}

#[streamlib::processor(execution = Continuous)]
pub struct ConfiguredProcessor {
    #[streamlib::output]
    data: LinkOutput<VideoFrame>,

    #[streamlib::config]
    config: MyConfig,
}

impl ConfiguredProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContext) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        // Access config
        let _threshold = self.config.threshold;
        Ok(())
    }
}

#[test]
fn test_config_field_access() {
    use streamlib::core::Processor;

    let config = MyConfig { threshold: 0.5 };
    let processor = ConfiguredProcessor::Processor::from_config(config).unwrap();

    // Config should be stored
    assert_eq!(processor.config.threshold, 0.5);
}

#[test]
fn test_config_update() {
    use streamlib::core::Processor;

    let config = MyConfig { threshold: 0.5 };
    let mut processor = ConfiguredProcessor::Processor::from_config(config).unwrap();

    // Update config
    let new_config = MyConfig { threshold: 0.8 };
    processor.update_config(new_config).unwrap();

    assert_eq!(processor.config.threshold, 0.8);
}
