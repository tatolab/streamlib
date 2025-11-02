//! StreamSource - Trait for data generators (sources)
//!
//! Sources are processors that generate data without consuming inputs.
//! They are the starting points of processing pipelines.
//!
//! ## Design Philosophy
//!
//! Inspired by GStreamer's GstBaseSrc, sources:
//! - Have no inputs, only outputs
//! - Run in continuous loops (scheduled by runtime)
//! - Generate data on demand
//! - Can synchronize to clocks (audio clock, vsync, software)
//!
//! ## Source Types
//!
//! - **Hardware sources**: Camera, microphone (callback-driven)
//! - **Software sources**: Test tone generator, pattern generator (loop-driven)
//! - **Network sources**: RTP receiver, WebRTC peer (async-driven)
//!
//! ## Scheduling
//!
//! Sources use declarative scheduling via `#[scheduling(...)]` attributes:
//!
//! ```rust,ignore
//! #[derive(StreamSource)]
//! #[scheduling(mode = "loop", clock = "audio", rate_hz = 23.44)]
//! struct TestToneGenerator {
//!     #[output()]
//!     audio: StreamOutput<AudioFrame>,
//!     frequency: f64,
//! }
//! ```
//!
//! The runtime handles:
//! - Loop execution at specified rate
//! - Clock synchronization
//! - Timestamp generation
//! - Backpressure handling
//!
//! ## Usage Example
//!
//! ```rust,ignore
//! use streamlib::{TestToneGenerator, TestToneConfig, StreamRuntime};
//!
//! let mut runtime = StreamRuntime::new();
//!
//! let tone = runtime.add_processor_with_config::<TestToneGenerator>(
//!     TestToneConfig {
//!         frequency: 440.0,
//!         amplitude: 0.5,
//!         sample_rate: 48000,
//!         timer_group_id: Some("audio_master".to_string()),
//!     }
//! )?;
//!
//! runtime.start().await?;
//! ```

use super::{StreamElement, ElementType};
use crate::core::error::Result;
use crate::core::schema::ProcessorDescriptor;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Configuration for source scheduling
///
/// Determines how the runtime executes this source's generate() method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulingConfig {
    /// Scheduling mode
    pub mode: SchedulingMode,

    /// Clock source for synchronization
    pub clock: ClockSource,

    /// Rate in Hz (for loop mode)
    ///
    /// Example: 23.44 Hz for audio at 48kHz/2048 buffer
    pub rate_hz: Option<f64>,

    /// Whether this source provides the clock for the pipeline
    ///
    /// Typically true for audio output (CoreAudio callback drives timing)
    pub provide_clock: bool,
}

impl Default for SchedulingConfig {
    fn default() -> Self {
        Self {
            mode: SchedulingMode::Loop,
            clock: ClockSource::Software,
            rate_hz: Some(60.0), // Default 60 Hz
            provide_clock: false,
        }
    }
}

/// Scheduling mode for sources
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchedulingMode {
    /// Continuous loop at specified rate
    ///
    /// Runtime spawns thread calling generate() in loop.
    /// Used by: TestToneGenerator, pattern generators
    Loop,

    /// Hardware callback-driven
    ///
    /// Hardware (CoreAudio, V4L2) calls generate() when data ready.
    /// Used by: Camera, microphone (hardware determines timing)
    Callback,

    /// Reactive to external events
    ///
    /// Source waits for network packets, file I/O, etc.
    /// Used by: RTP receiver, file reader
    Reactive,

    /// Pull-based (on-demand)
    ///
    /// Only generates when downstream requests data.
    /// Used by: Image loader, database query
    Pull,
}

/// Clock source for synchronization
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClockSource {
    /// Audio hardware clock (sample-accurate)
    ///
    /// Provided by CoreAudio, ALSA, WASAPI
    Audio,

    /// Video vsync clock (frame-accurate)
    ///
    /// Provided by CVDisplayLink, DRM vsync
    Vsync,

    /// Software clock (CPU timestamps)
    ///
    /// Used when no hardware clock available
    Software,

    /// Custom clock (user-provided)
    ///
    /// For specialized timing (genlock, PTP)
    Custom(String),
}

/// Trait for data source processors
///
/// Sources generate data without consuming inputs. They are the starting
/// points of processing pipelines.
///
/// ## Implementation Requirements
///
/// 1. Implement `StreamElement` base trait
/// 2. Implement `generate()` to produce data
/// 3. Define scheduling configuration
/// 4. Provide descriptor for AI/MCP discovery
///
/// ## Example
///
/// ```rust,ignore
/// use streamlib::core::traits::{StreamElement, StreamSource, ElementType};
/// use streamlib::core::{AudioFrame, ProcessorDescriptor};
/// use streamlib::core::error::Result;
///
/// struct TestToneGenerator {
///     name: String,
///     frequency: f64,
///     phase: f64,
///     sample_rate: u32,
/// }
///
/// impl StreamElement for TestToneGenerator {
///     fn name(&self) -> &str { &self.name }
///     fn element_type(&self) -> ElementType { ElementType::Source }
///     fn descriptor(&self) -> Option<ProcessorDescriptor> {
///         TestToneGenerator::descriptor()
///     }
/// }
///
/// impl StreamSource for TestToneGenerator {
///     type Output = AudioFrame;
///     type Config = TestToneConfig;
///
///     fn from_config(config: Self::Config) -> Result<Self> {
///         Ok(Self {
///             name: "test_tone".to_string(),
///             frequency: config.frequency,
///             phase: 0.0,
///             sample_rate: config.sample_rate,
///         })
///     }
///
///     fn generate(&mut self) -> Result<Self::Output> {
///         // Generate audio frame
///         let mut samples = Vec::new();
///         // ... fill samples ...
///         Ok(AudioFrame { samples, /* ... */ })
///     }
///
///     fn scheduling_config(&self) -> SchedulingConfig {
///         SchedulingConfig {
///             mode: SchedulingMode::Loop,
///             clock: ClockSource::Audio,
///             rate_hz: Some(23.44), // 48000 / 2048
///             provide_clock: false,
///         }
///     }
///
///     fn descriptor() -> Option<ProcessorDescriptor> {
///         // Return processor metadata
///         None
///     }
/// }
/// ```
pub trait StreamSource: StreamElement {
    /// Output data type
    ///
    /// Must implement PortMessage trait for serialization/transport.
    type Output: crate::core::ports::PortMessage;

    /// Configuration type
    ///
    /// Used by `from_config()` constructor.
    type Config: Serialize + for<'de> Deserialize<'de>;

    /// Create source from configuration
    ///
    /// Called by runtime when adding processor.
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Configuration is invalid
    /// - Resources cannot be allocated
    /// - Hardware is unavailable
    fn from_config(config: Self::Config) -> Result<Self>
    where
        Self: Sized;

    /// Generate one data unit
    ///
    /// Called by runtime loop/callback to produce data.
    /// Should be fast - avoid blocking operations.
    ///
    /// # Timing
    ///
    /// For loop-based sources, called at rate_hz frequency.
    /// For callback sources, called when hardware has data.
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Data generation fails (transient - runtime will retry)
    /// - Hardware error occurs (may trigger reconnection)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// fn generate(&mut self) -> Result<AudioFrame> {
    ///     let samples = vec![0.0; 2048];
    ///     // Fill samples with tone
    ///     for i in 0..2048 {
    ///         let t = self.phase + i as f64 / self.sample_rate as f64;
    ///         samples[i] = (t * self.frequency * 2.0 * PI).sin() as f32;
    ///     }
    ///     self.phase += 2048.0 / self.sample_rate as f64;
    ///
    ///     Ok(AudioFrame {
    ///         samples,
    ///         timestamp_ns: /* runtime provides */,
    ///         sample_rate: self.sample_rate,
    ///         channels: 2,
    ///     })
    /// }
    /// ```
    fn generate(&mut self) -> Result<Self::Output>;

    /// Get scheduling configuration
    ///
    /// Tells runtime how to execute this source:
    /// - Loop at fixed rate
    /// - Callback-driven by hardware
    /// - Reactive to events
    /// - Pull-based on demand
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// fn scheduling_config(&self) -> SchedulingConfig {
    ///     SchedulingConfig {
    ///         mode: SchedulingMode::Loop,
    ///         clock: ClockSource::Audio,
    ///         rate_hz: Some(48000.0 / 2048.0), // 23.44 Hz
    ///         provide_clock: false,
    ///     }
    /// }
    /// ```
    fn scheduling_config(&self) -> SchedulingConfig;

    /// Get clock sync point
    ///
    /// Returns the time offset for clock synchronization.
    /// Used by runtime to align multiple sources.
    ///
    /// Default: zero offset (no sync adjustment)
    ///
    /// # GStreamer Equivalent
    ///
    /// This is similar to GstClock's get_time() - provides a reference
    /// point for synchronizing buffers across sources.
    fn clock_sync_point(&self) -> Duration {
        Duration::ZERO
    }

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
    ///             "TestToneGenerator",
    ///             "Generates sine wave test tones"
    ///         )
    ///         .with_output(PortDescriptor::new("audio", SCHEMA_AUDIO_FRAME, ...))
    ///         .with_tags(vec!["source", "audio", "test"])
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
    struct MockSourceConfig {
        frequency: f64,
    }

    struct MockSource {
        name: String,
        frequency: f64,
    }

    impl StreamElement for MockSource {
        fn name(&self) -> &str {
            &self.name
        }

        fn element_type(&self) -> ElementType {
            ElementType::Source
        }

        fn descriptor(&self) -> Option<ProcessorDescriptor> {
            None
        }

        fn output_ports(&self) -> Vec<PortDescriptor> {
            vec![PortDescriptor {
                name: "audio".to_string(),
                schema: SCHEMA_AUDIO_FRAME.clone(),
                required: true,
                description: "Test audio output".to_string(),
            }]
        }
    }

    impl StreamSource for MockSource {
        type Output = AudioFrame;
        type Config = MockSourceConfig;

        fn from_config(config: Self::Config) -> Result<Self> {
            Ok(Self {
                name: "mock_source".to_string(),
                frequency: config.frequency,
            })
        }

        fn generate(&mut self) -> Result<Self::Output> {
            Ok(AudioFrame::new(
                vec![0.0; 2048],
                0,  // timestamp_ns
                0,  // frame_number
                48000,  // sample_rate
                2,  // channels
            ))
        }

        fn scheduling_config(&self) -> SchedulingConfig {
            SchedulingConfig {
                mode: SchedulingMode::Loop,
                clock: ClockSource::Audio,
                rate_hz: Some(23.44),
                provide_clock: false,
            }
        }

        fn descriptor() -> Option<ProcessorDescriptor> {
            None
        }
    }

    #[test]
    fn test_from_config() {
        let config = MockSourceConfig { frequency: 440.0 };
        let source = MockSource::from_config(config).unwrap();
        assert_eq!(source.frequency, 440.0);
        assert_eq!(source.name(), "mock_source");
    }

    #[test]
    fn test_generate() {
        let config = MockSourceConfig { frequency: 440.0 };
        let mut source = MockSource::from_config(config).unwrap();
        let frame = source.generate().unwrap();
        assert_eq!(frame.samples.len(), 2048);
        assert_eq!(frame.sample_rate, 48000);
        assert_eq!(frame.channels, 2);
    }

    #[test]
    fn test_scheduling_config() {
        let config = MockSourceConfig { frequency: 440.0 };
        let source = MockSource::from_config(config).unwrap();
        let sched = source.scheduling_config();
        assert_eq!(sched.mode, SchedulingMode::Loop);
        assert_eq!(sched.clock, ClockSource::Audio);
        assert_eq!(sched.rate_hz, Some(23.44));
        assert!(!sched.provide_clock);
    }

    #[test]
    fn test_clock_sync_point() {
        let config = MockSourceConfig { frequency: 440.0 };
        let source = MockSource::from_config(config).unwrap();
        assert_eq!(source.clock_sync_point(), Duration::ZERO);
    }

    #[test]
    fn test_element_type() {
        let config = MockSourceConfig { frequency: 440.0 };
        let source = MockSource::from_config(config).unwrap();
        assert_eq!(source.element_type(), ElementType::Source);
    }

    #[test]
    fn test_scheduling_mode_equality() {
        assert_eq!(SchedulingMode::Loop, SchedulingMode::Loop);
        assert_ne!(SchedulingMode::Loop, SchedulingMode::Callback);
    }

    #[test]
    fn test_clock_source_equality() {
        assert_eq!(ClockSource::Audio, ClockSource::Audio);
        assert_ne!(ClockSource::Audio, ClockSource::Vsync);
        assert_eq!(
            ClockSource::Custom("ptp".to_string()),
            ClockSource::Custom("ptp".to_string())
        );
    }
}
