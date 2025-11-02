use super::stream_processor::StreamProcessor;
use super::port_consumer::PortConsumer;
use super::super::context::RuntimeContext;
use super::super::schema::ProcessorDescriptor;
use super::super::Result;
use std::any::Any;

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
    fn on_start_dyn(&mut self, ctx: &RuntimeContext) -> Result<()>;

    /// Called when the processor stops - lifecycle hook
    fn on_stop_dyn(&mut self) -> Result<()>;

    /// Enable downcasting to concrete processor types
    fn as_any_mut_dyn(&mut self) -> &mut dyn Any;

    /// Set the wakeup channel for push-based operation
    fn set_wakeup_channel_dyn(&mut self, wakeup_tx: crossbeam_channel::Sender<super::super::runtime::WakeupEvent>);

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
    fn set_output_wakeup_dyn(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<super::super::runtime::WakeupEvent>);
}

// Blanket implementation: All StreamProcessor types automatically implement DynStreamProcessor
impl<T: StreamProcessor> DynStreamProcessor for T {
    fn process_dyn(&mut self) -> Result<()> {
        self.process()
    }

    fn on_start_dyn(&mut self, ctx: &RuntimeContext) -> Result<()> {
        self.on_start(&ctx.gpu)
    }

    fn on_stop_dyn(&mut self) -> Result<()> {
        self.on_stop()
    }

    fn as_any_mut_dyn(&mut self) -> &mut dyn Any {
        self.as_any_mut()
    }

    fn set_wakeup_channel_dyn(&mut self, wakeup_tx: crossbeam_channel::Sender<super::super::runtime::WakeupEvent>) {
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

    fn set_output_wakeup_dyn(&mut self, port_name: &str, wakeup_tx: crossbeam_channel::Sender<super::super::runtime::WakeupEvent>) {
        // Delegate to StreamProcessor method
        self.set_output_wakeup(port_name, wakeup_tx)
    }
}
