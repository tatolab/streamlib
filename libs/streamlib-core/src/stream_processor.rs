use crate::clock::TimedTick;
use crate::gpu_context::GpuContext;
use crate::Result;

pub trait StreamProcessor: Send + 'static {
    fn process(&mut self, tick: TimedTick) -> Result<()>;

    /// Called when the processor starts, passing the shared GPU context
    ///
    /// Processors receive the GPU context here and should store it for
    /// use in their process() method. The GPU context contains the shared
    /// WebGPU device and queue that all processors must use.
    fn on_start(&mut self, gpu_context: &GpuContext) -> Result<()> {
        let _ = gpu_context; // Allow unused parameter for default implementation
        Ok(())
    }

    fn on_stop(&mut self) -> Result<()> {
        Ok(())
    }
}
