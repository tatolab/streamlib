use super::clock::TimedTick;
use super::gpu_context::GpuContext;
use super::schema::ProcessorDescriptor;
use super::ports::{StreamInput, StreamOutput};
use super::VideoFrame;
use super::Result;
use std::any::Any;

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
}

/// Helper trait for port access (kept separate from StreamProcessor for object safety)
///
/// Enables generic port access by name for dynamic runtime connections.
/// This trait allows the runtime to connect ports without knowing the
/// concrete processor type at compile time.
///
/// # Purpose
///
/// With PortProvider, the runtime can:
/// - Connect any processor types (Camera, Display, Python, ML, etc.)
/// - Access ports by name dynamically
/// - Maintain a unified connection API instead of hardcoded downcasts
///
/// # Design
///
/// Uses a closure-based API to support different port storage patterns:
/// - Native processors (Camera, Display) provide direct &mut references to struct fields
/// - Dynamic processors (PythonProcessor) temporarily lock Arc<Mutex<>> to provide access
///
/// The closure pattern ensures mutable access is always properly scoped and released.
///
/// # Example
///
/// ```ignore
/// // Native processor with struct fields
/// impl PortProvider for MyProcessor {
///     fn with_video_output_mut<F, R>(&mut self, name: &str, f: F) -> Option<R>
///     where F: FnOnce(&mut StreamOutput<VideoFrame>) -> R
///     {
///         match name {
///             "video" => Some(f(&mut self.output_ports.video)),
///             _ => None,
///         }
///     }
///
///     fn with_video_input_mut<F, R>(&mut self, name: &str, f: F) -> Option<R>
///     where F: FnOnce(&mut StreamInput<VideoFrame>) -> R
///     {
///         match name {
///             "video" => Some(f(&mut self.input_ports.video)),
///             _ => None,
///         }
///     }
/// }
///
/// // Dynamic processor with HashMap<String, Arc<Mutex<Port>>>
/// impl PortProvider for PythonProcessor {
///     fn with_video_output_mut<F, R>(&mut self, name: &str, f: F) -> Option<R>
///     where F: FnOnce(&mut StreamOutput<VideoFrame>) -> R
///     {
///         let port_arc = self.output_ports.ports.get(name)?;
///         let mut port = port_arc.lock().unwrap();
///         Some(f(&mut *port))
///     }
/// }
/// ```
pub trait PortProvider {
    /// Provide temporary mutable access to a video output port by name
    ///
    /// # Arguments
    ///
    /// * `name` - Port name (e.g., "video", "output", "processed")
    /// * `f` - Closure that receives mutable access to the port
    ///
    /// # Returns
    ///
    /// Some(R) if port exists and closure was called, None if port not found
    ///
    /// # Example
    ///
    /// ```ignore
    /// processor.with_video_output_mut("video", |output| {
    ///     output.write(frame);
    /// });
    /// ```
    fn with_video_output_mut<F, R>(&mut self, name: &str, f: F) -> Option<R>
    where
        F: FnOnce(&mut StreamOutput<VideoFrame>) -> R;

    /// Provide temporary mutable access to a video input port by name
    ///
    /// # Arguments
    ///
    /// * `name` - Port name (e.g., "video", "input", "source")
    /// * `f` - Closure that receives mutable access to the port
    ///
    /// # Returns
    ///
    /// Some(R) if port exists and closure was called, None if port not found
    ///
    /// # Example
    ///
    /// ```ignore
    /// processor.with_video_input_mut("video", |input| {
    ///     input.connect(upstream_buffer);
    /// });
    /// ```
    fn with_video_input_mut<F, R>(&mut self, name: &str, f: F) -> Option<R>
    where
        F: FnOnce(&mut StreamInput<VideoFrame>) -> R;
}
