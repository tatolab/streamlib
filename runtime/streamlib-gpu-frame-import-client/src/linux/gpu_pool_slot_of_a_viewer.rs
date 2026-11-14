// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! A pool slot as a viewer holds it, a pixel buffer or a texture: the memory's
//! handles and layout, handed on to whatever displays the frame.

use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
use std::sync::{Arc, OnceLock};

use streamlib_consumer_rhi::{ConsumerVulkanBuffer, ConsumerVulkanDevice, TextureFormat};
use streamlib_surface_client::{
    CheckedOutPixelBuffer, CheckedOutPixelBufferMemory, CheckedOutTexture, CheckedOutTextureMemory,
    SurfaceShareCheckOutFds, resource_type_of_a_check_out,
};

use super::SurfaceShareTransferredHandle;

/// `DRM_FORMAT_MOD_LINEAR`: a pixel buffer's DMA-BUF planes are linear rows.
const DRM_FORMAT_MOD_LINEAR: u64 = 0;

/// One pool slot's memory, as the surface-sharing service handed it to a
/// viewer: shared by every frame over the slot.
pub struct GpuPoolSlotOfAViewer {
    memory_of_the_slot: MemoryOfAGpuPoolSlotOfAViewer,
    /// The CPU's view of an OPAQUE_FD pixel buffer, imported on its first read.
    cpu_readable_import: OnceLock<ConsumerVulkanBuffer>,
}

enum MemoryOfAGpuPoolSlotOfAViewer {
    PixelBuffer(CheckedOutPixelBuffer),
    Texture(CheckedOutTexture),
}

/// One DMA-BUF plane of a viewed pool slot, as a display imports it.
pub struct DmaBufPlaneOfAGpuPoolSlotOfAViewer<'pool_slot> {
    /// The plane's DMA-BUF; a texture's planes share its one memory fd.
    pub fd: BorrowedFd<'pool_slot>,
    /// The plane's byte offset into its DMA-BUF.
    pub offset: u64,
    /// The plane's row pitch, padding included.
    pub stride: u64,
    /// The plane's byte size from its offset.
    pub size: u64,
}

impl GpuPoolSlotOfAViewer {
    /// Read the service's answer to an `import_pool_slot` of `pool_slot_key`
    /// and the descriptors it carried.
    pub(crate) fn from_the_import_reply(
        pool_slot_key: &str,
        reply: &serde_json::Value,
        received_handles: Vec<SurfaceShareTransferredHandle>,
    ) -> Result<Self, String> {
        let SurfaceShareCheckOutFds { plane_fds, .. } =
            SurfaceShareCheckOutFds::split_off_the_timeline_fds(
                pool_slot_key,
                reply,
                received_handles,
            )?;
        let memory_of_the_slot = match resource_type_of_a_check_out(reply) {
            "pixel_buffer" => MemoryOfAGpuPoolSlotOfAViewer::PixelBuffer(
                CheckedOutPixelBuffer::from_check_out(pool_slot_key, reply, plane_fds)?,
            ),
            "texture" => MemoryOfAGpuPoolSlotOfAViewer::Texture(the_texture_a_viewer_can_show(
                pool_slot_key,
                CheckedOutTexture::from_check_out(pool_slot_key, reply, plane_fds)?,
            )?),
            other => {
                return Err(format!(
                    "pool slot {pool_slot_key:?} is registered as a {other:?}; a viewer imports \
                     pixel buffers and textures only"
                ));
            }
        };
        Ok(Self {
            memory_of_the_slot,
            cpu_readable_import: OnceLock::new(),
        })
    }

    /// The slot's width in pixels.
    pub fn width(&self) -> u32 {
        match &self.memory_of_the_slot {
            MemoryOfAGpuPoolSlotOfAViewer::PixelBuffer(pixel_buffer) => pixel_buffer.width,
            MemoryOfAGpuPoolSlotOfAViewer::Texture(texture) => texture.width,
        }
    }

    /// The slot's height in pixels.
    pub fn height(&self) -> u32 {
        match &self.memory_of_the_slot {
            MemoryOfAGpuPoolSlotOfAViewer::PixelBuffer(pixel_buffer) => pixel_buffer.height,
            MemoryOfAGpuPoolSlotOfAViewer::Texture(texture) => texture.height,
        }
    }

    /// The wire name of the slot's format: a pixel buffer's (`"rgba32"`,
    /// `"bgra32"`, …) or a texture's (`"rgba8_unorm"`, …).
    pub fn format_wire_name(&self) -> &str {
        match &self.memory_of_the_slot {
            MemoryOfAGpuPoolSlotOfAViewer::PixelBuffer(pixel_buffer) => {
                &pixel_buffer.format_wire_name
            }
            MemoryOfAGpuPoolSlotOfAViewer::Texture(texture) => &texture.format_wire_name,
        }
    }

    /// The first plane's row pitch, padding included: a texture's stated
    /// DMA-BUF row pitch, or its tightly packed rows when it crossed as
    /// OPAQUE_FD, whose tiling has no row pitch of its own.
    pub fn bytes_per_row(&self) -> u64 {
        match &self.memory_of_the_slot {
            MemoryOfAGpuPoolSlotOfAViewer::PixelBuffer(pixel_buffer) => pixel_buffer.bytes_per_row,
            MemoryOfAGpuPoolSlotOfAViewer::Texture(texture) => match &texture.memory {
                CheckedOutTextureMemory::DmaBuf { plane_strides, .. } => {
                    plane_strides.first().copied().unwrap_or(0)
                }
                CheckedOutTextureMemory::OpaqueFd { .. } => {
                    TextureFormat::from_wire_name(&texture.format_wire_name)
                        .map(|format| u64::from(format.bytes_per_pixel()))
                        .unwrap_or(0)
                        * u64::from(texture.width)
                }
            },
        }
    }

    /// The DRM format modifier of the slot's DMA-BUF planes, which a display
    /// imports them with: linear for a pixel buffer, the producer's for a
    /// texture; `None` when the memory crossed as OPAQUE_FD.
    pub fn dma_buf_drm_format_modifier(&self) -> Option<u64> {
        match &self.memory_of_the_slot {
            MemoryOfAGpuPoolSlotOfAViewer::PixelBuffer(CheckedOutPixelBuffer {
                memory: CheckedOutPixelBufferMemory::DmaBuf { .. },
                ..
            }) => Some(DRM_FORMAT_MOD_LINEAR),
            MemoryOfAGpuPoolSlotOfAViewer::Texture(CheckedOutTexture {
                memory:
                    CheckedOutTextureMemory::DmaBuf {
                        drm_format_modifier,
                        ..
                    },
                ..
            }) => Some(*drm_format_modifier),
            _ => None,
        }
    }

    /// Each DMA-BUF plane of the slot, first plane first, for a display that
    /// imports them itself; `None` when the memory crossed as OPAQUE_FD,
    /// which only [`Self::bytes_for_a_cpu_read`] reads.
    pub fn dma_buf_planes(&self) -> Option<Vec<DmaBufPlaneOfAGpuPoolSlotOfAViewer<'_>>> {
        match &self.memory_of_the_slot {
            MemoryOfAGpuPoolSlotOfAViewer::PixelBuffer(CheckedOutPixelBuffer {
                memory: CheckedOutPixelBufferMemory::DmaBuf { plane_fds },
                plane_sizes,
                bytes_per_row,
                ..
            }) => Some(
                plane_fds
                    .iter()
                    .zip(plane_sizes)
                    .map(
                        |(plane_fd, plane_size)| DmaBufPlaneOfAGpuPoolSlotOfAViewer {
                            fd: plane_fd.as_fd(),
                            offset: 0,
                            stride: *bytes_per_row,
                            size: *plane_size,
                        },
                    )
                    .collect(),
            ),
            MemoryOfAGpuPoolSlotOfAViewer::Texture(CheckedOutTexture {
                memory:
                    CheckedOutTextureMemory::DmaBuf {
                        memory_fd,
                        plane_offsets,
                        plane_strides,
                        ..
                    },
                allocation_byte_size,
                ..
            }) => Some(
                plane_offsets
                    .iter()
                    .zip(plane_strides)
                    .map(
                        |(plane_offset, plane_stride)| DmaBufPlaneOfAGpuPoolSlotOfAViewer {
                            fd: memory_fd.as_fd(),
                            offset: *plane_offset,
                            stride: *plane_stride,
                            size: allocation_byte_size.saturating_sub(*plane_offset),
                        },
                    )
                    .collect(),
            ),
            _ => None,
        }
    }

    /// The UUID of the device that exported memory which crossed as
    /// OPAQUE_FD, which a CPU read imports on; `None` for DMA-BUF planes.
    pub fn opaque_fd_exporting_device_uuid(&self) -> Option<[u8; 16]> {
        match &self.memory_of_the_slot {
            MemoryOfAGpuPoolSlotOfAViewer::PixelBuffer(CheckedOutPixelBuffer {
                memory:
                    CheckedOutPixelBufferMemory::OpaqueFd {
                        exporting_device_uuid,
                        ..
                    },
                ..
            })
            | MemoryOfAGpuPoolSlotOfAViewer::Texture(CheckedOutTexture {
                memory:
                    CheckedOutTextureMemory::OpaqueFd {
                        exporting_device_uuid,
                        ..
                    },
                ..
            }) => Some(*exporting_device_uuid),
            _ => None,
        }
    }

    /// The slot's first plane as the CPU reads it, through an import made at
    /// the first read on `consumer_vulkan_device` — the device
    /// [`Self::opaque_fd_exporting_device_uuid`] names, which every later read
    /// reuses: the fallback for memory that crossed as OPAQUE_FD, which a
    /// display cannot import.
    pub fn bytes_for_a_cpu_read(
        &self,
        consumer_vulkan_device: &Arc<ConsumerVulkanDevice>,
    ) -> Result<&[u8], String> {
        let (memory_fd, vk_memory_type_index, plane_sizes) = match &self.memory_of_the_slot {
            MemoryOfAGpuPoolSlotOfAViewer::PixelBuffer(CheckedOutPixelBuffer {
                memory:
                    CheckedOutPixelBufferMemory::OpaqueFd {
                        memory_fd,
                        vk_memory_type_index,
                        ..
                    },
                plane_sizes,
                ..
            }) => (memory_fd, *vk_memory_type_index, plane_sizes),
            MemoryOfAGpuPoolSlotOfAViewer::Texture(CheckedOutTexture {
                memory: CheckedOutTextureMemory::OpaqueFd { .. },
                width,
                height,
                format_wire_name,
                ..
            }) => {
                return Err(format!(
                    "the {width}x{height} {format_wire_name} texture crossed as OPAQUE_FD in \
                     the GPU's own tiling, which neither a display imports nor the CPU reads \
                     as rows"
                ));
            }
            _ => {
                return Err(
                    "a DMA-BUF pool slot is read by importing its planes, not through a CPU \
                     import"
                        .into(),
                );
            }
        };
        let plane0_size = plane_sizes
            .first()
            .copied()
            .filter(|&plane0_size| plane0_size > 0)
            .ok_or("the pool slot's import stated no first plane size")?;
        if let Some(imported) = self.cpu_readable_import.get() {
            return mapped_bytes(imported, plane0_size);
        }
        let imported = imported_for_a_cpu_read(
            consumer_vulkan_device,
            memory_fd,
            plane0_size,
            vk_memory_type_index,
        )?;
        mapped_bytes(
            self.cpu_readable_import.get_or_init(|| imported),
            plane0_size,
        )
    }
}

/// `texture` when a display can import it: a DMA-BUF texture states one
/// row pitch for each plane offset.
fn the_texture_a_viewer_can_show(
    pool_slot_key: &str,
    texture: CheckedOutTexture,
) -> Result<CheckedOutTexture, String> {
    if let CheckedOutTextureMemory::DmaBuf {
        plane_offsets,
        plane_strides,
        ..
    } = &texture.memory
        && (plane_offsets.is_empty() || plane_offsets.len() != plane_strides.len())
    {
        return Err(format!(
            "texture pool slot {pool_slot_key:?} is registered with {} plane offset(s) and {} \
             row pitch(es); a display imports a DMA-BUF texture by one of each per plane",
            plane_offsets.len(),
            plane_strides.len(),
        ));
    }
    Ok(texture)
}

fn imported_for_a_cpu_read(
    consumer_vulkan_device: &Arc<ConsumerVulkanDevice>,
    memory_fd: &OwnedFd,
    plane0_size: u64,
    vk_memory_type_index: u32,
) -> Result<ConsumerVulkanBuffer, String> {
    let duplicated_for_import = memory_fd.try_clone().map_err(|duplicate_failure| {
        format!("could not duplicate the pool slot's memory fd for import: {duplicate_failure}")
    })?;
    ConsumerVulkanBuffer::from_opaque_fd_at_stated_memory_type_index(
        consumer_vulkan_device,
        duplicated_for_import,
        plane0_size,
        vk_memory_type_index,
    )
    .map_err(|import_failure| {
        format!(
            "Vulkan could not import the pool slot's OPAQUE_FD memory at memory type index \
             {vk_memory_type_index}: {import_failure}"
        )
    })
}

fn mapped_bytes(imported: &ConsumerVulkanBuffer, plane0_size: u64) -> Result<&[u8], String> {
    let mapped = imported.mapped_ptr();
    if mapped.is_null() {
        return Err("the pool slot's memory imported without a host mapping".into());
    }
    // SAFETY: the import maps `plane0_size` bytes for as long as it lives,
    // and it lives as long as the slot that holds it.
    Ok(unsafe { std::slice::from_raw_parts(mapped, plane0_size as usize) })
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd as _;

    use super::*;

    fn pipe_fd() -> OwnedFd {
        let (read_end, _write_end) = std::io::pipe().expect("a pipe opens");
        OwnedFd::from(read_end)
    }

    fn texture_import_reply(handle_type: &str, timeline_fds_named: bool) -> serde_json::Value {
        serde_json::json!({
            "resource_type": "texture", "width": 8, "height": 4, "format": "rgba8_unorm",
            "handle_type": handle_type, "vk_image_allocation_size": 4096,
            "drm_format_modifier": 0x0300_0000_0000_0010u64,
            "plane_offsets": [256], "plane_strides": [64],
            "vk_memory_type_index": 2,
            "exporting_device_uuid": "00112233445566778899aabbccddeeff",
            "has_produce_done_fd": timeline_fds_named,
            "has_consume_done_fd": timeline_fds_named,
        })
    }

    fn import_of(
        reply: &serde_json::Value,
        fd_count: usize,
    ) -> Result<GpuPoolSlotOfAViewer, String> {
        GpuPoolSlotOfAViewer::from_the_import_reply(
            "slot",
            reply,
            (0..fd_count).map(|_| pipe_fd()).collect(),
        )
    }

    #[test]
    fn a_dma_buf_texture_slot_hands_a_display_its_modifier_and_plane_layout() {
        for (timeline_fds_named, fd_count) in [(false, 1), (true, 3)] {
            let pool_slot = import_of(
                &texture_import_reply("dma_buf", timeline_fds_named),
                fd_count,
            )
            .expect("a DMA-BUF texture slot imports");
            assert_eq!((pool_slot.width(), pool_slot.height()), (8, 4));
            assert_eq!(pool_slot.format_wire_name(), "rgba8_unorm");
            assert_eq!(pool_slot.bytes_per_row(), 64);
            assert_eq!(
                pool_slot.dma_buf_drm_format_modifier(),
                Some(0x0300_0000_0000_0010)
            );
            assert_eq!(pool_slot.opaque_fd_exporting_device_uuid(), None);
            let planes = pool_slot.dma_buf_planes().expect("DMA-BUF planes");
            let [plane] = planes.as_slice() else {
                panic!("one plane, got {}", planes.len());
            };
            assert!(plane.fd.as_raw_fd() >= 0);
            assert_eq!((plane.offset, plane.stride, plane.size), (256, 64, 3840));
        }
    }

    #[test]
    fn a_dma_buf_texture_slot_stating_no_row_pitch_for_a_plane_is_refused_by_count() {
        let mut reply = texture_import_reply("dma_buf", false);
        reply["plane_strides"] = serde_json::json!([]);
        let refusal = import_of(&reply, 1).err().expect("refused");
        assert!(
            refusal.contains("1 plane offset(s) and 0 row pitch(es)"),
            "{refusal}"
        );
    }

    #[test]
    fn an_opaque_fd_texture_slot_names_its_device_and_refuses_a_cpu_read_by_its_tiling() {
        let pool_slot = import_of(&texture_import_reply("opaque_fd", false), 1)
            .expect("an OPAQUE_FD texture slot imports");
        assert!(pool_slot.dma_buf_planes().is_none());
        assert_eq!(pool_slot.dma_buf_drm_format_modifier(), None);
        assert_eq!(pool_slot.bytes_per_row(), 32);
        assert_eq!(
            pool_slot.opaque_fd_exporting_device_uuid(),
            Some([
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff
            ])
        );
    }

    #[test]
    fn a_dma_buf_pixel_buffer_slot_hands_a_display_linear_planes() {
        let reply = serde_json::json!({
            "resource_type": "pixel_buffer", "width": 4, "height": 2, "format": "bgra32",
            "handle_type": "dma_buf", "plane_sizes": [32],
        });
        let pool_slot = import_of(&reply, 1).expect("a pixel buffer slot imports");
        assert_eq!(
            pool_slot.dma_buf_drm_format_modifier(),
            Some(DRM_FORMAT_MOD_LINEAR)
        );
        let planes = pool_slot.dma_buf_planes().expect("DMA-BUF planes");
        let [plane] = planes.as_slice() else {
            panic!("one plane, got {}", planes.len());
        };
        assert_eq!((plane.offset, plane.stride, plane.size), (0, 16, 32));
    }

    #[test]
    fn a_slot_of_another_resource_is_refused_by_its_registration() {
        let reply = serde_json::json!({
            "resource_type": "buffer", "width": 4, "height": 2, "plane_sizes": [32],
        });
        let refusal = import_of(&reply, 1).err().expect("refused");
        assert!(refusal.contains("registered as a \"buffer\""), "{refusal}");
    }
}
