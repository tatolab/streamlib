// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! TexturePool - Runtime-owned GPU texture management.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::{Condvar, Mutex};

use crate::core::rhi::{
    GpuDevice, NativeTextureHandle, StreamTexture, TextureFormat, TextureUsages,
};
use crate::core::{Result, StreamError};

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
    pub(crate) texture: StreamTexture,
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

/// Handle to a pooled texture. Returns texture to pool on Drop.
pub struct PooledTextureHandle {
    texture: StreamTexture,
    pool_inner: Arc<TexturePoolInner>,
    slot_id: PoolSlotId,
    width: u32,
    height: u32,
    format: TextureFormat,
}

impl PooledTextureHandle {
    /// Constructor for non-macOS platforms (Linux/Windows).
    /// On macOS, handles are created via `texture_pool_macos::allocate_iosurface_slot`.
    #[allow(dead_code)]
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn new(
        texture: StreamTexture,
        pool_inner: Arc<TexturePoolInner>,
        slot_id: PoolSlotId,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Self {
        Self {
            texture,
            pool_inner,
            slot_id,
            width,
            height,
            format,
        }
    }

    /// Get a reference to the underlying texture.
    pub fn texture(&self) -> &StreamTexture {
        &self.texture
    }

    /// Get a cloneable reference to the underlying texture.
    pub fn texture_clone(&self) -> StreamTexture {
        self.texture.clone()
    }

    /// Get the texture width.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Get the texture height.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Get the texture format.
    pub fn format(&self) -> TextureFormat {
        self.format
    }

    /// Get the pool slot ID.
    pub fn slot_id(&self) -> PoolSlotId {
        self.slot_id
    }

    /// Get the IOSurface ID for cross-framework sharing.
    ///
    /// Returns `Some(id)` on macOS/iOS if the texture is backed by an IOSurface.
    /// Returns `None` on other platforms or if no IOSurface is available.
    pub fn iosurface_id(&self) -> Option<u32> {
        self.texture.iosurface_id()
    }

    /// Get the platform-native sharing handle for this texture.
    ///
    /// Returns the appropriate handle type for the current platform:
    /// - macOS/iOS: `IOSurface { id }`
    /// - Linux: `DmaBuf { fd }` (when implemented)
    /// - Windows: `DxgiSharedHandle { handle }` (when implemented)
    ///
    /// Returns `None` if no sharing handle is available.
    pub fn native_handle(&self) -> Option<NativeTextureHandle> {
        self.texture.native_handle()
    }

    /// Get the underlying Metal texture (macOS only).
    #[cfg(target_os = "macos")]
    pub fn metal_texture(&self) -> &metal::TextureRef {
        self.texture.as_metal_texture()
    }

    /// Bind this texture to an OpenGL texture and return the binding info.
    ///
    /// This enables interop with OpenGL-based libraries like Skia.
    /// See [`StreamTexture::gl_texture_binding`] for details.
    pub fn gl_texture_binding(
        &self,
        gl_ctx: &mut crate::core::rhi::GlContext,
    ) -> crate::core::Result<crate::core::rhi::GlTextureBinding> {
        self.texture.gl_texture_binding(gl_ctx)
    }
}

impl Drop for PooledTextureHandle {
    fn drop(&mut self) {
        self.pool_inner.release(self.slot_id);
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
                    Err(StreamError::TextureError(
                        "Texture pool exhausted (max size reached)".into(),
                    ))
                }
            }
            TexturePoolExhaustionPolicy::ReturnError => Err(StreamError::TextureError(
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
                return Err(StreamError::TextureError(format!(
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
                return Err(StreamError::TextureError(format!(
                    "Texture pool exhausted (timeout after {}ms)",
                    timeout_ms
                )));
            }
        }
    }

    fn create_handle_from_slot(&self, slot: &Arc<PoolSlot>) -> PooledTextureHandle {
        PooledTextureHandle {
            texture: slot.texture.clone(),
            pool_inner: Arc::clone(&self.inner),
            slot_id: slot.id,
            width: slot.key.width,
            height: slot.key.height,
            format: slot.key.format,
        }
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
