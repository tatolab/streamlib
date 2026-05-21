// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! TexturePool - Runtime-owned GPU texture management.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::{Condvar, Mutex};
use streamlib_plugin_abi::GpuContextLimitedAccessVTable;

use crate::core::rhi::{
    GpuDevice, NativeTextureHandle, Texture, TextureDescriptor, TextureFormat, TextureUsages,
};
use crate::core::{Result, Error};

/// Request descriptor for acquiring a pooled texture.
#[derive(Clone, Debug)]
pub struct TexturePoolDescriptor {
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub usage: TextureUsages,
    pub label: Option<&'static str>,
}

impl TexturePoolDescriptor {
    /// Create a new pool descriptor.
    pub fn new(width: u32, height: u32, format: TextureFormat) -> Self {
        Self {
            width,
            height,
            format,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC,
            label: None,
        }
    }

    /// Set usage flags.
    pub fn with_usage(mut self, usage: TextureUsages) -> Self {
        self.usage = usage;
        self
    }

    /// Set label.
    pub fn with_label(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }
}

/// Unique identifier for a pool slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PoolSlotId(u64);

/// Key for texture bucket lookup (dimension + format + usage).
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct TexturePoolKey {
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub usage: TextureUsages,
}

impl TexturePoolKey {
    pub fn from_descriptor(desc: &TexturePoolDescriptor) -> Self {
        Self {
            width: desc.width,
            height: desc.height,
            format: desc.format,
            usage: desc.usage,
        }
    }
}

/// Policy for handling pool exhaustion.
#[derive(Clone, Debug)]
pub enum TexturePoolExhaustionPolicy {
    /// Block and wait for a texture to be released.
    Block { timeout_ms: u64 },
    /// Grow the pool up to the specified maximum size.
    GrowPool { max_size: usize },
    /// Return an error immediately.
    ReturnError,
}

impl Default for TexturePoolExhaustionPolicy {
    fn default() -> Self {
        Self::Block { timeout_ms: 1000 }
    }
}

/// Configuration for the texture pool.
#[derive(Clone, Debug)]
pub struct TexturePoolConfig {
    /// Initial number of textures per bucket.
    pub initial_pool_size_per_bucket: usize,
    /// Maximum number of textures per bucket.
    pub max_pool_size_per_bucket: usize,
    /// Policy when pool is exhausted.
    pub exhaustion_policy: TexturePoolExhaustionPolicy,
}

impl Default for TexturePoolConfig {
    fn default() -> Self {
        Self {
            initial_pool_size_per_bucket: 4,
            max_pool_size_per_bucket: 16,
            exhaustion_policy: TexturePoolExhaustionPolicy::default(),
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

/// A slot in the texture pool.
pub(crate) struct PoolSlot {
    pub(crate) id: PoolSlotId,
    pub(crate) texture: Texture,
    pub(crate) key: TexturePoolKey,
    pub(crate) in_use: AtomicBool,
}

impl PoolSlot {
    pub(crate) fn is_available(&self) -> bool {
        !self.in_use.load(Ordering::Acquire)
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
    pub(crate) config: TexturePoolConfig,
    pub(crate) device: Arc<GpuDevice>,
    pub(crate) next_slot_id: AtomicU64,
    pub(crate) available_condvar: Condvar,
    pub(crate) buckets_mutex_for_condvar: Mutex<()>,
}

impl TexturePoolInner {
    pub(crate) fn next_slot_id(&self) -> PoolSlotId {
        PoolSlotId(self.next_slot_id.fetch_add(1, Ordering::Relaxed))
    }

    pub(crate) fn release(&self, slot_id: PoolSlotId) {
        let buckets = self.buckets.lock();
        for slots in buckets.values() {
            for slot in slots {
                if slot.id == slot_id {
                    slot.release();
                    // Signal waiting acquirers
                    self.available_condvar.notify_one();
                    return;
                }
            }
        }
    }

    pub(crate) fn find_available_slot(&self, key: &TexturePoolKey) -> Option<Arc<PoolSlot>> {
        let buckets = self.buckets.lock();
        if let Some(slots) = buckets.get(key) {
            for slot in slots {
                if slot.try_acquire() {
                    return Some(Arc::clone(slot));
                }
            }
        }
        None
    }

    pub(crate) fn bucket_size(&self, key: &TexturePoolKey) -> usize {
        let buckets = self.buckets.lock();
        buckets.get(key).map(|v| v.len()).unwrap_or(0)
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

/// Host-only rich data backing a [`PooledTextureHandle`]. Holds the
/// `Arc<TexturePoolInner>` reference plus the `PoolSlotId` so Drop
/// can release the slot exactly once. Cdylib code never sees this
/// type; it reaches `PooledTextureHandle` through the
/// `(handle, vtable, Texture, POD)` β-shape.
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
    /// Vtable for cross-DSO Drop dispatch.
    pub(crate) vtable: *const GpuContextLimitedAccessVTable,
    /// The pooled texture. Already β-shape (`#[repr(C)]`, 32 bytes);
    /// embedding by value keeps the wire ABI flat without an
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
    /// Constructor for non-macOS platforms (Linux/Windows). The
    /// host's pool allocator builds a `PooledTextureHandleInner`,
    /// leaks it via `Box::into_raw`, resolves the host-mode vtable,
    /// and assembles the cross-DSO shape.
    ///
    /// On macOS, handles are created via
    /// `texture_pool_macos::allocate_iosurface_slot`.
    #[allow(dead_code)]
    #[cfg(not(target_os = "macos"))]
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
        let vtable = crate::core::plugin::host_services::host_gpu_context_limited_access_vtable();
        Self {
            handle,
            vtable,
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
        if crate::core::plugin::host_services::host_callbacks().is_some() {
            panic!(
                "PooledTextureHandle::slot_id() reached from cdylib code; \
                 the pool slot id is host-internal and not exposed via \
                 the GpuContextLimitedAccessVTable."
            );
        }
        // SAFETY: `self.handle` is `Box::into_raw(Box<PooledTextureHandleInner>)`
        // (see `from_parts`); the Box keeps the inner alive until Drop runs.
        unsafe {
            (*(self.handle as *const PooledTextureHandleInner)).slot_id
        }
    }

    /// Get the IOSurface ID for cross-framework sharing.
    pub fn iosurface_id(&self) -> Option<u32> {
        self.texture.iosurface_id()
    }

    /// Get the platform-native sharing handle for this texture.
    pub fn native_handle(&self) -> Option<NativeTextureHandle> {
        self.texture.native_handle()
    }

    /// Get the underlying Metal texture (macOS only).
    #[cfg(target_os = "macos")]
    pub fn metal_texture(&self) -> &metal::TextureRef {
        self.texture.as_metal_texture()
    }
}

impl Drop for PooledTextureHandle {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.vtable.is_null() {
            // SAFETY: matched with the `Box::into_raw` in `from_parts`.
            // The vtable's `drop_pooled_texture_handle` callback runs
            // `Box::from_raw + drop` on the host side, which fires
            // `Drop for PooledTextureHandleInner` and releases the
            // pool slot exactly once.
            unsafe {
                ((*self.vtable).drop_pooled_texture_handle)(self.handle);
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
    /// Create a new texture pool with default configuration.
    pub fn new(device: Arc<GpuDevice>) -> Self {
        Self::with_config(device, TexturePoolConfig::default())
    }

    /// Create a new texture pool with custom configuration.
    pub fn with_config(device: Arc<GpuDevice>, config: TexturePoolConfig) -> Self {
        Self {
            inner: Arc::new(TexturePoolInner {
                buckets: Mutex::new(HashMap::new()),
                config,
                device,
                next_slot_id: AtomicU64::new(0),
                available_condvar: Condvar::new(),
                buckets_mutex_for_condvar: Mutex::new(()),
            }),
        }
    }

    /// Acquire a texture from the pool.
    pub fn acquire(&self, desc: &TexturePoolDescriptor) -> Result<PooledTextureHandle> {
        let key = TexturePoolKey::from_descriptor(desc);

        // Try to find an available slot
        if let Some(slot) = self.inner.find_available_slot(&key) {
            return Ok(self.create_handle_from_slot(&slot));
        }

        // No available slot - check if we can grow
        let current_size = self.inner.bucket_size(&key);
        let can_grow = current_size < self.inner.config.max_pool_size_per_bucket;

        if can_grow {
            // Allocate a new texture
            let slot = self.allocate_slot(desc)?;
            slot.try_acquire(); // Mark as in use
            self.inner.add_slot(Arc::clone(&slot));
            return Ok(self.create_handle_from_slot(&slot));
        }

        // Pool exhausted - apply policy
        match &self.inner.config.exhaustion_policy {
            TexturePoolExhaustionPolicy::Block { timeout_ms } => {
                self.acquire_blocking(&key, desc, *timeout_ms)
            }
            TexturePoolExhaustionPolicy::GrowPool { max_size } => {
                if current_size < *max_size {
                    let slot = self.allocate_slot(desc)?;
                    slot.try_acquire();
                    self.inner.add_slot(Arc::clone(&slot));
                    Ok(self.create_handle_from_slot(&slot))
                } else {
                    Err(Error::TextureError(
                        "Texture pool exhausted (max size reached)".into(),
                    ))
                }
            }
            TexturePoolExhaustionPolicy::ReturnError => Err(Error::TextureError(
                "Texture pool exhausted (no available slots)".into(),
            )),
        }
    }

    fn acquire_blocking(
        &self,
        key: &TexturePoolKey,
        _desc: &TexturePoolDescriptor,
        timeout_ms: u64,
    ) -> Result<PooledTextureHandle> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);

        loop {
            // Try to acquire
            if let Some(slot) = self.inner.find_available_slot(key) {
                return Ok(self.create_handle_from_slot(&slot));
            }

            // Check timeout
            let now = std::time::Instant::now();
            if now >= deadline {
                return Err(Error::TextureError(format!(
                    "Texture pool exhausted (timeout after {}ms)",
                    timeout_ms
                )));
            }

            // Wait for a slot to become available
            let remaining = deadline - now;
            let mut guard = self.inner.buckets_mutex_for_condvar.lock();
            let result = self.inner.available_condvar.wait_for(&mut guard, remaining);

            if result.timed_out() {
                // Check one more time before giving up
                if let Some(slot) = self.inner.find_available_slot(key) {
                    return Ok(self.create_handle_from_slot(&slot));
                }
                return Err(Error::TextureError(format!(
                    "Texture pool exhausted (timeout after {}ms)",
                    timeout_ms
                )));
            }
        }
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

    /// Allocate a new texture slot.
    #[cfg(not(target_os = "macos"))]
    fn allocate_slot(&self, desc: &TexturePoolDescriptor) -> Result<Arc<PoolSlot>> {
        let texture_desc =
            TextureDescriptor::new(desc.width, desc.height, desc.format).with_usage(desc.usage);

        let texture = self.inner.device.create_texture(&texture_desc)?;

        Ok(Arc::new(PoolSlot {
            id: self.inner.next_slot_id(),
            texture,
            key: TexturePoolKey::from_descriptor(desc),
            in_use: AtomicBool::new(false),
        }))
    }

    /// Allocate a new IOSurface-backed texture slot (macOS).
    #[cfg(target_os = "macos")]
    fn allocate_slot(&self, desc: &TexturePoolDescriptor) -> Result<Arc<PoolSlot>> {
        // Delegate to macOS-specific implementation
        crate::apple::texture_pool_macos::allocate_iosurface_slot(&self.inner, desc)
    }

    /// Pre-warm the pool with textures of specific dimensions.
    pub fn prewarm(&self, desc: &TexturePoolDescriptor, count: usize) -> Result<()> {
        for _ in 0..count {
            let slot = self.allocate_slot(desc)?;
            self.inner.add_slot(slot);
        }
        Ok(())
    }

    /// Get statistics about pool usage.
    pub fn stats(&self) -> TexturePoolStats {
        self.inner.stats()
    }

    /// Clear all unused textures from the pool.
    pub fn clear_unused(&self) {
        let mut buckets = self.inner.buckets.lock();
        for slots in buckets.values_mut() {
            slots.retain(|slot| !slot.is_available());
        }
        // Remove empty buckets
        buckets.retain(|_, slots| !slots.is_empty());
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
// `PooledTextureHandle` crosses the cdylib DSO boundary as a
// `#[repr(C)]` struct. Drift in its byte-level shape would silently
// corrupt every `acquire_texture` return-path: the cdylib's Drop
// impl reads `vtable` and `handle` at fixed offsets to call
// `drop_pooled_texture_handle`. The vtable layout-version constant
// guards the dispatch table; this test guards the value type.

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn pooled_texture_handle_layout() {
        // Pin the byte-level shape. Fields:
        //   handle        : *const c_void   → offset 0,  size 8
        //   vtable        : *const VTable   → offset 8,  size 8
        //   texture       : Texture (β,32B) → offset 16, size 32
        //   width_cached  : u32             → offset 48, size 4
        //   height_cached : u32             → offset 52, size 4
        //   format_raw    : u32             → offset 56, size 4
        //   _padding      : u32             → offset 60, size 4
        // Total: 64 bytes, 8-byte alignment (pinned by the pointers).
        assert_eq!(size_of::<PooledTextureHandle>(), 64);
        assert_eq!(align_of::<PooledTextureHandle>(), 8);
        assert_eq!(offset_of!(PooledTextureHandle, handle), 0);
        assert_eq!(offset_of!(PooledTextureHandle, vtable), 8);
        assert_eq!(offset_of!(PooledTextureHandle, texture), 16);
        assert_eq!(offset_of!(PooledTextureHandle, width_cached), 48);
        assert_eq!(offset_of!(PooledTextureHandle, height_cached), 52);
        assert_eq!(offset_of!(PooledTextureHandle, format_raw), 56);
        assert_eq!(offset_of!(PooledTextureHandle, _padding), 60);
    }

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
