//! StreamTransform - Trait for data processors (transforms)
//!
//! Transforms process data, taking inputs and producing outputs.
//! They are the middle layer of processing pipelines.
//!
//! ## Design Philosophy
//!
//! Inspired by GStreamer's GstBaseTransform, transforms:
//! - Have both inputs and outputs
//! - Process data reactively (when input arrives)
//! - May have internal state (parameters, buffers)
//! - Can have any I/O configuration (1→1, N→1, 1→N, N→M)
//!
//! ## Transform Types
//!
//! - **Audio effects**: Process audio (reverb, EQ, compression)
//! - **Video effects**: Process video (color grading, filters)
//! - **Mixers**: Combine multiple inputs into one output (audio/video mixing)
//! - **Splitters**: Split one input into multiple outputs (multicasting)
//! - **Analyzers**: Extract metadata without modifying data
//!
//! ## Reactive Processing
//!
//! Transforms are **reactive** - they process when input data arrives:
//!
//! - Runtime detects input data available
//! - Runtime calls `process()`
//! - Transform reads from input port(s), processes, writes to output port(s)
//! - Transform returns control to runtime
//!
//! This is different from sources (which generate on schedule) and sinks
//! (which consume on arrival).
//!
//! ## Usage Example
//!
//! ```rust,ignore
//! use streamlib::core::traits::{StreamElement, StreamTransform, ElementType};
//! use streamlib::core::{AudioFrame, ProcessorDescriptor, AudioEffectConfig};
//! use streamlib::core::error::Result;
//!
//! struct ReverbEffect {
//!     name: String,
//!     room_size: f32,
//!     // ... internal reverb state
//! }
//!
//! impl StreamElement for ReverbEffect {
//!     fn name(&self) -> &str { &self.name }
//!     fn element_type(&self) -> ElementType { ElementType::Transform }
//!     fn descriptor(&self) -> Option<ProcessorDescriptor> {
//!         ReverbEffect::descriptor()
//!     }
//! }
//!
//! impl StreamTransform for ReverbEffect {
//!     type Config = AudioEffectConfig;
//!
//!     fn from_config(config: Self::Config) -> Result<Self> {
//!         Ok(Self {
//!             name: "reverb".to_string(),
//!             room_size: config.parameters.get("room_size").unwrap_or(0.5),
//!         })
//!     }
//!
//!     fn process(&mut self) -> Result<()> {
//!         // Read from input port, apply reverb, write to output
//!         Ok(())
//!     }
//!
//!     fn descriptor() -> Option<ProcessorDescriptor> {
//!         // Return processor metadata
//!         None
//!     }
//! }
//! ```

use super::{StreamElement, ElementType};
use crate::core::error::Result;
use crate::core::schema::ProcessorDescriptor;
use serde::{Deserialize, Serialize};

/// Trait for data transform processors
///
/// Transforms process data reactively, taking inputs and producing outputs.
/// They are the middle layer of processing pipelines.
///
/// ## Implementation Requirements
///
/// 1. Implement `StreamElement` base trait
/// 2. Implement `process()` to transform data
/// 3. Define configuration type (for `from_config()`)
/// 4. Provide descriptor for AI/MCP discovery
///
/// ## Processing Pattern
///
/// The `process()` method is called by the runtime when input data is available.
/// Inside `process()`, the transform should:
///
/// 1. Read from input port(s) via `StreamElement::input_ports()`
/// 2. Transform the data
/// 3. Write to output port(s) via `StreamElement::output_ports()`
///
/// ## I/O Flexibility
///
/// Transforms can have any I/O configuration:
///
/// - **1→1**: Single input, single output (most effects)
/// - **N→1**: Multiple inputs, single output (mixers)
/// - **1→N**: Single input, multiple outputs (splitters)
/// - **N→M**: Multiple inputs, multiple outputs (complex processors)
///
/// The runtime discovers ports via `StreamElement::input_ports()` and
/// `StreamElement::output_ports()`.
///
/// ## Example: Simple 1→1 Transform
///
/// ```rust,ignore
/// use streamlib::core::traits::{StreamElement, StreamTransform};
/// use streamlib::core::{AudioFrame, StreamInput, StreamOutput};
///
/// struct GainEffect {
///     input: StreamInput<AudioFrame>,
///     output: StreamOutput<AudioFrame>,
///     gain: f32,
/// }
///
/// impl StreamTransform for GainEffect {
///     type Config = GainConfig;
///
///     fn from_config(config: Self::Config) -> Result<Self> {
///         Ok(Self {
///             input: StreamInput::new("audio"),
///             output: StreamOutput::new("audio"),
///             gain: config.gain,
///         })
///     }
///
///     fn process(&mut self) -> Result<()> {
///         if let Some(mut frame) = self.input.read_latest() {
///             // Apply gain to each sample
///             let samples: Vec<f32> = frame.samples.iter()
///                 .map(|s| s * self.gain)
///                 .collect();
///             frame.samples = Arc::new(samples);
///
///             // Write to output
///             self.output.write(frame);
///         }
///         Ok(())
///     }
///
///     fn descriptor() -> Option<ProcessorDescriptor> {
///         // Metadata for AI discovery
///         None
///     }
/// }
/// ```
///
/// ## Example: Multi-Input Transform (Mixer)
///
/// ```rust,ignore
/// use streamlib::core::traits::{StreamElement, StreamTransform};
/// use std::collections::HashMap;
///
/// struct AudioMixer {
///     inputs: HashMap<String, StreamInput<AudioFrame>>,
///     output: StreamOutput<AudioFrame>,
/// }
///
/// impl StreamTransform for AudioMixer {
///     type Config = MixerConfig;
///
///     fn from_config(config: Self::Config) -> Result<Self> {
///         // Create dynamic inputs
///         let mut inputs = HashMap::new();
///         for i in 0..config.num_inputs {
///             inputs.insert(
///                 format!("input_{}", i),
///                 StreamInput::new(&format!("input_{}", i))
///             );
///         }
///
///         Ok(Self {
///             inputs,
///             output: StreamOutput::new("audio"),
///         })
///     }
///
///     fn process(&mut self) -> Result<()> {
///         // Read from all inputs, mix, write to output
///         let mut mixed_samples = Vec::new();
///
///         for input in self.inputs.values_mut() {
///             if let Some(frame) = input.read_latest() {
///                 // Mix logic...
///             }
///         }
///
///         // Write mixed output
///         self.output.write(mixed_frame);
///         Ok(())
///     }
///
///     fn descriptor() -> Option<ProcessorDescriptor> {
///         // Metadata for AI discovery
///         None
///     }
/// }
/// ```
pub trait StreamTransform: StreamElement {
    /// Configuration type
    ///
    /// Used by `from_config()` constructor.
    type Config: Serialize + for<'de> Deserialize<'de>;

    /// Create transform from configuration
    ///
    /// Called by runtime when adding processor.
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Configuration is invalid
    /// - Resources cannot be allocated
    /// - Required dependencies are unavailable
    fn from_config(config: Self::Config) -> Result<Self>
    where
        Self: Sized;

    /// Process one cycle of data transformation
    ///
    /// Called by runtime when input data is available on any input port.
    /// Should be fast - blocking operations should be async.
    ///
    /// # Processing Pattern
    ///
    /// 1. Read from input port(s)
    /// 2. Transform the data
    /// 3. Write to output port(s)
    ///
    /// For multi-input processors (like mixers), this may wait until ALL
    /// inputs have data before processing.
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Processing fails (transient - runtime may retry)
    /// - Resources are unavailable (may trigger reconnection)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// fn process(&mut self) -> Result<()> {
    ///     // Read latest frame from input
    ///     if let Some(frame) = self.input_port.read_latest() {
    ///         // Apply effect
    ///         let processed = self.apply_effect(&frame)?;
    ///
    ///         // Write to output
    ///         self.output_port.write(processed);
    ///     }
    ///     Ok(())
    /// }
    /// ```
    fn process(&mut self) -> Result<()>;

    /// Get processor descriptor (static)
    ///
    /// Returns metadata for AI/MCP discovery.
    /// Called once during registration.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// fn descriptor() -> Option<ProcessorDescriptor> {
    ///     Some(
    ///         ProcessorDescriptor::new(
    ///             "ReverbEffect",
    ///             "Applies reverb effect to audio"
    ///         )
    ///         .with_input(PortDescriptor::new("audio", SCHEMA_AUDIO_FRAME, ...))
    ///         .with_output(PortDescriptor::new("audio", SCHEMA_AUDIO_FRAME, ...))
    ///         .with_tags(vec!["transform", "audio", "effect", "reverb"])
    ///     )
    /// }
    /// ```
    fn descriptor() -> Option<ProcessorDescriptor>
    where
        Self: Sized;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{AudioFrame, schema::SCHEMA_AUDIO_FRAME, schema::PortDescriptor};

    #[derive(Serialize, Deserialize)]
    struct MockTransformConfig {
        gain: f32,
    }

    struct MockTransform {
        name: String,
        gain: f32,
        process_count: u64,
    }

    impl StreamElement for MockTransform {
        fn name(&self) -> &str {
            &self.name
        }

        fn element_type(&self) -> ElementType {
            ElementType::Transform
        }

        fn descriptor(&self) -> Option<ProcessorDescriptor> {
            None
        }

        fn input_ports(&self) -> Vec<PortDescriptor> {
            vec![PortDescriptor {
                name: "audio".to_string(),
                schema: SCHEMA_AUDIO_FRAME.clone(),
                required: true,
                description: "Audio input".to_string(),
            }]
        }

        fn output_ports(&self) -> Vec<PortDescriptor> {
            vec![PortDescriptor {
                name: "audio".to_string(),
                schema: SCHEMA_AUDIO_FRAME.clone(),
                required: true,
                description: "Processed audio output".to_string(),
            }]
        }
    }

    impl StreamTransform for MockTransform {
        type Config = MockTransformConfig;

        fn from_config(config: Self::Config) -> Result<Self> {
            Ok(Self {
                name: "mock_transform".to_string(),
                gain: config.gain,
                process_count: 0,
            })
        }

        fn process(&mut self) -> Result<()> {
            self.process_count += 1;
            Ok(())
        }

        fn descriptor() -> Option<ProcessorDescriptor> {
            None
        }
    }

    #[test]
    fn test_from_config() {
        let config = MockTransformConfig { gain: 1.5 };
        let transform = MockTransform::from_config(config).unwrap();
        assert_eq!(transform.gain, 1.5);
        assert_eq!(transform.name(), "mock_transform");
    }

    #[test]
    fn test_process() {
        let config = MockTransformConfig { gain: 1.0 };
        let mut transform = MockTransform::from_config(config).unwrap();

        assert_eq!(transform.process_count, 0);
        transform.process().unwrap();
        assert_eq!(transform.process_count, 1);
        transform.process().unwrap();
        assert_eq!(transform.process_count, 2);
    }

    #[test]
    fn test_element_type() {
        let config = MockTransformConfig { gain: 1.0 };
        let transform = MockTransform::from_config(config).unwrap();
        assert_eq!(transform.element_type(), ElementType::Transform);
    }

    #[test]
    fn test_input_output_ports() {
        let config = MockTransformConfig { gain: 1.0 };
        let transform = MockTransform::from_config(config).unwrap();

        let inputs = transform.input_ports();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].name, "audio");

        let outputs = transform.output_ports();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].name, "audio");
    }
}
