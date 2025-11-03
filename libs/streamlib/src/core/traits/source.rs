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
//! #[scheduling(mode = "loop", clock = "audio")]
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
//!     }
//! )?;
//!
//! runtime.start().await?;
//! ```

use super::{StreamElement, ElementType};
use crate::core::error::Result;
use crate::core::schema::ProcessorDescriptor;
use crate::core::scheduling::{SchedulingConfig, SchedulingMode, ClockSource};
use serde::{Deserialize, Serialize};
use std::time::Duration;

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
///             provide_clock: false,
///         }
///     }
///
///     fn frame_duration_ns(&self) -> Option<i64> {
///         Some((1_000_000_000.0 / 23.44) as i64)
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
    /// For loop-based sources, called at rate calculated from buffer characteristics.
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
    ///         provide_clock: false,
    ///     }
    /// }
    ///
    /// fn frame_duration_ns(&self) -> Option<i64> {
    ///     Some((1_000_000_000.0 / 23.44) as i64)
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

    fn frame_duration_ns(&self) -> Option<i64> {
        None
    }

    fn run_source_loop(
        &mut self,
        clock: std::sync::Arc<dyn crate::core::clocks::Clock>,
        shutdown: crossbeam_channel::Receiver<()>,
    ) -> Result<()> {
        let config = self.scheduling_config();

        if config.mode != SchedulingMode::Loop {
            return Ok(());
        }

        let frame_duration_ns = self.frame_duration_ns().ok_or_else(|| {
            crate::core::error::StreamError::Configuration(
                "Loop mode sources must implement frame_duration_ns()".to_string()
            )
        })?;

        let mut next_frame_time = clock.now_ns();

        loop {
            if shutdown.try_recv().is_ok() {
                break;
            }

            let now = clock.now_ns();
            if now < next_frame_time {
                let sleep_ns = (next_frame_time - now) as u64;
                std::thread::sleep(std::time::Duration::from_nanos(sleep_ns));
            }

            if let Err(e) = self.generate() {
                tracing::error!("Source generate() error: {}", e);
            }

            next_frame_time += frame_duration_ns;

            if clock.now_ns() > next_frame_time {
                tracing::warn!("Source running behind schedule");
                next_frame_time = clock.now_ns();
            }
        }

        Ok(())
    }
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
                provide_clock: false,
                priority: crate::core::scheduling::ThreadPriority::Normal,
            }
        }

        fn frame_duration_ns(&self) -> Option<i64> {
            Some((1_000_000_000.0 / 23.44) as i64)
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
        assert!(!sched.provide_clock);
        assert_eq!(source.frame_duration_ns(), Some((1_000_000_000.0 / 23.44) as i64));
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
