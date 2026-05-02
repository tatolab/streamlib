// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::core::rhi::{PixelBufferPoolId, PixelFormat, RhiPixelBuffer, RhiPixelBufferRef};
use crate::core::{Result, StreamError};

use super::{HostVulkanDevice, HostVulkanPixelBuffer};

/// Reusable pool of [`HostVulkanPixelBuffer`]s for efficient buffer recycling.
pub struct VulkanPixelBufferPool {
    device: Arc<HostVulkanDevice>,
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
    format: PixelFormat,
    buffers: Vec<Arc<HostVulkanPixelBuffer>>,
    next_index: AtomicUsize,
    buffer_to_pool_id: Mutex<HashMap<usize, PixelBufferPoolId>>,
}

impl VulkanPixelBufferPool {
    /// Create a new pool, pre-allocating up to `pre_allocate` buffers.
    ///
    /// Returns successfully if AT LEAST 1 buffer was allocated. NVIDIA limits
    /// DMA-BUF exportable allocations after swapchain creation, so partial
    /// pre-allocation is acceptable — the pool degrades gracefully under
    /// memory pressure rather than failing the entire pipeline.
    pub fn new(
        device: Arc<HostVulkanDevice>,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        format: PixelFormat,
        pre_allocate: usize,
    ) -> Result<Self> {
        let mut buffers = Vec::with_capacity(pre_allocate);
        let mut buffer_to_pool_id = HashMap::with_capacity(pre_allocate);
        let mut last_err: Option<StreamError> = None;

        for i in 0..pre_allocate {
            match HostVulkanPixelBuffer::new(&device, width, height, bytes_per_pixel, format) {
                Ok(buffer) => {
                    buffers.push(Arc::new(buffer));
                    buffer_to_pool_id.insert(i, PixelBufferPoolId::new());
                }
                Err(e) => {
                    tracing::warn!(
                        "VulkanPixelBufferPool: allocation {}/{} failed: {} \
                         (likely NVIDIA DMA-BUF limit after swapchain creation)",
                        i + 1, pre_allocate, e
                    );
                    last_err = Some(e);
                    break;
                }
            }
        }

        if buffers.is_empty() {
            return Err(last_err.unwrap_or_else(|| {
                StreamError::BufferError(
                    "VulkanPixelBufferPool: failed to allocate any buffers".into(),
                )
            }));
        }

        if buffers.len() < pre_allocate {
            tracing::warn!(
                "VulkanPixelBufferPool: degraded to {} buffers (requested {}). \
                 Pipeline will run with reduced parallelism.",
                buffers.len(),
                pre_allocate
            );
        } else {
            tracing::info!(
                "VulkanPixelBufferPool: pre-allocated {} buffers ({}x{} {:?})",
                buffers.len(),
                width,
                height,
                format
            );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vulkan::rhi::HostVulkanDevice;

    #[test]
    fn test_pool_acquire_returns_buffer() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let pool = VulkanPixelBufferPool::new(
            Arc::clone(&device),
            64, 64, 4, PixelFormat::Bgra32, 3,
        )
        .expect("pool creation failed");

        let result = pool.acquire();
        assert!(result.is_ok(), "acquire must succeed on a fresh pool");

        let (pool_id, buf) = result.unwrap();
        assert_eq!(buf.width, 64);
        assert_eq!(buf.height, 64);
        assert_ne!(pool_id, PixelBufferPoolId::new(), "pool id must be stable, not a fresh zero-id");
    }

    #[test]
    fn test_pool_exhaustion_returns_error() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let pool = VulkanPixelBufferPool::new(
            Arc::clone(&device),
            64, 64, 4, PixelFormat::Bgra32, 1,
        )
        .expect("pool creation failed");

        // Hold the only buffer so all buffers are externally referenced
        let (_id, _held) = pool.acquire().expect("first acquire must succeed");

        let result = pool.acquire();
        assert!(
            result.is_err(),
            "acquire must return Err when all buffers are in use"
        );
    }

    #[test]
    fn test_pool_reuses_buffer_after_release() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let pool = VulkanPixelBufferPool::new(
            Arc::clone(&device),
            64, 64, 4, PixelFormat::Bgra32, 1,
        )
        .expect("pool creation failed");

        let (_id, buf) = pool.acquire().expect("first acquire must succeed");
        drop(buf); // release back to pool (Arc strong_count returns to 1)

        let result = pool.acquire();
        assert!(
            result.is_ok(),
            "acquire must succeed after the previously acquired buffer is released"
        );
    }
}
