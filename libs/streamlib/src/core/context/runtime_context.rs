//! Runtime context passed to stream elements during initialization
//!
//! Provides access to shared runtime resources like GPU context, clocks,
//! and potentially allocators, shared buffers, etc. in the future.

use super::GpuContext;
use std::sync::Arc;

/// Runtime context passed to elements during initialization
///
/// This follows GStreamer's GstContext pattern - elements receive this
/// once during start() and can store whatever they need from it.
///
/// # Future Expansion
///
/// This struct will grow to include:
/// - Clock references for sync
/// - Memory allocators for zero-copy
/// - Shared buffer pools
/// - Performance monitoring hooks
#[derive(Clone)]
pub struct RuntimeContext {
    /// Shared GPU context (device + queue)
    pub gpu: GpuContext,

    // Future fields (commented out until needed):
    // pub clock: Arc<dyn Clock>,
    // pub allocator: Arc<dyn Allocator>,
    // pub buffer_pool: Arc<BufferPool>,
}

impl RuntimeContext {
    /// Create new runtime context
    pub fn new(gpu: GpuContext) -> Self {
        Self { gpu }
    }
}
