//! Procedural macros for streamlib
//!
//! This crate provides the `#[derive(StreamProcessor)]` macro to automatically generate:
//! - Config struct (or use existing/EmptyConfig)
//! - `from_config()` method for processor construction
//! - `descriptor()` method with type-safe schemas
//! - Smart defaults for descriptions, tags, examples
//!
//! # Example Usage
//!
//! ## Level 0: Minimal (Everything Auto-Generated)
//!
//! ```rust
//! use streamlib::{StreamInput, StreamOutput, VideoFrame, AudioFrame};
//!
//! #[derive(StreamProcessor)]
//! struct VideoEffectProcessor {
//!     #[input]
//!     video_in: StreamInput<VideoFrame>,
//!
//!     #[output]
//!     video_out: StreamOutput<VideoFrame>,
//! }
//!
//! impl VideoEffectProcessor {
//!     fn process(&mut self, tick: TimedTick) -> Result<()> {
//!         if let Some(frame) = self.video_in.read_latest() {
//!             // Process frame...
//!             self.video_out.write(frame);
//!         }
//!         Ok(())
//!     }
//! }
//! ```
//!
//! ## Level 1: With Descriptions and Config
//!
//! ```rust
//! #[derive(StreamProcessor)]
//! #[processor(
//!     description = "Applies blur effect to video",
//!     usage = "Connect video input, adjust blur_radius, connect output",
//!     tags = ["video", "effect", "blur"]
//! )]
//! struct BlurProcessor {
//!     #[input(description = "Video input to blur")]
//!     video: StreamInput<VideoFrame>,
//!
//!     #[output(description = "Blurred video output")]
//!     output: StreamOutput<VideoFrame>,
//!
//!     // Config fields (not ports)
//!     blur_radius: f32,
//! }
//! ```
//!
//! ## Level 2: Full Control with Custom Config
//!
//! ```rust
//! #[derive(StreamProcessor)]
//! #[processor(
//!     config = BlurConfig,
//!     audio_requirements = {
//!         sample_rate: 48000,
//!         buffer_size: 2048,
//!     }
//! )]
//! struct AdvancedProcessor {
//!     #[input(name = "video_input", description = "Main video", required = true)]
//!     video_in: StreamInput<VideoFrame>,
//!
//!     #[output(name = "video_output")]
//!     video_out: StreamOutput<VideoFrame>,
//! }
//! ```

mod attributes;
mod analysis;
mod codegen;

use proc_macro::TokenStream;
use syn::{parse_macro_input, DeriveInput};

/// Derive macro for StreamProcessor trait
///
/// Automatically generates:
/// - Config struct (or uses existing/EmptyConfig)
/// - `StreamProcessorFactory::from_config()` implementation
/// - `DescriptorProvider::descriptor()` implementation
/// - `DynStreamProcessor::as_any_mut()` implementation
///
/// Smart defaults:
/// - Description: Generated from struct name and port configuration
/// - Usage context: Generated from port types and names
/// - Tags: Auto-detected from port types (video, audio, data, source, sink, effect)
/// - Examples: Extracted from `PortMessage::examples()`
/// - Audio requirements: Auto-detected if processor has audio ports
///
/// # Attributes
///
/// ## `#[processor(...)]` - Processor-level attributes
///
/// - `config = MyConfig` - Use custom config type (instead of auto-generating)
/// - `description = "..."` - Processor description (overrides smart default)
/// - `usage = "..."` - Usage context (overrides smart default)
/// - `tags = ["tag1", "tag2"]` - Custom tags (overrides smart defaults)
/// - `audio_requirements = {...}` - Custom audio requirements
/// - `mode = Pull` or `mode = Push` - Scheduling mode (Pull = pull-based, Push = push-based)
/// - `unsafe_send` - Generate `unsafe impl Send` for types with !Send hardware resources
///
/// ## `#[input(...)]` or `#[output(...)]` - Port-level attributes
///
/// - `name = "custom_name"` - Custom port name (instead of field name)
/// - `description = "..."` - Port description (overrides smart default)
/// - `required = true` - Mark input as required (inputs only)
///
/// # Type Safety
///
/// Schemas are extracted from generic type parameters at compile time:
/// - `StreamInput<VideoFrame>` → `VideoFrame::schema()`
/// - No string-based type references
/// - Full IDE autocomplete support
/// - Compile-time type checking
///
/// # Generated Code
///
/// For a processor with ports and config fields:
///
/// ```rust
/// #[derive(StreamProcessor)]
/// struct MyProcessor {
///     #[input]
///     input: StreamInput<VideoFrame>,
///
///     #[output]
///     output: StreamOutput<VideoFrame>,
///
///     config_value: f32,
/// }
/// ```
///
/// Generates approximately:
///
/// ```rust
/// struct Config {
///     config_value: f32,
/// }
///
/// impl StreamProcessorFactory for MyProcessor {
///     type Config = Config;
///
///     fn from_config(config: Config) -> Result<Self> {
///         Ok(Self {
///             input: StreamInput::new("input"),
///             output: StreamOutput::new("output"),
///             config_value: config.config_value,
///         })
///     }
/// }
///
/// impl DescriptorProvider for MyProcessor {
///     fn descriptor() -> Option<ProcessorDescriptor> {
///         Some(
///             ProcessorDescriptor::new("MyProcessor", "My effect processor")
///                 .with_input("input", VideoFrame::schema(), "Input")
///                 .with_output("output", VideoFrame::schema(), "Output")
///                 .with_examples(VideoFrame::examples())
///                 .with_tags(vec!["video", "effect"])
///         )
///     }
/// }
///
/// impl DynStreamProcessor for MyProcessor {
///     fn as_any_mut(&mut self) -> &mut dyn Any {
///         self
///     }
/// }
/// ```
#[proc_macro_derive(StreamProcessor, attributes(processor, input, output, state, config))]
pub fn derive_stream_processor(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    // Phase 1: Analyze struct (classify fields, extract types)
    let analysis = match analysis::AnalysisResult::analyze(&input) {
        Ok(result) => result,
        Err(err) => return err.to_compile_error().into(),
    };

    // Phase 2: Generate code
    let generated = codegen::generate_processor_impl(&analysis);

    TokenStream::from(generated)
}

