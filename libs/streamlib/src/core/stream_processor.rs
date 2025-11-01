use super::gpu_context::GpuContext;
use super::schema::ProcessorDescriptor;
use super::ports::{StreamInput, StreamOutput};
use super::{VideoFrame, AudioFrame};
use super::Result;
use std::any::Any;

/// Type-erased port consumer for dynamic port wiring
///
/// This enum allows the runtime to transfer consumers between ports
/// without knowing the specific message type at compile time.
/// Each variant wraps a typed rtrb::Consumer for a specific message type.
pub enum PortConsumer {
    /// Video frame consumer (GPU textures)
    Video(rtrb::Consumer<VideoFrame>),
    /// Audio frame consumer (sample buffers)
    Audio(rtrb::Consumer<AudioFrame>),
    // Future: Add more variants as needed (Data, ML, etc.)
}

/// Object-safe runtime interface for processors
///
/// This trait contains only the runtime execution methods and is object-safe
/// (no associated types, no `Self: Sized` bounds). This allows processors to be
/// stored as `Box<dyn DynStreamProcessor>` at runtime.
///
/// You don't implement this directly - it's automatically implemented for all
/// types that implement `StreamProcessor`.
pub trait DynStreamProcessor: Send + 'static {
    /// Process a wakeup event - runtime execution method
    fn process_dyn(&mut self) -> Result<()>;

    /// Called when the processor starts - lifecycle hook
    fn on_start_dyn(&mut self, gpu_context: &GpuContext) -> Result<()>;

    /// Called when the processor stops - lifecycle hook
    fn on_stop_dyn(&mut self) -> Result<()>;

    /// Enable downcasting to concrete processor types
    fn as_any_mut_dyn(&mut self) -> &mut dyn Any;

    /// Set the wakeup channel for push-based operation
    fn set_wakeup_channel_dyn(&mut self, wakeup_tx: crossbeam_channel::Sender<super::runtime::WakeupEvent>);

    /// Get processor descriptor for this instance (for runtime validation)
    fn descriptor_instance_dyn(&self) -> Option<ProcessorDescriptor>;

    /// Extract a consumer from an output port for dynamic wiring
    ///
    /// This method is used by the runtime during wire_pending_connections() to transfer
    /// the rtrb consumer from an output port to an input port.
    ///
    /// # Arguments
    ///
    /// * `port_name` - Name of the output port (e.g., "video", "audio")
    ///
    /// # Returns
    ///
    /// Some(PortConsumer) if port exists and consumer is available, None otherwise
    ///
    /// # Platform Agnostic
    ///
    /// This method is object-safe and works across all platforms without downcasting.
    fn take_output_consumer_dyn(&mut self, port_name: &str) -> Option<PortConsumer>;

    /// Connect a consumer to an input port for dynamic wiring
    ///
    /// This method is used by the runtime during wire_pending_connections() to establish
    /// the lock-free data flow between processors.
    ///
    /// # Arguments
    ///
    /// * `port_name` - Name of the input port (e.g., "video", "audio")
    /// * `consumer` - The type-erased consumer from the upstream output port
    ///
    /// # Returns
    ///
    /// true if port exists and connection succeeded, false otherwise
    ///
    /// # Platform Agnostic
    ///
    /// This method is object-safe and works across all platforms without downcasting.
    fn connect_input_consumer_dyn(&mut self, port_name: &str, consumer: PortConsumer) -> bool;

    /// Set wakeup channel on an output port for push-based notifications
    ///
    /// This method is used by the runtime during wire_pending_connections() to connect
    /// output ports to downstream processors' wakeup channels.
    ///
    /// # Arguments
    ///
    /// * `port_name` - Name of the output port (e.g., "video", "audio")
    /// * `wakeup_tx` - Channel to send WakeupEvent::DataAvailable when data is written
    ///
    /// # Platform Agnostic
    ///
    /// This method is object-safe and works across all platforms without downcasting.
    fn set_output_wakeup_dyn(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<super::runtime::WakeupEvent>);
}

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
    fn set_wakeup_channel(&mut self, _wakeup_tx: crossbeam_channel::Sender<super::runtime::WakeupEvent>) {
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
    fn set_output_wakeup(&mut self, _port_name: &str, _wakeup_tx: crossbeam_channel::Sender<super::runtime::WakeupEvent>) {
        // Default: no output ports, do nothing
    }
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

// Blanket implementation: All StreamProcessor types automatically implement DynStreamProcessor
impl<T: StreamProcessor> DynStreamProcessor for T {
    fn process_dyn(&mut self) -> Result<()> {
        self.process()
    }

    fn on_start_dyn(&mut self, gpu_context: &GpuContext) -> Result<()> {
        self.on_start(gpu_context)
    }

    fn on_stop_dyn(&mut self) -> Result<()> {
        self.on_stop()
    }

    fn as_any_mut_dyn(&mut self) -> &mut dyn Any {
        self.as_any_mut()
    }

    fn set_wakeup_channel_dyn(&mut self, wakeup_tx: crossbeam_channel::Sender<super::runtime::WakeupEvent>) {
        self.set_wakeup_channel(wakeup_tx)
    }

    fn descriptor_instance_dyn(&self) -> Option<ProcessorDescriptor> {
        self.descriptor_instance()
    }

    fn take_output_consumer_dyn(&mut self, port_name: &str) -> Option<PortConsumer> {
        // Delegate to StreamProcessor method
        self.take_output_consumer(port_name)
    }

    fn connect_input_consumer_dyn(&mut self, port_name: &str, consumer: PortConsumer) -> bool {
        // Delegate to StreamProcessor method
        self.connect_input_consumer(port_name, consumer)
    }

    fn set_output_wakeup_dyn(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<super::runtime::WakeupEvent>) {
        // Delegate to StreamProcessor method
        self.set_output_wakeup(port_name, wakeup_tx)
    }
}
