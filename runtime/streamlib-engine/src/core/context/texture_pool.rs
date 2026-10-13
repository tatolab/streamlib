// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! TexturePool - Runtime-owned GPU texture management.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use parking_lot::Mutex;

use super::lease_aware_pool_slot_ring::PoolSlotReuse;
use super::shared_pool_entry_freeing::{
    LONGEST_A_STOPPING_STREAM_WAITS_FOR_ITS_POOL_ENTRIES_GPU_WORK, SharedPoolEntryFreeingRecord,
    SharedPoolEntryIdleWindow,
};
use super::{SurfaceCheckOutLeaseRegistry, SurfaceStore};
use crate::core::rhi::{
    GpuDevice, GpuQueueSubmissionsSnapshot, NativeTextureHandle, Texture, TextureDescriptor,
    TextureFormat, TextureUsages,
};
use crate::core::runtime::LoadedStreamTag;
use crate::core::{Error, Result};

/// How a pooled texture's allocation crosses a process boundary, decided at
/// allocation because the memory flavor cannot change afterwards.
///
/// The default flavor exports a DMA-BUF fd whose image layout is
/// driver-opaque (`VK_IMAGE_TILING_OPTIMAL`, no modifier), so no other
/// process can reconstruct the image from it — importable flavors must be
/// asked for. Part of the pool bucket key: one slot serves one flavor.
#[derive(Clone, Copy, Debug, Default, Hash, Eq, PartialEq)]
pub enum TextureCrossProcessImportability {
    /// The exported fd carries memory only; the image layout is
    /// driver-opaque and a foreign process cannot rebuild the image.
    #[default]
    NotImportable,
    /// OPAQUE_FD allocation — a foreign Vulkan device or CUDA imports the
    /// whole memory block and rebuilds the image from the registered
    /// `VkImageCreateInfo` shape.
    OpaqueFd,
    /// Explicit-DRM-modifier DMA-BUF allocation — a foreign process
    /// reproduces the tiled image via
    /// `VkImageDrmFormatModifierExplicitCreateInfoEXT` or EGL.
    RenderTargetDmaBuf,
    /// A private IOSurface the image is created over — a foreign process
    /// rebuilds the image from the surface's Mach port. macOS.
    IOSurface,
}

/// Request descriptor for acquiring a pooled texture. Constructed through
/// [`Self::new`] and the `with_*` builders — non-exhaustive so the next
/// axis is a non-breaking addition rather than shotgun surgery.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TexturePoolDescriptor {
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub usage: TextureUsages,
    pub label: Option<&'static str>,
    pub cross_process_importability: TextureCrossProcessImportability,
}

impl TexturePoolDescriptor {
    /// Create a new pool descriptor.
    #[must_use]
    pub fn new(width: u32, height: u32, format: TextureFormat) -> Self {
        Self {
            width,
            height,
            format,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC,
            label: None,
            cross_process_importability: TextureCrossProcessImportability::default(),
        }
    }

    /// Set usage flags.
    #[must_use]
    pub fn with_usage(mut self, usage: TextureUsages) -> Self {
        self.usage = usage;
        self
    }

    /// Set label.
    #[must_use]
    pub fn with_label(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }

    /// Set how the allocation crosses a process boundary.
    #[must_use]
    pub fn with_cross_process_importability(
        mut self,
        cross_process_importability: TextureCrossProcessImportability,
    ) -> Self {
        self.cross_process_importability = cross_process_importability;
        self
    }
}

/// Unique identifier for a pool slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PoolSlotId(u64);

/// Key for texture bucket lookup (dimension + format + usage + importability).
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct TexturePoolKey {
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub usage: TextureUsages,
    pub cross_process_importability: TextureCrossProcessImportability,
}

impl TexturePoolKey {
    pub fn from_descriptor(desc: &TexturePoolDescriptor) -> Self {
        Self {
            width: desc.width,
            height: desc.height,
            format: desc.format,
            usage: desc.usage,
            cross_process_importability: desc.cross_process_importability,
        }
    }
}

/// Statistics about texture pool usage.
#[derive(Clone, Debug, Default)]
pub struct TexturePoolStats {
    pub total_textures: usize,
    pub textures_in_use: usize,
    pub textures_available: usize,
    pub bucket_count: usize,
}

/// What decides when a texture pool slot is freed, beyond its in-use flag.
#[derive(Default)]
pub(crate) struct TexturePoolSlotFreeingBookkeeping {
    freeing_record: SharedPoolEntryFreeingRecord,
    /// The surface-share ids the slot's texture was registered under that a
    /// helper may still hold checked out; dropped once no lease names them.
    surface_share_ids_naming_the_slot: Vec<String>,
}

/// A slot in the texture pool.
pub(crate) struct PoolSlot {
    pub(crate) id: PoolSlotId,
    pub(crate) texture: Texture,
    pub(crate) key: TexturePoolKey,
    pub(crate) in_use: AtomicBool,
    /// Taken under the pool's bucket lock, never the other way round.
    pub(crate) freeing_bookkeeping: Mutex<TexturePoolSlotFreeingBookkeeping>,
}

impl PoolSlot {
    pub(crate) fn is_available(&self) -> bool {
        !self.in_use.load(Ordering::Acquire)
    }

    /// Whether nothing holds the slot: no handle, no other reference to its
    /// texture, no checkout lease on any id it was registered under, and on
    /// macOS no process using its IOSurface. Forgets the ids no lease names.
    fn is_unheld(
        &self,
        freeing_bookkeeping: &mut TexturePoolSlotFreeingBookkeeping,
        cross_process_holders: &PoolSlotReuse<'_>,
    ) -> bool {
        if !self.is_available() || self.texture.strong_count() > 1 {
            return false;
        }
        if self.backing_iosurface_is_in_use() {
            return false;
        }
        freeing_bookkeeping
            .surface_share_ids_naming_the_slot
            .retain(|surface_share_id| !cross_process_holders.permits(surface_share_id));
        freeing_bookkeeping
            .surface_share_ids_naming_the_slot
            .is_empty()
    }

    /// Whether the kernel reports the slot's IOSurface in use by any process.
    #[cfg(target_os = "macos")]
    fn backing_iosurface_is_in_use(&self) -> bool {
        use crate::host_rhi::HostTextureExt as _;

        self.texture
            .vulkan_inner()
            .backing_iosurface()
            .is_some_and(|iosurface| objc2_io_surface::IOSurfaceRef::is_in_use(iosurface))
    }

    #[cfg(not(target_os = "macos"))]
    fn backing_iosurface_is_in_use(&self) -> bool {
        false
    }

    pub(crate) fn try_acquire(&self) -> bool {
        self.in_use
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn release(&self) {
        self.in_use.store(false, Ordering::Release);
    }
}

/// Inner pool state (behind Arc for sharing).
pub(crate) struct TexturePoolInner {
    pub(crate) buckets: Mutex<HashMap<TexturePoolKey, Vec<Arc<PoolSlot>>>>,
    pub(crate) device: Arc<GpuDevice>,
    pub(crate) next_slot_id: AtomicU64,
    /// The engine's own surface store, once installed: its lease table is
    /// where a helper's checkout of a slot's texture shows.
    engines_own_surface_store: Arc<OnceLock<SurfaceStore>>,
    idle_window: SharedPoolEntryIdleWindow,
}

impl TexturePoolInner {
    pub(crate) fn next_slot_id(&self) -> PoolSlotId {
        PoolSlotId(self.next_slot_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Mark the slot available, then free at most one slot due to be freed.
    pub(crate) fn release(&self, slot_id: PoolSlotId) {
        self.with_slot(slot_id, PoolSlot::release);
        self.free_at_most_one_slot_due_to_be_freed();
    }

    fn with_slot(&self, slot_id: PoolSlotId, act_on_the_slot: impl FnOnce(&PoolSlot)) {
        let buckets = self.buckets.lock();
        if let Some(slot) = buckets.values().flatten().find(|slot| slot.id == slot_id) {
            act_on_the_slot(slot);
        }
    }

    /// Free the slot without waiting out the idle window once it is unheld.
    pub(crate) fn flag_slot_freed_as_soon_as_unheld(&self, slot_id: PoolSlotId) {
        self.with_slot(slot_id, |slot| {
            slot.freeing_bookkeeping
                .lock()
                .freeing_record
                .flag_freed_as_soon_as_unheld();
        });
    }

    /// Record that the slot's texture is registered with the surface-share
    /// service under `surface_share_id`, where a helper can check it out.
    pub(crate) fn note_surface_share_id_naming_slot(
        &self,
        slot_id: PoolSlotId,
        surface_share_id: &str,
    ) {
        self.with_slot(slot_id, |slot| {
            slot.freeing_bookkeeping
                .lock()
                .surface_share_ids_naming_the_slot
                .push(surface_share_id.to_string());
        });
    }

    pub(crate) fn find_available_slot(
        &self,
        key: &TexturePoolKey,
        acquiring_stream: Option<LoadedStreamTag>,
    ) -> Option<Arc<PoolSlot>> {
        let buckets = self.buckets.lock();
        let slot = buckets.get(key)?.iter().find(|slot| slot.try_acquire())?;
        slot.freeing_bookkeeping
            .lock()
            .freeing_record
            .note_handed_out_again_to(acquiring_stream);
        Some(Arc::clone(slot))
    }

    fn check_out_leases(&self) -> Option<&SurfaceCheckOutLeaseRegistry> {
        self.engines_own_surface_store
            .get()
            .and_then(SurfaceStore::check_out_leases)
            .map(Arc::as_ref)
    }

    fn every_queue_submission_in_snapshot_has_completed(
        &self,
        snapshot: &GpuQueueSubmissionsSnapshot,
    ) -> bool {
        self.device
            .every_queue_submission_in_snapshot_has_completed(snapshot)
            .unwrap_or_else(|unreadable| {
                tracing::warn!(
                    "the GPU's queue completion could not be read, so no texture pool slot is \
                     freed: {unreadable}"
                );
                false
            })
    }

    /// Observe every slot held or unheld, and free at most one that may be
    /// freed: freeing can take milliseconds, and this runs on the producer's
    /// acquire and release.
    pub(crate) fn free_at_most_one_slot_due_to_be_freed(&self) {
        if let Some(slot_taken_out) = self.take_out_at_most_one_slot_due_to_be_freed() {
            log_the_texture_pool_slot_freed(&slot_taken_out);
        }
    }

    fn take_out_at_most_one_slot_due_to_be_freed(&self) -> Option<Arc<PoolSlot>> {
        let now = Instant::now();
        let idle_window = self.idle_window.duration();
        let cross_process_holders = PoolSlotReuse::over(self.check_out_leases());
        let mut buckets = self.buckets.lock();
        let mut queue_submissions_made_so_far = None;
        let mut slot_due_to_be_freed = None;
        for (key, slots) in buckets.iter() {
            for (slot_index, slot) in slots.iter().enumerate() {
                let mut freeing_bookkeeping = slot.freeing_bookkeeping.lock();
                if !slot.is_unheld(&mut freeing_bookkeeping, &cross_process_holders) {
                    freeing_bookkeeping.freeing_record.note_observed_held();
                    continue;
                }
                freeing_bookkeeping
                    .freeing_record
                    .note_observed_unheld(now, || {
                        *queue_submissions_made_so_far.get_or_insert_with(|| {
                            self.device.snapshot_of_queue_submissions_made_so_far()
                        })
                    });
                if slot_due_to_be_freed.is_none()
                    && freeing_bookkeeping.freeing_record.may_be_freed_now(
                        now,
                        idle_window,
                        |snapshot| self.every_queue_submission_in_snapshot_has_completed(snapshot),
                    )
                {
                    slot_due_to_be_freed = Some((key.clone(), slot_index));
                }
            }
        }
        drop(cross_process_holders);
        let (key, slot_index) = slot_due_to_be_freed?;
        let slots = buckets.get_mut(&key)?;
        let slot_taken_out = slots.remove(slot_index);
        if slots.is_empty() {
            buckets.remove(&key);
        }
        Some(slot_taken_out)
    }

    /// Free every unheld slot `stream` last acquired, waiting at most
    /// [`LONGEST_A_STOPPING_STREAM_WAITS_FOR_ITS_POOL_ENTRIES_GPU_WORK`] for the
    /// GPU work naming them; a slot still held, or whose work outlasts the
    /// wait, is flagged to be freed as soon as it is unheld. Answers how many
    /// were freed.
    pub(crate) fn free_every_unheld_slot_the_stream_last_acquired(
        &self,
        stream: LoadedStreamTag,
    ) -> usize {
        let slots_taken_out = self.take_out_every_unheld_slot_the_stream_last_acquired(stream);
        if slots_taken_out.is_empty() {
            return 0;
        }
        let queue_submissions_naming_them = slots_taken_out.iter().fold(
            GpuQueueSubmissionsSnapshot::default(),
            |covering_every_slot, slot| {
                slot.freeing_bookkeeping
                    .lock()
                    .freeing_record
                    .queue_submissions_made_before_it_was_unheld()
                    .map_or(covering_every_slot, |queue_submissions| {
                        covering_every_slot.covering_both(&queue_submissions)
                    })
            },
        );
        match self.device.wait_for_every_queue_submission_in_snapshot(
            &queue_submissions_naming_them,
            LONGEST_A_STOPPING_STREAM_WAITS_FOR_ITS_POOL_ENTRIES_GPU_WORK,
        ) {
            Ok(true) => {
                let freed_slot_count = slots_taken_out.len();
                for slot_taken_out in slots_taken_out {
                    log_the_texture_pool_slot_freed(&slot_taken_out);
                }
                freed_slot_count
            }
            outlasted_or_unreadable => {
                tracing::warn!(
                    stream = %stream,
                    slot_count = slots_taken_out.len(),
                    "the GPU work naming a stopped stream's texture pool slots did not complete \
                     within {:?} ({outlasted_or_unreadable:?}); they are freed once it has",
                    LONGEST_A_STOPPING_STREAM_WAITS_FOR_ITS_POOL_ENTRIES_GPU_WORK
                );
                for slot_taken_out in &slots_taken_out {
                    slot_taken_out
                        .freeing_bookkeeping
                        .lock()
                        .freeing_record
                        .flag_freed_as_soon_as_unheld();
                }
                for slot_taken_out in slots_taken_out {
                    self.add_slot(slot_taken_out);
                }
                0
            }
        }
    }

    fn take_out_every_unheld_slot_the_stream_last_acquired(
        &self,
        stream: LoadedStreamTag,
    ) -> Vec<Arc<PoolSlot>> {
        let now = Instant::now();
        let cross_process_holders = PoolSlotReuse::over(self.check_out_leases());
        let mut buckets = self.buckets.lock();
        let mut queue_submissions_made_so_far = None;
        let mut slots_taken_out = Vec::new();
        for slots in buckets.values_mut() {
            let (unheld_slots_of_the_stream, slots_kept): (Vec<_>, Vec<_>) =
                std::mem::take(slots).into_iter().partition(|slot| {
                    let mut freeing_bookkeeping = slot.freeing_bookkeeping.lock();
                    if !freeing_bookkeeping
                        .freeing_record
                        .was_last_acquired_by(stream)
                    {
                        return false;
                    }
                    if !slot.is_unheld(&mut freeing_bookkeeping, &cross_process_holders) {
                        freeing_bookkeeping.freeing_record.note_observed_held();
                        freeing_bookkeeping
                            .freeing_record
                            .flag_freed_as_soon_as_unheld();
                        return false;
                    }
                    freeing_bookkeeping
                        .freeing_record
                        .note_observed_unheld(now, || {
                            *queue_submissions_made_so_far.get_or_insert_with(|| {
                                self.device.snapshot_of_queue_submissions_made_so_far()
                            })
                        });
                    true
                });
            *slots = slots_kept;
            slots_taken_out.extend(unheld_slots_of_the_stream);
        }
        buckets.retain(|_, slots| !slots.is_empty());
        slots_taken_out
    }

    pub(crate) fn add_slot(&self, slot: Arc<PoolSlot>) {
        let mut buckets = self.buckets.lock();
        buckets.entry(slot.key.clone()).or_default().push(slot);
    }

    pub(crate) fn stats(&self) -> TexturePoolStats {
        let buckets = self.buckets.lock();
        let mut total = 0;
        let mut in_use = 0;
        for slots in buckets.values() {
            for slot in slots {
                total += 1;
                if !slot.is_available() {
                    in_use += 1;
                }
            }
        }
        TexturePoolStats {
            total_textures: total,
            textures_in_use: in_use,
            textures_available: total - in_use,
            bucket_count: buckets.len(),
        }
    }
}

fn log_the_texture_pool_slot_freed(slot: &PoolSlot) {
    tracing::debug!(
        slot = slot.id.0,
        width = slot.key.width,
        height = slot.key.height,
        format = ?slot.key.format,
        importability = ?slot.key.cross_process_importability,
        "freed a texture pool slot"
    );
}

/// Rich data backing a [`PooledTextureHandle`]. Holds the
/// `Arc<TexturePoolInner>` reference plus the `PoolSlotId` so Drop
/// can release the slot exactly once, reached through the handle's
/// opaque pointer.
pub(crate) struct PooledTextureHandleInner {
    pub(crate) pool_inner: Arc<TexturePoolInner>,
    pub(crate) slot_id: PoolSlotId,
}

impl Drop for PooledTextureHandleInner {
    fn drop(&mut self) {
        self.pool_inner.release(self.slot_id);
    }
}

/// Handle to a pooled texture. Returns texture to pool on Drop.
///
/// Layout-stable: every field is either a primitive, an opaque
/// pointer, or the β-reshaped [`Texture`] (itself layout-stable).
/// The pool-release state lives behind the opaque `handle`; cdylib
/// code never reaches it.
///
/// Deliberately **not** `Clone`: Drop releases the underlying pool
/// slot exactly once. Cloning would duplicate the raw `handle`
/// pointer and double-release the slot. Consumers that need shared
/// access wrap the handle in `Arc<PooledTextureHandle>`.
#[repr(C)]
pub struct PooledTextureHandle {
    /// Opaque host handle (`Box::into_raw(Box<PooledTextureHandleInner>)`).
    pub(crate) handle: *const c_void,
    /// The pooled texture. Already a `#[repr(C)]` handle (32 bytes);
    /// embedding by value keeps the layout flat without an
    /// indirection through another `Arc`.
    pub(crate) texture: Texture,
    /// Cached width (mirrors `slot.key.width` at allocation time).
    pub(crate) width_cached: u32,
    /// Cached height (mirrors `slot.key.height`).
    pub(crate) height_cached: u32,
    /// Cached `#[repr(u32)]` discriminant of [`TextureFormat`].
    pub(crate) format_raw: u32,
    /// Reserved padding (keeps total size at 64 bytes; zero today,
    /// never read).
    pub(crate) _padding: u32,
}

// SAFETY: `handle` points at a host-owned `Box<PooledTextureHandleInner>`
// that is itself `Send + Sync` (Arc<TexturePoolInner> carries atomic
// refcounts, PoolSlotId is Copy). `texture` is Send+Sync per its own
// unsafe impls. Pool-slot release runs in host-compiled code via the
// vtable callback.
unsafe impl Send for PooledTextureHandle {}
unsafe impl Sync for PooledTextureHandle {}

impl PooledTextureHandle {
    /// The host's pool allocator builds a `PooledTextureHandleInner`,
    /// leaks it via `Box::into_raw`, resolves the host-mode vtable,
    /// and assembles the plugin ABI shape.
    #[allow(dead_code)]
    pub(crate) fn new(
        texture: Texture,
        pool_inner: Arc<TexturePoolInner>,
        slot_id: PoolSlotId,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Self {
        Self::from_parts(texture, pool_inner, slot_id, width, height, format)
    }

    /// Internal helper used by every platform-specific allocator
    /// path. Leaks a `Box<PooledTextureHandleInner>` as the opaque
    /// handle and captures the host-mode vtable pointer.
    pub(crate) fn from_parts(
        texture: Texture,
        pool_inner: Arc<TexturePoolInner>,
        slot_id: PoolSlotId,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Self {
        let inner = Box::new(PooledTextureHandleInner {
            pool_inner,
            slot_id,
        });
        let handle = Box::into_raw(inner) as *const c_void;
        Self {
            handle,
            texture,
            width_cached: width,
            height_cached: height,
            format_raw: format as u32,
            _padding: 0,
        }
    }

    /// Get a reference to the underlying texture.
    pub fn texture(&self) -> &Texture {
        &self.texture
    }

    /// Get a cloneable reference to the underlying texture.
    pub fn texture_clone(&self) -> Texture {
        self.texture.clone()
    }

    /// Get the texture width.
    pub fn width(&self) -> u32 {
        self.width_cached
    }

    /// Get the texture height.
    pub fn height(&self) -> u32 {
        self.height_cached
    }

    /// Get the texture format.
    pub fn format(&self) -> TextureFormat {
        match self.format_raw {
            0 => TextureFormat::Rgba8Unorm,
            1 => TextureFormat::Rgba8UnormSrgb,
            2 => TextureFormat::Bgra8Unorm,
            3 => TextureFormat::Bgra8UnormSrgb,
            4 => TextureFormat::Rgba16Float,
            5 => TextureFormat::Rgba32Float,
            6 => TextureFormat::Nv12,
            _ => TextureFormat::Rgba8Unorm,
        }
    }

    /// Get the pool slot ID. Engine-internal access; cdylib code
    /// cannot reach this without an additional vtable callback
    /// (deliberately omitted — the slot id is a host implementation
    /// detail).
    ///
    /// **Panics if called from cdylib code** for the same reason
    /// [`Texture::host_inner`] does.
    pub fn slot_id(&self) -> PoolSlotId {
        self.host_inner().slot_id
    }

    fn host_inner(&self) -> &PooledTextureHandleInner {
        // SAFETY: `self.handle` is `Box::into_raw(Box<PooledTextureHandleInner>)`
        // (see `from_parts`); the Box keeps the inner alive until Drop runs.
        unsafe { &*(self.handle as *const PooledTextureHandleInner) }
    }

    /// Free this texture's pool slot as soon as it is unheld, without waiting
    /// out the idle window — for a slot whose owner went away.
    pub(crate) fn flag_its_pool_slot_freed_as_soon_as_unheld(&self) {
        let inner = self.host_inner();
        inner
            .pool_inner
            .flag_slot_freed_as_soon_as_unheld(inner.slot_id);
    }

    /// Record that this texture is registered with the surface-share service
    /// under `surface_share_id`, so its slot is not freed while a helper holds
    /// it checked out.
    pub(crate) fn note_a_surface_share_id_naming_its_pool_slot(&self, surface_share_id: &str) {
        let inner = self.host_inner();
        inner
            .pool_inner
            .note_surface_share_id_naming_slot(inner.slot_id, surface_share_id);
    }

    /// Get the platform-native sharing handle for this texture — on Linux
    /// the fd's ownership transfers to the caller, per
    /// [`Texture::native_handle`].
    pub fn native_handle(&self) -> Option<NativeTextureHandle> {
        self.texture.native_handle()
    }
}

impl Drop for PooledTextureHandle {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: matched with the `Box::into_raw` in `from_parts`.
            // Dropping the Box fires `Drop for PooledTextureHandleInner`,
            // which releases the pool slot exactly once.
            unsafe {
                drop(Box::from_raw(self.handle as *mut PooledTextureHandleInner));
            }
        }
    }
}

/// The public texture pool API.
pub struct TexturePool {
    inner: Arc<TexturePoolInner>,
}

impl Clone for TexturePool {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl TexturePool {
    /// Create an empty texture pool on `device` that sees no surface-share
    /// service.
    pub fn new(device: Arc<GpuDevice>) -> Self {
        Self::new_seeing_the_engines_surface_store(device, Arc::default())
    }

    /// Create an empty texture pool on `device` whose slots are kept while the
    /// lease table of `engines_own_surface_store`, once installed, shows a
    /// helper holding one.
    pub(crate) fn new_seeing_the_engines_surface_store(
        device: Arc<GpuDevice>,
        engines_own_surface_store: Arc<OnceLock<SurfaceStore>>,
    ) -> Self {
        Self {
            inner: Arc::new(TexturePoolInner {
                buckets: Mutex::new(HashMap::new()),
                device,
                next_slot_id: AtomicU64::new(0),
                engines_own_surface_store,
                idle_window: SharedPoolEntryIdleWindow::default(),
            }),
        }
    }

    /// A texture of the descriptor's shape for `acquiring_stream` (`None` for
    /// the engine's own work): an unheld slot, or a fresh one; past the
    /// device's memory budget the allocation is refused by name. Frees at most
    /// one slot due to be freed.
    pub fn acquire(
        &self,
        desc: &TexturePoolDescriptor,
        acquiring_stream: Option<LoadedStreamTag>,
    ) -> Result<PooledTextureHandle> {
        let key = TexturePoolKey::from_descriptor(desc);
        if let Some(slot) = self.inner.find_available_slot(&key, acquiring_stream) {
            self.inner.free_at_most_one_slot_due_to_be_freed();
            return Ok(self.create_handle_from_slot(&slot));
        }
        self.inner.free_at_most_one_slot_due_to_be_freed();
        let slot = self.allocate_slot(desc, acquiring_stream)?;
        slot.try_acquire();
        self.inner.add_slot(Arc::clone(&slot));
        Ok(self.create_handle_from_slot(&slot))
    }

    /// Free every unheld slot `stream` last acquired, and flag the ones still
    /// held to be freed as soon as they are unheld; answers how many were
    /// freed.
    pub(crate) fn free_every_unheld_slot_the_stream_last_acquired(
        &self,
        stream: LoadedStreamTag,
    ) -> usize {
        self.inner
            .free_every_unheld_slot_the_stream_last_acquired(stream)
    }

    /// Replace this pool's idle window for one test.
    #[cfg(test)]
    pub(crate) fn set_idle_window_for_a_test(&self, idle_window: std::time::Duration) {
        self.inner.idle_window.set_for_a_test(idle_window);
    }

    /// How many slots of `desc`'s shape the pool holds, held or not.
    #[cfg(test)]
    pub(crate) fn slot_count_of_the_shape_of(&self, desc: &TexturePoolDescriptor) -> usize {
        self.inner
            .buckets
            .lock()
            .get(&TexturePoolKey::from_descriptor(desc))
            .map_or(0, Vec::len)
    }

    fn create_handle_from_slot(&self, slot: &Arc<PoolSlot>) -> PooledTextureHandle {
        PooledTextureHandle::from_parts(
            slot.texture.clone(),
            Arc::clone(&self.inner),
            slot.id,
            slot.key.width,
            slot.key.height,
            slot.key.format,
        )
    }

    /// Allocate a new texture slot, attributed to `acquiring_stream`.
    fn allocate_slot(
        &self,
        desc: &TexturePoolDescriptor,
        acquiring_stream: Option<LoadedStreamTag>,
    ) -> Result<Arc<PoolSlot>> {
        let texture_desc =
            TextureDescriptor::new(desc.width, desc.height, desc.format).with_usage(desc.usage);

        let texture = match desc.cross_process_importability {
            TextureCrossProcessImportability::NotImportable => {
                self.inner.device.create_texture(&texture_desc)?
            }
            #[cfg(target_os = "linux")]
            TextureCrossProcessImportability::OpaqueFd => self
                .inner
                .device
                .create_texture_opaque_fd_export(&texture_desc)?,
            #[cfg(target_os = "linux")]
            TextureCrossProcessImportability::RenderTargetDmaBuf => self
                .inner
                .device
                .create_texture_render_target_dma_buf(&texture_desc)?,
            #[cfg(target_os = "macos")]
            TextureCrossProcessImportability::IOSurface => self
                .inner
                .device
                .create_texture_iosurface_backed(&texture_desc)?,
            other => {
                return Err(Error::TextureError(format!(
                    "pooled texture importability {other:?} is not a flavour this platform \
                     allocates"
                )));
            }
        };

        Ok(Arc::new(PoolSlot {
            id: self.inner.next_slot_id(),
            texture,
            key: TexturePoolKey::from_descriptor(desc),
            in_use: AtomicBool::new(false),
            freeing_bookkeeping: Mutex::new(TexturePoolSlotFreeingBookkeeping {
                freeing_record: SharedPoolEntryFreeingRecord::handed_out_to(acquiring_stream),
                surface_share_ids_naming_the_slot: Vec::new(),
            }),
        }))
    }

    /// Pre-warm the pool with textures of specific dimensions.
    pub fn prewarm(&self, desc: &TexturePoolDescriptor, count: usize) -> Result<()> {
        for _ in 0..count {
            let slot = self.allocate_slot(desc, None)?;
            self.inner.add_slot(slot);
        }
        Ok(())
    }

    /// Get statistics about pool usage.
    pub fn stats(&self) -> TexturePoolStats {
        self.inner.stats()
    }

    /// Get the inner pool reference (for advanced usage).
    #[allow(dead_code)]
    pub(crate) fn inner(&self) -> &Arc<TexturePoolInner> {
        &self.inner
    }
}

impl std::fmt::Debug for TexturePool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let stats = self.stats();
        f.debug_struct("TexturePool")
            .field("total_textures", &stats.total_textures)
            .field("textures_in_use", &stats.textures_in_use)
            .field("textures_available", &stats.textures_available)
            .field("bucket_count", &stats.bucket_count)
            .finish()
    }
}

// =============================================================================
// Layout regression tests
// =============================================================================
//
// `PooledTextureHandle` crosses the plugin ABI as a
// `#[repr(C)]` struct. Drift in its byte-level shape would silently
// corrupt every `acquire_texture` return-path: the cdylib's Drop
// impl reads `vtable` and `handle` at fixed offsets to call
// `drop_pooled_texture_handle`. The vtable layout-version constant
// guards the dispatch table; this test guards the value type.

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;

    /// Compile-time witness that `PooledTextureHandle` is Send + Sync.
    #[test]
    fn pooled_texture_handle_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PooledTextureHandle>();
    }

    /// `PooledTextureHandle` is intentionally NOT `Clone`: Drop
    /// releases the underlying pool slot via the vtable. Cloning
    /// would duplicate the raw `handle` pointer and double-release
    /// the slot.
    ///
    /// This compile-fail doctest is the type-system witness — the
    /// `assert_not_clone` helper requires `T: Clone`, which
    /// `PooledTextureHandle` deliberately does not implement.
    ///
    /// ```compile_fail
    /// fn assert_not_clone<T: Clone>() {}
    /// assert_not_clone::<streamlib::sdk::context::PooledTextureHandle>();
    /// ```
    #[test]
    fn pooled_texture_handle_is_not_clone_doc_witness() {
        // No-op test body — the contract is locked by the
        // `compile_fail` doctest above. We keep this as a regular
        // `#[test]` so the test name shows up in `cargo test`
        // output as a discoverable assertion.
    }
}

#[cfg(test)]
mod texture_pool_growth_tests {
    use super::*;
    use crate::core::rhi::{TextureFormat, TextureUsages};

    /// The runtime-wide pool caps no shape by count: with every texture of a
    /// shape held, the next acquire allocates another.
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    #[allow(clippy::disallowed_macros)]
    fn the_texture_pool_holds_more_than_sixteen_textures_of_one_shape_at_once() {
        let Ok(device) = GpuDevice::new() else {
            println!("no GPU device — skipping");
            return;
        };
        let pool = TexturePool::new(Arc::new(device));
        let desc = TexturePoolDescriptor::new(16, 16, TextureFormat::Rgba8Unorm)
            .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST);
        let held: Vec<PooledTextureHandle> = (0..17)
            .map(|_| {
                pool.acquire(&desc, None)
                    .expect("an acquire with every texture held allocates")
            })
            .collect();
        let distinct_slot_ids: std::collections::HashSet<PoolSlotId> =
            held.iter().map(PooledTextureHandle::slot_id).collect();
        assert_eq!(
            distinct_slot_ids.len(),
            held.len(),
            "every held texture is its own slot"
        );
        assert_eq!(pool.stats().textures_in_use, held.len());
    }
}

#[cfg(test)]
mod texture_pool_freeing_tests {
    use std::time::Duration;

    use super::*;
    use crate::core::context::{GpuContext, SurfaceCheckOutLeaseRegistry, SurfaceStore};

    fn gpu_context_or_skip() -> Option<GpuContext> {
        match GpuContext::init_for_platform_sync() {
            Ok(gpu) => Some(gpu),
            Err(no_device) => {
                tracing::warn!(
                    "no GPU device, so the texture pool freeing test skips: {no_device}"
                );
                None
            }
        }
    }

    fn a_16_high_rgba8_texture_of_width(width: u32) -> TexturePoolDescriptor {
        TexturePoolDescriptor::new(width, 16, TextureFormat::Rgba8Unorm)
            .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST)
    }

    /// Block until every GPU submission made so far has completed, so a
    /// freeing decision in the test turns on holders and time alone.
    fn wait_for_every_gpu_submission_made_so_far(gpu: &GpuContext) {
        let device = gpu.device();
        let made_so_far = device.snapshot_of_queue_submissions_made_so_far();
        assert!(
            device
                .wait_for_every_queue_submission_in_snapshot(&made_so_far, Duration::from_secs(5))
                .expect("the GPU's queue completion reads"),
            "the GPU work made before the test did not complete"
        );
    }

    #[test]
    fn a_texture_pool_slot_unheld_past_the_idle_window_is_freed_on_a_later_acquire() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let pool = gpu.texture_pool();
        pool.set_idle_window_for_a_test(Duration::ZERO);
        let idle_shape = a_16_high_rgba8_texture_of_width(16);
        drop(pool.acquire(&idle_shape, None).expect("a texture"));
        assert_eq!(pool.slot_count_of_the_shape_of(&idle_shape), 1);
        wait_for_every_gpu_submission_made_so_far(&gpu);

        let _another_shape = pool
            .acquire(&a_16_high_rgba8_texture_of_width(32), None)
            .expect("a texture of another shape");
        assert_eq!(
            pool.slot_count_of_the_shape_of(&idle_shape),
            0,
            "an unheld slot past the idle window survived an acquire"
        );
    }

    #[test]
    fn a_texture_pool_slot_unheld_inside_the_idle_window_is_kept_and_handed_out_again() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let pool = gpu.texture_pool();
        let idle_shape = a_16_high_rgba8_texture_of_width(16);
        let first = pool.acquire(&idle_shape, None).expect("a texture");
        let first_slot_id = first.slot_id();
        drop(first);
        wait_for_every_gpu_submission_made_so_far(&gpu);

        let _another_shape = pool
            .acquire(&a_16_high_rgba8_texture_of_width(32), None)
            .expect("a texture of another shape");
        assert_eq!(pool.slot_count_of_the_shape_of(&idle_shape), 1);
        let again = pool.acquire(&idle_shape, None).expect("a texture");
        assert_eq!(again.slot_id(), first_slot_id);
    }

    #[test]
    fn at_most_one_idle_texture_pool_slot_is_freed_per_acquire() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let pool = gpu.texture_pool();
        pool.set_idle_window_for_a_test(Duration::ZERO);
        let idle_shape = a_16_high_rgba8_texture_of_width(16);
        let held: Vec<PooledTextureHandle> = (0..3)
            .map(|_| pool.acquire(&idle_shape, None).expect("a texture"))
            .collect();
        // Each handle's own release sees a reader still on its texture, so no
        // slot is freed while the three are let go.
        let readers: Vec<Texture> = held
            .iter()
            .map(PooledTextureHandle::texture_clone)
            .collect();
        drop(held);
        drop(readers);
        assert_eq!(pool.slot_count_of_the_shape_of(&idle_shape), 3);
        wait_for_every_gpu_submission_made_so_far(&gpu);

        let _first_other_shape = pool
            .acquire(&a_16_high_rgba8_texture_of_width(32), None)
            .expect("a texture");
        assert_eq!(pool.slot_count_of_the_shape_of(&idle_shape), 2);
        let _second_other_shape = pool
            .acquire(&a_16_high_rgba8_texture_of_width(48), None)
            .expect("a texture");
        assert_eq!(pool.slot_count_of_the_shape_of(&idle_shape), 1);
    }

    #[test]
    fn a_texture_pool_slot_a_reader_still_holds_is_never_freed() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let pool = gpu.texture_pool();
        pool.set_idle_window_for_a_test(Duration::ZERO);
        let idle_shape = a_16_high_rgba8_texture_of_width(16);
        let handle = pool.acquire(&idle_shape, None).expect("a texture");
        let reader = handle.texture_clone();
        drop(handle);
        wait_for_every_gpu_submission_made_so_far(&gpu);

        let _another_shape = pool
            .acquire(&a_16_high_rgba8_texture_of_width(32), None)
            .expect("a texture");
        assert_eq!(
            pool.slot_count_of_the_shape_of(&idle_shape),
            1,
            "a slot was freed under a reader of its texture"
        );
        drop(reader);
        let _a_third_shape = pool
            .acquire(&a_16_high_rgba8_texture_of_width(48), None)
            .expect("a texture");
        assert_eq!(pool.slot_count_of_the_shape_of(&idle_shape), 0);
    }

    #[test]
    fn a_texture_pool_slot_a_helper_holds_checked_out_is_never_freed() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let check_out_leases = Arc::new(SurfaceCheckOutLeaseRegistry::new());
        gpu.install_the_engines_surface_store(SurfaceStore::new_reading_check_out_leases(
            "the-texture-pool-reads-this-lease-table-in-process".to_string(),
            "texture-pool-freeing-test-runtime".to_string(),
            Arc::clone(&check_out_leases),
        ))
        .expect("the engine's surface store installs once");
        let pool = gpu.texture_pool();
        pool.set_idle_window_for_a_test(Duration::ZERO);
        let checked_out_shape = a_16_high_rgba8_texture_of_width(16);
        let handle = pool.acquire(&checked_out_shape, None).expect("a texture");
        handle.note_a_surface_share_id_naming_its_pool_slot("texture-checked-out-by-a-helper");
        let helper = check_out_leases.mint_holder_id();
        check_out_leases
            .record_check_out_lease("texture-checked-out-by-a-helper", helper)
            .expect("the lease records");
        drop(handle);
        wait_for_every_gpu_submission_made_so_far(&gpu);

        let _another_shape = pool
            .acquire(&a_16_high_rgba8_texture_of_width(32), None)
            .expect("a texture");
        assert_eq!(
            pool.slot_count_of_the_shape_of(&checked_out_shape),
            1,
            "a slot a helper holds checked out was freed"
        );
        check_out_leases
            .release_one_check_out_lease("texture-checked-out-by-a-helper", helper)
            .expect("the lease releases");
        let _a_third_shape = pool
            .acquire(&a_16_high_rgba8_texture_of_width(48), None)
            .expect("a texture");
        assert_eq!(pool.slot_count_of_the_shape_of(&checked_out_shape), 0);
    }

    #[test]
    fn a_texture_pool_slot_flagged_freed_as_soon_as_unheld_skips_the_idle_window() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let pool = gpu.texture_pool();
        let owner_gone_shape = a_16_high_rgba8_texture_of_width(16);
        let handle = pool.acquire(&owner_gone_shape, None).expect("a texture");
        handle.flag_its_pool_slot_freed_as_soon_as_unheld();
        drop(handle);
        wait_for_every_gpu_submission_made_so_far(&gpu);

        let _another_shape = pool
            .acquire(&a_16_high_rgba8_texture_of_width(32), None)
            .expect("a texture");
        assert_eq!(
            pool.slot_count_of_the_shape_of(&owner_gone_shape),
            0,
            "a flagged slot waited out the idle window"
        );
    }

    #[test]
    fn a_stopped_streams_unheld_texture_pool_slots_are_freed_and_its_held_ones_once_let_go() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let stopping_stream = LoadedStreamTag::numbered_for_a_test(1);
        let running_stream = LoadedStreamTag::numbered_for_a_test(2);
        let pool = gpu.texture_pool();
        let shape = a_16_high_rgba8_texture_of_width(16);
        let unheld_of_the_stopping_stream = pool
            .acquire(&shape, Some(stopping_stream))
            .expect("a texture");
        let still_held_by_the_stopping_stream = pool
            .acquire(&shape, Some(stopping_stream))
            .expect("a texture");
        let unheld_of_the_running_stream = pool
            .acquire(&shape, Some(running_stream))
            .expect("a texture");
        drop(unheld_of_the_stopping_stream);
        drop(unheld_of_the_running_stream);
        assert_eq!(pool.slot_count_of_the_shape_of(&shape), 3);

        assert_eq!(
            pool.free_every_unheld_slot_the_stream_last_acquired(stopping_stream),
            1
        );
        assert_eq!(pool.slot_count_of_the_shape_of(&shape), 2);

        drop(still_held_by_the_stopping_stream);
        wait_for_every_gpu_submission_made_so_far(&gpu);
        let _another_shape = pool
            .acquire(&a_16_high_rgba8_texture_of_width(32), None)
            .expect("a texture");
        assert_eq!(
            pool.slot_count_of_the_shape_of(&shape),
            1,
            "the running stream's idle slot was freed, or the stopped stream's held one was not \
             once let go"
        );
    }

    #[test]
    fn a_texture_pool_slot_handed_out_again_belongs_to_the_stream_that_acquired_it_last() {
        let Some(gpu) = gpu_context_or_skip() else {
            return;
        };
        let first_stream = LoadedStreamTag::numbered_for_a_test(1);
        let second_stream = LoadedStreamTag::numbered_for_a_test(2);
        let pool = gpu.texture_pool();
        let shape = a_16_high_rgba8_texture_of_width(16);
        let first = pool.acquire(&shape, Some(first_stream)).expect("a texture");
        let slot_id = first.slot_id();
        drop(first);
        let second = pool
            .acquire(&shape, Some(second_stream))
            .expect("a texture");
        assert_eq!(second.slot_id(), slot_id);
        drop(second);

        assert_eq!(
            pool.free_every_unheld_slot_the_stream_last_acquired(first_stream),
            0
        );
        assert_eq!(
            pool.free_every_unheld_slot_the_stream_last_acquired(second_stream),
            1
        );
        assert_eq!(pool.slot_count_of_the_shape_of(&shape), 0);
    }
}
