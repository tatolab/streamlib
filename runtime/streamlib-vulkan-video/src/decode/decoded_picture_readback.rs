// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Reading a decoded picture back: the NV12 and RGBA staging buffers and draining the picture whose
//! decode was submitted last.

use std::ptr;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma::{self as vma, Alloc};

use super::SimpleDecoder;
use super::types::*;
use crate::VideoError;

impl SimpleDecoder {
    /// The buffer the next decode copies its NV12 picture into and, when it is
    /// an exported one, its ring slot — or no buffer when the RGBA converter
    /// samples the DPB slot itself.
    pub(super) fn staging_buffer_for_the_next_picture(
        &mut self,
        coded_width: u32,
        coded_height: u32,
    ) -> Result<(Option<StagingBuffer>, Option<usize>), VideoError> {
        match self.config.decoded_picture_delivery {
            DecodedPictureDelivery::ReadBackRgba => Ok((None, None)),
            DecodedPictureDelivery::ReadBackNv12 => {
                self.ensure_readback_staging(coded_width, coded_height)?;
                let &(buffer, _, _, _) = self.readback_staging.as_ref().ok_or_else(|| {
                    VideoError::BitstreamError("No readback staging buffer available".into())
                })?;
                Ok((Some(StagingBuffer { buffer }), None))
            }
            DecodedPictureDelivery::Nv12InAnExportedDmaBuf => {
                // SAFETY: the decode before this one was waited for when it
                // was drained, so no decode writes the ring.
                let (buffer, slot_index) = unsafe {
                    self.exported_nv12_dma_buf_ring.next_buffer_for_a_picture(
                        self.codec_device.instance(),
                        self.codec_device.physical_device(),
                        coded_width,
                        coded_height,
                    )?
                };
                Ok((Some(StagingBuffer { buffer }), Some(slot_index)))
            }
        }
    }

    /// Ensure the persistent readback staging buffer is large enough for the
    /// given frame dimensions. Allocates or grows as needed.
    fn ensure_readback_staging(&mut self, width: u32, height: u32) -> Result<(), VideoError> {
        let total = (width * height) as u64 + (width * height / 2) as u64;

        if self.readback_staging.as_ref().is_none_or(|s| s.2 < total) {
            if let Some((buf, alloc, _, _)) = self.readback_staging.take() {
                unsafe {
                    self.ctx.allocator().destroy_buffer(buf, alloc);
                }
            }
            let buf_info = vk::BufferCreateInfo::builder()
                .size(total)
                .usage(vk::BufferUsageFlags::TRANSFER_DST)
                .sharing_mode(vk::SharingMode::EXCLUSIVE);
            // The CPU reads this buffer back every frame, so it must not
            // land in write-combined memory: WC reads run at a few hundred
            // MB/s and one 1080p RGBA frame is 8.3 MB, which turns the
            // readback memcpy into tens of milliseconds — measured at 37 ms
            // median (225 MB/s) on NVIDIA 595.84, 93% of the whole decode
            // path. HOST_ACCESS_RANDOM declares the read access so VMA picks
            // a HOST_CACHED type, the same trap
            // `codec_utils/vulkan_bitstream_buffer_impl.rs` documents for the
            // encoder's bitstream readback.
            let alloc_opts = vma::AllocationOptions {
                flags: vma::AllocationCreateFlags::MAPPED
                    | vma::AllocationCreateFlags::HOST_ACCESS_RANDOM,
                required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                    | vk::MemoryPropertyFlags::HOST_COHERENT,
                preferred_flags: vk::MemoryPropertyFlags::HOST_CACHED,
                ..Default::default()
            };
            let (buf, alloc) = unsafe {
                self.ctx
                    .allocator()
                    .create_buffer(buf_info, &alloc_opts)
                    .map_err(VideoError::from)?
            };
            let info = self.ctx.allocator().get_allocation_info(alloc);
            self.readback_staging = Some((buf, alloc, total, info.pMappedData as *mut u8));
        }

        Ok(())
    }

    /// The RGBA staging buffer, grown to hold a W*H*4 readback: its buffer, byte size and mapping.
    fn ensure_rgba_staging(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<(vk::Buffer, u64, *mut u8), VideoError> {
        let total = (width as u64) * (height as u64) * 4;

        if let Some(&(buf, _, size, mapped)) = self.rgba_staging.as_ref().filter(|s| s.2 >= total) {
            return Ok((buf, size, mapped));
        }
        if let Some((buf, alloc, _, _)) = self.rgba_staging.take() {
            unsafe {
                self.ctx.allocator().destroy_buffer(buf, alloc);
            }
        }
        let buf_info = vk::BufferCreateInfo::builder()
            .size(total)
            .usage(vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        // Same read-back constraint as `ensure_readback_staging` above:
        // HOST_ACCESS_RANDOM + preferred HOST_CACHED, or the per-frame
        // 8.3 MB memcpy runs at write-combined speed.
        let alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::MAPPED
                | vma::AllocationCreateFlags::HOST_ACCESS_RANDOM,
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            preferred_flags: vk::MemoryPropertyFlags::HOST_CACHED,
            ..Default::default()
        };
        let (buf, alloc) = unsafe {
            self.ctx
                .allocator()
                .create_buffer(buf_info, &alloc_opts)
                .map_err(VideoError::from)?
        };
        let info = self.ctx.allocator().get_allocation_info(alloc);
        let mapped = info.pMappedData as *mut u8;
        self.rgba_staging = Some((buf, alloc, total, mapped));
        Ok((buf, total, mapped))
    }

    /// Wait for the in-flight decode to finish and read back the decoded frame.
    pub(super) fn drain_pending_frame(&mut self) -> Result<Option<SimpleDecodedFrame>, VideoError> {
        let pending = match self.pending_frame.take() {
            Some(p) => p,
            None => return Ok(None),
        };

        // Wait for the GPU to finish writing to the staging buffer
        if let Some(ref mut vk_dec) = self.vk_decoder {
            unsafe {
                vk_dec.wait_for_decode()?;
            }
        }

        // RGBA path: run NV12→RGBA GPU compute conversion, then readback RGBA
        if self.nv12_converter.is_some() {
            let dpb_image = self
                .vk_decoder
                .as_ref()
                .ok_or_else(|| VideoError::BitstreamError("VkVideoDecoder not initialized".into()))?
                .dpb_image();
            let converter = self.nv12_converter_for(&pending.decoded_picture_color_vui)?;
            let (rgba_img, _) = unsafe {
                converter.convert(
                    dpb_image,
                    pending.setup_slot as u32,
                    vk::ImageLayout::VIDEO_DECODE_DPB_KHR,
                )?
            };

            // Sized for the copy below, which is the display window alone —
            // the padding rows outside it never reach a consumer.
            let display_window = pending.display_window;
            let (stg_buf, stg_size, stg_ptr) =
                self.ensure_rgba_staging(display_window.width, display_window.height)?;

            // Copy RGBA image → staging buffer via transfer command buffer
            unsafe {
                self.device
                    .reset_command_buffer(self.transfer_cb, vk::CommandBufferResetFlags::empty())
                    .map_err(VideoError::from)?;
                self.device
                    .begin_command_buffer(
                        self.transfer_cb,
                        &vk::CommandBufferBeginInfo::builder()
                            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                    )
                    .map_err(VideoError::from)?;

                let copy_region = vk::BufferImageCopy {
                    buffer_offset: 0,
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    },
                    // The conformance window's origin, not the picture's:
                    // a stream that offsets its window would otherwise copy
                    // the padding and call it the picture.
                    image_offset: vk::Offset3D {
                        x: display_window.origin_x as i32,
                        y: display_window.origin_y as i32,
                        z: 0,
                    },
                    image_extent: vk::Extent3D {
                        width: display_window.width,
                        height: display_window.height,
                        depth: 1,
                    },
                };

                self.device.cmd_copy_image_to_buffer(
                    self.transfer_cb,
                    rgba_img,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    stg_buf,
                    &[copy_region],
                );

                self.device
                    .end_command_buffer(self.transfer_cb)
                    .map_err(VideoError::from)?;
                self.device
                    .reset_fences(&[self.transfer_fence])
                    .map_err(VideoError::from)?;
                let cb_submit = vk::CommandBufferSubmitInfo::builder()
                    .command_buffer(self.transfer_cb)
                    .build();
                let cb_submits = [cb_submit];
                let submit_info = vk::SubmitInfo2::builder()
                    .command_buffer_infos(&cb_submits)
                    .build();
                self.codec_device.submit_to_queue(
                    self.transfer_queue,
                    &[submit_info],
                    self.transfer_fence,
                )?;
                self.device
                    .wait_for_fences(&[self.transfer_fence], true, u64::MAX)
                    .map_err(VideoError::from)?;
            }

            // Read RGBA data from staging buffer
            let rgba_size = (display_window.width * display_window.height * 4) as usize;
            let read_size = rgba_size.min(stg_size as usize);
            let mut rgba_data = vec![0u8; read_size];
            unsafe {
                ptr::copy_nonoverlapping(stg_ptr, rgba_data.as_mut_ptr(), read_size);
            }

            // Update DPB slot layout (converter transitions to SHADER_READ_ONLY_OPTIMAL)
            let vk_dec = self.vk_decoder.as_mut().ok_or_else(|| {
                VideoError::BitstreamError("VkVideoDecoder not initialized".into())
            })?;
            vk_dec.set_dpb_slot_layout(
                pending.setup_slot,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            );

            return Ok(Some(SimpleDecodedFrame {
                data: rgba_data,
                width: display_window.width,
                height: display_window.height,
                decode_order: pending.decode_order,
                picture_order_count: pending.poc,
                is_rgba: true,
                decoded_picture_color_vui: pending.decoded_picture_color_vui,
                exported_nv12: None,
            }));
        }

        if let Some(slot_index) = pending.exported_nv12_ring_slot_index {
            let display_window = pending.display_window;
            let exported_nv12 = self
                .exported_nv12_dma_buf_ring
                .picture_in_slot(slot_index, display_window.origin_x, display_window.origin_y)
                .ok_or_else(|| {
                    VideoError::BitstreamError(format!(
                        "exported NV12 ring slot {slot_index} was reallocated before its picture \
                         was handed out"
                    ))
                })?;
            return Ok(Some(SimpleDecodedFrame {
                data: Vec::new(),
                width: display_window.width,
                height: display_window.height,
                decode_order: pending.decode_order,
                picture_order_count: pending.poc,
                is_rgba: false,
                decoded_picture_color_vui: pending.decoded_picture_color_vui,
                exported_nv12: Some(exported_nv12),
            }));
        }

        // NV12 path: read decoded NV12 data from the persistent staging buffer
        let &(_, _, size, ptr) = self.readback_staging.as_ref().ok_or_else(|| {
            VideoError::BitstreamError("No readback staging buffer available".into())
        })?;
        let mut decoded_data = vec![0u8; size as usize];
        unsafe {
            ptr::copy_nonoverlapping(ptr, decoded_data.as_mut_ptr(), decoded_data.len());
        }

        Ok(Some(SimpleDecodedFrame {
            data: decoded_data,
            width: pending.coded_width,
            height: pending.coded_height,
            decode_order: pending.decode_order,
            picture_order_count: pending.poc,
            is_rgba: false,
            decoded_picture_color_vui: pending.decoded_picture_color_vui,
            exported_nv12: None,
        }))
    }
}
