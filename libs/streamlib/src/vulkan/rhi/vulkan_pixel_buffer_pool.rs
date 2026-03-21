// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::core::rhi::{PixelBufferPoolId, PixelFormat, RhiPixelBuffer, RhiPixelBufferRef};
use crate::core::{Result, StreamError};

use super::{VulkanDevice, VulkanPixelBuffer};

/// Reusable pool of [`VulkanPixelBuffer`]s for efficient buffer recycling.
pub struct VulkanPixelBufferPool {
    device: Arc<VulkanDevice>,
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
    format: PixelFormat,
    buffers: Vec<Arc<VulkanPixelBuffer>>,
    next_index: AtomicUsize,
    buffer_to_pool_id: Mutex<HashMap<usize, PixelBufferPoolId>>,
}

impl VulkanPixelBufferPool {
    /// Create a new pool, pre-allocating the given number of buffers.
    pub fn new(
        device: Arc<VulkanDevice>,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        format: PixelFormat,
        pre_allocate: usize,
    ) -> Result<Self> {
        let mut buffers = Vec::with_capacity(pre_allocate);
        let mut buffer_to_pool_id = HashMap::with_capacity(pre_allocate);

        for i in 0..pre_allocate {
            let buffer = VulkanPixelBuffer::new(&device, width, height, bytes_per_pixel, format)?;
            buffers.push(Arc::new(buffer));
            buffer_to_pool_id.insert(i, PixelBufferPoolId::new());
        }

        Ok(Self {
            device,
            width,
            height,
            bytes_per_pixel,
            format,
            buffers,
            next_index: AtomicUsize::new(0),
            buffer_to_pool_id: Mutex::new(buffer_to_pool_id),
        })
    }

    /// Acquire a buffer from the pool via ring-cycling.
    ///
    /// Skips buffers still held externally (Arc::strong_count > 1).
    /// Returns error if all buffers are in use.
    pub fn acquire(&self) -> Result<(PixelBufferPoolId, RhiPixelBuffer)> {
        let len = self.buffers.len();
        if len == 0 {
            return Err(StreamError::BufferError(
                "VulkanPixelBufferPool has no buffers".into(),
            ));
        }

        let start = self.next_index.fetch_add(1, Ordering::Relaxed) % len;

        for offset in 0..len {
            let index = (start + offset) % len;
            let buffer = &self.buffers[index];

            // strong_count == 1 means only the pool holds it — it's available
            if Arc::strong_count(buffer) == 1 {
                let pool_id = {
                    let map = self.buffer_to_pool_id.lock().unwrap();
                    map.get(&index)
                        .cloned()
                        .unwrap_or_else(PixelBufferPoolId::new)
                };

                let pixel_buffer_ref = RhiPixelBufferRef {
                    inner: Arc::clone(buffer),
                };

                let rhi_pixel_buffer = RhiPixelBuffer::new(pixel_buffer_ref);

                return Ok((pool_id, rhi_pixel_buffer));
            }
        }

        Err(StreamError::BufferError(
            "All VulkanPixelBufferPool buffers are in use".into(),
        ))
    }
}

// Safety: All fields are Send + Sync (Arc, AtomicUsize, Mutex)
unsafe impl Send for VulkanPixelBufferPool {}
unsafe impl Sync for VulkanPixelBufferPool {}
