use crate::clock::TimedTick;
use crate::gpu_context::GpuContext;
use crate::schema::ProcessorDescriptor;
use crate::Result;

/// StreamProcessor trait
///
/// All processors must implement this trait. Processors are the core
/// building blocks of streamlib pipelines.
///
/// # Lifecycle
///
/// 1. `on_start()` - Called once when processor thread starts
/// 2. `process()` - Called for each tick/frame
/// 3. `on_stop()` - Called once when processor shuts down
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
    /// Process a single tick/frame
    ///
    /// This is called on every clock tick. Processors should:
    /// - Read inputs from their input ports
    /// - Perform their processing (GPU operations, ML inference, etc.)
    /// - Write outputs to their output ports
    fn process(&mut self, tick: TimedTick) -> Result<()>;

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
}
