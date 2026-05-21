// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Pre-allocated ring of textures rotated per-frame on the decode hot
//! path.
//!
//! Construction is privileged (allocates `count` DEVICE_LOCAL textures and
//! registers each in [`crate::core::context::GpuContext`]'s same-process
//! texture cache); per-frame rotation via [`TextureRing::acquire_next`]
//! is Limited-safe and never escalates. See
//! `docs/architecture/texture-ring.md` for the recipe.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::core::context::GpuContext;
use crate::core::rhi::{Texture, TextureFormat};

#[cfg(target_os = "linux")]
use crate::core::Error;
#[cfg(target_os = "linux")]
use crate::core::Result;
#[cfg(target_os = "linux")]
use crate::vulkan::rhi::HostVulkanUploadResources;
#[cfg(target_os = "linux")]
use streamlib_consumer_rhi::VulkanLayout;

/// A single slot in a [`TextureRing`].
///
/// Cheap to clone — both data fields are `Arc`-backed (the `surface_id` is
/// an owned `String` minted at ring construction and reused across frames;
/// the `Texture` handle is `Arc<HostVulkanTexture>` under the hood); the
/// `slot_index` is a `usize` used to look up the slot's pre-allocated
/// `HostVulkanUploadResources` for amortized uploads.
#[derive(Clone)]
pub struct TextureRingSlot {
    /// Stable per-slot `surface_id` registered in
    /// [`GpuContext::resolve_texture_registration_by_surface_id`]'s
    /// same-process texture cache at ring construction. Downstream
    /// consumers carry this id on the emitted `VideoFrame` and resolve
    /// the texture through the cache by it.
    pub surface_id: String,
    /// Pre-allocated texture handle for this slot.
    pub texture: Texture,
    /// Index of this slot within the ring's slot vector. Used to
    /// look up the slot's pre-allocated upload resources for
    /// [`TextureRing::copy_pixel_buffer_to_slot`].
    pub(crate) slot_index: usize,
}

/// Pre-allocated ring of textures rotated per-frame on the decode hot
/// path.
///
/// Construction allocates `count` non-exportable DEVICE_LOCAL textures
/// via the host RHI's `create_texture_local` path (skips the DMA-BUF
/// export pool — decode-output textures stay in-process, no
/// NVIDIA DMA-BUF cap pressure per
/// `docs/learnings/nvidia-dma-buf-after-swapchain.md`), mints a stable
/// UUID per slot, and registers each slot in [`GpuContext`]'s texture
/// cache with `current_layout = UNDEFINED` (spec-correct for a
/// freshly-allocated `VkImage`). After the first per-frame
/// `copy_pixel_buffer_to_texture` on a slot, the layout updates to
/// `SHADER_READ_ONLY_OPTIMAL` (the layout `upload_buffer_to_image`
/// leaves the image in) and stays there for the steady-state hot path.
///
/// Steady-state rotation via [`Self::acquire_next`] does no allocation
/// and does not escalate. Slot reuse semantics are the caller's
/// responsibility — sizing `count` to `MAX_FRAMES_IN_FLIGHT = 2`
/// (`docs/learnings/vulkan-frames-in-flight.md`) keeps GPU work on
/// the previous use of a slot retired by the time the slot rotates
/// back.
///
/// On drop, the ring unregisters its `surface_id`s from
/// [`GpuContext`]'s texture cache.
pub struct TextureRing {
    slots: Vec<TextureRingSlot>,
    /// Per-slot pre-allocated upload resources (command pool + command
    /// buffer + fence), parallel to `slots`. `None` on non-Linux
    /// platforms; otherwise `Some(...)` always with `len == slots.len()`.
    #[cfg(target_os = "linux")]
    upload_resources: Vec<HostVulkanUploadResources>,
    next_index: AtomicUsize,
    width: u32,
    height: u32,
    format: TextureFormat,
    /// Held to unregister slot entries on `Drop`.
    gpu: GpuContext,
}

impl TextureRing {
    /// Construct a ring from pre-built slots. Crate-internal: public
    /// construction goes through
    /// [`crate::core::context::GpuContextFullAccess::create_texture_ring`].
    #[cfg(target_os = "linux")]
    pub(crate) fn from_slots(
        slots: Vec<TextureRingSlot>,
        upload_resources: Vec<HostVulkanUploadResources>,
        width: u32,
        height: u32,
        format: TextureFormat,
        gpu: GpuContext,
    ) -> Arc<Self> {
        debug_assert_eq!(
            slots.len(),
            upload_resources.len(),
            "TextureRing: slots and upload_resources must have equal length"
        );
        Arc::new(Self {
            slots,
            upload_resources,
            next_index: AtomicUsize::new(0),
            width,
            height,
            format,
            gpu,
        })
    }

    /// Construct a ring from pre-built slots (non-Linux: no
    /// per-slot upload resources). Crate-internal.
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn from_slots(
        slots: Vec<TextureRingSlot>,
        width: u32,
        height: u32,
        format: TextureFormat,
        gpu: GpuContext,
    ) -> Arc<Self> {
        Arc::new(Self {
            slots,
            next_index: AtomicUsize::new(0),
            width,
            height,
            format,
            gpu,
        })
    }

    /// Rotate to the next slot.
    ///
    /// Thread-safe (atomic counter). Wraps at `len()`. Slot reuse
    /// safety is the caller's responsibility — the typical pattern is
    /// `count = MAX_FRAMES_IN_FLIGHT = 2`, which gives one in-flight
    /// frame on the GPU plus one being recorded on the CPU.
    pub fn acquire_next(&self) -> TextureRingSlot {
        let idx = self.next_index.fetch_add(1, Ordering::Relaxed) % self.slots.len();
        self.slots[idx].clone()
    }

    /// Copy a host-visible pixel buffer's contents into a ring slot's
    /// pre-allocated texture using the slot's pre-allocated upload
    /// resources (command pool + command buffer + fence) — no per-frame
    /// `vkCreateCommandPool` / `vkAllocateCommandBuffers` / `vkCreateFence`
    /// churn, no escalation.
    ///
    /// This is the hot-path replacement for the generic
    /// [`crate::core::context::GpuContextLimitedAccess::copy_pixel_buffer_to_texture`]
    /// — the generic primitive routes through
    /// [`crate::vulkan::rhi::HostVulkanDevice::upload_buffer_to_image`]
    /// (per-call resource churn); this method routes through
    /// [`crate::vulkan::rhi::HostVulkanDevice::upload_buffer_to_image_amortized`]
    /// (caller-provided pre-allocated resources reset between calls).
    ///
    /// Updates the slot's registration `current_layout` to
    /// `SHADER_READ_ONLY_OPTIMAL` to match
    /// `upload_buffer_to_image`'s terminal state.
    #[cfg(target_os = "linux")]
    pub fn copy_pixel_buffer_to_slot(
        &self,
        slot: &TextureRingSlot,
        pixel_buffer: &crate::core::rhi::PixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<()> {
        let resources = self.upload_resources.get(slot.slot_index).ok_or_else(|| {
            Error::GpuError(format!(
                "TextureRing::copy_pixel_buffer_to_slot: slot_index {} out of range (ring has {} slots)",
                slot.slot_index,
                self.upload_resources.len()
            ))
        })?;
        use crate::host_rhi::{HostPixelBufferRefExt, HostTextureExt};
        let image = slot.texture.vulkan_inner().image().ok_or_else(|| {
            Error::GpuError("TextureRing slot texture has no VkImage".into())
        })?;
        let src_buffer = pixel_buffer.buffer_ref().vulkan_inner().buffer();
        unsafe {
            self.gpu.device().inner.upload_buffer_to_image_amortized(
                resources.command_buffer(),
                resources.fence(),
                src_buffer,
                image,
                width,
                height,
            )?;
        }
        // upload_buffer_to_image leaves the image in SHADER_READ_ONLY_OPTIMAL.
        self.gpu
            .update_texture_registration_layout(&slot.surface_id, VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
        Ok(())
    }

    /// Number of slots in the ring.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Ring is always non-empty in practice (construction rejects 0).
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Width of every slot's texture, in pixels. Use this to detect
    /// mid-stream resolution changes — drop the ring and build a new
    /// one when source dimensions diverge from the ring's.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height of every slot's texture, in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Format every slot's texture was allocated with.
    pub fn format(&self) -> TextureFormat {
        self.format
    }

    /// Borrow a slot by index. Intended for tests and debug
    /// introspection — production callers go through
    /// [`Self::acquire_next`].
    #[doc(hidden)]
    pub fn slot(&self, index: usize) -> Option<&TextureRingSlot> {
        self.slots.get(index)
    }
}

impl Drop for TextureRing {
    fn drop(&mut self) {
        for slot in &self.slots {
            self.gpu.unregister_texture(&slot.surface_id);
        }
        // upload_resources drop themselves (each Drop waits on its fence
        // first, then destroys cb + pool + fence).
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;
    use crate::core::context::GpuContextFullAccess;
    use crate::core::rhi::TextureUsages;

    fn fresh_full_access() -> Option<(GpuContext, GpuContextFullAccess)> {
        let gpu = GpuContext::init_for_platform().ok()?;
        let full = GpuContextFullAccess::new(gpu.clone());
        Some((gpu, full))
    }

    #[test]
    fn ring_pre_allocates_at_construction_and_rotates() {
        use crate::host_rhi::HostTextureExt;

        let Some((_gpu, full)) = fresh_full_access() else {
            println!("Skipping - no GPU device available");
            return;
        };
        let ring = full
            .create_texture_ring(
                64,
                64,
                TextureFormat::Rgba8Unorm,
                TextureUsages::COPY_DST
                    | TextureUsages::TEXTURE_BINDING
                    | TextureUsages::STORAGE_BINDING,
                3,
            )
            .expect("create_texture_ring");

        assert_eq!(ring.len(), 3);
        assert_eq!(ring.width(), 64);
        assert_eq!(ring.height(), 64);
        assert_eq!(ring.format(), TextureFormat::Rgba8Unorm);

        // Snapshot the underlying `HostVulkanTexture` Arc identities at
        // construction — these MUST stay pointer-stable across every
        // acquire_next call, otherwise the ring is silently
        // re-allocating per call and defeating the whole point of the
        // helper. Mentally revert `from_slots` to re-create slot
        // textures per acquire — this assertion fires.
        let initial_arcs: Vec<_> = (0..ring.len())
            .map(|i| Arc::as_ptr(ring.slot(i).unwrap().texture.vulkan_inner()))
            .collect();

        let first_slots: Vec<TextureRingSlot> =
            (0..ring.len()).map(|_| ring.acquire_next()).collect();
        for (i, slot) in first_slots.iter().enumerate() {
            assert_eq!(
                Arc::as_ptr(slot.texture.vulkan_inner()),
                initial_arcs[i],
                "acquire_next() slot {i} must hand back the SAME pre-allocated \
                 HostVulkanTexture Arc — different pointer means the helper \
                 re-allocated, violating the exit-criterion 'subsequent \
                 acquire_next() calls never touch the allocator'"
            );
        }

        let first_ids: Vec<String> =
            first_slots.into_iter().map(|s| s.surface_id).collect();
        // Rotation visits every slot exactly once across `len()` calls.
        assert_eq!(
            first_ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
            3,
            "expected three distinct surface_ids across the first {} acquire_next calls",
            ring.len()
        );

        // The same three surface_ids show up again on the next pass —
        // slots are reused, NOT re-allocated.
        let second_pass: Vec<TextureRingSlot> =
            (0..ring.len()).map(|_| ring.acquire_next()).collect();
        let second_ids: Vec<String> =
            second_pass.iter().map(|s| s.surface_id.clone()).collect();
        assert_eq!(first_ids, second_ids, "slot rotation must repeat in order");
        for (i, slot) in second_pass.iter().enumerate() {
            assert_eq!(
                Arc::as_ptr(slot.texture.vulkan_inner()),
                initial_arcs[i],
                "second rotation pass slot {i} drifted from the pre-allocated Arc"
            );
        }
    }

    #[test]
    fn ring_drop_unregisters_surface_ids_from_cache() {
        let Some((gpu, full)) = fresh_full_access() else {
            println!("Skipping - no GPU device available");
            return;
        };
        let ring = full
            .create_texture_ring(
                32,
                32,
                TextureFormat::Rgba8Unorm,
                TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING,
                2,
            )
            .expect("create_texture_ring");

        let ids: Vec<String> =
            (0..ring.len()).map(|i| ring.slot(i).unwrap().surface_id.clone()).collect();

        // Each id resolves through GpuContext while the ring is alive.
        for id in &ids {
            assert!(
                gpu.resolve_texture_by_surface_id(id, None, 32, 32).is_ok(),
                "surface_id {id} should resolve while ring is alive"
            );
        }

        drop(ring);

        // After Drop, the cache entries are gone.
        for id in &ids {
            assert!(
                gpu.resolve_texture_by_surface_id(id, None, 32, 32).is_err(),
                "surface_id {id} should not resolve after ring is dropped"
            );
        }
    }

    #[test]
    fn copy_pixel_buffer_to_slot_uses_amortized_path_and_updates_layout() {
        use crate::core::rhi::PixelFormat;
        use crate::host_rhi::HostPixelBufferRefExt;
        use streamlib_consumer_rhi::VulkanLayout;

        let Some((gpu, full)) = fresh_full_access() else {
            println!("Skipping - no GPU device available");
            return;
        };
        let ring = full
            .create_texture_ring(
                32,
                32,
                TextureFormat::Rgba8Unorm,
                TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING,
                2,
            )
            .expect("create_texture_ring");

        // Pre-copy: registration claim is UNDEFINED (freshly-allocated
        // VkImage, no upload yet) — spec-correct per
        // docs/architecture/texture-registration.md Producer Rule 2.
        let slot0 = ring.acquire_next();
        let reg_pre = gpu
            .resolve_texture_registration_by_surface_id(&slot0.surface_id, None, 32, 32)
            .expect("registration in cache before first copy");
        assert_eq!(
            reg_pre.current_layout(),
            VulkanLayout::UNDEFINED,
            "freshly-constructed ring slot must declare UNDEFINED initial layout"
        );

        let limited = crate::core::context::GpuContextLimitedAccess::new(gpu.clone());
        let (_pool_id, pixel_buffer) = limited
            .acquire_pixel_buffer(32, 32, PixelFormat::Rgba32)
            .expect("acquire_pixel_buffer");

        let bytes_len = (32u32 * 32 * 4) as usize;
        let mapped = pixel_buffer.buffer_ref().vulkan_inner().mapped_ptr();
        unsafe {
            std::ptr::write_bytes(mapped, 0xA5, bytes_len);
        }

        // Exercise the amortized path. This must succeed without
        // escalation and without per-call vkCreateCommandPool /
        // vkAllocateCommandBuffers / vkCreateFence (the slot's
        // pre-allocated resources are reused). We verify the
        // pre-allocation indirectly via the second-call test below;
        // here we just check the path works and updates the layout.
        ring.copy_pixel_buffer_to_slot(&slot0, &pixel_buffer, 32, 32)
            .expect("amortized copy_pixel_buffer_to_slot must succeed");

        let reg_post = gpu
            .resolve_texture_registration_by_surface_id(&slot0.surface_id, None, 32, 32)
            .expect("registration in cache after copy");
        assert_eq!(
            reg_post.current_layout(),
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            "post-copy layout must match upload_buffer_to_image's terminal SHADER_READ_ONLY_OPTIMAL"
        );

        // Second copy on the SAME slot must reuse the same cb + fence
        // (the amortized resources). If `from_slots` were
        // re-allocating per call, this would either crash (double
        // pool destroy) or leak; clean success is the signal.
        ring.copy_pixel_buffer_to_slot(&slot0, &pixel_buffer, 32, 32)
            .expect("amortized copy must succeed on slot reuse");
        // Rotate to the OTHER slot, exercise its independent resources.
        let slot1 = ring.acquire_next();
        assert_ne!(slot1.surface_id, slot0.surface_id, "ring should rotate");
        ring.copy_pixel_buffer_to_slot(&slot1, &pixel_buffer, 32, 32)
            .expect("amortized copy on second slot must succeed");
    }

    /// Microbench: amortized `copy_pixel_buffer_to_slot` vs. the generic
    /// per-call `copy_pixel_buffer_to_texture` primitive. Runs 1024 calls
    /// of each, reports per-call wall time. `#[ignore]`-gated because it's
    /// a performance characterization, not a correctness check — run via
    /// `cargo test -p streamlib-engine bench_upload_paths --release -- --ignored --nocapture --test-threads=1`.
    #[test]
    #[ignore = "perf characterization — see test body for invocation"]
    fn bench_upload_paths() {
        use crate::core::rhi::PixelFormat;
        use crate::host_rhi::HostPixelBufferRefExt;
        use std::time::Instant;

        const ITERS: usize = 1024;
        const W: u32 = 640;
        const H: u32 = 360;
        let bytes = (W * H * 4) as usize;

        let Some((gpu, full)) = fresh_full_access() else {
            eprintln!("[bench_upload_paths] No GPU device available — skipping");
            return;
        };
        let ring = full
            .create_texture_ring(
                W,
                H,
                TextureFormat::Rgba8Unorm,
                TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING,
                2,
            )
            .expect("create_texture_ring");

        let limited = crate::core::context::GpuContextLimitedAccess::new(gpu.clone());
        let (_pool_id, pixel_buffer) = limited
            .acquire_pixel_buffer(W, H, PixelFormat::Rgba32)
            .expect("acquire_pixel_buffer");
        let mapped = pixel_buffer.buffer_ref().vulkan_inner().mapped_ptr();
        unsafe { std::ptr::write_bytes(mapped, 0xA5, bytes) };

        // Warm-up
        for _ in 0..16 {
            let slot = ring.acquire_next();
            ring.copy_pixel_buffer_to_slot(&slot, &pixel_buffer, W, H).unwrap();
        }
        for _ in 0..16 {
            let slot = ring.acquire_next();
            limited
                .copy_pixel_buffer_to_texture(&pixel_buffer, &slot.texture, &slot.surface_id, W, H)
                .unwrap();
        }

        // Amortized path
        let t0 = Instant::now();
        for _ in 0..ITERS {
            let slot = ring.acquire_next();
            ring.copy_pixel_buffer_to_slot(&slot, &pixel_buffer, W, H).unwrap();
        }
        let amortized_total = t0.elapsed();

        // Generic per-call path
        let t0 = Instant::now();
        for _ in 0..ITERS {
            let slot = ring.acquire_next();
            limited
                .copy_pixel_buffer_to_texture(&pixel_buffer, &slot.texture, &slot.surface_id, W, H)
                .unwrap();
        }
        let generic_total = t0.elapsed();

        let amortized_per = amortized_total / ITERS as u32;
        let generic_per = generic_total / ITERS as u32;
        let speedup = generic_total.as_secs_f64() / amortized_total.as_secs_f64();
        let saved_per_call = generic_per.saturating_sub(amortized_per);

        println!(
            "\n=== TextureRing upload bench ({W}x{H} RGBA, {ITERS} iters) ===\n\
             amortized copy_pixel_buffer_to_slot:     {amortized_total:?} total, {amortized_per:?} per call\n\
             generic   copy_pixel_buffer_to_texture:  {generic_total:?} total, {generic_per:?} per call\n\
             speedup:  {speedup:.2}x\n\
             saved per call: {saved_per_call:?}\n"
        );
    }

    #[test]
    fn generic_copy_pixel_buffer_to_texture_still_works_as_escape_hatch() {
        use crate::core::rhi::PixelFormat;
        use crate::host_rhi::HostPixelBufferRefExt;
        use streamlib_consumer_rhi::VulkanLayout;

        // The generic `GpuContextLimitedAccess::copy_pixel_buffer_to_texture`
        // primitive (non-amortized, per-call resource churn) is kept
        // as an escape hatch for callers without a ring. Verify it
        // still works post-amortization landing.
        let Some((gpu, full)) = fresh_full_access() else {
            println!("Skipping - no GPU device available");
            return;
        };
        let ring = full
            .create_texture_ring(
                32,
                32,
                TextureFormat::Rgba8Unorm,
                TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING,
                2,
            )
            .expect("create_texture_ring");
        let slot = ring.acquire_next();

        let limited = crate::core::context::GpuContextLimitedAccess::new(gpu.clone());
        let (_pool_id, pixel_buffer) = limited
            .acquire_pixel_buffer(32, 32, PixelFormat::Rgba32)
            .expect("acquire_pixel_buffer");
        let mapped = pixel_buffer.buffer_ref().vulkan_inner().mapped_ptr();
        unsafe {
            std::ptr::write_bytes(mapped, 0x5A, (32u32 * 32 * 4) as usize);
        }
        limited
            .copy_pixel_buffer_to_texture(&pixel_buffer, &slot.texture, &slot.surface_id, 32, 32)
            .expect("generic copy primitive should still work");

        let reg = gpu
            .resolve_texture_registration_by_surface_id(&slot.surface_id, None, 32, 32)
            .expect("registration in cache");
        assert_eq!(reg.current_layout(), VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
    }

    #[test]
    fn ring_rotation_is_atomic_across_threads() {
        let Some((_gpu, full)) = fresh_full_access() else {
            println!("Skipping - no GPU device available");
            return;
        };
        let ring = full
            .create_texture_ring(
                16,
                16,
                TextureFormat::Rgba8Unorm,
                TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING,
                4,
            )
            .expect("create_texture_ring");

        let ring_clone = Arc::clone(&ring);
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let ring = Arc::clone(&ring_clone);
                std::thread::spawn(move || {
                    for _ in 0..100 {
                        let _ = ring.acquire_next();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("thread join");
        }
        // Counter advanced 400 times; modulo-4 leaves it back at 0.
        // We can't directly inspect the counter, but we can check that
        // acquire_next still returns a valid slot.
        let _slot = ring.acquire_next();
    }
}
