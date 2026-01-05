// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Metal command buffer implementation for RHI.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLBlitCommandEncoder, MTLCommandBuffer, MTLCommandEncoder, MTLOrigin, MTLSize};

use super::MetalTexture;

/// Metal command buffer wrapper.
pub struct MetalCommandBuffer {
    command_buffer: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
}

impl MetalCommandBuffer {
    /// Create a new command buffer wrapper.
    pub fn new(command_buffer: Retained<ProtocolObject<dyn MTLCommandBuffer>>) -> Self {
        Self { command_buffer }
    }

    /// Copy one texture to another.
    pub fn copy_texture(&mut self, src: &MetalTexture, dst: &MetalTexture) {
        let encoder = self
            .command_buffer
            .blitCommandEncoder()
            .expect("Failed to create blit encoder");

        let src_texture = src.metal_texture();
        let dst_texture = dst.metal_texture();

        let size = MTLSize {
            width: src.width().min(dst.width()) as usize,
            height: src.height().min(dst.height()) as usize,
            depth: 1,
        };
        let origin = MTLOrigin { x: 0, y: 0, z: 0 };

        unsafe {
            encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                src_texture,
                0,  // source slice
                0,  // source level
                origin,
                size,
                dst_texture,
                0,  // destination slice
                0,  // destination level
                origin,
            );
        }

        encoder.endEncoding();
    }

    /// Commit the command buffer for execution.
    pub fn commit(self) {
        self.command_buffer.commit();
    }

    /// Commit and wait for completion.
    pub fn commit_and_wait(self) {
        self.command_buffer.commit();
        self.command_buffer.waitUntilCompleted();
    }

    /// Get the underlying Metal command buffer.
    pub fn as_metal_command_buffer(&self) -> &ProtocolObject<dyn MTLCommandBuffer> {
        &self.command_buffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apple::rhi::MetalDevice;
    use crate::core::rhi::{TextureDescriptor, TextureFormat, TextureUsages};
    use objc2_metal::MTLCommandQueue;

    #[test]
    fn test_command_buffer_commit() {
        let device = MetalDevice::new().expect("Metal device");
        let cmd_buffer = device
            .command_queue()
            .commandBuffer()
            .expect("command buffer");
        let wrapper = MetalCommandBuffer::new(cmd_buffer);
        wrapper.commit();
    }

    #[test]
    fn test_texture_copy() {
        let device = MetalDevice::new().expect("Metal device");

        let desc = TextureDescriptor::new(64, 64, TextureFormat::Rgba8Unorm)
            .with_usage(TextureUsages::COPY_SRC | TextureUsages::COPY_DST);
        let src = device.create_texture(&desc).expect("src texture");
        let dst = device.create_texture(&desc).expect("dst texture");

        let cmd_buffer = device
            .command_queue()
            .commandBuffer()
            .expect("command buffer");
        let mut wrapper = MetalCommandBuffer::new(cmd_buffer);
        wrapper.copy_texture(&src, &dst);
        wrapper.commit_and_wait();
    }
}
