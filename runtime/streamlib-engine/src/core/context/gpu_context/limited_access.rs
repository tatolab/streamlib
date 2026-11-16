// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

// =============================================================================
// Capability-typed wrappers
// =============================================================================
//
// `GpuContextLimitedAccess` is the capability handed to `process()` — at runtime
// it is meant to expose only cheap, pool-backed, non-allocating operations.
// `GpuContextFullAccess` is handed to `setup()` and inside
// `limited.escalate(|full| …)` closures — it exposes the full API, including
// GPU memory allocation.
//
// In this task (#321) both types are thin newtype wrappers around `GpuContext`
// and expose the **same** full API. This is a pure compile-time addition with
// no behavior change. The API surface split and the `escalate()` primitive
// land in #323/#324.

/// Restricted GPU capability handed to `process()`.
///
/// In the final design this type exposes only cheap, pool-backed, non-allocating
/// operations; heavier work must go through [`GpuContextLimitedAccess::escalate`].
///
/// Restricted GPU capability shim: an opaque `*const c_void` handle
/// pointing at a host-leaked `Box<Arc<GpuContext>>`.
#[repr(C)]
pub struct GpuContextLimitedAccess {
    /// Opaque host handle. Points at a `Box<Arc<GpuContext>>`.
    pub(crate) handle: *const std::ffi::c_void,
    /// The node this capability acquires one-offs for; `None` for the shared
    /// context, which counts none.
    one_off_acquisitions_of_the_node: Option<Arc<OneOffAcquisitionsCountedForOneNativeNode>>,
}

// SAFETY: `handle` points at a host-owned `Box<Arc<GpuContext>>` that
// is `Send + Sync` (Arc carries atomic refcounts, GpuContext's
// fields are themselves Send + Sync via their Arc wrappers). The
// SAFETY: `handle` points at a `Box<Arc<GpuContext>>` whose interior is
// Send + Sync; every method reaches the GpuContext through the handle.
unsafe impl Send for GpuContextLimitedAccess {}

unsafe impl Sync for GpuContextLimitedAccess {}

impl Clone for GpuContextLimitedAccess {
    /// Bumps the host's `Arc<GpuContext>` refcount.
    fn clone(&self) -> Self {
        let new_handle = if !self.handle.is_null() {
            // SAFETY: `handle` is `Box::into_raw(Box<Arc<GpuContext>>)`;
            // clone the Arc, re-box, and leak — released by the matching Drop.
            unsafe {
                let arc = &*(self.handle as *const std::sync::Arc<GpuContext>);
                Box::into_raw(Box::new(std::sync::Arc::clone(arc))) as *const std::ffi::c_void
            }
        } else {
            std::ptr::null()
        };
        Self {
            handle: new_handle,
            one_off_acquisitions_of_the_node: self.one_off_acquisitions_of_the_node.clone(),
        }
    }
}

impl Drop for GpuContextLimitedAccess {
    /// Releases the host-owned handle.
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: `handle` is `Box::into_raw(Box<Arc<GpuContext>>)`,
            // from `new()` or `Clone`; reclaim it.
            unsafe {
                drop(Box::from_raw(
                    self.handle as *mut std::sync::Arc<GpuContext>,
                ));
            }
        }
    }
}

impl GpuContextLimitedAccess {
    /// Wrap a [`GpuContext`] as a limited-access capability.
    ///
    /// The handle is the sole owning reference to the
    /// `Arc<GpuContext>`; every method reaches it through
    /// [`Self::host_inner`]. Allocates a host-side
    /// `Box<Arc<GpuContext>>` as the opaque handle.
    pub(crate) fn new(inner: GpuContext) -> Self {
        Self::counting_one_offs_against_the_node(inner, None)
    }

    /// Wrap a [`GpuContext`] as a limited-access capability whose one-off
    /// acquisitions count against `one_off_acquisitions_of_the_node`.
    pub(crate) fn counting_one_offs_against_the_node(
        inner: GpuContext,
        one_off_acquisitions_of_the_node: Option<Arc<OneOffAcquisitionsCountedForOneNativeNode>>,
    ) -> Self {
        // Leak a fresh `Arc<GpuContext>` to back the opaque handle.
        // The handle is the sole owner; `host_inner()` derefs it.
        let arc: std::sync::Arc<GpuContext> = std::sync::Arc::new(inner);
        let boxed: Box<std::sync::Arc<GpuContext>> = Box::new(arc);
        let handle = Box::into_raw(boxed) as *const std::ffi::c_void;
        Self {
            handle,
            one_off_acquisitions_of_the_node,
        }
    }

    /// Engine-internal borrow of the host's [`GpuContext`] (read
    /// through the handle's `Box<Arc<GpuContext>>`).
    ///
    pub(crate) fn host_inner(&self) -> &GpuContext {
        // SAFETY: `self.handle` was produced by `Self::new` or
        // `host_gpu_lim_clone_handle` — both produce
        // `Box::into_raw(Box::new(Arc::new(GpuContext)))`. The
        // matching `host_gpu_lim_drop_handle` runs on Drop, so the
        // `Arc<GpuContext>` is alive for the duration of `&self`.
        // We deref the Box, then the Arc, to borrow the inner
        // `GpuContext`.
        unsafe { &*(self.handle as *const std::sync::Arc<GpuContext>) }
    }

    /// Produce a [`GpuContextFullAccess`] view of the same underlying context.
    #[cfg(test)]
    pub(crate) fn to_full_access(&self) -> GpuContextFullAccess {
        GpuContextFullAccess::counting_one_offs_against_the_node(
            self.host_inner().clone(),
            self.one_off_acquisitions_of_the_node.clone(),
        )
    }

    pub fn escalate<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&GpuContextFullAccess) -> Result<T>,
    {
        self.escalate_in_process(f)
    }

    /// Engine-internal escalate path.
    pub(super) fn escalate_in_process<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&GpuContextFullAccess) -> Result<T>,
    {
        let inner = self.host_inner();
        let lock_start = std::time::Instant::now();
        let _gate_guard = inner.escalate_gate().enter_scoped();
        let mutex_wait_ns = lock_start.elapsed().as_nanos() as u64;

        let closure_start = std::time::Instant::now();
        let full = GpuContextFullAccess::counting_one_offs_against_the_node(
            inner.clone(),
            self.one_off_acquisitions_of_the_node.clone(),
        );
        let closure_result = f(&full);
        drop(full);
        let closure_duration_ns = closure_start.elapsed().as_nanos() as u64;

        let wait_start = std::time::Instant::now();
        let wait_result = inner.wait_device_idle();
        let wait_idle_ns = wait_start.elapsed().as_nanos() as u64;

        tracing::trace!(
            target: "streamlib::gpu_context::escalate",
            dispatch = "in_process",
            mutex_wait_ns,
            closure_duration_ns,
            wait_idle_ns,
            closure_ok = closure_result.is_ok(),
            "GpuContextLimitedAccess::escalate completed"
        );

        check_sustained_escalation_rate();

        match (closure_result, wait_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(e), _) => Err(e),
            (Ok(_), Err(e)) => Err(e),
        }
    }
}

std::thread_local! {
    static ESCALATION_TIMESTAMPS_NS: std::cell::RefCell<std::collections::VecDeque<u64>> =
        std::cell::RefCell::new(std::collections::VecDeque::with_capacity(16));
    static ESCALATION_LAST_WARN_NS: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

const ESCALATION_RATE_WARN_THRESHOLD_PER_SEC: usize = 1;

const ESCALATION_RATE_WINDOW_NS: u64 = 1_000_000_000;

const ESCALATION_WARN_DEBOUNCE_NS: u64 = 5_000_000_000;

fn check_sustained_escalation_rate() {
    let now_ns = MediaClock::now().as_nanos() as u64;
    let cutoff = now_ns.saturating_sub(ESCALATION_RATE_WINDOW_NS);

    let (count, last_warn) = ESCALATION_TIMESTAMPS_NS.with(|buf| {
        let mut buf = buf.borrow_mut();
        while buf.front().is_some_and(|&ts| ts < cutoff) {
            buf.pop_front();
        }
        buf.push_back(now_ns);
        let count = buf.len();
        let last_warn = ESCALATION_LAST_WARN_NS.with(|c| c.get());
        (count, last_warn)
    });

    if count > ESCALATION_RATE_WARN_THRESHOLD_PER_SEC
        && last_warn.is_none_or(|at_ns| now_ns.saturating_sub(at_ns) >= ESCALATION_WARN_DEBOUNCE_NS)
    {
        ESCALATION_LAST_WARN_NS.with(|c| c.set(Some(now_ns)));
        let thread = std::thread::current();
        tracing::warn!(
            thread = thread.name().unwrap_or("<unnamed>"),
            escalations_last_second = count,
            "sustained GpuContextLimitedAccess::escalate rate on this thread — \
             node likely needs more pre-reservation in setup()"
        );
    }
}

// -----------------------------------------------------------------------------
// Capability-split API surface.
//
// `GpuContextLimitedAccess` exposes the Sandbox surface only: pool acquires
// (pre-reserved), texture sampling, writes to mapped pixel buffers, read-only
// queries, and the shared command queue. Methods that allocate new GPU
// memory, create sessions/descriptors, or hand out raw device
// handles live exclusively on [`GpuContextFullAccess`] and are reachable from
// `process()` only via [`GpuContextLimitedAccess::escalate`].
// -----------------------------------------------------------------------------

impl GpuContextLimitedAccess {
    /// Acquire a pixel buffer from a pre-reserved pool (Split: fast path).
    ///
    /// The expected steady-state is a ring-slot hit. Callers should pre-reserve
    /// the pool in `setup()` by calling `acquire_pixel_buffer` on the
    /// [`GpuContextFullAccess`] with the target `(width, height, format)`.
    /// When every slot of the shape is held — by consumers, or lent to native
    /// nodes' output pools — the pool grows; past the device's memory budget
    /// that growth is refused by name, and nothing waits on another holder.
    /// Counted against this node's one-offs of its shape; see
    /// [`OneOffAcquisitionCountedForItsNode`].
    pub fn acquire_pixel_buffer(
        &self,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<(
        PublishedPixelBufferFrameId,
        OneOffAcquisitionCountedForItsNode<PixelBuffer>,
    )> {
        acquire_pixel_buffer_counted_against_the_node(
            self.host_inner(),
            self.one_off_acquisitions_of_the_node.as_ref(),
            width,
            height,
            format,
        )
    }

    /// Acquire a caller-held storage buffer sized for `layout`.
    /// See [`GpuContext::acquire_storage_buffer`].
    /// Counted against this node's one-offs of its shape; see
    /// [`OneOffAcquisitionCountedForItsNode`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acquire_storage_buffer(
        &self,
        layout: &crate::core::rhi::TensorStorageBufferLayout,
    ) -> Result<OneOffAcquisitionCountedForItsNode<crate::core::rhi::StorageBuffer>> {
        acquire_storage_buffer_counted_against_the_node(
            self.host_inner(),
            self.one_off_acquisitions_of_the_node.as_ref(),
            layout,
        )
    }

    /// Acquire a HOST_VISIBLE uniform buffer.
    /// See [`GpuContext::acquire_uniform_buffer`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acquire_uniform_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::core::rhi::UniformBuffer> {
        self.host_inner().acquire_uniform_buffer(byte_size)
    }

    /// Acquire a HOST_VISIBLE vertex buffer.
    /// See [`GpuContext::acquire_vertex_buffer`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acquire_vertex_buffer(&self, byte_size: u64) -> Result<crate::core::rhi::VertexBuffer> {
        self.host_inner().acquire_vertex_buffer(byte_size)
    }

    /// Acquire a HOST_VISIBLE index buffer.
    /// See [`GpuContext::acquire_index_buffer`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acquire_index_buffer(&self, byte_size: u64) -> Result<crate::core::rhi::IndexBuffer> {
        self.host_inner().acquire_index_buffer(byte_size)
    }

    /// Get a pixel buffer by its published surface id (Split: local cache).
    pub fn get_pixel_buffer(&self, surface_id: &str) -> Result<PixelBuffer> {
        self.host_inner().get_pixel_buffer(surface_id)
    }

    /// Resolve a VideoFrame's buffer from its surface_id.
    pub fn resolve_pixel_buffer_by_surface_id(&self, surface_id: &str) -> Result<PixelBuffer> {
        self.host_inner()
            .resolve_pixel_buffer_by_surface_id(surface_id)
    }

    /// Register a texture in the same-process texture cache.
    ///
    /// The host-side impl bumps the
    /// `Arc<TextureInner>` refcount before stashing a clone in the
    /// cache, so dropping the caller's `texture` here releases
    /// exactly the caller's owned ref.
    pub fn register_texture(&self, id: &str, texture: Texture) {
        self.host_inner().register_texture(id, texture)
    }

    /// Register a texture with a declared initial Vulkan image layout.
    /// See [`GpuContext::register_texture_with_layout`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn register_texture_with_layout(
        &self,
        id: &str,
        texture: Texture,
        initial_layout: VulkanLayout,
    ) {
        self.host_inner()
            .register_texture_with_layout(id, texture, initial_layout)
    }

    /// Update a registered texture's tracked layout after a transition.
    /// See [`GpuContext::update_texture_registration_layout`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn update_texture_registration_layout(&self, id: &str, layout: VulkanLayout) {
        self.host_inner()
            .update_texture_registration_layout(id, layout)
    }

    /// Resolve a VideoFrame's full registration record (texture + layout).
    ///
    /// Returns a [`TextureRegistration`] handle;
    /// Clone is cheap (refcount bump via vtable), Drop releases the
    /// host's `Arc<TextureRegistrationInner>` strong count.
    pub fn resolve_texture_registration_by_surface_id(
        &self,
        surface_id: &str,
        texture_layout: Option<i32>,
        width: u32,
        height: u32,
    ) -> Result<TextureRegistration> {
        self.host_inner()
            .resolve_texture_registration_by_surface_id(surface_id, texture_layout, width, height)
    }

    /// Resolve a VideoFrame's texture (Split: cache hit).
    pub fn resolve_texture_by_surface_id(
        &self,
        surface_id: &str,
        texture_layout: Option<i32>,
        width: u32,
        height: u32,
    ) -> Result<Texture> {
        self.host_inner()
            .resolve_texture_by_surface_id(surface_id, texture_layout, width, height)
    }

    /// Acquire a pooled texture from a pre-reserved pool (Split: fast path).
    ///
    /// `VK_IMAGE_TILING_OPTIMAL`, in-process use only. For cross-process
    /// render targets, see [`GpuContextFullAccess::acquire_render_target_dma_buf_image`]
    /// (Linux) — Sandbox callers don't have a render-target alloc path
    /// because allocating a new RT-capable image is a privileged op
    /// that goes through escalate.
    /// Counted against this node's one-offs of its shape; see
    /// [`OneOffAcquisitionCountedForItsNode`].
    pub fn acquire_texture(
        &self,
        desc: &TexturePoolDescriptor,
    ) -> Result<OneOffAcquisitionCountedForItsNode<PooledTextureHandle>> {
        acquire_texture_counted_against_the_node(
            self.host_inner(),
            self.one_off_acquisitions_of_the_node.as_ref(),
            desc,
        )
    }

    /// Copy a host-visible pixel buffer's contents into a pre-allocated
    /// device-local texture (e.g. a [`TextureRing`](crate::core::context::TextureRing)
    /// slot the caller already owns).
    ///
    /// Sandbox-safe: no allocation, no descriptor / pipeline construction,
    /// just a `vkCmdCopyBufferToImage` queue submit on the shared queue.
    /// See [`GpuContext::copy_pixel_buffer_to_texture`] for the full
    /// contract.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn copy_pixel_buffer_to_texture(
        &self,
        pixel_buffer: &PixelBuffer,
        texture: &Texture,
        surface_id: &str,
        width: u32,
        height: u32,
    ) -> Result<()> {
        self.host_inner().copy_pixel_buffer_to_texture(
            pixel_buffer,
            texture,
            surface_id,
            width,
            height,
        )
    }

    /// See [`GpuContext::unregister_texture`].
    pub fn unregister_texture(&self, id: &str) {
        self.host_inner().unregister_texture(id)
    }

    /// Drop a torn-down lease-aware pool slot from the generation index.
    pub(crate) fn forget_lease_aware_pool_slot(&self, pool_slot_key: &str) {
        self.host_inner()
            .lease_aware_pool_minted_frame_generations
            .forget_slot(pool_slot_key)
    }

    /// Get the shared command queue.
    ///
    /// Submitting recorded command buffers from `process()` is safe: the
    /// images/buffers a Sandbox caller can construct are pool-backed and
    /// pre-reserved. See design doc §8 Q5.
    ///
    /// Returns an owned [`RhiCommandQueue`] handle with the host's
    /// `Arc<RhiCommandQueueInner>` refcount bumped.
    pub fn command_queue(&self) -> RhiCommandQueue {
        self.host_inner().command_queue().clone()
    }

    /// Create a CPU-side command buffer from the shared queue.
    pub fn create_command_buffer(&self) -> Result<CommandBuffer> {
        self.host_inner().create_command_buffer()
    }

    /// Copy pixels between same-format, same-size buffers (Split: cache hit).
    pub fn blit_copy(&self, src: &PixelBuffer, dest: &PixelBuffer) -> Result<()> {
        self.host_inner().blit_copy(src, dest)
    }

    /// Get the surface store, if initialized.
    ///
    /// Returns `Some(SurfaceStore)` (refcount bumped) when the host has
    /// one, else `None`. The handle's own Clone/Drop manage the inner
    /// `Arc<SurfaceStoreInner>` strong count.
    pub fn surface_store(&self) -> Option<SurfaceStore> {
        self.host_inner().surface_store()
    }

    /// Check out a surface by ID (Split: cache hit).
    pub fn check_out_surface(&self, surface_id: &str) -> Result<PixelBuffer> {
        self.host_inner().check_out_surface(surface_id)
    }
}

impl std::fmt::Debug for GpuContextLimitedAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContextLimitedAccess")
            .field("handle", &self.handle)
            .finish()
    }
}
