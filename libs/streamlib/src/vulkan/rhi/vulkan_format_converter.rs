// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use ash::vk;

use crate::core::rhi::{PixelFormat, RhiPixelBuffer};
use crate::core::{Result, StreamError};

/// Vulkan format converter for pixel buffer format conversion.
pub struct VulkanFormatConverter {
    device: ash::Device,
    #[allow(dead_code)]
    queue: vk::Queue,
    #[allow(dead_code)]
    queue_family_index: u32,
    command_pool: vk::CommandPool,
    source_bytes_per_pixel: u32,
    dest_bytes_per_pixel: u32,
}

impl VulkanFormatConverter {
    /// Create a new format converter with a dedicated command pool.
    pub fn new(
        device: &ash::Device,
        queue: vk::Queue,
        queue_family_index: u32,
        source_bytes_per_pixel: u32,
        dest_bytes_per_pixel: u32,
    ) -> Result<Self> {
        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(queue_family_index)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);

        let command_pool = unsafe { device.create_command_pool(&pool_info, None) }
            .map_err(|e| StreamError::GpuError(format!("Failed to create command pool: {e}")))?;

        Ok(Self {
            device: device.clone(),
            queue,
            queue_family_index,
            command_pool,
            source_bytes_per_pixel,
            dest_bytes_per_pixel,
        })
    }

    /// Convert pixel data from source buffer to destination buffer.
    ///
    /// Phase 1: CPU-based conversion via mapped GPU staging buffers.
    /// Supports NV12 ↔ RGBA/BGRA conversions for codec I/O.
    pub fn convert(&self, source: &RhiPixelBuffer, dest: &RhiPixelBuffer) -> Result<()> {
        let src_ref = source.buffer_ref();
        let dst_ref = dest.buffer_ref();
        let src_vk = &src_ref.inner;
        let dst_vk = &dst_ref.inner;

        let width = source.width;
        let height = source.height;
        let src_format = src_vk.format();
        let dst_format = dst_vk.format();
        let src_ptr = src_vk.mapped_ptr();
        let dst_ptr = dst_vk.mapped_ptr();

        if width != dest.width || height != dest.height {
            return Err(StreamError::GpuError(
                "Source and destination buffers must have the same dimensions".into(),
            ));
        }

        match (src_format, dst_format) {
            // RGBA/BGRA → NV12
            (
                PixelFormat::Rgba32 | PixelFormat::Bgra32,
                PixelFormat::Nv12VideoRange | PixelFormat::Nv12FullRange,
            ) => {
                let is_bgra = matches!(src_format, PixelFormat::Bgra32);
                let full_range = matches!(dst_format, PixelFormat::Nv12FullRange);

                unsafe {
                    let y_plane = dst_ptr;
                    let uv_plane = dst_ptr.add((width * height) as usize);

                    for row in 0..height {
                        for col in 0..width {
                            let px_offset = ((row * width + col) * 4) as usize;
                            let (r, g, b) = if is_bgra {
                                (
                                    *src_ptr.add(px_offset + 2),
                                    *src_ptr.add(px_offset + 1),
                                    *src_ptr.add(px_offset),
                                )
                            } else {
                                (
                                    *src_ptr.add(px_offset),
                                    *src_ptr.add(px_offset + 1),
                                    *src_ptr.add(px_offset + 2),
                                )
                            };

                            let (rf, gf, bf) = (r as f32, g as f32, b as f32);

                            let y = if full_range {
                                (0.299 * rf + 0.587 * gf + 0.114 * bf).clamp(0.0, 255.0)
                            } else {
                                (16.0
                                    + 65.481 * rf / 255.0
                                    + 128.553 * gf / 255.0
                                    + 24.966 * bf / 255.0)
                                    .clamp(16.0, 235.0)
                            };
                            *y_plane.add((row * width + col) as usize) = y as u8;

                            // Subsample UV at 2x2 (write on even row/col)
                            if row % 2 == 0 && col % 2 == 0 {
                                let u = if full_range {
                                    (-0.14713 * rf - 0.28886 * gf + 0.436 * bf + 128.0)
                                        .clamp(0.0, 255.0)
                                } else {
                                    (128.0 - 37.797 * rf / 255.0 - 74.203 * gf / 255.0
                                        + 112.0 * bf / 255.0)
                                        .clamp(16.0, 240.0)
                                };
                                let v = if full_range {
                                    (0.615 * rf - 0.51499 * gf - 0.10001 * bf + 128.0)
                                        .clamp(0.0, 255.0)
                                } else {
                                    (128.0 + 112.0 * rf / 255.0
                                        - 93.786 * gf / 255.0
                                        - 18.214 * bf / 255.0)
                                        .clamp(16.0, 240.0)
                                };
                                let uv_offset = ((row / 2) * width + col) as usize;
                                *uv_plane.add(uv_offset) = u as u8;
                                *uv_plane.add(uv_offset + 1) = v as u8;
                            }
                        }
                    }
                }
                Ok(())
            }
            // NV12 → RGBA/BGRA
            (
                PixelFormat::Nv12VideoRange | PixelFormat::Nv12FullRange,
                PixelFormat::Rgba32 | PixelFormat::Bgra32,
            ) => {
                let is_bgra = matches!(dst_format, PixelFormat::Bgra32);
                let full_range = matches!(src_format, PixelFormat::Nv12FullRange);

                unsafe {
                    let y_plane = src_ptr;
                    let uv_plane = src_ptr.add((width * height) as usize);

                    for row in 0..height {
                        for col in 0..width {
                            let y_val = *y_plane.add((row * width + col) as usize) as f32;
                            let uv_offset = ((row / 2) * width + (col & !1)) as usize;
                            let u_val = *uv_plane.add(uv_offset) as f32;
                            let v_val = *uv_plane.add(uv_offset + 1) as f32;

                            let (r, g, b) = if full_range {
                                let c = y_val;
                                let d = u_val - 128.0;
                                let e = v_val - 128.0;
                                (
                                    (c + 1.402 * e).clamp(0.0, 255.0),
                                    (c - 0.344136 * d - 0.714136 * e).clamp(0.0, 255.0),
                                    (c + 1.772 * d).clamp(0.0, 255.0),
                                )
                            } else {
                                let c = y_val - 16.0;
                                let d = u_val - 128.0;
                                let e = v_val - 128.0;
                                (
                                    (1.164 * c + 1.596 * e).clamp(0.0, 255.0),
                                    (1.164 * c - 0.392 * d - 0.813 * e).clamp(0.0, 255.0),
                                    (1.164 * c + 2.017 * d).clamp(0.0, 255.0),
                                )
                            };

                            let px_offset = ((row * width + col) * 4) as usize;
                            if is_bgra {
                                *dst_ptr.add(px_offset) = b as u8;
                                *dst_ptr.add(px_offset + 1) = g as u8;
                                *dst_ptr.add(px_offset + 2) = r as u8;
                                *dst_ptr.add(px_offset + 3) = 255;
                            } else {
                                *dst_ptr.add(px_offset) = r as u8;
                                *dst_ptr.add(px_offset + 1) = g as u8;
                                *dst_ptr.add(px_offset + 2) = b as u8;
                                *dst_ptr.add(px_offset + 3) = 255;
                            }
                        }
                    }
                }
                Ok(())
            }
            _ => Err(StreamError::NotSupported(format!(
                "Unsupported format conversion: {:?} → {:?}",
                src_format, dst_format
            ))),
        }
    }

    /// Source format bytes per pixel.
    pub fn source_bytes_per_pixel(&self) -> u32 {
        self.source_bytes_per_pixel
    }

    /// Destination format bytes per pixel.
    pub fn dest_bytes_per_pixel(&self) -> u32 {
        self.dest_bytes_per_pixel
    }
}

impl Drop for VulkanFormatConverter {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_command_pool(self.command_pool, None);
        }
    }
}

// Safety: Vulkan handles are thread-safe
unsafe impl Send for VulkanFormatConverter {}
unsafe impl Sync for VulkanFormatConverter {}
