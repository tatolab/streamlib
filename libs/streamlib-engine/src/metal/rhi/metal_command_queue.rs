// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Metal command queue wrapper for RHI.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::MTLCommandQueue;

use crate::core::{Result, StreamError};

use super::MetalCommandBuffer;

/// Metal command queue wrapper.
///
/// Wraps MTLCommandQueue and provides command buffer creation.
/// Command queues are long-lived and shared across all processors.
pub struct MetalCommandQueue {
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
}

impl MetalCommandQueue {
    /// Create a new wrapper from an existing Metal command queue.
    pub fn new(queue: Retained<ProtocolObject<dyn MTLCommandQueue>>) -> Self {
        Self { queue }
    }

    /// Create a new command buffer from this queue.
    ///
    /// Command buffers are single-use: create, record, commit.
    pub fn create_command_buffer(&self) -> Result<MetalCommandBuffer> {
        let cmd_buffer = self
            .queue
            .commandBuffer()
            .ok_or_else(|| StreamError::GpuError("Failed to create Metal command buffer".into()))?;
        Ok(MetalCommandBuffer::new(cmd_buffer))
    }

    /// Get the underlying Metal command queue protocol object.
    pub fn queue(&self) -> &ProtocolObject<dyn MTLCommandQueue> {
        &self.queue
    }

    /// Get the raw Metal command queue reference for interop with the `metal` crate.
    pub fn queue_ref(&self) -> &metal::CommandQueueRef {
        use metal::foreign_types::ForeignTypeRef;
        let obj_ptr =
            &*self.queue as *const ProtocolObject<dyn MTLCommandQueue> as *mut std::ffi::c_void;
        // SAFETY: The Retained keeps the queue alive for the lifetime of self
        unsafe { metal::CommandQueueRef::from_ptr(obj_ptr as *mut _) }
    }

    /// Clone the queue handle.
    pub fn clone_queue(&self) -> Retained<ProtocolObject<dyn MTLCommandQueue>> {
        Retained::clone(&self.queue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metal::MetalDevice;

    #[test]
    fn test_command_buffer_creation() {
        let device = MetalDevice::new().expect("Metal device");
        let queue = MetalCommandQueue::new(device.clone_command_queue());
        let cmd_buffer = queue.create_command_buffer();
        assert!(cmd_buffer.is_ok());
    }

    #[test]
    fn test_multiple_command_buffers() {
        let device = MetalDevice::new().expect("Metal device");
        let queue = MetalCommandQueue::new(device.clone_command_queue());

        // Should be able to create multiple command buffers from same queue
        let cb1 = queue.create_command_buffer().expect("cb1");
        let cb2 = queue.create_command_buffer().expect("cb2");
        let cb3 = queue.create_command_buffer().expect("cb3");

        cb1.commit();
        cb2.commit();
        cb3.commit();
    }
}
