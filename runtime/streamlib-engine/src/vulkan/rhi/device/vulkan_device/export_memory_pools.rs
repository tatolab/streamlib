// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use vma::Alloc as _;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

use crate::core::{Error, Result};
use crate::vulkan::rhi::device::drm_modifier_probe::{self, DrmModifierTable};

use super::HostVulkanDevice;
use super::VmaFrameAllocationSource;
use super::memory_type_index_is_host_cached;

/// A VMA export pool and the one memory type it allocates from.
pub(crate) struct VmaExportPoolPinnedToOneMemoryType {
    pub(crate) pool: vma::Pool,
    pub(crate) memory_type_index: u32,
}

impl<'a> VmaFrameAllocationSource<'a> {
    /// `export_pool` where the device created it, the default allocator otherwise.
    pub(crate) fn export_pool_or_the_default_allocator(
        export_pool: Option<&'a VmaExportPoolPinnedToOneMemoryType>,
    ) -> Self {
        export_pool.map_or(Self::DefaultAllocator, Self::ExportPool)
    }
}

/// Refuse, naming the image's format and usage, an image whose
/// `memory_type_bits` exclude the one memory type an export pool is pinned
/// to — the pool was probed with one image shape, and an image of another
/// may not bind its type.
pub(super) fn refuse_an_image_the_export_pools_memory_type_cannot_bind(
    image_memory_type_bits: u32,
    export_pool_memory_type_index: u32,
    image_create_info: &vk::ImageCreateInfo,
    frame_allocation_site: &'static str,
) -> Result<()> {
    let export_pool_memory_type_bit = 1u32.checked_shl(export_pool_memory_type_index).unwrap_or(0);
    if image_memory_type_bits & export_pool_memory_type_bit != 0 {
        return Ok(());
    }
    Err(Error::GpuError(format!(
        "{frame_allocation_site}: a {:?} image with usage {:?} cannot bind memory type {} its \
         export pool is pinned to (the image's memory_type_bits are {:#x})",
        image_create_info.format,
        image_create_info.usage,
        export_pool_memory_type_index,
        image_memory_type_bits,
    )))
}

/// How a mapped OPAQUE_FD buffer allocation is reached from the host.
///
/// One choice rather than two flags: VMA asserts when both HOST_ACCESS
/// bits are set on one allocation. A pool and every allocation it serves
/// must carry the same one, or the probe's `memoryTypeBits` stop matching
/// the real buffer's and the bind trips
/// VUID-vkBindBufferMemory-memory-01035.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum MappedOpaqueFdBufferHostAccessPattern {
    /// Producer-write memory — the shared OPAQUE_FD pool's hint.
    SequentialWrite,
    /// CPU-read memory; steers VMA to prefer a HOST_CACHED type.
    Random,
}

impl MappedOpaqueFdBufferHostAccessPattern {
    /// `MAPPED` plus this pattern's single HOST_ACCESS bit.
    pub(crate) fn vma_allocation_create_flags(self) -> vma::AllocationCreateFlags {
        vma::AllocationCreateFlags::MAPPED
            | match self {
                Self::SequentialWrite => vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
                Self::Random => vma::AllocationCreateFlags::HOST_ACCESS_RANDOM,
            }
    }
}

/// Pick a DEVICE_LOCAL memory type for a tiled DMA-BUF VkImage.
///
/// VMA's `find_memory_type_index_for_image_info` cannot probe images with
/// `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT` (it asserts on the missing
/// plane aspect for multi-plane modifiers), so we create a real probe
/// image, query its memory requirements, and find a matching DEVICE_LOCAL
/// type ourselves. The probe image is destroyed before the function returns.
fn probe_tiled_dma_buf_memory_type(
    device: &vulkanalia::Device,
    memory_properties: &vk::PhysicalDeviceMemoryProperties,
    tiled_modifiers: &[u64],
) -> Result<u32> {
    let mut probe_modifier_info = vk::ImageDrmFormatModifierListCreateInfoEXT::builder()
        .drm_format_modifiers(tiled_modifiers);
    let mut probe_external_info = vk::ExternalMemoryImageCreateInfo::builder()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let probe_image_info = vk::ImageCreateInfo::builder()
        .image_type(vk::ImageType::_2D)
        .format(vk::Format::B8G8R8A8_UNORM)
        .extent(vk::Extent3D {
            width: 64,
            height: 64,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::_1)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(
            vk::ImageUsageFlags::COLOR_ATTACHMENT
                | vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::TRANSFER_SRC
                | vk::ImageUsageFlags::TRANSFER_DST,
        )
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(&mut probe_modifier_info)
        .push_next(&mut probe_external_info);

    let probe_image = unsafe { device.create_image(&probe_image_info, None) }
        .map_err(|e| Error::GpuError(format!("tiled-DMA-BUF probe vkCreateImage failed: {e}")))?;

    let mem_reqs = unsafe { device.get_image_memory_requirements(probe_image) };
    unsafe { device.destroy_image(probe_image, None) };

    // First DEVICE_LOCAL type that satisfies memory_type_bits, preferring
    // one without HOST_VISIBLE (main VRAM heap) on discrete GPUs.
    for pass in 0..2 {
        for i in 0..memory_properties.memory_type_count {
            if (mem_reqs.memory_type_bits & (1 << i)) == 0 {
                continue;
            }
            let flags = memory_properties.memory_types[i as usize].property_flags;
            if !flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL) {
                continue;
            }
            if pass == 0 && flags.contains(vk::MemoryPropertyFlags::HOST_VISIBLE) {
                continue;
            }
            return Ok(i);
        }
    }
    Err(Error::GpuError(format!(
        "no DEVICE_LOCAL memory type accepts tiled DMA-BUF probe image (memory_type_bits=0x{:x})",
        mem_reqs.memory_type_bits
    )))
}

impl HostVulkanDevice {
    /// Build VMA pools dedicated to DMA-BUF exportable allocations.
    ///
    /// Each pool is pinned to a memory type that supports the relevant property
    /// flags and carries VkExportMemoryAllocateInfo::DMA_BUF_EXT via
    /// pMemoryAllocateNext. The export info structs are heap-boxed and returned
    /// alongside the pools — the caller must keep them alive for the pool's
    /// lifetime (VMA stores raw pointers to them).
    ///
    /// The third pool (tiled) is conditional on `tiled_modifiers` being
    /// non-empty — we need at least one render-target-capable DRM modifier to
    /// create the probe image. When the EGL probe returned no modifiers (no
    /// libEGL, no display, extension missing) the tiled pool is `None` and
    /// callers fall back to refusing render-target allocations.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn create_dma_buf_pools(
        allocator: &Arc<vma::Allocator>,
        device: &vulkanalia::Device,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        tiled_modifiers: &[u64],
    ) -> Result<(
        VmaExportPoolPinnedToOneMemoryType,
        VmaExportPoolPinnedToOneMemoryType,
        Option<VmaExportPoolPinnedToOneMemoryType>,
        Box<vk::ExportMemoryAllocateInfo>,
        Box<vk::ExportMemoryAllocateInfo>,
        Option<Box<vk::ExportMemoryAllocateInfo>>,
    )> {
        // ── Find memory type for HOST_VISIBLE DMA-BUF exportable buffers ──
        // The probe must mirror the real buffer create info used by
        // `HostVulkanBuffer::new`, including the `ExternalMemoryBufferCreateInfo`
        // pNext chain — DMA-BUF external buffers have a narrower
        // `memoryTypeBits` than plain buffers, and omitting the chain lets VMA
        // pick a memory type the real buffer won't accept at bind time
        // (VUID-vkBindBufferMemory-memory-01035).
        let mut probe_buffer_external_info = vk::ExternalMemoryBufferCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let probe_buffer_info = vk::BufferCreateInfo::builder()
            .size(64 * 1024)
            .usage(
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::STORAGE_BUFFER,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut probe_buffer_external_info);
        let probe_buffer_alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY
                | vma::AllocationCreateFlags::MAPPED
                | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            ..Default::default()
        };
        let buffer_mem_type_idx = unsafe {
            allocator
                .find_memory_type_index_for_buffer_info(probe_buffer_info, &probe_buffer_alloc_opts)
        }
        .map_err(|e| Error::GpuError(format!("find memory type for DMA-BUF buffer pool: {e}")))?;

        // ── Find memory type for DEVICE_LOCAL DMA-BUF exportable images ──
        // Same rationale: the real image (`HostVulkanTexture::new`) carries
        // `ExternalMemoryImageCreateInfo::DMA_BUF_EXT` which can narrow
        // `memoryTypeBits`.
        let mut probe_image_external_info = vk::ExternalMemoryImageCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let probe_image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk::Format::B8G8R8A8_UNORM)
            .extent(vk::Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::SAMPLED,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut probe_image_external_info);
        let probe_image_alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };
        let image_mem_type_idx = unsafe {
            allocator
                .find_memory_type_index_for_image_info(probe_image_info, &probe_image_alloc_opts)
        }
        .map_err(|e| Error::GpuError(format!("find memory type for DMA-BUF image pool: {e}")))?;

        // ── Box the VkExportMemoryAllocateInfo structs (need stable pointers) ──
        let mut buffer_export_info = Box::new(
            vk::ExportMemoryAllocateInfo::builder()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                .build(),
        );
        let mut image_export_info = Box::new(
            vk::ExportMemoryAllocateInfo::builder()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                .build(),
        );

        // ── Create the pools ──────────────────────────────────────────────────
        // VMA's pMemoryAllocateNext stores a raw pointer to the export info.
        // Box gives a stable heap address — we keep the Boxes alive by returning
        // them alongside the pools. Drop order (handled by HostVulkanDevice::drop):
        // pool → Box, so the pointer is valid for the pool's entire lifetime.
        let mut buffer_pool_options = vma::PoolOptions::default();
        buffer_pool_options = buffer_pool_options.push_next(buffer_export_info.as_mut());
        buffer_pool_options.memory_type_index = buffer_mem_type_idx;
        let buffer_pool = allocator
            .create_pool(&buffer_pool_options)
            .map_err(|e| Error::GpuError(format!("create DMA-BUF buffer pool: {e}")))?;

        let mut image_pool_options = vma::PoolOptions::default();
        image_pool_options = image_pool_options.push_next(image_export_info.as_mut());
        image_pool_options.memory_type_index = image_mem_type_idx;
        let image_pool = allocator
            .create_pool(&image_pool_options)
            .map_err(|e| Error::GpuError(format!("create DMA-BUF image pool: {e}")))?;

        // ── Tiled (DRM_FORMAT_MODIFIER) image pool ────────────────────────
        // Only created when the EGL probe returned at least one
        // render-target-capable modifier. VMA's
        // `find_memory_type_index_for_image_info` asserts on
        // `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT` (it can't infer the plane
        // aspect for multi-plane modifiers), so we probe via raw
        // `vkCreateImage` + `vkGetImageMemoryRequirements` and pick a
        // DEVICE_LOCAL type ourselves. The probe image is destroyed before
        // the pool is created — VMA only needs the memory type index.
        let (tiled_pool, tiled_export_info) = if tiled_modifiers.is_empty() {
            (None, None)
        } else {
            match probe_tiled_dma_buf_memory_type(device, memory_properties, tiled_modifiers) {
                Ok(tiled_mem_type_idx) => {
                    let mut tiled_export_info = Box::new(
                        vk::ExportMemoryAllocateInfo::builder()
                            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                            .build(),
                    );
                    let mut tiled_pool_options = vma::PoolOptions::default();
                    tiled_pool_options = tiled_pool_options.push_next(tiled_export_info.as_mut());
                    tiled_pool_options.memory_type_index = tiled_mem_type_idx;
                    match allocator.create_pool(&tiled_pool_options) {
                        Ok(tiled_pool) => {
                            tracing::info!(
                                "DMA-BUF VMA tiled image pool created — mem_type={}, modifiers_probed={}",
                                tiled_mem_type_idx,
                                tiled_modifiers.len(),
                            );
                            (
                                Some(VmaExportPoolPinnedToOneMemoryType {
                                    pool: tiled_pool,
                                    memory_type_index: tiled_mem_type_idx,
                                }),
                                Some(tiled_export_info),
                            )
                        }
                        Err(e) => {
                            tracing::warn!(
                                "DMA-BUF VMA tiled image pool creation failed (mem_type={}): {e} — render-target DMA-BUF allocations will be unavailable",
                                tiled_mem_type_idx,
                            );
                            (None, None)
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Tiled DMA-BUF memory-type probe failed: {e} — render-target DMA-BUF allocations will be unavailable",
                    );
                    (None, None)
                }
            }
        };

        tracing::info!(
            "DMA-BUF VMA pools created — buffer mem_type={}, image mem_type={}, tiled={}",
            buffer_mem_type_idx,
            image_mem_type_idx,
            tiled_pool.is_some(),
        );

        Ok((
            VmaExportPoolPinnedToOneMemoryType {
                pool: buffer_pool,
                memory_type_index: buffer_mem_type_idx,
            },
            VmaExportPoolPinnedToOneMemoryType {
                pool: image_pool,
                memory_type_index: image_mem_type_idx,
            },
            tiled_pool,
            buffer_export_info,
            image_export_info,
            tiled_export_info,
        ))
    }

    /// Probe the memory type index an OPAQUE_FD-exportable HOST_VISIBLE
    /// buffer pool must pin itself to under `host_access_pattern`.
    ///
    /// The probe shape mirrors `HostVulkanBuffer`'s real allocations —
    /// HOST_VISIBLE | HOST_COHERENT, TRANSFER_SRC | TRANSFER_DST |
    /// STORAGE_BUFFER, chained `VkExternalMemoryBufferCreateInfo::OPAQUE_FD`.
    /// The probe's `memoryTypeBits` must match the real buffer's or the
    /// bind fails with VUID-vkBindBufferMemory-memory-01035.
    fn probe_opaque_fd_host_visible_buffer_memory_type_index(
        allocator: &Arc<vma::Allocator>,
        host_access_pattern: MappedOpaqueFdBufferHostAccessPattern,
    ) -> Result<u32> {
        let mut probe_external_info = vk::ExternalMemoryBufferCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let probe_buffer_info = vk::BufferCreateInfo::builder()
            .size(64 * 1024)
            .usage(
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::STORAGE_BUFFER,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut probe_external_info);
        let probe_alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY
                | host_access_pattern.vma_allocation_create_flags(),
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            ..Default::default()
        };
        unsafe {
            allocator.find_memory_type_index_for_buffer_info(probe_buffer_info, &probe_alloc_opts)
        }
        .map_err(|e| Error::GpuError(format!("find memory type for OPAQUE_FD buffer pool: {e}")))
    }

    /// Pin a VMA pool chaining `VkExportMemoryAllocateInfo::OPAQUE_FD` to
    /// `memory_type_index`.
    ///
    /// The export info `Box` is returned alongside the pool — callers
    /// must keep it alive for the pool's entire lifetime (VMA stores
    /// raw pointers via `pMemoryAllocateNext`).
    fn create_opaque_fd_buffer_pool_pinned_to_memory_type_index(
        allocator: &Arc<vma::Allocator>,
        memory_type_index: u32,
    ) -> Result<(
        VmaExportPoolPinnedToOneMemoryType,
        Box<vk::ExportMemoryAllocateInfo>,
    )> {
        let mut export_info = Box::new(
            vk::ExportMemoryAllocateInfo::builder()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
                .build(),
        );
        let mut pool_options = vma::PoolOptions::default();
        pool_options = pool_options.push_next(export_info.as_mut());
        pool_options.memory_type_index = memory_type_index;
        let pool = allocator
            .create_pool(&pool_options)
            .map_err(|e| Error::GpuError(format!("create OPAQUE_FD buffer pool: {e}")))?;
        Ok((
            VmaExportPoolPinnedToOneMemoryType {
                pool,
                memory_type_index,
            },
            export_info,
        ))
    }

    /// Build a VMA pool dedicated to OPAQUE_FD-exportable HOST_VISIBLE buffers.
    ///
    /// Used by CUDA / OpenCL interop: the host allocates from this pool,
    /// `vkGetMemoryFdKHR` with `OPAQUE_FD` produces a kernel fd, and the
    /// remote API (`cudaImportExternalMemory` etc.) imports the same
    /// underlying memory by fd. Sibling of [`Self::create_dma_buf_pools`];
    /// kept separate because a VMA pool can chain only one
    /// `VkExportMemoryAllocateInfo` per pool, and OPAQUE_FD vs DMA_BUF_EXT
    /// are mutually-exclusive handle types.
    ///
    /// `HOST_ACCESS_SEQUENTIAL_WRITE` is the right hint for this pool's
    /// producer-write consumers; the CPU-read sibling is
    /// [`Self::create_opaque_fd_buffer_pool_host_cached`].
    pub(super) fn create_opaque_fd_buffer_pool(
        allocator: &Arc<vma::Allocator>,
    ) -> Result<(
        VmaExportPoolPinnedToOneMemoryType,
        Box<vk::ExportMemoryAllocateInfo>,
    )> {
        let memory_type_index = Self::probe_opaque_fd_host_visible_buffer_memory_type_index(
            allocator,
            MappedOpaqueFdBufferHostAccessPattern::SequentialWrite,
        )?;
        let (pinned_pool, export_info) =
            Self::create_opaque_fd_buffer_pool_pinned_to_memory_type_index(
                allocator,
                memory_type_index,
            )?;

        tracing::info!(
            "OPAQUE_FD VMA buffer pool created — mem_type={}",
            memory_type_index
        );
        Ok((pinned_pool, export_info))
    }

    /// Build a VMA pool dedicated to OPAQUE_FD-exportable HOST_VISIBLE
    /// buffers on a HOST_CACHED memory type — the CPU-read sibling of
    /// [`Self::create_opaque_fd_buffer_pool`], whose write-combined memory
    /// the CPU reads roughly an order of magnitude slower.
    ///
    /// `Ok(None)` is the device having no cached exportable type, not a
    /// failure: `HOST_ACCESS_RANDOM` makes VMA *prefer* HOST_CACHED and
    /// `vk_mem_alloc.h` is explicit that it "cannot require it", so the
    /// probe succeeds on a cache-less device and hands back an uncached
    /// index. Reading the probed index's flags is therefore the detection,
    /// not a probe error.
    ///
    /// HOST_COHERENT stays required: the child-side OPAQUE_FD import binds
    /// the memory as HOST_VISIBLE | HOST_COHERENT and no flush or
    /// invalidate exists anywhere on this path.
    pub(super) fn create_opaque_fd_buffer_pool_host_cached(
        allocator: &Arc<vma::Allocator>,
    ) -> Result<
        Option<(
            VmaExportPoolPinnedToOneMemoryType,
            Box<vk::ExportMemoryAllocateInfo>,
        )>,
    > {
        let memory_type_index = Self::probe_opaque_fd_host_visible_buffer_memory_type_index(
            allocator,
            MappedOpaqueFdBufferHostAccessPattern::Random,
        )?;
        if !memory_type_index_is_host_cached(allocator.get_memory_properties(), memory_type_index) {
            return Ok(None);
        }
        let (pinned_pool, export_info) =
            Self::create_opaque_fd_buffer_pool_pinned_to_memory_type_index(
                allocator,
                memory_type_index,
            )?;

        tracing::info!(
            "OPAQUE_FD VMA buffer pool (HOST_CACHED) created — mem_type={}",
            memory_type_index
        );
        Ok(Some((pinned_pool, export_info)))
    }

    /// Build a VMA pool dedicated to OPAQUE_FD-exportable DEVICE_LOCAL
    /// buffers (GPU-resident sibling of [`Self::create_opaque_fd_buffer_pool`]).
    ///
    /// Same `VkExportMemoryAllocateInfo::handleTypes = OPAQUE_FD`, but the
    /// memory-type probe excludes HOST_VISIBLE so VMA picks a true VRAM
    /// type instead of the BAR aperture or pinned-host fallback. Used by
    /// hot-path camera→cuda flows where pinned-host PCIe bandwidth is a
    /// real bottleneck (~16-32 GB/s vs ~600 GB/s on VRAM, RTX 3090).
    /// Pool storage is single-memory-type-index by VMA contract — that's
    /// why this is a separate pool from the HOST_VISIBLE one.
    pub(super) fn create_opaque_fd_buffer_pool_device_local(
        allocator: &Arc<vma::Allocator>,
    ) -> Result<(
        VmaExportPoolPinnedToOneMemoryType,
        Box<vk::ExportMemoryAllocateInfo>,
    )> {
        let mut probe_external_info = vk::ExternalMemoryBufferCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let probe_buffer_info = vk::BufferCreateInfo::builder()
            .size(64 * 1024)
            .usage(
                vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::STORAGE_BUFFER,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut probe_external_info);
        let probe_alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };
        let mem_type_idx = unsafe {
            allocator.find_memory_type_index_for_buffer_info(probe_buffer_info, &probe_alloc_opts)
        }
        .map_err(|e| {
            Error::GpuError(format!(
                "find memory type for DEVICE_LOCAL OPAQUE_FD buffer pool: {e}"
            ))
        })?;

        let mut export_info = Box::new(
            vk::ExportMemoryAllocateInfo::builder()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
                .build(),
        );
        let mut pool_options = vma::PoolOptions::default();
        pool_options = pool_options.push_next(export_info.as_mut());
        pool_options.memory_type_index = mem_type_idx;
        let pool = allocator.create_pool(&pool_options).map_err(|e| {
            Error::GpuError(format!("create DEVICE_LOCAL OPAQUE_FD buffer pool: {e}"))
        })?;

        tracing::info!(
            "OPAQUE_FD VMA buffer pool (DEVICE_LOCAL) created — mem_type={}",
            mem_type_idx
        );
        Ok((
            VmaExportPoolPinnedToOneMemoryType {
                pool,
                memory_type_index: mem_type_idx,
            },
            export_info,
        ))
    }

    /// Build a VMA pool dedicated to OPAQUE_FD-exportable DEVICE_LOCAL
    /// **images** — image-flavored sibling of
    /// [`Self::create_opaque_fd_buffer_pool_device_local`].
    ///
    /// Probe shape matches [`HostVulkanTexture::new_opaque_fd_export`](crate::vulkan::rhi::HostVulkanTexture::new_opaque_fd_export)
    /// exactly so the pool's `memoryTypeBits` matches the real
    /// allocations' bind requirements (VUID-vkBindImageMemory-memory-01047):
    ///
    /// - `vk::Format::R8G8B8A8_UNORM` — the canonical CUDA-mappable
    ///   8-bit RGBA format. Other CUDA-mappable variants (`R16G16B16A16_SFLOAT`,
    ///   `R32G32B32A32_SFLOAT`) share the same DEVICE_LOCAL memory-type
    ///   bits on every Vulkan driver shipping today.
    /// - `vk::ImageTiling::OPTIMAL` — LINEAR returns
    ///   `CUDA_ERROR_INVALID_VALUE` from `cuExternalMemoryGetMappedMipmappedArray`;
    ///   no DRM modifier (OPAQUE_FD + `DRM_FORMAT_MODIFIER_EXT` is not
    ///   a valid combination on NVIDIA).
    /// - `VkExternalMemoryImageCreateInfo::OPAQUE_FD` — the export handle
    ///   type that pairs with `cudaImportExternalMemory(OPAQUE_FD)` on
    ///   the consumer side.
    /// - `DEDICATED_MEMORY` allocation flag — CUDA's
    ///   `cudaExternalMemoryGetMappedMipmappedArray` imports the whole
    ///   `VkDeviceMemory` block.
    ///
    /// VMA's `find_memory_type_index_for_image_info` works for OPTIMAL
    /// tiling (unlike `DRM_FORMAT_MODIFIER_EXT`, which requires the
    /// raw `vkCreateImage` + `vkGetImageMemoryRequirements` probe used
    /// in `probe_tiled_dma_buf_memory_type`).
    pub(super) fn create_opaque_fd_image_pool(
        allocator: &Arc<vma::Allocator>,
    ) -> Result<(
        VmaExportPoolPinnedToOneMemoryType,
        Box<vk::ExportMemoryAllocateInfo>,
    )> {
        let mut probe_external_info = vk::ExternalMemoryImageCreateInfo::builder()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let probe_image_info = vk::ImageCreateInfo::builder()
            .image_type(vk::ImageType::_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D {
                width: 64,
                height: 64,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::STORAGE,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut probe_external_info);
        let probe_alloc_opts = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::DEDICATED_MEMORY,
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };
        let mem_type_idx = unsafe {
            allocator.find_memory_type_index_for_image_info(probe_image_info, &probe_alloc_opts)
        }
        .map_err(|e| Error::GpuError(format!("find memory type for OPAQUE_FD image pool: {e}")))?;

        let mut export_info = Box::new(
            vk::ExportMemoryAllocateInfo::builder()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD)
                .build(),
        );
        let mut pool_options = vma::PoolOptions::default();
        pool_options = pool_options.push_next(export_info.as_mut());
        pool_options.memory_type_index = mem_type_idx;
        let pool = allocator
            .create_pool(&pool_options)
            .map_err(|e| Error::GpuError(format!("create OPAQUE_FD image pool: {e}")))?;

        tracing::info!(
            "OPAQUE_FD VMA image pool created — mem_type={}",
            mem_type_idx
        );
        Ok((
            VmaExportPoolPinnedToOneMemoryType {
                pool,
                memory_type_index: mem_type_idx,
            },
            export_info,
        ))
    }
}

impl HostVulkanDevice {
    /// Get the VMA pool for DMA-BUF exportable HOST_VISIBLE buffers.
    /// Returns None on a device that exports no DMA-BUF.
    pub fn dma_buf_buffer_pool(&self) -> Option<&vma::Pool> {
        self.dma_buf_buffer_pool
            .as_ref()
            .map(|pinned_pool| &pinned_pool.pool)
    }

    /// Get the VMA pool for DMA-BUF exportable DEVICE_LOCAL images.
    /// Returns None on a device that exports no DMA-BUF.
    pub fn dma_buf_image_pool(&self) -> Option<&vma::Pool> {
        self.dma_buf_image_pool
            .as_ref()
            .map(|pinned_pool| &pinned_pool.pool)
    }

    /// Get the VMA pool for DMA-BUF exportable images created with
    /// `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`.
    ///
    /// Returns `None` when the EGL probe didn't find any RT-capable modifier
    /// (no libEGL, no display, extension missing, or driver doesn't expose
    /// `external_only=FALSE` modifiers for BGRA). Callers must refuse the
    /// allocation in that case rather than fall back to LINEAR — LINEAR is
    /// sampler-only on NVIDIA.
    pub fn dma_buf_image_pool_tiled(&self) -> Option<&vma::Pool> {
        self.dma_buf_image_pool_tiled
            .as_ref()
            .map(|pinned_pool| &pinned_pool.pool)
    }

    /// VMA pool dedicated to OPAQUE_FD-exportable HOST_VISIBLE buffers.
    ///
    /// Returns `None` when external memory isn't supported on this device,
    /// or when the pool's memory-type probe failed at construction.
    /// Used by CUDA / OpenCL interop — see
    /// [`crate::vulkan::rhi::HostVulkanBuffer::new_opaque_fd_export`].
    pub fn opaque_fd_buffer_pool(&self) -> Option<&vma::Pool> {
        self.opaque_fd_buffer_pool
            .as_ref()
            .map(|pinned_pool| &pinned_pool.pool)
    }

    /// VMA pool dedicated to OPAQUE_FD-exportable HOST_VISIBLE buffers on
    /// a HOST_CACHED memory type — the CPU-read sibling of
    /// [`Self::opaque_fd_buffer_pool`].
    ///
    /// `HOST_CACHED` is a VMA *preference*, never a requirement, so this
    /// is `None` on a device whose probe came back uncached — as well as
    /// when external memory is unsupported.
    ///
    /// `None` is not a refusal:
    /// [`crate::vulkan::rhi::HostVulkanBuffer::new_opaque_fd_export_host_cached`]
    /// degrades to [`Self::opaque_fd_buffer_pool`].
    pub fn opaque_fd_buffer_pool_host_cached(&self) -> Option<&vma::Pool> {
        self.opaque_fd_buffer_pool_host_cached
            .as_ref()
            .map(|pinned_pool| &pinned_pool.pool)
    }

    /// VMA pool dedicated to OPAQUE_FD-exportable DEVICE_LOCAL buffers
    /// (GPU-resident sibling of [`Self::opaque_fd_buffer_pool`]).
    ///
    /// Returns `None` when external memory isn't supported or the
    /// DEVICE_LOCAL probe failed at construction. Used by GPU-resident
    /// CUDA interop paths — see
    /// [`crate::vulkan::rhi::HostVulkanBuffer::new_opaque_fd_export_device_local`].
    pub fn opaque_fd_buffer_pool_device_local(&self) -> Option<&vma::Pool> {
        self.opaque_fd_buffer_pool_device_local
            .as_ref()
            .map(|pinned_pool| &pinned_pool.pool)
    }

    /// VMA pool dedicated to OPAQUE_FD-exportable DEVICE_LOCAL **images**
    /// (image-flavored sibling of [`Self::opaque_fd_buffer_pool_device_local`]).
    /// `VK_IMAGE_TILING_OPTIMAL`, no DRM modifier.
    ///
    /// Returns `None` when external memory isn't supported or the
    /// image-side probe failed at construction. Used by CUDA's
    /// `cudaExternalMemoryGetMappedMipmappedArray` import path — see
    /// [`crate::vulkan::rhi::HostVulkanTexture::new_opaque_fd_export`].
    pub fn opaque_fd_image_pool(&self) -> Option<&vma::Pool> {
        self.opaque_fd_image_pool
            .as_ref()
            .map(|pinned_pool| &pinned_pool.pool)
    }

    /// [`Self::dma_buf_buffer_pool`] with the memory type it is pinned to.
    pub(crate) fn dma_buf_buffer_pool_pinned_to_one_memory_type(
        &self,
    ) -> Option<&VmaExportPoolPinnedToOneMemoryType> {
        self.dma_buf_buffer_pool.as_ref()
    }

    /// [`Self::dma_buf_image_pool`] with the memory type it is pinned to.
    pub(crate) fn dma_buf_image_pool_pinned_to_one_memory_type(
        &self,
    ) -> Option<&VmaExportPoolPinnedToOneMemoryType> {
        self.dma_buf_image_pool.as_ref()
    }

    /// [`Self::dma_buf_image_pool_tiled`] with the memory type it is pinned to.
    pub(crate) fn dma_buf_image_pool_tiled_pinned_to_one_memory_type(
        &self,
    ) -> Option<&VmaExportPoolPinnedToOneMemoryType> {
        self.dma_buf_image_pool_tiled.as_ref()
    }

    /// [`Self::opaque_fd_buffer_pool`] with the memory type it is pinned to.
    pub(crate) fn opaque_fd_buffer_pool_pinned_to_one_memory_type(
        &self,
    ) -> Option<&VmaExportPoolPinnedToOneMemoryType> {
        self.opaque_fd_buffer_pool.as_ref()
    }

    /// [`Self::opaque_fd_buffer_pool_host_cached`] with the memory type it is pinned to.
    pub(crate) fn opaque_fd_buffer_pool_host_cached_pinned_to_one_memory_type(
        &self,
    ) -> Option<&VmaExportPoolPinnedToOneMemoryType> {
        self.opaque_fd_buffer_pool_host_cached.as_ref()
    }

    /// [`Self::opaque_fd_buffer_pool_device_local`] with the memory type it is pinned to.
    pub(crate) fn opaque_fd_buffer_pool_device_local_pinned_to_one_memory_type(
        &self,
    ) -> Option<&VmaExportPoolPinnedToOneMemoryType> {
        self.opaque_fd_buffer_pool_device_local.as_ref()
    }

    /// [`Self::opaque_fd_image_pool`] with the memory type it is pinned to.
    pub(crate) fn opaque_fd_image_pool_pinned_to_one_memory_type(
        &self,
    ) -> Option<&VmaExportPoolPinnedToOneMemoryType> {
        self.opaque_fd_image_pool.as_ref()
    }

    /// Render-target-capable DRM format modifiers, by DRM FOURCC, from the
    /// EGL probe at device init. Empty when the probe failed. Callers
    /// pass [`DrmModifierTable::rt_modifiers`] into
    /// [`HostVulkanTexture::new_render_target_dma_buf`](crate::vulkan::rhi::HostVulkanTexture::new_render_target_dma_buf).
    pub fn drm_modifier_table(&self) -> &Arc<DrmModifierTable> {
        &self.drm_modifier_table
    }

    /// Whether this device can allocate a cross-process-importable DMA-BUF
    /// image for `format`: the format has a DRM FOURCC and the EGL probe
    /// advertised at least one RT-capable modifier for it.
    pub fn has_render_target_modifier_for_texture_format(
        &self,
        format: crate::core::rhi::TextureFormat,
    ) -> bool {
        drm_modifier_probe::fourcc::drm_fourcc_for_texture_format(format)
            .is_some_and(|fourcc| self.drm_modifier_table.has_rt_modifier(fourcc))
    }
}
