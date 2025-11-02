use super::port_consumer::PortConsumer;
use super::super::context::GpuContext;
use super::super::schema::ProcessorDescriptor;
use super::super::Result;
use std::any::Any;

/// StreamProcessor trait
///
/// All processors must implement this trait. Processors are the core
/// building blocks of streamlib pipelines.
///
/// # Configuration
///
/// Each processor has an associated `Config` type that defines its construction parameters.
/// Use `EmptyConfig` for processors that don't need configuration.
///
/// # Lifecycle
///
/// 1. `from_config()` - Construct processor from config
/// 2. `on_start()` - Called once when processor thread starts
/// 3. `process()` - Called for each wakeup event
/// 4. `on_stop()` - Called once when processor shuts down
///
/// # AI Discovery
///
/// Processors can optionally implement `descriptor()` to provide metadata
/// for AI agents. This enables:
/// - Runtime discovery of available processors
/// - Understanding of processor capabilities
/// - Automatic validation of connections
/// - Code generation by AI agents
pub trait StreamProcessor: Send + 'static {
    /// Configuration type for this processor
    ///
    /// Define the parameters needed to construct this processor.
    /// Use `EmptyConfig` if no configuration is needed.
    ///
    /// # Example
    ///
    /// ```ignore
    /// impl StreamProcessor for CameraProcessor {
    ///     type Config = CameraConfig;
    ///     // ...
    /// }
    /// ```
    type Config: Default + Send + 'static;

    /// Construct processor from configuration
    ///
    /// This is called by `runtime.add_processor()` to instantiate the processor.
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration parameters for this processor
    ///
    /// # Example
    ///
    /// ```ignore
    /// fn from_config(config: Self::Config) -> Result<Self> {
    ///     // Construct processor from config
    ///     Ok(CameraProcessor::new(config.device_id)?)
    /// }
    /// ```
    fn from_config(config: Self::Config) -> Result<Self>
    where
        Self: Sized;
    /// Process a wakeup event
    ///
    /// Called when the processor receives a wakeup event (DataAvailable, TimerTick, etc.).
    /// Processors should:
    /// - Read inputs from their input ports
    /// - Perform their processing (GPU operations, ML inference, etc.)
    /// - Write outputs to their output ports
    ///
    /// Processors no longer receive tick objects - they wake on events only.
    fn process(&mut self) -> Result<()>;

    /// Called when the processor starts, passing the shared GPU context
    ///
    /// Processors receive the GPU context here and should store it for
    /// use in their process() method. The GPU context contains the shared
    /// WebGPU device and queue that all processors must use.
    ///
    /// # Arguments
    ///
    /// * `gpu_context` - Shared GPU device/queue for all processors
    fn on_start(&mut self, gpu_context: &GpuContext) -> Result<()> {
        let _ = gpu_context; // Allow unused parameter for default implementation
        Ok(())
    }

    /// Called when the processor stops
    ///
    /// Use this to clean up resources (close files, release GPU buffers, etc.)
    fn on_stop(&mut self) -> Result<()> {
        Ok(())
    }

    /// Return processor metadata for AI discovery (optional)
    ///
    /// Implement this to make your processor discoverable by AI agents.
    /// The descriptor includes:
    /// - Name and description
    /// - Input/output port schemas
    /// - Usage context (when to use this processor)
    /// - Examples
    /// - Tags for semantic search
    ///
    /// # Example
    ///
    /// ```ignore
    /// fn descriptor() -> Option<ProcessorDescriptor> {
    ///     Some(ProcessorDescriptor::new(
    ///         "ObjectDetector",
    ///         "Detects objects in video frames using YOLOv8"
    ///     )
    ///     .with_usage_context("Use for identifying objects, people, or animals in real-time video")
    ///     .with_input(PortDescriptor::new(
    ///         "video",
    ///         SCHEMA_VIDEO_FRAME.clone(),
    ///         true,
    ///         "Input video frame to analyze"
    ///     ))
    ///     .with_output(PortDescriptor::new(
    ///         "detections",
    ///         SCHEMA_OBJECT_DETECTIONS.clone(),
    ///         true,
    ///         "Detected objects with bounding boxes"
    ///     ))
    ///     .with_tags(vec!["ml", "vision", "detection"]))
    /// }
    /// ```
    fn descriptor() -> Option<ProcessorDescriptor>
    where
        Self: Sized,
    {
        None
    }

    /// Get processor descriptor for this instance (for runtime validation)
    ///
    /// This is the instance version of `descriptor()` that can be called
    /// on trait objects. Used by the runtime for audio requirement validation
    /// and other runtime checks.
    ///
    /// Default implementation returns None. Processors should override this
    /// to return their descriptor.
    fn descriptor_instance(&self) -> Option<ProcessorDescriptor> {
        None
    }

    /// Enable downcasting to concrete processor types
    ///
    /// This method enables dynamic connections at runtime by allowing
    /// the runtime to downcast trait objects to their concrete types.
    /// This is required for accessing type-specific ports (e.g., CameraOutputPorts).
    ///
    /// # Implementation
    ///
    /// All processors must implement this as:
    /// ```ignore
    /// fn as_any_mut(&mut self) -> &mut dyn Any {
    ///     self
    /// }
    /// ```
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// Set the wakeup channel for push-based operation (optional)
    ///
    /// Push-based processors (audio capture, camera, etc.) can implement this
    /// to receive their wakeup channel. They can then trigger their own processing
    /// when hardware data arrives, instead of waiting for ticks.
    ///
    /// # Arguments
    ///
    /// * `wakeup_tx` - Channel sender to trigger WakeupEvent::DataAvailable
    ///
    /// # Example
    ///
    /// ```ignore
    /// fn set_wakeup_channel(&mut self, wakeup_tx: crossbeam_channel::Sender<WakeupEvent>) {
    ///     self.wakeup_tx = Some(wakeup_tx);
    /// }
    /// ```
    ///
    /// Then in hardware callback:
    /// ```ignore
    /// if let Some(tx) = &self.wakeup_tx {
    ///     let _ = tx.send(WakeupEvent::DataAvailable);
    /// }
    /// ```
    fn set_wakeup_channel(&mut self, _wakeup_tx: crossbeam_channel::Sender<super::super::runtime::WakeupEvent>) {
        // Default: no-op for processors that don't need push-based wakeup
    }

    /// Extract a consumer from an output port for dynamic wiring
    ///
    /// This method is called by the DynStreamProcessor blanket impl to support
    /// platform-agnostic port wiring. Processors with output ports should override this.
    ///
    /// # Arguments
    ///
    /// * `port_name` - Name of the output port (e.g., "video", "audio")
    ///
    /// # Returns
    ///
    /// Some(PortConsumer) if port exists and consumer is available, None otherwise
    fn take_output_consumer(&mut self, _port_name: &str) -> Option<PortConsumer> {
        None  // Default: no output ports
    }

    /// Connect a consumer to an input port for dynamic wiring
    ///
    /// This method is called by the DynStreamProcessor blanket impl to support
    /// platform-agnostic port wiring. Processors with input ports should override this.
    ///
    /// # Arguments
    ///
    /// * `port_name` - Name of the input port (e.g., "video", "audio")
    /// * `consumer` - The type-erased consumer from the upstream output port
    ///
    /// # Returns
    ///
    /// true if port exists and connection succeeded, false otherwise
    fn connect_input_consumer(&mut self, _port_name: &str, _consumer: PortConsumer) -> bool {
        false  // Default: no input ports
    }

    /// Set wakeup channel on an output port for push-based notifications
    ///
    /// This method is called by the DynStreamProcessor blanket impl to support
    /// platform-agnostic wakeup channel wiring. Processors with output ports should override this.
    ///
    /// # Arguments
    ///
    /// * `port_name` - Name of the output port (e.g., "video", "audio")
    /// * `wakeup_tx` - Channel to send WakeupEvent::DataAvailable when data is written
    fn set_output_wakeup(&mut self, _port_name: &str, _wakeup_tx: crossbeam_channel::Sender<super::super::runtime::WakeupEvent>) {
        // Default: no output ports, do nothing
    }
}
