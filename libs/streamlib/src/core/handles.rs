//! Processor and Port Handle Types
//!
//! Handles provide type-safe references to processors and ports after they've been
//! added to the runtime. They enable the config-based API pattern where processors
//! are added before connections are made.

use std::marker::PhantomData;
use super::PortMessage;

/// Unique identifier for a processor in the runtime
pub type ProcessorId = String;

/// Handle to a processor that has been added to the runtime
///
/// This handle is returned by `add_processor()` and can be used to reference
/// the processor's ports for making connections.
///
/// # Example
///
/// ```ignore
/// let camera = runtime.add_processor::<CameraProcessor>(())?;
/// let display = runtime.add_processor::<DisplayProcessor>(())?;
///
/// runtime.connect(
///     camera.output_port::<VideoFrame>("video")?,
///     display.input_port::<VideoFrame>("video")?
/// )?;
/// ```
#[derive(Debug, Clone)]
pub struct ProcessorHandle {
    /// Unique identifier for this processor in the runtime
    pub(crate) id: ProcessorId,
}

impl ProcessorHandle {
    /// Create a new processor handle
    pub(crate) fn new(id: ProcessorId) -> Self {
        Self { id }
    }

    /// Get the processor ID
    pub fn id(&self) -> &ProcessorId {
        &self.id
    }

    /// Get a type-safe reference to an output port
    ///
    /// # Arguments
    ///
    /// * `name` - Port name (e.g., "video", "audio")
    ///
    /// # Type Parameters
    ///
    /// * `T` - The message type for this port (e.g., VideoFrame, AudioFrame)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let video_out = camera.output_port::<VideoFrame>("video")?;
    /// ```
    pub fn output_port<T: PortMessage>(&self, name: &str) -> OutputPortRef<T> {
        OutputPortRef {
            processor_id: self.id.clone(),
            port_name: name.to_string(),
            _phantom: PhantomData,
        }
    }

    /// Get a type-safe reference to an input port
    ///
    /// # Arguments
    ///
    /// * `name` - Port name (e.g., "video", "audio")
    ///
    /// # Type Parameters
    ///
    /// * `T` - The message type for this port (e.g., VideoFrame, AudioFrame)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let video_in = display.input_port::<VideoFrame>("video")?;
    /// ```
    pub fn input_port<T: PortMessage>(&self, name: &str) -> InputPortRef<T> {
        InputPortRef {
            processor_id: self.id.clone(),
            port_name: name.to_string(),
            _phantom: PhantomData,
        }
    }
}

/// Type-safe reference to an output port
///
/// This reference includes:
/// - The processor ID that owns the port
/// - The port name
/// - Type information for compile-time safety
///
/// The type parameter `T` ensures that connections are type-safe - you can only
/// connect an `OutputPortRef<VideoFrame>` to an `InputPortRef<VideoFrame>`.
#[derive(Debug, Clone)]
pub struct OutputPortRef<T: PortMessage> {
    /// ID of the processor that owns this port
    pub(crate) processor_id: ProcessorId,
    /// Name of the port (e.g., "video", "audio")
    pub(crate) port_name: String,
    /// Type marker for compile-time type safety
    _phantom: PhantomData<T>,
}

impl<T: PortMessage> OutputPortRef<T> {
    /// Get the processor ID
    pub fn processor_id(&self) -> &ProcessorId {
        &self.processor_id
    }

    /// Get the port name
    pub fn port_name(&self) -> &str {
        &self.port_name
    }
}

/// Type-safe reference to an input port
///
/// This reference includes:
/// - The processor ID that owns the port
/// - The port name
/// - Type information for compile-time safety
///
/// The type parameter `T` ensures that connections are type-safe - you can only
/// connect an `OutputPortRef<VideoFrame>` to an `InputPortRef<VideoFrame>`.
#[derive(Debug, Clone)]
pub struct InputPortRef<T: PortMessage> {
    /// ID of the processor that owns this port
    pub(crate) processor_id: ProcessorId,
    /// Name of the port (e.g., "video", "audio")
    pub(crate) port_name: String,
    /// Type marker for compile-time type safety
    _phantom: PhantomData<T>,
}

impl<T: PortMessage> InputPortRef<T> {
    /// Get the processor ID
    pub fn processor_id(&self) -> &ProcessorId {
        &self.processor_id
    }

    /// Get the port name
    pub fn port_name(&self) -> &str {
        &self.port_name
    }
}

/// Information about a pending connection that will be wired during runtime.start()
///
/// This stores connection details in a type-erased way. During start(), the runtime:
/// 1. Looks up both processors from the registry
/// 2. Uses PortProvider trait to access their ports
/// 3. Transfers the rtrb::Consumer from output to input
/// 4. Establishes lock-free data flow through the ring buffer
#[derive(Debug, Clone)]
pub(crate) struct PendingConnection {
    /// Source processor ID
    pub source_processor_id: ProcessorId,
    /// Source port name
    pub source_port_name: String,
    /// Destination processor ID
    pub dest_processor_id: ProcessorId,
    /// Destination port name
    pub dest_port_name: String,
}

impl PendingConnection {
    pub fn new(
        source_processor_id: ProcessorId,
        source_port_name: String,
        dest_processor_id: ProcessorId,
        dest_port_name: String,
    ) -> Self {
        Self {
            source_processor_id,
            source_port_name,
            dest_processor_id,
            dest_port_name,
        }
    }
}
