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

use std::ffi::c_void;
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

/// Maximum inline `surface_id` length in bytes — fits any UUID
/// representation (canonical 36-byte form plus generous headroom for
/// future identifier shapes) without a heap `String`.
pub const TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES: usize = 64;

/// A single slot in a [`TextureRing`].
///
/// Layout-stable `#[repr(C)]` handle. The `Texture` is itself a
/// `#[repr(C)]` handle over an opaque pointer plus cached PODs
/// (Clone bumps its Arc; Drop balances). The `surface_id` is stored
/// inline as a fixed 64-byte buffer plus a length — UUIDs are pure
/// ASCII so a single UTF-8 validation at construction time lets
/// [`Self::surface_id`] use `from_utf8_unchecked` on every read.
///
/// `Clone` here is structural — `Texture`'s `Clone` impl runs (Arc-
/// bumped) and the POD bytes copy. `Drop` is also structural —
/// `Texture::Drop` decrements the texture's Arc; the inline bytes
/// have no destructor.
#[repr(C)]
pub struct TextureRingSlot {
    /// Pre-allocated texture handle for this slot; Clone/Drop manage
    /// the underlying Arc's strong count.
    pub texture: Texture,
    /// Stable per-slot `surface_id` registered in
    /// [`GpuContext::resolve_texture_registration_by_surface_id`]'s
    /// same-process texture cache at ring construction. Stored as
    /// the first [`surface_id_len`](Self::surface_id_len) bytes of
    /// this fixed buffer (UTF-8, validated once at construction).
    /// Downstream consumers reach the str view via
    /// [`Self::surface_id`].
    pub(crate) surface_id_bytes: [u8; TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES],
    /// Valid UTF-8 byte length in
    /// [`surface_id_bytes`](Self::surface_id_bytes).
    pub(crate) surface_id_len: u32,
    /// Index of this slot within the ring's slot vector. Used to
    /// look up the slot's pre-allocated upload resources for
    /// [`TextureRing::copy_pixel_buffer_to_slot`].
    pub(crate) slot_index: u32,
}

// SAFETY: `Texture` is `Send + Sync` (a handle over an Arc); the
// inline POD bytes carry no thread-state. `TextureRingSlot` is Send
// + Sync because every field is.
unsafe impl Send for TextureRingSlot {}
unsafe impl Sync for TextureRingSlot {}

impl TextureRingSlot {
    /// Construct a slot from owned fields. Engine-internal — public
    /// construction is through
    /// [`crate::core::context::GpuContextFullAccess::create_texture_ring`].
    ///
    /// Panics if `surface_id.len() > TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES`
    /// — UUIDs are 36 bytes, so the 64-byte budget has comfortable
    /// headroom; tripping the panic indicates the producer started
    /// minting non-UUID identifiers and the budget needs a real
    /// re-think (not silent truncation).
    pub(crate) fn new(texture: Texture, surface_id: &str, slot_index: u32) -> Self {
        let bytes = surface_id.as_bytes();
        assert!(
            bytes.len() <= TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES,
            "TextureRingSlot::new: surface_id length {} exceeds \
             TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES = {} \
             (UUIDs are 36 bytes; a longer id needs an explicit \
             budget bump in the slot, not silent truncation)",
            bytes.len(),
            TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES,
        );
        let mut surface_id_bytes = [0u8; TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES];
        surface_id_bytes[..bytes.len()].copy_from_slice(bytes);
        Self {
            texture,
            surface_id_bytes,
            surface_id_len: bytes.len() as u32,
            slot_index,
        }
    }

    /// The slot's `surface_id` as a UTF-8 string. Reads back the
    /// bytes [`surface_id_bytes`](Self::surface_id_bytes) up to
    /// `surface_id_len`. Uses `from_utf8_unchecked` because the
    /// invariant is locked at construction: [`Self::new`] always
    /// passes the bytes through `str::as_bytes()` (which produces
    /// valid UTF-8 by construction).
    pub fn surface_id(&self) -> &str {
        // SAFETY: bytes [0, surface_id_len) are valid UTF-8 by the
        // construction invariant (always sourced from `&str`-bytes
        // via `TextureRingSlot::new`). `surface_id_len` is bounded
        // by `TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES` per the
        // assertion in `new`.
        let len = (self.surface_id_len as usize).min(TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES);
        unsafe { std::str::from_utf8_unchecked(&self.surface_id_bytes[..len]) }
    }

    /// Index of this slot within the ring's slot vector. Public for
    /// callers who need the slot identity beyond the `surface_id`
    /// (e.g. third-party backends that cache per-slot CUDA imports —
    /// see the parked nvJPEG backend at
    /// `vulkan/_nvjpeg_impl_pending_/resources.rs`).
    pub fn slot_index(&self) -> u32 {
        self.slot_index
    }
}

impl Clone for TextureRingSlot {
    fn clone(&self) -> Self {
        // `Texture::clone` runs the texture's Clone (Arc-bumped);
        // remaining fields are POD bytes.
        Self {
            texture: self.texture.clone(),
            surface_id_bytes: self.surface_id_bytes,
            surface_id_len: self.surface_id_len,
            slot_index: self.slot_index,
        }
    }
}

impl std::fmt::Debug for TextureRingSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextureRingSlot")
            .field("surface_id", &self.surface_id())
            .field("slot_index", &self.slot_index)
            .field("texture", &self.texture)
            .finish()
    }
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
/// Rich data backing a [`TextureRing`], reached through the ring's
/// opaque handle.
pub struct TextureRingInner {
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

impl TextureRingInner {
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
        let resources = self.upload_resources.get(slot.slot_index as usize).ok_or_else(|| {
            Error::GpuError(format!(
                "TextureRing::copy_pixel_buffer_to_slot: slot_index {} out of range (ring has {} slots)",
                slot.slot_index,
                self.upload_resources.len()
            ))
        })?;
        use crate::host_rhi::{HostPixelBufferRefExt, HostTextureExt};
        let image = slot
            .texture
            .vulkan_inner()
            .image()
            .ok_or_else(|| Error::GpuError("TextureRing slot texture has no VkImage".into()))?;
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
        self.gpu.update_texture_registration_layout(
            slot.surface_id(),
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        );
        Ok(())
    }

    /// Copy a host-visible pixel buffer's contents into a ring slot's
    /// pre-allocated texture, identified by `(slot_index, surface_id)`
    /// rather than a [`TextureRingSlot`] reference.
    #[cfg(target_os = "linux")]
    pub(crate) fn copy_pixel_buffer_to_slot_by_index(
        &self,
        slot_index: u32,
        surface_id: &str,
        pixel_buffer: &crate::core::rhi::PixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<()> {
        let idx = slot_index as usize;
        let resources = self.upload_resources.get(idx).ok_or_else(|| {
            Error::GpuError(format!(
                "TextureRing::copy_pixel_buffer_to_slot: slot_index {} out of range (ring has {} slots)",
                slot_index,
                self.upload_resources.len()
            ))
        })?;
        let slot = self.slots.get(idx).ok_or_else(|| {
            Error::GpuError(format!(
                "TextureRing::copy_pixel_buffer_to_slot: slot_index {} out of range (ring has {} slots)",
                slot_index,
                self.slots.len()
            ))
        })?;
        use crate::host_rhi::{HostPixelBufferRefExt, HostTextureExt};
        let image = slot
            .texture
            .vulkan_inner()
            .image()
            .ok_or_else(|| Error::GpuError("TextureRing slot texture has no VkImage".into()))?;
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
            .update_texture_registration_layout(surface_id, VulkanLayout::SHADER_READ_ONLY_OPTIMAL);
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

impl Drop for TextureRingInner {
    fn drop(&mut self) {
        for slot in &self.slots {
            self.gpu.unregister_texture(slot.surface_id());
        }
        // upload_resources drop themselves (each Drop waits on its fence
        // first, then destroys cb + pool + fence).
    }
}

// =============================================================================
// Public handle
// =============================================================================

/// Pre-allocated ring of textures rotated per-frame on the decode hot
/// path. A layout-stable `#[repr(C)]` handle over an opaque pointer to
/// an `Arc<TextureRingInner>`, plus cached POD descriptors. `Clone` /
/// `Drop` manage the inner Arc's strong count.
///
/// The four POD getters (`len`, `width`, `height`, `format`) read
/// directly from cached fields on this struct. The values are captured
/// by [`Self::from_arc_into_raw`] at construction and never mutate over
/// the ring's lifetime.
#[repr(C)]
pub struct TextureRing {
    /// Opaque handle to the host's `Arc<TextureRingInner>`.
    pub(crate) handle: *const c_void,
    /// Cached slot count. Set at construction; ring depth is fixed.
    pub(crate) cached_len: u32,
    /// Cached pixel width every slot's texture was allocated with.
    pub(crate) cached_width: u32,
    /// Cached pixel height every slot's texture was allocated with.
    pub(crate) cached_height: u32,
    /// Cached pixel format every slot's texture was allocated with.
    /// Stored as the `u32` discriminant (matches `TextureFormat`'s
    /// `#[repr(u32)]`).
    pub(crate) cached_format: u32,
}

// SAFETY: handle points at an `Arc<TextureRingInner>`; inner state is
// Send+Sync (atomic counter + GPU resources guarded by host queue
// mutex).
unsafe impl Send for TextureRing {}
unsafe impl Sync for TextureRing {}

impl TextureRing {
    /// Internal helper: leak an initial Arc strong count via
    /// `Arc::into_raw` and snapshot the ring's POD descriptors
    /// (len / width / height / format — fixed across the ring's
    /// lifetime).
    pub(crate) fn from_arc_into_raw(arc: Arc<TextureRingInner>) -> Self {
        let cached_len = arc.len() as u32;
        let cached_width = arc.width();
        let cached_height = arc.height();
        let cached_format = arc.format() as u32;
        let handle = Arc::into_raw(arc) as *const c_void;
        Self {
            handle,
            cached_len,
            cached_width,
            cached_height,
            cached_format,
        }
    }

    /// Engine-internal borrow of the host-owned `TextureRingInner`.
    pub(crate) fn host_inner(&self) -> &TextureRingInner {
        // SAFETY: `self.handle` is `Arc::into_raw(Arc<TextureRingInner>)`.
        unsafe { &*(self.handle as *const TextureRingInner) }
    }

    /// Rotate to the next slot. Thread-safe (atomic counter).
    pub fn acquire_next(&self) -> TextureRingSlot {
        self.host_inner().acquire_next()
    }

    /// Copy a host-visible pixel buffer's contents into a ring slot.
    /// See [`TextureRingInner::copy_pixel_buffer_to_slot`] for details.
    #[cfg(target_os = "linux")]
    pub fn copy_pixel_buffer_to_slot(
        &self,
        slot: &TextureRingSlot,
        pixel_buffer: &crate::core::rhi::PixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<()> {
        self.host_inner()
            .copy_pixel_buffer_to_slot(slot, pixel_buffer, width, height)
    }

    /// Number of slots in the ring. Cached POD.
    pub fn len(&self) -> usize {
        self.cached_len as usize
    }

    /// Ring is always non-empty in practice (construction rejects 0).
    /// Cached POD.
    pub fn is_empty(&self) -> bool {
        self.cached_len == 0
    }

    /// Width of every slot's texture, in pixels. Cached POD.
    pub fn width(&self) -> u32 {
        self.cached_width
    }

    /// Height of every slot's texture, in pixels. Cached POD.
    pub fn height(&self) -> u32 {
        self.cached_height
    }

    /// Format every slot's texture was allocated with. Decoded from the
    /// cached `u32` discriminant via `match` (matches `TextureFormat`'s
    /// `#[repr(u32)]` layout).
    pub fn format(&self) -> TextureFormat {
        match self.cached_format {
            0 => TextureFormat::Rgba8Unorm,
            1 => TextureFormat::Rgba8UnormSrgb,
            2 => TextureFormat::Bgra8Unorm,
            3 => TextureFormat::Bgra8UnormSrgb,
            4 => TextureFormat::Rgba16Float,
            5 => TextureFormat::Rgba32Float,
            6 => TextureFormat::Nv12,
            // Unknown discriminant — should be unreachable because
            // `from_arc_into_raw` captures from a typed
            // `TextureFormat`. Default to `Rgba8Unorm` defensively.
            _ => TextureFormat::Rgba8Unorm,
        }
    }

    /// Borrow a slot by index — engine-internal / debug only.
    #[doc(hidden)]
    pub fn slot(&self, index: usize) -> Option<TextureRingSlot> {
        self.host_inner().slot(index).cloned()
    }
}

impl Clone for TextureRing {
    fn clone(&self) -> Self {
        if !self.handle.is_null() {
            // SAFETY: `handle` is `Arc::into_raw(Arc<TextureRingInner>)`;
            // the increment in `Clone` and this decrement are balanced.
            unsafe {
                Arc::increment_strong_count(self.handle as *const TextureRingInner);
            }
        }
        Self {
            handle: self.handle,
            cached_len: self.cached_len,
            cached_width: self.cached_width,
            cached_height: self.cached_height,
            cached_format: self.cached_format,
        }
    }
}

impl Drop for TextureRing {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: `handle` is `Arc::into_raw(Arc<TextureRingInner>)`;
            // the increment in `Clone` and this decrement are balanced.
            unsafe {
                Arc::decrement_strong_count(self.handle as *const TextureRingInner);
            }
        }
    }
}

impl std::fmt::Debug for TextureRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextureRing").finish()
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn texture_ring_slot_surface_id_max_bytes_constant() {
        // The constant must match the in-line buffer length on the
        // TextureRingSlot handle.
        assert_eq!(TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES, 64);
    }

    #[test]
    fn texture_ring_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TextureRing>();
        assert_send_sync::<TextureRingSlot>();
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

        let first_ids: Vec<String> = first_slots
            .iter()
            .map(|s| s.surface_id().to_string())
            .collect();
        // Rotation visits every slot exactly once across `len()` calls.
        assert_eq!(
            first_ids
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            3,
            "expected three distinct surface_ids across the first {} acquire_next calls",
            ring.len()
        );

        // The same three surface_ids show up again on the next pass —
        // slots are reused, NOT re-allocated.
        let second_pass: Vec<TextureRingSlot> =
            (0..ring.len()).map(|_| ring.acquire_next()).collect();
        let second_ids: Vec<String> = second_pass
            .iter()
            .map(|s| s.surface_id().to_string())
            .collect();
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

        let ids: Vec<String> = (0..ring.len())
            .map(|i| ring.slot(i).unwrap().surface_id().to_string())
            .collect();

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
            .resolve_texture_registration_by_surface_id(slot0.surface_id(), None, 32, 32)
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
            .resolve_texture_registration_by_surface_id(slot0.surface_id(), None, 32, 32)
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
        assert_ne!(slot1.surface_id(), slot0.surface_id(), "ring should rotate");
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
            ring.copy_pixel_buffer_to_slot(&slot, &pixel_buffer, W, H)
                .unwrap();
        }
        for _ in 0..16 {
            let slot = ring.acquire_next();
            limited
                .copy_pixel_buffer_to_texture(&pixel_buffer, &slot.texture, slot.surface_id(), W, H)
                .unwrap();
        }

        // Amortized path
        let t0 = Instant::now();
        for _ in 0..ITERS {
            let slot = ring.acquire_next();
            ring.copy_pixel_buffer_to_slot(&slot, &pixel_buffer, W, H)
                .unwrap();
        }
        let amortized_total = t0.elapsed();

        // Generic per-call path
        let t0 = Instant::now();
        for _ in 0..ITERS {
            let slot = ring.acquire_next();
            limited
                .copy_pixel_buffer_to_texture(&pixel_buffer, &slot.texture, slot.surface_id(), W, H)
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
            .copy_pixel_buffer_to_texture(&pixel_buffer, &slot.texture, slot.surface_id(), 32, 32)
            .expect("generic copy primitive should still work");

        let reg = gpu
            .resolve_texture_registration_by_surface_id(slot.surface_id(), None, 32, 32)
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

        let handles: Vec<_> = (0..4)
            .map(|_| {
                let ring = ring.clone();
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

    #[test]
    fn slot_surface_id_round_trips_through_inline_buffer() {
        // Lock the construction-time invariant the `surface_id`
        // accessor's `from_utf8_unchecked` depends on. Mentally revert
        // `TextureRingSlot::new` to skip the bytes-copy step (i.e.
        // leave `surface_id_bytes` zeroed) — this assertion fires.
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
                1,
            )
            .expect("create_texture_ring");
        let slot = ring.acquire_next();
        // UUID v4 canonical form is 36 ASCII bytes.
        assert_eq!(
            slot.surface_id().len(),
            36,
            "expected UUID v4 canonical 36-byte form"
        );
        assert_eq!(
            slot.surface_id().as_bytes().len(),
            slot.surface_id_len as usize,
            "accessor length must match the cached length field"
        );
        // Assert actual UUID content shape: hex chars + dashes, no
        // NUL bytes. If `TextureRingSlot::new` is mentally reverted
        // to skip the bytes-copy step, `surface_id_bytes` stays
        // zero-initialized and `surface_id()` returns 36 NUL bytes —
        // these assertions fire. (The bare length check above passes
        // either way.)
        let sid = slot.surface_id();
        assert!(
            !sid.contains('\0'),
            "surface_id must not contain NUL bytes; got {sid:?}"
        );
        assert!(
            sid.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
            "surface_id must be UUID-shaped (hex + dashes); got {sid:?}"
        );
        assert_eq!(
            sid.matches('-').count(),
            4,
            "UUID v4 canonical form has exactly 4 dashes; got {sid:?}"
        );
        // Round-trip cloning the slot must preserve the surface_id
        // identity (POD bytes copy + cached length).
        let clone = slot.clone();
        assert_eq!(slot.surface_id(), clone.surface_id());
        assert_eq!(slot.slot_index(), clone.slot_index());
    }
}
