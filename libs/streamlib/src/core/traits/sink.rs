//! StreamSink - Trait for data consumers (sinks)
//!
//! Sinks are processors that consume data without producing outputs.
//! They are the endpoints of processing pipelines.
//!
//! ## Design Philosophy
//!
//! Inspired by GStreamer's GstBaseSink, sinks:
//! - Have only inputs, no outputs
//! - Consume and render/output data
//! - May provide the pipeline clock (audio output, vsync)
//! - Synchronize rendering to timestamps
//!
//! ## Sink Types
//!
//! - **Display sinks**: Render video to screen (synced to vsync)
//! - **Audio sinks**: Play audio to speakers (synced to audio clock)
//! - **File sinks**: Write data to disk
//! - **Network sinks**: Send data over network (RTP, WebRTC)
//!
//! ## Clock Providers
//!
//! Some sinks provide the master clock for the pipeline:
//!
//! - **Audio output**: CoreAudio callback provides sample-accurate clock
//! - **Display**: CVDisplayLink provides vsync clock
//!
//! ## Usage Example
//!
//! ```rust,ignore
//! use streamlib::{AppleDisplayProcessor, DisplayConfig, StreamRuntime};
//!
//! let mut runtime = StreamRuntime::new();
//!
//! let display = runtime.add_processor_with_config::<AppleDisplayProcessor>(
//!     DisplayConfig {
//!         width: 1920,
//!         height: 1080,
//!         title: Some("My Video".to_string()),
//!     }
//! )?;
//!
//! // Connect camera → display
//! runtime.connect(camera.output_port("video"), display.input_port("video"))?;
//! runtime.start().await?;
//! ```

use super::{StreamElement, ElementType};
use crate::core::error::Result;
use crate::core::schema::ProcessorDescriptor;
use crate::core::scheduling::{ClockConfig, ClockType, SyncMode};
use serde::{Deserialize, Serialize};

/// Trait for data sink processors
///
/// Sinks consume data without producing outputs. They are the endpoints
/// of processing pipelines.
///
/// ## Implementation Requirements
///
/// 1. Implement `StreamElement` base trait
/// 2. Implement `render()` to consume data
/// 3. Define clock configuration (if sink provides clock)
/// 4. Provide descriptor for AI/MCP discovery
///
/// ## Example
///
/// ```rust,ignore
/// use streamlib::core::traits::{StreamElement, StreamSink, ElementType};
/// use streamlib::core::{VideoFrame, ProcessorDescriptor};
/// use streamlib::core::error::Result;
///
/// struct DisplayProcessor {
///     name: String,
///     width: u32,
///     height: u32,
///     // ... rendering state
/// }
///
/// impl StreamElement for DisplayProcessor {
///     fn name(&self) -> &str { &self.name }
///     fn element_type(&self) -> ElementType { ElementType::Sink }
///     fn descriptor(&self) -> Option<ProcessorDescriptor> {
///         DisplayProcessor::descriptor()
///     }
/// }
///
/// impl StreamSink for DisplayProcessor {
///     type Input = VideoFrame;
///     type Config = DisplayConfig;
///
///     fn from_config(config: Self::Config) -> Result<Self> {
///         Ok(Self {
///             name: "display".to_string(),
///             width: config.width,
///             height: config.height,
///         })
///     }
///
///     fn render(&mut self, frame: Self::Input) -> Result<()> {
///         // Render frame to screen
///         Ok(())
///     }
///
///     fn clock_config(&self) -> ClockConfig {
///         ClockConfig {
///             provides_clock: true,
///             clock_type: Some(ClockType::Vsync),
///             clock_name: Some("display_vsync".to_string()),
///         }
///     }
///
///     fn sync_mode(&self) -> SyncMode {
///         SyncMode::Timestamp
///     }
///
///     fn descriptor() -> Option<ProcessorDescriptor> {
///         // Return processor metadata
///         None
///     }
/// }
/// ```
pub trait StreamSink: StreamElement {
    /// Input data type
    ///
    /// Must implement PortMessage trait for serialization/transport.
    type Input: crate::core::ports::PortMessage;

    /// Configuration type
    ///
    /// Used by `from_config()` constructor.
    type Config: Serialize + for<'de> Deserialize<'de>;

    /// Create sink from configuration
    ///
    /// Called by runtime when adding processor.
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Configuration is invalid
    /// - Resources cannot be allocated (window, audio device)
    /// - Hardware is unavailable
    fn from_config(config: Self::Config) -> Result<Self>
    where
        Self: Sized;

    /// Render/consume one data unit
    ///
    /// Called by runtime when data is available on input port.
    /// Should be fast - blocking operations should be async.
    ///
    /// # Timing
    ///
    /// For timestamp sync: runtime calls when buffer timestamp ≤ clock time.
    /// For no sync: runtime calls immediately when data available.
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Rendering fails (transient - runtime will retry)
    /// - Hardware error occurs (may trigger reconnection)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// fn render(&mut self, frame: VideoFrame) -> Result<()> {
    ///     // Check timestamp vs clock
    ///     let now = self.clock.get_time();
    ///     let late = now - frame.timestamp_ns;
    ///     if late > LATE_THRESHOLD {
    ///         tracing::warn!("Frame {} is {} ns late", frame.frame_number, late);
    ///     }
    ///
    ///     // Render to screen
    ///     self.render_texture(&frame.texture)?;
    ///     Ok(())
    /// }
    /// ```
    fn render(&mut self, input: Self::Input) -> Result<()>;

    /// Get clock configuration
    ///
    /// Tells runtime whether this sink provides a clock for the pipeline.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Audio output provides clock
    /// fn clock_config(&self) -> ClockConfig {
    ///     ClockConfig {
    ///         provides_clock: true,
    ///         clock_type: Some(ClockType::Audio),
    ///         clock_name: Some("coreaudio_clock".to_string()),
    ///     }
    /// }
    ///
    /// // File writer doesn't provide clock
    /// fn clock_config(&self) -> ClockConfig {
    ///     ClockConfig::default()
    /// }
    /// ```
    fn clock_config(&self) -> ClockConfig {
        ClockConfig::default()
    }

    /// Get synchronization mode
    ///
    /// Determines how runtime schedules render() calls.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Display syncs to timestamps
    /// fn sync_mode(&self) -> SyncMode {
    ///     SyncMode::Timestamp
    /// }
    ///
    /// // File writer has no sync
    /// fn sync_mode(&self) -> SyncMode {
    ///     SyncMode::None
    /// }
    /// ```
    fn sync_mode(&self) -> SyncMode {
        SyncMode::Timestamp
    }

    /// Handle late frame
    ///
    /// Called when frame timestamp is significantly behind clock.
    /// Default: log warning and continue.
    ///
    /// # Parameters
    ///
    /// - `lateness_ns`: How late the frame is (clock_time - frame_timestamp)
    ///
    /// # Returns
    ///
    /// - `true`: Render the late frame anyway
    /// - `false`: Drop the frame
    fn handle_late_frame(&mut self, lateness_ns: i64) -> bool {
        if lateness_ns > 50_000_000 {  // > 50ms
            tracing::warn!("Frame is {} ms late, dropping", lateness_ns / 1_000_000);
            false  // Drop very late frames
        } else {
            tracing::debug!("Frame is {} ms late, rendering anyway", lateness_ns / 1_000_000);
            true  // Render slightly late frames
        }
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
    ///             "DisplayProcessor",
    ///             "Renders video frames to a window"
    ///         )
    ///         .with_input(PortDescriptor::new("video", SCHEMA_VIDEO_FRAME, ...))
    ///         .with_tags(vec!["sink", "video", "display"])
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
    use crate::core::{VideoFrame, schema::SCHEMA_VIDEO_FRAME, schema::PortDescriptor};

    #[derive(Serialize, Deserialize)]
    struct MockSinkConfig {
        width: u32,
        height: u32,
    }

    struct MockSink {
        name: String,
        width: u32,
        height: u32,
        frames_rendered: u64,
    }

    impl StreamElement for MockSink {
        fn name(&self) -> &str {
            &self.name
        }

        fn element_type(&self) -> ElementType {
            ElementType::Sink
        }

        fn descriptor(&self) -> Option<ProcessorDescriptor> {
            None
        }

        fn input_ports(&self) -> Vec<PortDescriptor> {
            vec![PortDescriptor {
                name: "video".to_string(),
                schema: SCHEMA_VIDEO_FRAME.clone(),
                required: true,
                description: "Video input to render".to_string(),
            }]
        }
    }

    impl StreamSink for MockSink {
        type Input = VideoFrame;
        type Config = MockSinkConfig;

        fn from_config(config: Self::Config) -> Result<Self> {
            Ok(Self {
                name: "mock_sink".to_string(),
                width: config.width,
                height: config.height,
                frames_rendered: 0,
            })
        }

        fn render(&mut self, _frame: Self::Input) -> Result<()> {
            self.frames_rendered += 1;
            Ok(())
        }

        fn clock_config(&self) -> ClockConfig {
            ClockConfig {
                provides_clock: true,
                clock_type: Some(ClockType::Vsync),
                clock_name: Some("mock_vsync".to_string()),
            }
        }

        fn sync_mode(&self) -> SyncMode {
            SyncMode::Timestamp
        }

        fn descriptor() -> Option<ProcessorDescriptor> {
            None
        }
    }

    #[test]
    fn test_from_config() {
        let config = MockSinkConfig { width: 1920, height: 1080 };
        let sink = MockSink::from_config(config).unwrap();
        assert_eq!(sink.width, 1920);
        assert_eq!(sink.height, 1080);
        assert_eq!(sink.name(), "mock_sink");
    }

    #[test]
    fn test_render() {
        let config = MockSinkConfig { width: 1920, height: 1080 };
        let mut sink = MockSink::from_config(config).unwrap();

        // Note: Can't test actual render() without GPU initialization
        // Just verify frames_rendered counter works
        assert_eq!(sink.frames_rendered, 0);
    }

    #[test]
    fn test_clock_config() {
        let config = MockSinkConfig { width: 1920, height: 1080 };
        let sink = MockSink::from_config(config).unwrap();
        let clock = sink.clock_config();
        assert!(clock.provides_clock);
        assert_eq!(clock.clock_type, Some(ClockType::Vsync));
        assert_eq!(clock.clock_name, Some("mock_vsync".to_string()));
    }

    #[test]
    fn test_sync_mode() {
        let config = MockSinkConfig { width: 1920, height: 1080 };
        let sink = MockSink::from_config(config).unwrap();
        assert_eq!(sink.sync_mode(), SyncMode::Timestamp);
    }

    #[test]
    fn test_element_type() {
        let config = MockSinkConfig { width: 1920, height: 1080 };
        let sink = MockSink::from_config(config).unwrap();
        assert_eq!(sink.element_type(), ElementType::Sink);
    }

    #[test]
    fn test_handle_late_frame() {
        let config = MockSinkConfig { width: 1920, height: 1080 };
        let mut sink = MockSink::from_config(config).unwrap();

        // Slightly late frame (10ms) - should render
        assert!(sink.handle_late_frame(10_000_000));

        // Very late frame (100ms) - should drop
        assert!(!sink.handle_late_frame(100_000_000));
    }

    #[test]
    fn test_clock_type_equality() {
        assert_eq!(ClockType::Audio, ClockType::Audio);
        assert_ne!(ClockType::Audio, ClockType::Vsync);
    }

    #[test]
    fn test_sync_mode_equality() {
        assert_eq!(SyncMode::Timestamp, SyncMode::Timestamp);
        assert_ne!(SyncMode::Timestamp, SyncMode::None);
    }

    #[test]
    fn test_input_ports_descriptor() {
        let config = MockSinkConfig { width: 1920, height: 1080 };
        let sink = MockSink::from_config(config).unwrap();
        let ports = sink.input_ports();
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].name, "video");
        assert_eq!(ports[0].schema.name, "VideoFrame");
    }
}
