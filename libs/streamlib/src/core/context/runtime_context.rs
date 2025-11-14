
use super::GpuContext;

#[derive(Clone)]
pub struct RuntimeContext {
    pub gpu: GpuContext,
}

impl RuntimeContext {
    pub fn new(gpu: GpuContext) -> Self {
        Self {
            gpu,
        }
    }
}
