//! Object-safe trait for runtime interaction with stream elements
//!
//! Provides type-erased dispatch to StreamSource/StreamSink/StreamTransform methods.
//! This allows the runtime to store all elements as `Box<dyn DynStreamElement>`
//! and call the appropriate specialized methods based on element type.
//!
//! ## Design
//!
//! Inspired by GStreamer's GstElement virtual method dispatch:
//! - Runtime calls `dispatch_dyn()` on each wakeup
//! - `dispatch_dyn()` routes to `process()` for all element types (unified API)
//! - Type-safe downcasting via `as_source()`, `as_sink()`, `as_transform()`
//!
//! ## Blanket Implementation
//!
//! Any type implementing `StreamElement` automatically gets `DynStreamElement` for free.

use crate::core::{RuntimeContext, Result};
use crate::core::traits::PortConsumer;
use crate::core::schema::ProcessorDescriptor;
use crate::core::runtime::WakeupEvent;
use crate::core::traits::ElementType;

/// Object-safe runtime interface for stream elements
///
/// This trait provides type-erased methods that the runtime can call
/// without knowing the concrete element type. Automatically implemented
/// for all types that implement `StreamElement`.
///
/// ## Usage
///
/// ```ignore
/// let mut element: Box<dyn DynStreamElement> = Box::new(my_camera);
///
/// // Runtime lifecycle
/// element.start_dyn(&runtime_context)?;
/// element.dispatch_dyn()?;  // Calls appropriate method based on type
/// element.stop_dyn()?;
/// ```
pub trait DynStreamElement: Send + 'static {
    // ============================================================
    // Lifecycle Methods
    // ============================================================

    /// Start the element with runtime context
    fn start_dyn(&mut self, ctx: &RuntimeContext) -> Result<()>;

    /// Stop the element
    fn stop_dyn(&mut self) -> Result<()>;

    // ============================================================
    // Dispatch Method (GStreamer-inspired)
    // ============================================================

    /// Dispatch to appropriate processing method based on element type
    ///
    /// This is the main execution method called by the runtime.
    /// **All element types now use the unified `process()` method:**
    /// - Sources: `process()` generates data and writes to output ports
    /// - Sinks: `process()` reads from input ports and renders/consumes data
    /// - Transforms: `process()` reads inputs, transforms, and writes outputs
    ///
    /// This unified API simplifies runtime dispatch - just call `process()` on everything.
    ///
    /// Returns Ok(()) if processing succeeded or no data available.
    fn dispatch_dyn(&mut self) -> Result<()>;

    // ============================================================
    // Port Wiring (Dynamic)
    // ============================================================

    /// Extract a consumer from an output port for dynamic wiring
    fn take_output_consumer_dyn(&mut self, port_name: &str) -> Option<PortConsumer>;

    /// Connect a consumer to an input port for dynamic wiring
    fn connect_input_consumer_dyn(&mut self, port_name: &str, consumer: PortConsumer) -> bool;

    /// Set wakeup channel on an output port for push-based notifications
    fn set_output_wakeup_dyn(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<WakeupEvent>);

    /// Set wakeup channel for processor-level wakeup (optional)
    fn set_wakeup_channel_dyn(&mut self, wakeup_tx: crossbeam_channel::Sender<WakeupEvent>);

    // ============================================================
    // Introspection
    // ============================================================

    /// Get element type for dispatch routing
    fn element_type_dyn(&self) -> ElementType;

    /// Get processor descriptor (if available)
    fn descriptor_dyn(&self) -> Option<ProcessorDescriptor>;

    /// Get element name for logging
    fn name_dyn(&self) -> &str;

    // ============================================================
    // Type-Safe Downcasting
    // ============================================================

    /// Downcast to any type (for accessing concrete processor fields)
    fn as_any_mut_dyn(&mut self) -> &mut dyn std::any::Any;
}
