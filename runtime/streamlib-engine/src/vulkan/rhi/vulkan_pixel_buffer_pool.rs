// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::core::rhi::{PixelBuffer, PixelBufferPoolSlotId, PixelBufferRef, PixelFormat};
use crate::core::{Error, Result};

use super::{HostVulkanBuffer, HostVulkanDevice};

/// One pool slot's memory in the flavour the surface-share wire carries: on
/// Linux a DMA-BUF-exportable allocation where the device exports DMA-BUF and
/// an OPAQUE_FD-exportable one where it does not, a private IOSurface on macOS.
fn allocate_pool_slot_buffer(
    device: &Arc<HostVulkanDevice>,
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
    format: PixelFormat,
) -> Result<HostVulkanBuffer> {
    #[cfg(target_os = "macos")]
    {
        HostVulkanBuffer::new_iosurface_backed_pixel_buffer(
            device,
            width,
            height,
            bytes_per_pixel,
            format,
        )
    }
    #[cfg(target_os = "linux")]
    {
        let _ = format;
        let slot_byte_size = (width as u64) * (height as u64) * (bytes_per_pixel as u64);
        if device.supports_dma_buf_export() {
            HostVulkanBuffer::new(device, slot_byte_size)
        } else {
            HostVulkanBuffer::new_opaque_fd_export(device, slot_byte_size)
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = format;
        HostVulkanBuffer::new(
            device,
            (width as u64) * (height as u64) * (bytes_per_pixel as u64),
        )
    }
}

/// Reusable pool of [`HostVulkanBuffer`]s for efficient buffer recycling.
pub struct VulkanPixelBufferPool {
    device: Arc<HostVulkanDevice>,
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
    format: PixelFormat,
    /// Each buffer under the pool id it is handed out as.
    buffers_by_pool_id: Vec<(PixelBufferPoolSlotId, Arc<HostVulkanBuffer>)>,
    next_index: AtomicUsize,
}

impl VulkanPixelBufferPool {
    /// Create a new pool, pre-allocating up to `pre_allocate` buffers.
    ///
    /// Returns successfully if at least one buffer was allocated: a later
    /// allocation refused — past the device's memory budget, say — leaves a
    /// smaller pool rather than failing the pipeline. With none allocated,
    /// the first allocation's refusal is the answer.
    pub fn new(
        device: Arc<HostVulkanDevice>,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        format: PixelFormat,
        pre_allocate: usize,
    ) -> Result<Self> {
        let mut buffers_by_pool_id = Vec::with_capacity(pre_allocate);
        let mut last_err: Option<Error> = None;

        for _ in 0..pre_allocate {
            match allocate_pool_slot_buffer(&device, width, height, bytes_per_pixel, format) {
                Ok(buffer) => {
                    buffers_by_pool_id.push((PixelBufferPoolSlotId::new(), Arc::new(buffer)));
                }
                Err(e) => {
                    last_err = Some(e);
                    break;
                }
            }
        }

        if buffers_by_pool_id.is_empty() {
            return Err(last_err.unwrap_or_else(|| {
                Error::BufferError("VulkanPixelBufferPool: failed to allocate any buffers".into())
            }));
        }

        if let Some(refusal_of_the_next_buffer) = &last_err {
            tracing::warn!(
                "VulkanPixelBufferPool: pre-allocated {} of the {} buffers requested ({}x{} {:?}); \
                 the next was refused: {}",
                buffers_by_pool_id.len(),
                pre_allocate,
                width,
                height,
                format,
                refusal_of_the_next_buffer
            );
        } else {
            tracing::info!(
                "VulkanPixelBufferPool: pre-allocated {} buffers ({}x{} {:?})",
                buffers_by_pool_id.len(),
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
            buffers_by_pool_id,
            next_index: AtomicUsize::new(0),
        })
    }

    /// Allocate one more buffer and hand it out.
    ///
    /// `pre_allocate` is a starting depth, not a ceiling: a consumer holding
    /// frames makes a producer need more slots than its ring began with. The
    /// device's memory budget bounds it; past it the allocation is refused by
    /// name.
    ///
    /// A fresh allocation, never a recycled slot, so what comes back is
    /// guaranteed free — [`Self::acquire`] cannot answer here, because a
    /// caller that keeps its own permanent share of every buffer never sees a
    /// strong count of 1 again.
    pub fn allocate_additional_buffer(&mut self) -> Result<(PixelBufferPoolSlotId, PixelBuffer)> {
        let buffer = allocate_pool_slot_buffer(
            &self.device,
            self.width,
            self.height,
            self.bytes_per_pixel,
            self.format,
        )?;

        let buffer = Arc::new(buffer);
        let pool_id = PixelBufferPoolSlotId::new();
        self.buffers_by_pool_id
            .push((pool_id.clone(), Arc::clone(&buffer)));

        Ok((
            pool_id,
            PixelBuffer::new(PixelBufferRef {
                inner: buffer,
                width: self.width,
                height: self.height,
                bytes_per_pixel: self.bytes_per_pixel,
                format: self.format,
            }),
        ))
    }

    /// Let go of the pool's share of the buffer `pool_slot_id` names, for a
    /// slot being freed; the memory goes once its last holder drops it.
    /// Answers whether the pool held such a buffer.
    pub fn forget_the_buffer_of_pool_slot(&mut self, pool_slot_id: &PixelBufferPoolSlotId) -> bool {
        let Some(forgotten_index) = self
            .buffers_by_pool_id
            .iter()
            .position(|(pool_id, _)| pool_id == pool_slot_id)
        else {
            return false;
        };
        self.buffers_by_pool_id.remove(forgotten_index);
        true
    }

    /// Acquire a buffer from the pool via ring-cycling.
    ///
    /// Skips buffers still held externally (Arc::strong_count > 1).
    /// Returns error if all buffers are in use.
    pub fn acquire(&self) -> Result<(PixelBufferPoolSlotId, PixelBuffer)> {
        let len = self.buffers_by_pool_id.len();
        if len == 0 {
            return Err(Error::BufferError(
                "VulkanPixelBufferPool has no buffers".into(),
            ));
        }

        let start = self.next_index.fetch_add(1, Ordering::Relaxed) % len;

        for offset in 0..len {
            let index = (start + offset) % len;
            let (pool_id, buffer) = &self.buffers_by_pool_id[index];

            // strong_count == 1 means only the pool holds it — it's available
            if Arc::strong_count(buffer) == 1 {
                let pool_id = pool_id.clone();

                let pixel_buffer_ref = PixelBufferRef {
                    inner: Arc::clone(buffer),
                    width: self.width,
                    height: self.height,
                    bytes_per_pixel: self.bytes_per_pixel,
                    format: self.format,
                };

                let rhi_pixel_buffer = PixelBuffer::new(pixel_buffer_ref);

                return Ok((pool_id, rhi_pixel_buffer));
            }
        }

        Err(Error::BufferError(
            "All VulkanPixelBufferPool buffers are in use".into(),
        ))
    }
}

// Safety: All fields are Send + Sync (Arc, AtomicUsize)
unsafe impl Send for VulkanPixelBufferPool {}
unsafe impl Sync for VulkanPixelBufferPool {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vulkan::rhi::HostVulkanDevice;

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn test_pool_acquire_returns_buffer() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let pool =
            VulkanPixelBufferPool::new(Arc::clone(&device), 64, 64, 4, PixelFormat::Bgra32, 3)
                .expect("pool creation failed");

        let result = pool.acquire();
        assert!(result.is_ok(), "acquire must succeed on a fresh pool");

        let (pool_id, buf) = result.unwrap();
        assert_eq!(buf.width, 64);
        assert_eq!(buf.height, 64);
        assert_ne!(
            pool_id,
            PixelBufferPoolSlotId::new(),
            "pool id must be stable, not a fresh zero-id"
        );
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn test_pool_exhaustion_returns_error() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let pool =
            VulkanPixelBufferPool::new(Arc::clone(&device), 64, 64, 4, PixelFormat::Bgra32, 1)
                .expect("pool creation failed");

        // Hold the only buffer so all buffers are externally referenced
        let (_id, _held) = pool.acquire().expect("first acquire must succeed");

        let result = pool.acquire();
        assert!(
            result.is_err(),
            "acquire must return Err when all buffers are in use"
        );
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn test_pool_reuses_buffer_after_release() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };

        let pool =
            VulkanPixelBufferPool::new(Arc::clone(&device), 64, 64, 4, PixelFormat::Bgra32, 1)
                .expect("pool creation failed");

        let (_id, buf) = pool.acquire().expect("first acquire must succeed");
        drop(buf); // release back to pool (Arc strong_count returns to 1)

        let result = pool.acquire();
        assert!(
            result.is_ok(),
            "acquire must succeed after the previously acquired buffer is released"
        );
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn forgetting_one_buffer_keeps_every_other_buffer_under_its_own_pool_id() {
        let Ok(device) = HostVulkanDevice::new() else {
            tracing::warn!("no Vulkan device, so the pool forget test skips");
            return;
        };
        let mut pool =
            VulkanPixelBufferPool::new(Arc::clone(&device), 64, 64, 4, PixelFormat::Bgra32, 1)
                .expect("pool creation failed");
        let (first_id, first_buffer) = pool.acquire().expect("the pre-allocated buffer");
        let (second_id, second_buffer) =
            pool.allocate_additional_buffer().expect("a second buffer");
        let (third_id, third_buffer) = pool.allocate_additional_buffer().expect("a third buffer");
        drop((first_buffer, third_buffer));
        assert_eq!(Arc::strong_count(&second_buffer.buffer_ref().inner), 2);

        assert!(pool.forget_the_buffer_of_pool_slot(&second_id));
        assert!(
            !pool.forget_the_buffer_of_pool_slot(&second_id),
            "a forgotten buffer is forgotten once"
        );
        assert_eq!(
            Arc::strong_count(&second_buffer.buffer_ref().inner),
            1,
            "the pool kept its share of a forgotten buffer"
        );
        let handed_out_again: Vec<_> = (0..2)
            .map(|_| pool.acquire().expect("an unheld buffer"))
            .collect();
        let ids_handed_out_again: Vec<_> = handed_out_again
            .iter()
            .map(|(pool_id, _)| pool_id)
            .collect();
        assert!(ids_handed_out_again.contains(&&first_id));
        assert!(ids_handed_out_again.contains(&&third_id));
        assert!(
            pool.acquire().is_err(),
            "the forgotten buffer was handed out again"
        );
    }
}
