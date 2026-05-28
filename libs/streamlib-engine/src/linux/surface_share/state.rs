// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-runtime surface table backing the runtime-internal surface-sharing
//! service. Stores DMA-BUF fds keyed by `surface_id` so polyglot
//! subprocesses can `check_out` them via `SCM_RIGHTS`.
//!
//! Each surface may hold up to [`streamlib_surface_client::MAX_DMA_BUF_PLANES`]
//! fds — one per plane for multi-plane DMA-BUFs under DRM format modifiers
//! (e.g. NV12 with separate Y and UV allocations). Single-plane surfaces
//! register a one-element vec; the multi-plane path is strictly additive.

use std::collections::HashMap;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;

#[derive(Debug)]
pub struct SurfaceMetadata {
    pub surface_id: String,
    pub runtime_id: String,
    /// Memory FDs for the surface — one per plane for multi-plane DMA-BUFs,
    /// a single FD for OPAQUE_FD `VkBuffer`-backed surfaces. The wire type
    /// (and the importer-side API to use) is encoded in
    /// [`Self::handle_type`].
    pub dma_buf_fds: Vec<RawFd>,
    pub plane_sizes: Vec<u64>,
    pub plane_offsets: Vec<u64>,
    /// Per-plane row pitch in bytes — what the consumer-side EGL or
    /// Vulkan import passes via `EGL_DMA_BUF_PLANE{N}_PITCH_EXT` /
    /// `VkSubresourceLayout::rowPitch`. One entry per plane fd; defaults
    /// to a vec of zeros for legacy registrations that didn't supply it.
    /// Unused for OPAQUE_FD registrations (flat memory, no per-plane
    /// layout) — set to a single zero entry there.
    pub plane_strides: Vec<u64>,
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub resource_type: String,
    /// Wire-level discriminator for the FDs in [`Self::dma_buf_fds`]:
    /// `"dma_buf"` (default, every legacy surface) for DMA-BUF-typed
    /// FDs that EGL / V4L2 / multi-plane Vulkan importers consume; or
    /// `"opaque_fd"` for OPAQUE_FD-typed FDs that Vulkan-aware importers
    /// (CUDA via UUID-matched device, peer VkInstance) consume. The host
    /// sets this from the `RhiExternalHandle` variant returned by
    /// [`crate::vulkan::rhi::HostVulkanBuffer::export_external_handle`].
    pub handle_type: String,
    /// DRM format modifier of the underlying VkImage. Zero means
    /// `DRM_FORMAT_MOD_LINEAR` (sampler-only on NVIDIA — see
    /// `docs/learnings/nvidia-egl-dmabuf-render-target.md`) or "not set"
    /// for legacy `VkBuffer`-backed surfaces (CPU-readable pixel buffers).
    /// Render-target adapters MUST receive a non-zero modifier picked
    /// from the EGL `external_only=FALSE` set; otherwise consumer-side
    /// FBO completeness will fail on NVIDIA.
    pub drm_format_modifier: u64,
    /// Optional OPAQUE_FD timeline-semaphore handle for the `produce_done`
    /// edge — signaled by the producer process when GPU writes complete,
    /// waited on by the consumer before reading. The host always owns the
    /// underlying `HostVulkanTimelineSemaphore`; consumers import the FD
    /// via `ConsumerVulkanTimelineSemaphore::from_imported_opaque_fd` (in
    /// subprocess adapters) or `HostVulkanTimelineSemaphore::from_imported_opaque_fd`
    /// (host adapters consuming a peer surface). `None` for surfaces that
    /// don't need explicit Vulkan sync (the OpenGL adapter path, legacy
    /// `VkBuffer` pixel buffers without dual-timeline coordination).
    ///
    /// Half of the single-writer-per-edge pair documented in
    /// `docs/architecture/adapter-timeline-single-writer.md`; the
    /// consumer-side companion lives in [`Self::consume_done_fd`].
    pub produce_done_fd: Option<RawFd>,
    /// Optional OPAQUE_FD timeline-semaphore handle for the `consume_done`
    /// edge — signaled by the consumer process when consumption completes,
    /// waited on by the producer before re-writing. Host-allocated like
    /// [`Self::produce_done_fd`] (cdylib consumers can't allocate exportable
    /// memory per the import-side carve-out); the consumer process imports
    /// the FD and signals it via host-CPU `vkSignalSemaphore`. `None` for
    /// surfaces that don't need the producer-side drain-wait — typically
    /// the same surfaces that have `produce_done_fd = None`, but they're
    /// independent options.
    pub consume_done_fd: Option<RawFd>,
    pub checkout_count: u64,
    /// Cross-process Vulkan-image-layout (i32 per `VkImageLayout`), the
    /// **single source of truth for cross-process layout state** — same
    /// semantics as [`streamlib_adapter_abi::SurfaceSyncState::current_image_layout`],
    /// lifted into the surface-share daemon so any peer (host engine,
    /// subprocess adapter, host adapter) can read or update it through
    /// the same wire format. Producers update on QFOT release;
    /// consumers (`GpuContext::resolve_texture_registration_by_surface_id` Path 2,
    /// host-side adapters) read for the source layout of their first
    /// QFOT acquire barrier.
    ///
    /// `0` (`VK_IMAGE_LAYOUT_UNDEFINED`) is the back-compat default for
    /// surfaces registered before the IPC schema lift (issue #633). Atomic
    /// because producer-release and consumer-acquire can race; load with
    /// `Acquire`, store with `Release`.
    pub current_image_layout: AtomicI32,
    /// `VkImageType` (raw `i32`): `_1D = 0`, `_2D = 1`, `_3D = 2`. Carries
    /// the host's `VkImageCreateInfo::imageType` across the wire so the
    /// consumer can reconstruct a matching `VkImage` for OPAQUE_FD import
    /// — `cudaExternalMemoryGetMappedMipmappedArray` requires the
    /// consumer-side `VkImageCreateInfo` to match the host's byte-for-byte.
    /// Defaults to `1` (`_2D`) when absent — the only flavor every
    /// in-tree allocator emits today.
    pub vk_image_type: i32,
    /// `VkImageCreateInfo::mipLevels`. Defaults to `1` when absent.
    pub vk_image_mip_levels: u32,
    /// `VkImageCreateInfo::arrayLayers`. Defaults to `1` when absent.
    pub vk_image_array_layers: u32,
    /// `VkSampleCountFlagBits` (raw `i32`): `_1 = 1`, `_2 = 2`, `_4 = 4`,
    /// `_8 = 8`, etc. Defaults to `1` (`_1`) when absent.
    pub vk_image_samples: i32,
    /// `VkImageTiling` (raw `i32`): `OPTIMAL = 0`, `LINEAR = 1`,
    /// `DRM_FORMAT_MODIFIER_EXT = 1000158000`. Defaults to `0` (`OPTIMAL`)
    /// when absent — the OPAQUE_FD image flavor the new field set
    /// primarily serves.
    pub vk_image_tiling: i32,
    /// `VkImageUsageFlags` (raw `u32` bitfield). Defaults to
    /// `TRANSFER_SRC | TRANSFER_DST | SAMPLED | STORAGE = 0x0F` when
    /// absent — the usage set [`crate::vulkan::rhi::HostVulkanTexture::new_opaque_fd_export`]
    /// emits and the consumer side hardcodes today.
    pub vk_image_usage: u32,
    /// Host-side `VkMemoryRequirements::size` (i.e. `vmaGetAllocationInfo().size`)
    /// of the imported VkImage's backing memory. Required for the
    /// consumer's `vkAllocateMemory(VkImportMemoryFdInfoKHR)` size
    /// argument; cross-device size mismatches reject the import.
    /// Defaults to `0` ("unknown — consumer computes from width / height
    /// / format") when absent — preserves the existing
    /// pixel-buffer / DMA-BUF behavior where size is derived consumer-side.
    pub vk_image_allocation_size: u64,
}

// The atomic field makes `SurfaceMetadata` not `Clone`-by-derive. Hand-roll
// a snapshot clone that promotes the layout to its current loaded value.
impl Clone for SurfaceMetadata {
    fn clone(&self) -> Self {
        Self {
            surface_id: self.surface_id.clone(),
            runtime_id: self.runtime_id.clone(),
            dma_buf_fds: self.dma_buf_fds.clone(),
            plane_sizes: self.plane_sizes.clone(),
            plane_offsets: self.plane_offsets.clone(),
            plane_strides: self.plane_strides.clone(),
            width: self.width,
            height: self.height,
            format: self.format.clone(),
            resource_type: self.resource_type.clone(),
            handle_type: self.handle_type.clone(),
            drm_format_modifier: self.drm_format_modifier,
            produce_done_fd: self.produce_done_fd,
            consume_done_fd: self.consume_done_fd,
            checkout_count: self.checkout_count,
            current_image_layout: AtomicI32::new(
                self.current_image_layout.load(Ordering::Acquire),
            ),
            vk_image_type: self.vk_image_type,
            vk_image_mip_levels: self.vk_image_mip_levels,
            vk_image_array_layers: self.vk_image_array_layers,
            vk_image_samples: self.vk_image_samples,
            vk_image_tiling: self.vk_image_tiling,
            vk_image_usage: self.vk_image_usage,
            vk_image_allocation_size: self.vk_image_allocation_size,
        }
    }
}

/// Documented defaults for the `vk_image_*` fields when the wire
/// payload omits them. Match
/// [`crate::vulkan::rhi::HostVulkanTexture::new_opaque_fd_export`]'s
/// hardcoded shape so a daemon serving these defaults produces a
/// `VkImageCreateInfo` byte-equal to what the existing consumer-side
/// `from_opaque_fd` constructor already builds.
pub const VK_IMAGE_TYPE_DEFAULT: i32 = 1; // VK_IMAGE_TYPE_2D
pub const VK_IMAGE_MIP_LEVELS_DEFAULT: u32 = 1;
pub const VK_IMAGE_ARRAY_LAYERS_DEFAULT: u32 = 1;
pub const VK_IMAGE_SAMPLES_DEFAULT: i32 = 1; // VK_SAMPLE_COUNT_1_BIT
pub const VK_IMAGE_TILING_DEFAULT: i32 = 0; // VK_IMAGE_TILING_OPTIMAL
/// `TRANSFER_SRC (0x01) | TRANSFER_DST (0x02) | SAMPLED (0x04) | STORAGE (0x08)`.
pub const VK_IMAGE_USAGE_DEFAULT: u32 = 0x0F;
pub const VK_IMAGE_ALLOCATION_SIZE_DEFAULT: u64 = 0;

/// Thread-safe surface table for the runtime-internal surface-share service.
#[derive(Clone, Default)]
pub struct SurfaceShareState {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    surfaces: RwLock<HashMap<String, SurfaceMetadata>>,
    surface_counter: AtomicU64,
}

/// Result of [`SurfaceShareState::get_surface_planes`] — everything a
/// consumer needs to import the DMA-BUF as a Vulkan or EGL image, or the
/// OPAQUE_FD memory as a Vulkan-aware buffer (CUDA / peer VkInstance).
#[derive(Clone, Debug)]
pub struct SurfacePlaneCheckout {
    pub dma_buf_fds: Vec<RawFd>,
    pub plane_sizes: Vec<u64>,
    pub plane_offsets: Vec<u64>,
    pub plane_strides: Vec<u64>,
    pub drm_format_modifier: u64,
    /// Wire-level discriminator for [`Self::dma_buf_fds`] — see
    /// [`SurfaceMetadata::handle_type`]. Consumer-side import dispatches
    /// on this value: `"dma_buf"` → `from_dma_buf_fds`, `"opaque_fd"` →
    /// `from_opaque_fd`.
    pub handle_type: String,
    /// Optional OPAQUE_FD for the producer-side `produce_done` timeline
    /// semaphore — see [`SurfaceMetadata::produce_done_fd`]. `None` for
    /// surfaces registered without one. The table-owned fd is returned
    /// as-is; callers that hand it out via SCM_RIGHTS must `dup` it
    /// first, just like the memory fds.
    pub produce_done_fd: Option<RawFd>,
    /// Optional OPAQUE_FD for the consumer-side `consume_done` timeline
    /// semaphore — see [`SurfaceMetadata::consume_done_fd`]. Same
    /// table-owned-fd semantics as [`Self::produce_done_fd`].
    pub consume_done_fd: Option<RawFd>,
    /// Snapshot of the surface's [`SurfaceMetadata::current_image_layout`]
    /// at lookup time (i32 per `VkImageLayout`). Consumers feed this into
    /// the source layout of their first QFOT acquire barrier. `0`
    /// (UNDEFINED) when no producer has declared a layout — the
    /// back-compat default for surfaces registered before issue #633.
    pub current_image_layout: i32,
    /// Snapshot of [`SurfaceMetadata::vk_image_type`] at lookup time.
    pub vk_image_type: i32,
    /// Snapshot of [`SurfaceMetadata::vk_image_mip_levels`] at lookup time.
    pub vk_image_mip_levels: u32,
    /// Snapshot of [`SurfaceMetadata::vk_image_array_layers`] at lookup time.
    pub vk_image_array_layers: u32,
    /// Snapshot of [`SurfaceMetadata::vk_image_samples`] at lookup time.
    pub vk_image_samples: i32,
    /// Snapshot of [`SurfaceMetadata::vk_image_tiling`] at lookup time.
    pub vk_image_tiling: i32,
    /// Snapshot of [`SurfaceMetadata::vk_image_usage`] at lookup time.
    pub vk_image_usage: u32,
    /// Snapshot of [`SurfaceMetadata::vk_image_allocation_size`] at
    /// lookup time.
    pub vk_image_allocation_size: u64,
}

/// Arguments to [`SurfaceShareState::register_surface`]. Grouped so the
/// signature stays legible as the per-plane fields grow.
pub struct SurfaceRegistration<'a> {
    pub surface_id: &'a str,
    pub runtime_id: &'a str,
    pub dma_buf_fds: Vec<RawFd>,
    pub plane_sizes: Vec<u64>,
    pub plane_offsets: Vec<u64>,
    /// Per-plane row pitch in bytes. Length must match `dma_buf_fds`.
    pub plane_strides: Vec<u64>,
    pub width: u32,
    pub height: u32,
    pub format: &'a str,
    pub resource_type: &'a str,
    /// Wire-level discriminator: `"dma_buf"` (default for legacy paths)
    /// or `"opaque_fd"` (Vulkan-aware importers like CUDA). Pass
    /// `"dma_buf"` if you don't know.
    pub handle_type: &'a str,
    /// DRM format modifier of the underlying VkImage. See
    /// [`SurfaceMetadata::drm_format_modifier`].
    pub drm_format_modifier: u64,
    /// Optional OPAQUE_FD timeline-semaphore handle for the producer-side
    /// `produce_done` edge (`HostVulkanTimelineSemaphore::export_opaque_fd`).
    /// The table takes ownership on success and closes it on
    /// `release_surface`. `None` for adapters that don't need explicit
    /// Vulkan sync. See [`SurfaceMetadata::produce_done_fd`] for the
    /// full single-writer-per-edge contract.
    pub produce_done_fd: Option<RawFd>,
    /// Optional OPAQUE_FD timeline-semaphore handle for the consumer-side
    /// `consume_done` edge. Same ownership semantics as
    /// [`Self::produce_done_fd`]. `None` for surfaces that don't carry a
    /// consumer-side signal channel (e.g. one-shot setup surfaces, or
    /// pre-single-writer-lift legacy registrations).
    pub consume_done_fd: Option<RawFd>,
    /// Initial `VkImageLayout` (i32) to seed
    /// [`SurfaceMetadata::current_image_layout`]. Producers that publish
    /// in a known steady-state layout (camera, OpenGL adapter wiring,
    /// Vulkan compute outputs) pass the layout the surface lives in
    /// immediately after the registration returns. `0` (UNDEFINED) is
    /// the back-compat default — host consumers fall back to
    /// `oldLayout=UNDEFINED` (content-discard permitted) when no producer
    /// has declared a layout.
    pub current_image_layout: i32,
    /// `VkImageCreateInfo::imageType` (raw `i32`). See
    /// [`SurfaceMetadata::vk_image_type`]. Pass [`VK_IMAGE_TYPE_DEFAULT`]
    /// when the surface isn't an OPAQUE_FD `VkImage` or when the consumer
    /// can rely on the default `_2D` shape.
    pub vk_image_type: i32,
    /// `VkImageCreateInfo::mipLevels`. Pass [`VK_IMAGE_MIP_LEVELS_DEFAULT`]
    /// (= 1) for the back-compat shape.
    pub vk_image_mip_levels: u32,
    /// `VkImageCreateInfo::arrayLayers`. Pass
    /// [`VK_IMAGE_ARRAY_LAYERS_DEFAULT`] (= 1) for the back-compat shape.
    pub vk_image_array_layers: u32,
    /// `VkSampleCountFlagBits` (raw `i32`). Pass [`VK_IMAGE_SAMPLES_DEFAULT`]
    /// (= 1) for the back-compat shape.
    pub vk_image_samples: i32,
    /// `VkImageTiling` (raw `i32`). Pass [`VK_IMAGE_TILING_DEFAULT`]
    /// (= `OPTIMAL`) for OPAQUE_FD images.
    pub vk_image_tiling: i32,
    /// `VkImageUsageFlags` (raw `u32` bitfield). Pass
    /// [`VK_IMAGE_USAGE_DEFAULT`] for the back-compat OPAQUE_FD usage set.
    pub vk_image_usage: u32,
    /// Host-side `VkMemoryRequirements::size`. Pass
    /// [`VK_IMAGE_ALLOCATION_SIZE_DEFAULT`] (= 0) when the consumer
    /// derives the size from `width * height * bytes_per_pixel`.
    pub vk_image_allocation_size: u64,
}

impl SurfaceShareState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a surface into the table.
    ///
    /// On rejection (duplicate surface_id), ownership of `dma_buf_fds`,
    /// `produce_done_fd`, and `consume_done_fd` is returned to the caller
    /// (the timeline FDs as the `Err` tuple's second and third slots) so
    /// it can decide whether to close them or hand them to the next
    /// attempt. On success, the table owns every fd passed in and closes
    /// them on [`Self::release_surface`].
    pub fn register_surface(
        &self,
        reg: SurfaceRegistration<'_>,
    ) -> Result<(), (Vec<RawFd>, Option<RawFd>, Option<RawFd>)> {
        let mut surfaces = self.inner.surfaces.write();

        if surfaces.contains_key(reg.surface_id) {
            return Err((reg.dma_buf_fds, reg.produce_done_fd, reg.consume_done_fd));
        }

        self.inner.surface_counter.fetch_add(1, Ordering::Relaxed);

        surfaces.insert(
            reg.surface_id.to_string(),
            SurfaceMetadata {
                surface_id: reg.surface_id.to_string(),
                runtime_id: reg.runtime_id.to_string(),
                dma_buf_fds: reg.dma_buf_fds,
                plane_sizes: reg.plane_sizes,
                plane_offsets: reg.plane_offsets,
                plane_strides: reg.plane_strides,
                width: reg.width,
                height: reg.height,
                format: reg.format.to_string(),
                resource_type: reg.resource_type.to_string(),
                handle_type: reg.handle_type.to_string(),
                drm_format_modifier: reg.drm_format_modifier,
                produce_done_fd: reg.produce_done_fd,
                consume_done_fd: reg.consume_done_fd,
                checkout_count: 0,
                current_image_layout: AtomicI32::new(reg.current_image_layout),
                vk_image_type: reg.vk_image_type,
                vk_image_mip_levels: reg.vk_image_mip_levels,
                vk_image_array_layers: reg.vk_image_array_layers,
                vk_image_samples: reg.vk_image_samples,
                vk_image_tiling: reg.vk_image_tiling,
                vk_image_usage: reg.vk_image_usage,
                vk_image_allocation_size: reg.vk_image_allocation_size,
            },
        );
        Ok(())
    }

    /// Update the surface's published `current_image_layout` atomically.
    /// Producers call this through the surface-share `update_layout` op
    /// after their QFOT release barrier records, so the next consumer's
    /// lookup sees the post-release layout. Returns `false` if the
    /// surface_id is unknown.
    pub fn update_image_layout(&self, surface_id: &str, layout: i32) -> bool {
        let surfaces = self.inner.surfaces.read();
        match surfaces.get(surface_id) {
            Some(metadata) => {
                metadata
                    .current_image_layout
                    .store(layout, Ordering::Release);
                true
            }
            None => false,
        }
    }

    /// Return a clone of the surface's plane fd vec plus its plane-layout
    /// arrays, the underlying VkImage's DRM format modifier, the
    /// last-published `current_image_layout`, and (if registered) the
    /// timeline-semaphore OPAQUE_FD. The returned fds are the table's
    /// own — callers that hand them out via SCM_RIGHTS must `dup` each
    /// fd first.
    pub fn get_surface_planes(
        &self,
        surface_id: &str,
    ) -> Option<SurfacePlaneCheckout> {
        let mut surfaces = self.inner.surfaces.write();
        surfaces.get_mut(surface_id).map(|metadata| {
            metadata.checkout_count += 1;
            SurfacePlaneCheckout {
                dma_buf_fds: metadata.dma_buf_fds.clone(),
                plane_sizes: metadata.plane_sizes.clone(),
                plane_offsets: metadata.plane_offsets.clone(),
                plane_strides: metadata.plane_strides.clone(),
                drm_format_modifier: metadata.drm_format_modifier,
                handle_type: metadata.handle_type.clone(),
                produce_done_fd: metadata.produce_done_fd,
                consume_done_fd: metadata.consume_done_fd,
                current_image_layout: metadata
                    .current_image_layout
                    .load(Ordering::Acquire),
                vk_image_type: metadata.vk_image_type,
                vk_image_mip_levels: metadata.vk_image_mip_levels,
                vk_image_array_layers: metadata.vk_image_array_layers,
                vk_image_samples: metadata.vk_image_samples,
                vk_image_tiling: metadata.vk_image_tiling,
                vk_image_usage: metadata.vk_image_usage,
                vk_image_allocation_size: metadata.vk_image_allocation_size,
            }
        })
    }

    pub fn release_surface(&self, surface_id: &str, runtime_id: &str) -> bool {
        let mut surfaces = self.inner.surfaces.write();
        if let Some(metadata) = surfaces.get(surface_id) {
            if metadata.runtime_id == runtime_id {
                for fd in &metadata.dma_buf_fds {
                    unsafe { libc::close(*fd) };
                }
                if let Some(fd) = metadata.produce_done_fd {
                    unsafe { libc::close(fd) };
                }
                if let Some(fd) = metadata.consume_done_fd {
                    unsafe { libc::close(fd) };
                }
                surfaces.remove(surface_id);
                return true;
            }
        }
        false
    }

    pub fn get_surfaces(&self) -> Vec<SurfaceMetadata> {
        self.inner.surfaces.read().values().cloned().collect()
    }

    /// Surface ids registered by `runtime_id`. Used by the EPOLLHUP watchdog
    /// to find what to release when a subprocess connection drops.
    pub fn surface_ids_by_runtime(&self, runtime_id: &str) -> Vec<String> {
        self.inner
            .surfaces
            .read()
            .values()
            .filter(|m| m.runtime_id == runtime_id)
            .map(|m| m.surface_id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg<'a>(surface_id: &'a str, runtime_id: &'a str, resource_type: &'a str) -> SurfaceRegistration<'a> {
        SurfaceRegistration {
            surface_id,
            runtime_id,
            dma_buf_fds: vec![-1],
            plane_sizes: vec![0],
            plane_offsets: vec![0],
            plane_strides: vec![0],
            width: 1920,
            height: 1080,
            format: "Rgba8Unorm",
            resource_type,
            handle_type: "dma_buf",
            drm_format_modifier: 0,
            produce_done_fd: None,
            consume_done_fd: None,
            current_image_layout: 0,
            vk_image_type: VK_IMAGE_TYPE_DEFAULT,
            vk_image_mip_levels: VK_IMAGE_MIP_LEVELS_DEFAULT,
            vk_image_array_layers: VK_IMAGE_ARRAY_LAYERS_DEFAULT,
            vk_image_samples: VK_IMAGE_SAMPLES_DEFAULT,
            vk_image_tiling: VK_IMAGE_TILING_DEFAULT,
            vk_image_usage: VK_IMAGE_USAGE_DEFAULT,
            vk_image_allocation_size: VK_IMAGE_ALLOCATION_SIZE_DEFAULT,
        }
    }

    #[test]
    fn register_surface_with_resource_type() {
        let state = SurfaceShareState::new();
        assert!(state
            .register_surface(reg("buf-001", "runtime-1", "pixel_buffer"))
            .is_ok());
        assert!(state
            .register_surface(reg("tex-001", "runtime-1", "texture"))
            .is_ok());

        let surfaces = state.get_surfaces();
        assert_eq!(surfaces.len(), 2);
        let buf = surfaces.iter().find(|s| s.surface_id == "buf-001").unwrap();
        assert_eq!(buf.resource_type, "pixel_buffer");
        let tex = surfaces.iter().find(|s| s.surface_id == "tex-001").unwrap();
        assert_eq!(tex.resource_type, "texture");
    }

    #[test]
    fn duplicate_surface_id_rejected() {
        let state = SurfaceShareState::new();
        assert!(state.register_surface(reg("dup", "rt", "texture")).is_ok());
        let (rejected_planes, rejected_produce, rejected_consume) = state
            .register_surface(reg("dup", "rt", "texture"))
            .expect_err("duplicate must be rejected");
        assert_eq!(rejected_planes, vec![-1], "rejected plane fds returned to caller");
        assert_eq!(rejected_produce, None, "no produce_done fd was registered, none returned");
        assert_eq!(rejected_consume, None, "no consume_done fd was registered, none returned");
    }

    /// The watchdog uses `surface_ids_by_runtime` to discover what to
    /// release when a subprocess connection drops. The query must group by
    /// `runtime_id` precisely — surfaces from sibling runtimes must not
    /// appear in the result, or one crash would clean up another runtime's
    /// state.
    #[test]
    fn surface_ids_by_runtime_groups_by_owner() {
        let state = SurfaceShareState::new();
        state
            .register_surface(reg("a-1", "runtime-A", "pixel_buffer"))
            .expect("a-1");
        state
            .register_surface(reg("a-2", "runtime-A", "pixel_buffer"))
            .expect("a-2");
        state
            .register_surface(reg("b-1", "runtime-B", "pixel_buffer"))
            .expect("b-1");

        let mut for_a = state.surface_ids_by_runtime("runtime-A");
        for_a.sort();
        assert_eq!(for_a, vec!["a-1".to_string(), "a-2".to_string()]);

        let for_b = state.surface_ids_by_runtime("runtime-B");
        assert_eq!(for_b, vec!["b-1".to_string()]);

        assert!(state.surface_ids_by_runtime("runtime-C").is_empty());

        // After release, the owner's set shrinks and others are unaffected.
        assert!(state.release_surface("a-1", "runtime-A"));
        let mut for_a_after = state.surface_ids_by_runtime("runtime-A");
        for_a_after.sort();
        assert_eq!(for_a_after, vec!["a-2".to_string()]);
        assert_eq!(
            state.surface_ids_by_runtime("runtime-B"),
            vec!["b-1".to_string()]
        );
    }

    /// `current_image_layout` round-trips through register → lookup, and
    /// `update_image_layout` updates the value visible to subsequent
    /// lookups. This is the per-surface half of issue #633's IPC schema
    /// lift: producers seed the layout at registration and re-publish it
    /// after their QFOT release barrier records, so cross-process
    /// consumers can `oldLayout=current_image_layout` instead of
    /// barriering defensively from `UNDEFINED`.
    #[test]
    fn current_image_layout_round_trip_and_update() {
        let state = SurfaceShareState::new();
        let mut initial = reg("layout-test", "rt", "texture");
        // VK_IMAGE_LAYOUT_GENERAL = 1
        initial.current_image_layout = 1;
        state
            .register_surface(initial)
            .expect("register seeded with GENERAL");

        let checkout = state
            .get_surface_planes("layout-test")
            .expect("lookup after register");
        assert_eq!(
            checkout.current_image_layout, 1,
            "lookup must echo the producer-declared layout"
        );

        // VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL = 5
        assert!(state.update_image_layout("layout-test", 5));
        let checkout = state
            .get_surface_planes("layout-test")
            .expect("lookup after update");
        assert_eq!(
            checkout.current_image_layout, 5,
            "subsequent lookup sees the post-update layout"
        );

        // Updating a missing surface_id returns false rather than panicking
        // — producer-side races with cleanup_runtime_surfaces shouldn't
        // bring the daemon down.
        assert!(!state.update_image_layout("missing", 0));
    }

    /// The seven `vk_image_*` fields round-trip through register → lookup:
    /// the producer's declared `VkImageCreateInfo` shape (#800) must
    /// survive verbatim so an OPAQUE_FD `VkImage` consumer can rebuild a
    /// matching `VkImage` whose `cudaExternalMemoryMipmappedArrayDesc`
    /// equals the host's byte-for-byte. Mentally revert the
    /// `register_surface` body (drop the seven assignments) and this test
    /// fails on the first field — locking the contract end-to-end through
    /// the typed `SurfaceShareState` API.
    #[test]
    fn vk_image_create_info_fields_round_trip_through_register_lookup() {
        let state = SurfaceShareState::new();
        // Pick a deliberately non-default value for every field — so a
        // regression that silently zeroes one is visible against the
        // assert. Values are spec-realistic (mipmapped 3D image with
        // multisample + multi-layer + custom usage).
        state
            .register_surface(SurfaceRegistration {
                surface_id: "vk-image-rt",
                runtime_id: "rt",
                dma_buf_fds: vec![-1],
                plane_sizes: vec![0],
                plane_offsets: vec![0],
                plane_strides: vec![0],
                width: 256,
                height: 256,
                format: "Rgba16Float",
                resource_type: "texture",
                handle_type: "opaque_fd",
                drm_format_modifier: 0,
                produce_done_fd: None,
            consume_done_fd: None,
                current_image_layout: 0,
                vk_image_type: 2,        // VK_IMAGE_TYPE_3D
                vk_image_mip_levels: 9,  // 256 = 2^8, plus base = 9 levels
                vk_image_array_layers: 6,
                vk_image_samples: 4,                // _4
                vk_image_tiling: 1000158000,        // DRM_FORMAT_MODIFIER_EXT
                vk_image_usage: 0x4F,               // 0x0F | COLOR_ATTACHMENT (0x40)
                vk_image_allocation_size: 16_777_216,
            })
            .expect("register vk-image-rt");

        let checkout = state
            .get_surface_planes("vk-image-rt")
            .expect("lookup after register");
        assert_eq!(checkout.vk_image_type, 2);
        assert_eq!(checkout.vk_image_mip_levels, 9);
        assert_eq!(checkout.vk_image_array_layers, 6);
        assert_eq!(checkout.vk_image_samples, 4);
        assert_eq!(checkout.vk_image_tiling, 1000158000);
        assert_eq!(checkout.vk_image_usage, 0x4F);
        assert_eq!(checkout.vk_image_allocation_size, 16_777_216);
    }

    /// Releasing a surface registered with multiple plane fds must close
    /// every fd — the state is the last owner of the table's fd dups and
    /// leaking any plane would leak the whole DMA-BUF. Verified via pipes:
    /// register hands the write end to the table, and after release the
    /// read end yields EOF on the next `read`. EOF is sticky and tied to
    /// the pipe's underlying kernel object, so unlike `fcntl(F_GETFD)` on
    /// a raw fd number, the assertion does not race against parallel
    /// threads recycling fd-table slots.
    #[test]
    fn release_surface_closes_every_plane_fd() {
        let state = SurfaceShareState::new();

        // Three pipes; we keep the read ends, hand the write ends to the
        // table.
        let mut read_fds: Vec<RawFd> = Vec::with_capacity(3);
        let mut write_fds: Vec<RawFd> = Vec::with_capacity(3);
        for _ in 0..3 {
            let mut fds = [0i32; 2];
            let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
            assert_eq!(rc, 0, "pipe: {}", std::io::Error::last_os_error());
            read_fds.push(fds[0]);
            write_fds.push(fds[1]);
        }

        state
            .register_surface(SurfaceRegistration {
                surface_id: "multi",
                runtime_id: "rt",
                dma_buf_fds: write_fds,
                plane_sizes: vec![8192, 2048, 2048],
                plane_offsets: vec![0, 0, 0],
                plane_strides: vec![64, 32, 32],
                width: 640,
                height: 480,
                format: "Nv12VideoRange",
                resource_type: "pixel_buffer",
                handle_type: "dma_buf",
                drm_format_modifier: 0,
                produce_done_fd: None,
            consume_done_fd: None,
                current_image_layout: 0,
                vk_image_type: VK_IMAGE_TYPE_DEFAULT,
                vk_image_mip_levels: VK_IMAGE_MIP_LEVELS_DEFAULT,
                vk_image_array_layers: VK_IMAGE_ARRAY_LAYERS_DEFAULT,
                vk_image_samples: VK_IMAGE_SAMPLES_DEFAULT,
                vk_image_tiling: VK_IMAGE_TILING_DEFAULT,
                vk_image_usage: VK_IMAGE_USAGE_DEFAULT,
                vk_image_allocation_size: VK_IMAGE_ALLOCATION_SIZE_DEFAULT,
            })
            .expect("register multi-plane");

        assert!(state.release_surface("multi", "rt"));

        // With the write ends closed, every read end now yields EOF (0
        // bytes) on the next read — the kernel signals that no more data
        // is coming and the pipe will never refill.
        for fd in &read_fds {
            let mut buf = [0u8; 1];
            let n = unsafe {
                libc::read(*fd, buf.as_mut_ptr() as *mut libc::c_void, 1)
            };
            assert_eq!(
                n, 0,
                "pipe read end {} should yield EOF after write end was closed by release_surface",
                fd
            );
            unsafe { libc::close(*fd) };
        }
    }
}
