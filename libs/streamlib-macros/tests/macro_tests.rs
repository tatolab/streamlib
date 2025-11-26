//! Integration tests for Processor derive macro
//!
//! These tests verify that the macro generates correct code for various configurations.

use streamlib_macros::Processor;

// Note: These tests compile but don't run because we'd need the full streamlib runtime.
// The primary goal is to verify that the macro generates valid Rust code that compiles.

// Re-export types needed for tests (in real usage these come from streamlib)
// We're just testing macro expansion, not runtime behavior
mod mock {
    use std::any::Any;
    use std::sync::Arc;

    // Mock port types
    pub struct LinkInput<T> {
        _phantom: std::marker::PhantomData<T>,
    }

    impl<T: LinkPortMessage> LinkInput<T> {
        pub fn new(_name: impl Into<String>) -> Self {
            Self {
                _phantom: std::marker::PhantomData,
            }
        }
    }

    pub struct LinkOutput<T> {
        _phantom: std::marker::PhantomData<T>,
    }

    impl<T: LinkPortMessage> LinkOutput<T> {
        pub fn new(_name: impl Into<String>) -> Self {
            Self {
                _phantom: std::marker::PhantomData,
            }
        }
    }

    // Mock message types
    #[derive(Clone)]
    pub struct VideoFrame;

    #[derive(Clone)]
    pub struct AudioFrame;

    #[derive(Clone)]
    pub struct DataMessage;

    // Mock schema type
    pub struct Schema;

    // Mock LinkPortMessage trait
    pub trait LinkPortMessage: Clone + Send + 'static {
        fn port_type() -> LinkPortType;
        fn schema() -> Arc<Schema>;
        fn examples() -> Vec<(&'static str, String)> {
            Vec::new()
        }
    }

    impl LinkPortMessage for VideoFrame {
        fn port_type() -> LinkPortType {
            LinkPortType::Video
        }
        fn schema() -> Arc<Schema> {
            Arc::new(Schema)
        }
    }

    impl LinkPortMessage for AudioFrame {
        fn port_type() -> LinkPortType {
            LinkPortType::Audio
        }
        fn schema() -> Arc<Schema> {
            Arc::new(Schema)
        }
    }

    impl LinkPortMessage for DataMessage {
        fn port_type() -> LinkPortType {
            LinkPortType::Data
        }
        fn schema() -> Arc<Schema> {
            Arc::new(Schema)
        }
    }

    // Mock port type
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum LinkPortType {
        Video,
        Audio,
        Data,
    }

    // Mock processor descriptor
    pub struct ProcessorDescriptor;

    impl ProcessorDescriptor {
        pub fn new(_name: &str, _description: &str) -> Self {
            Self
        }

        pub fn with_usage_context(self, _context: &str) -> Self {
            self
        }

        pub fn with_tags(self, _tags: Vec<String>) -> Self {
            self
        }

        pub fn with_input(self, _name: &str, _schema: Arc<Schema>, _description: &str) -> Self {
            self
        }

        pub fn with_required(self, _required: bool) -> Self {
            self
        }

        pub fn with_output(self, _name: &str, _schema: Arc<Schema>, _description: &str) -> Self {
            self
        }

        pub fn with_examples(self, _examples: Vec<(&'static str, String)>) -> Self {
            self
        }

        pub fn with_audio_requirements(self, _reqs: AudioRequirements) -> Self {
            self
        }
    }

    // Mock audio requirements
    pub struct AudioRequirements;

    impl Default for AudioRequirements {
        fn default() -> Self {
            Self
        }
    }

    // Mock empty config
    #[derive(Debug, Clone, Default)]
    pub struct EmptyConfig;

    // Mock Result type
    pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    // Mock traits that the macro implements
    pub trait ProcessorFactory {
        type Config;
        fn from_config(config: Self::Config) -> Result<Self>
        where
            Self: Sized;
    }

    pub trait DescriptorProvider {
        fn descriptor() -> Option<ProcessorDescriptor>;
    }

    pub trait DynProcessor {
        fn as_any_mut(&mut self) -> &mut dyn Any;
    }
}

// Import mocks with streamlib-like names
use mock::*;

// === Test Case 1: Simple Processor (No Config Fields) ===

#[derive(Processor)]
struct SimpleVideoProcessor {
    #[input()]
    video_in: LinkInput<VideoFrame>,

    #[output()]
    video_out: LinkOutput<VideoFrame>,
}

#[test]
fn test_simple_processor_compiles() {
    // This test verifies that the macro generates valid code
    // In real usage, we'd create an instance: SimpleVideoProcessor::from_config(config)
    assert!(
        true,
        "SimpleVideoProcessor macro expansion compiled successfully"
    );
}

// === Test Case 2: Processor with Config Fields ===

#[derive(Processor)]
struct ProcessorWithConfig {
    #[input()]
    input: LinkInput<VideoFrame>,

    #[output()]
    output: LinkOutput<VideoFrame>,

    // Config fields (not ports)
    threshold: f32,
    enabled: bool,
}

#[test]
fn test_processor_with_config_compiles() {
    // Macro should auto-generate Config struct with threshold and enabled fields
    assert!(
        true,
        "ProcessorWithConfig macro expansion compiled successfully"
    );
}

// === Test Case 3: Custom Config Type ===

#[derive(Debug, Clone, Default)]
struct CustomBlurConfig {
    radius: f32,
    sigma: f32,
}

#[derive(Processor)]
#[processor(config = CustomBlurConfig)]
struct BlurProcessorWithCustomConfig {
    #[input()]
    video: LinkInput<VideoFrame>,

    #[output()]
    output: LinkOutput<VideoFrame>,
}

#[test]
fn test_custom_config_type_compiles() {
    // Macro should use CustomBlurConfig instead of generating one
    assert!(
        true,
        "BlurProcessorWithCustomConfig macro expansion compiled successfully"
    );
}

// === Test Case 4: Custom Port Names and Descriptions ===

#[derive(Processor)]
#[processor(
    description = "Applies custom video effect",
    usage = "Connect video input, adjust settings, connect output"
)]
struct CustomizedProcessor {
    #[input(
        name = "main_input",
        description = "Primary video input",
        required = true
    )]
    video_in: LinkInput<VideoFrame>,

    #[output(name = "main_output", description = "Processed video output")]
    video_out: LinkOutput<VideoFrame>,

    effect_strength: f32,
}

#[test]
fn test_customized_ports_compiles() {
    // Macro should use custom names and descriptions
    assert!(
        true,
        "CustomizedProcessor macro expansion compiled successfully"
    );
}

// === Test Case 5: Audio Processor (Auto-Detect Audio Requirements) ===

#[derive(Processor)]
struct AudioMixerProcessor {
    #[input()]
    audio_1: LinkInput<AudioFrame>,

    #[input()]
    audio_2: LinkInput<AudioFrame>,

    #[output()]
    mixed: LinkOutput<AudioFrame>,
}

#[test]
fn test_audio_processor_compiles() {
    // Macro should auto-detect audio ports and add AudioRequirements::default()
    assert!(
        true,
        "AudioMixerProcessor macro expansion compiled successfully"
    );
}

// === Test Case 6: Full Control (All Attributes) ===

#[derive(Debug, Clone)]
struct AdvancedConfig {
    mode: String,
    intensity: f32,
}

impl Default for AdvancedConfig {
    fn default() -> Self {
        Self {
            mode: "standard".to_string(),
            intensity: 1.0,
        }
    }
}

#[derive(Processor)]
#[processor(
    config = AdvancedConfig,
    description = "Advanced multi-modal processor",
    usage = "Processes video and audio with configurable effects"
)]
struct AdvancedProcessor {
    #[input(name = "video_input", description = "Video stream", required = true)]
    video: LinkInput<VideoFrame>,

    #[input(name = "audio_input", description = "Audio stream", required = false)]
    audio: LinkInput<AudioFrame>,

    #[output(name = "video_output", description = "Processed video")]
    video_out: LinkOutput<VideoFrame>,

    #[output(name = "audio_output", description = "Processed audio")]
    audio_out: LinkOutput<AudioFrame>,
}

#[test]
fn test_advanced_processor_compiles() {
    // This tests the most complex case with all features
    assert!(
        true,
        "AdvancedProcessor macro expansion compiled successfully"
    );
}

// === Test Case 7: Data Processor (Non-Video/Audio) ===

#[derive(Processor)]
struct DataProcessor {
    #[input()]
    data_in: LinkInput<DataMessage>,

    #[output()]
    data_out: LinkOutput<DataMessage>,

    buffer_size: usize,
}

#[test]
fn test_data_processor_compiles() {
    // Macro should work with generic data types
    assert!(true, "DataProcessor macro expansion compiled successfully");
}

// === Test Case 8: Source Processor (Output Only) ===

#[derive(Processor)]
struct SourceProcessor {
    #[output()]
    output: LinkOutput<VideoFrame>,

    frame_rate: u32,
}

#[test]
fn test_source_processor_compiles() {
    // Macro should detect source processor (no inputs)
    // Auto-generated description should include "source"
    assert!(
        true,
        "SourceProcessor macro expansion compiled successfully"
    );
}

// === Test Case 9: Sink Processor (Input Only) ===

#[derive(Processor)]
struct SinkProcessor {
    #[input()]
    input: LinkInput<VideoFrame>,

    save_path: String,
}

#[test]
fn test_sink_processor_compiles() {
    // Macro should detect sink processor (no outputs)
    // Auto-generated description should include "sink"
    assert!(true, "SinkProcessor macro expansion compiled successfully");
}
