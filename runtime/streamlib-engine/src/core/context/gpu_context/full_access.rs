// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use streamlib_scope::CallTooSlowForAFrame;

pub struct GpuContextFullAccess {
    pub(crate) handle: *const std::ffi::c_void,
    /// The node this capability acquires one-offs for; `None` for the shared
    /// context, which counts none.
    pub(super) one_off_acquisitions_of_the_node:
        Option<Arc<OneOffAcquisitionsCountedForOneNativeNode>>,
}

unsafe impl Send for GpuContextFullAccess {}

unsafe impl Sync for GpuContextFullAccess {}

impl Drop for GpuContextFullAccess {
    /// Releases the handle: runs `Box::from_raw` on the boxed
    /// `Arc<GpuContext>`.
    fn drop(&mut self) {
        if self.handle.is_null() {
            return;
        }
        // SAFETY: handle was produced by `Self::counting_one_offs_against_the_node` via
        // `Box::into_raw(Box::new(Arc::new(GpuContext)))`.
        // `Box::from_raw` releases the box; the resulting
        // Arc<GpuContext>'s Drop releases the per-scope clone.
        let _ = unsafe { Box::from_raw(self.handle as *mut std::sync::Arc<GpuContext>) };
    }
}

impl GpuContextFullAccess {
    /// Back-room constructor. Wraps an in-process [`GpuContext`] as a
    /// full-access capability whose methods route through
    /// [`Self::host_inner`] for direct dispatch, and whose one-off
    /// acquisitions count against `one_off_acquisitions_of_the_node`.
    ///
    /// Scope tightened to `pub(in crate::core::context)` so only
    /// [`GpuContextLimitedAccess::escalate`]'s host-mode body can
    /// construct one. Other engine code that wants FullAccess goes
    /// through `escalate(|full| ...)`; the privilege gate enforces
    /// serialization + `wait_device_idle`.
    pub(in crate::core::context) fn counting_one_offs_against_the_node(
        inner: GpuContext,
        one_off_acquisitions_of_the_node: Option<Arc<OneOffAcquisitionsCountedForOneNativeNode>>,
    ) -> Self {
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
    pub(crate) fn host_inner(&self) -> &GpuContext {
        // SAFETY: `self.handle` was produced by `Self::counting_one_offs_against_the_node`, which
        // calls `Box::into_raw(Box::new(Arc::new(GpuContext)))`. The
        // matching Drop reclaims it, so the `Arc<GpuContext>` is alive
        // for the duration of `&self`.
        unsafe { &*(self.handle as *const std::sync::Arc<GpuContext>) }
    }
}

impl GpuContextFullAccess {
    /// A non-exportable timeline semaphore for host-side waits on this
    /// process's own submits. See [`GpuContext::create_timeline_semaphore`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_timeline_semaphore(
        &self,
        initial_value: u64,
    ) -> Result<std::sync::Arc<crate::vulkan::rhi::HostVulkanTimelineSemaphore>> {
        self.host_inner().create_timeline_semaphore(initial_value)
    }

    /// Build a [`crate::vulkan::rhi::VulkanOffscreenCompositor`] for
    /// `attachment_format`. In-process (Boxed) only.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_offscreen_compositor(
        &self,
        attachment_format: crate::core::rhi::TextureFormat,
    ) -> Result<crate::vulkan::rhi::VulkanOffscreenCompositor> {
        self.host_inner()
            .create_offscreen_compositor(attachment_format)
    }

    /// Wait for the GPU device to become idle.
    ///
    pub fn wait_device_idle(&self) -> Result<()> {
        self.host_inner().wait_device_idle()
    }

    pub fn create_node_output_pixel_buffer_pool_for_a_native_node(
        &self,
        node_run_id_of_the_owning_node: &str,
    ) -> crate::core::context::NodeOutputPixelBufferPoolOfOneNativeNode {
        self.host_inner()
            .create_node_output_pixel_buffer_pool_for_a_native_node(node_run_id_of_the_owning_node)
    }

    /// Acquire a pixel buffer from the shared pool. LimitedAccess
    /// mirror.
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acquire_uniform_buffer(
        &self,
        byte_size: u64,
    ) -> Result<crate::core::rhi::UniformBuffer> {
        self.host_inner().acquire_uniform_buffer(byte_size)
    }

    /// Acquire a HOST_VISIBLE vertex buffer.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acquire_vertex_buffer(&self, byte_size: u64) -> Result<crate::core::rhi::VertexBuffer> {
        self.host_inner().acquire_vertex_buffer(byte_size)
    }

    /// Acquire a HOST_VISIBLE index buffer.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acquire_index_buffer(&self, byte_size: u64) -> Result<crate::core::rhi::IndexBuffer> {
        self.host_inner().acquire_index_buffer(byte_size)
    }

    /// Get a pixel buffer by its published surface id.
    pub fn get_pixel_buffer(&self, surface_id: &str) -> Result<PixelBuffer> {
        self.host_inner().get_pixel_buffer(surface_id)
    }

    /// Resolve a VideoFrame's buffer from its surface_id.
    pub fn resolve_pixel_buffer_by_surface_id(&self, surface_id: &str) -> Result<PixelBuffer> {
        self.host_inner()
            .resolve_pixel_buffer_by_surface_id(surface_id)
    }

    /// Register a texture in the same-process texture cache.
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

    /// Resolve a VideoFrame's texture.
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

    /// Acquire a new output texture registered in the cache under a fresh
    /// UUID until the returned handle drops.
    /// Counted against this node's one-offs of its shape; see
    /// [`OneOffAcquisitionCountedForItsNode`].
    pub fn acquire_output_texture(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<(
        String,
        OneOffAcquisitionCountedForItsNode<OutputTextureRegisteredUntilDropped>,
    )> {
        let gpu = self.host_inner();
        acquire_a_one_off_counted_against_its_node(
            self.one_off_acquisitions_of_the_node.as_ref(),
            OneOffAcquisitionShape::Texture {
                width,
                height,
                format,
                usage: output_texture_usages(),
            },
            || {
                let (registered_under_id, texture) =
                    gpu.acquire_output_texture(width, height, format)?;
                Ok((
                    registered_under_id.clone(),
                    OutputTextureRegisteredUntilDropped {
                        texture,
                        registered_under_id,
                        gpu_the_texture_is_registered_with: gpu.clone(),
                    },
                ))
            },
        )
        .map(OneOffAcquisitionCountedForItsNode::split_off_the_id_it_was_acquired_under)
    }

    /// Upload a pixel buffer's contents to a GPU texture and register it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn upload_pixel_buffer_as_texture(
        &self,
        surface_id: &str,
        pixel_buffer: &PixelBuffer,
        width: u32,
        height: u32,
    ) -> Result<()> {
        self.host_inner()
            .upload_pixel_buffer_as_texture(surface_id, pixel_buffer, width, height)
    }

    /// Copy a host-visible pixel buffer's contents into an *already-allocated*
    /// device-local texture.
    ///
    /// See [`GpuContext::copy_pixel_buffer_to_texture`] for the
    /// underlying contract; the same primitive is exposed on
    /// [`GpuContextLimitedAccess`] for hot-path callers that already
    /// hold a texture (e.g. from a [`TextureRing`](crate::core::context::TextureRing)
    /// slot).
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

    /// Pre-allocate a ring of `count` non-exportable DEVICE_LOCAL
    /// textures and register each in the same-process texture cache.
    ///
    /// The returned [`crate::core::context::TextureRing`] is the
    /// canonical engine helper for decode-output hot paths — replaces
    /// every per-frame `upload_pixel_buffer_as_texture` escalation
    /// with a one-shot setup-time allocation plus a sandbox-safe
    /// rotation in `process()`.
    /// for the recipe and CLAUDE.md → "Texture rings — single
    /// canonical abstraction" for the engine-model context.
    ///
    /// `count` is rejected if zero; sizing to
    /// `MAX_FRAMES_IN_FLIGHT = 2`
    /// is the standard
    /// for hot-path decoders.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_texture_ring(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
        usages: TextureUsages,
        count: usize,
    ) -> Result<crate::core::context::TextureRing> {
        self.host_inner()
            .create_texture_ring(width, height, format, usages, count)
    }

    /// Create a single-in-flight GPU→CPU texture readback bound to a
    /// fixed format/extent and return it as the layout-stable
    /// [`crate::core::rhi::TextureReadback`] handle. The staging
    /// buffer + command resources + timeline semaphore are allocated
    /// once at construction and reused across every submit; for parallel
    /// readbacks, hold N handles. Planar `Nv12` is rejected (the readback
    /// staging model assumes a flat interleaved plane).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_texture_readback(
        &self,
        label: &str,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<crate::core::rhi::TextureReadback> {
        if matches!(format, TextureFormat::Nv12) {
            return Err(Error::GpuError(
                "create_texture_readback: planar Nv12 is not supported \
                     (readback assumes a flat interleaved plane)"
                    .into(),
            ));
        }
        let descriptor = crate::core::rhi::TextureReadbackDescriptor {
            label,
            format,
            width,
            height,
        };
        let arc = self.host_inner().create_texture_readback(&descriptor)?;
        // Cached POD sourced from the primitive itself — never
        // recomputed here.
        let cached_handle_id = arc.handle_id();
        let cached_staging_size = arc.staging_size();
        // Box-shaped opaque handle: `Box<Arc<VulkanTextureReadback>>`.
        let handle = Box::into_raw(Box::new(arc)) as *const std::ffi::c_void;
        Ok(crate::core::rhi::TextureReadback {
            handle,
            cached_handle_id,
            cached_staging_size,
            cached_width: width,
            cached_height: height,
            cached_format_raw: format as u32,
            _reserved_padding: 0,
        })
    }

    /// See [`GpuContext::unregister_texture`].
    pub fn unregister_texture(&self, id: &str) {
        self.host_inner().unregister_texture(id)
    }

    /// Get a reference to the RHI GPU device.
    ///
    /// **Engine-only** — returns `&Arc<GpuDevice>` which borrows into
    /// host-private state (the `Box<Arc<GpuContext>>` behind the
    /// handle). Consumers use the higher-level FullAccess methods
    /// (kernel construction, buffer/texture allocation, etc.) instead.
    pub fn device(&self) -> &Arc<GpuDevice> {
        self.host_inner().device()
    }

    /// Clone the host's `Arc<HostVulkanDevice>`. Engine-internal
    /// accessor for in-process RHI helpers (subprocess
    /// escalate handle assignment). Consumer GPU code builds
    /// through the FullAccess primitives, never the raw device.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn host_vulkan_device_arc(&self) -> Result<Arc<crate::vulkan::rhi::HostVulkanDevice>> {
        Ok(Arc::clone(
            crate::host_rhi::HostGpuDeviceExt::vulkan_device(self.host_inner().device().as_ref()),
        ))
    }

    /// Get the texture pool for acquiring pooled textures.
    ///
    /// **Engine-only** — returns `&TexturePool` which borrows into
    /// host-private state. Consumers use [`Self::acquire_texture`]
    /// instead.
    pub fn texture_pool(&self) -> &TexturePool {
        self.host_inner().texture_pool()
    }

    /// Acquire a pooled texture for in-process GPU work
    /// (`VK_IMAGE_TILING_OPTIMAL`). For cross-process render targets the
    /// host adapter layer wants on Linux, see
    /// [`Self::acquire_render_target_dma_buf_image`].
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

    /// Get the shared command queue.
    ///
    /// Hands out a refcount-bumped owned [`RhiCommandQueue`] handle; its
    /// Drop decrements the inner Arc's strong count.
    pub fn command_queue(&self) -> RhiCommandQueue {
        self.host_inner().command_queue().clone()
    }

    /// Create a command buffer from the shared queue.
    pub fn create_command_buffer(&self) -> Result<CommandBuffer> {
        self.host_inner().create_command_buffer()
    }

    /// Acquire a cached `(src, dst)`-keyed color converter. See
    /// [`GpuContext::color_converter`](crate::core::context::GpuContext::color_converter)
    /// on the inner context for usage.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn color_converter(&self, src: PixelFormat, dst: PixelFormat) -> Result<RhiColorConverter> {
        self.host_inner().color_converter(src, dst)
    }

    /// A color converter of the caller's own. See
    /// [`GpuContext::create_color_converter`](crate::core::context::GpuContext::create_color_converter).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_color_converter(
        &self,
        src: PixelFormat,
        dst: PixelFormat,
    ) -> Result<RhiColorConverter> {
        self.host_inner().create_color_converter(src, dst)
    }

    /// Create a compute kernel from a SPIR-V shader and a binding declaration.
    ///
    /// Runs the host's [`GpuContext::create_compute_kernel`], which
    /// returns the kernel as `Arc::into_raw`; this wrapper reconstructs
    /// it via `Arc::from_raw`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_compute_kernel(
        &self,
        descriptor: &crate::core::rhi::ComputeKernelDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::VulkanComputeKernel> {
        CallTooSlowForAFrame::CreatesAKernel.refused_while_a_frame_runs("create_compute_kernel")?;
        self.host_inner().create_compute_kernel(descriptor)
    }

    /// Build an engine-owned multi-step command-buffer recorder. See
    /// [`GpuContext::create_command_recorder`](crate::core::context::GpuContext::create_command_recorder)
    /// for the per-frame usage protocol.
    ///
    /// FullAccess-only because the recorder dispatches
    /// [`VulkanComputeKernel`](crate::vulkan::rhi::VulkanComputeKernel),
    /// which itself is FullAccess-only (privileged pipeline
    /// construction is excluded from the consumer-rhi carve-out). Subprocess
    /// consumers that need cross-process recording must escalate
    /// dispatch through the escalate IPC.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_command_recorder(
        &self,
        label: &str,
    ) -> Result<crate::vulkan::rhi::RhiCommandRecorder> {
        self.host_inner().create_command_recorder(label)
    }

    /// Create a graphics kernel from a multi-stage SPIR-V set, binding
    /// declaration, and fixed-function pipeline state.
    ///
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_graphics_kernel(
        &self,
        descriptor: &crate::core::rhi::GraphicsKernelDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::VulkanGraphicsKernel> {
        CallTooSlowForAFrame::CreatesAKernel
            .refused_while_a_frame_runs("create_graphics_kernel")?;
        self.host_inner().create_graphics_kernel(descriptor)
    }

    /// Create a ray-tracing kernel from shader stages, shader-group
    /// layout, binding declaration, and push-constant range.
    ///
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_ray_tracing_kernel(
        &self,
        descriptor: &crate::core::rhi::RayTracingKernelDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::VulkanRayTracingKernel> {
        CallTooSlowForAFrame::CreatesAKernel
            .refused_while_a_frame_runs("create_ray_tracing_kernel")?;
        self.host_inner().create_ray_tracing_kernel(descriptor)
    }

    /// Build a triangle-geometry bottom-level acceleration structure
    /// from CPU-side vertex + index data.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn build_triangles_blas(
        &self,
        label: &str,
        vertices: &[f32],
        indices: &[u32],
    ) -> Result<crate::vulkan::rhi::VulkanAccelerationStructure> {
        self.host_inner()
            .build_triangles_blas(label, vertices, indices)
    }

    /// Build a top-level acceleration structure from BLAS instances.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn build_tlas(
        &self,
        label: &str,
        instances: &[crate::vulkan::rhi::TlasInstanceDesc],
    ) -> Result<crate::vulkan::rhi::VulkanAccelerationStructure> {
        self.host_inner().build_tlas(label, instances)
    }

    /// Whether the underlying GPU exposes the
    /// `VK_KHR_ray_tracing_pipeline` extension chain.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn supports_ray_tracing_pipeline(&self) -> bool {
        self.host_inner().supports_ray_tracing_pipeline()
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn gpu_capabilities(&self) -> Result<GpuCapabilitiesSnapshot> {
        Ok(self.host_inner().gpu_capabilities())
    }

    /// Copy pixels between same-format, same-size buffers.
    pub fn blit_copy(&self, src: &PixelBuffer, dest: &PixelBuffer) -> Result<()> {
        self.host_inner().blit_copy(src, dest)
    }

    /// Clear the blitter's texture cache to free GPU memory.
    ///
    /// **Engine-only** — engine setup-time housekeeping.
    pub fn clear_blitter_cache(&self) {
        self.host_inner().clear_blitter_cache();
    }

    /// Get the surface store, if initialized.
    pub fn surface_store(&self) -> Option<SurfaceStore> {
        self.host_inner().surface_store()
    }

    /// Check in a pixel buffer to the surface-share service.
    pub fn check_in_surface(&self, pixel_buffer: &PixelBuffer) -> Result<String> {
        self.host_inner().check_in_surface(pixel_buffer)
    }

    /// Check out a surface by ID.
    pub fn check_out_surface(&self, surface_id: &str) -> Result<PixelBuffer> {
        self.host_inner().check_out_surface(surface_id)
    }

    /// Build a compute kernel, reusing an identical one this context already
    /// built. Reachable only inside `escalate(|full| ...)` since it requires
    /// `FullAccess`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_or_reuse_compute_kernel(
        &self,
        spv: &[u8],
        push_constant_size: u32,
        declared_bindings: &[crate::core::rhi::ComputeBindingDeclaration],
        entry_point: &str,
    ) -> Result<(String, Arc<crate::vulkan::rhi::VulkanComputeKernel>)> {
        CallTooSlowForAFrame::CreatesAKernel
            .refused_while_a_frame_runs("create_or_reuse_compute_kernel")?;
        self.host_inner().create_or_reuse_compute_kernel(
            spv,
            push_constant_size,
            declared_bindings,
            entry_point,
        )
    }

    /// Runs the host's [`GpuContext::release_compute_kernel`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn release_compute_kernel(&self, kernel_id: &str) -> bool {
        self.host_inner().release_compute_kernel(kernel_id)
    }

    /// Look up a compute kernel a prior `create_or_reuse_compute_kernel`
    /// returned, while a registration still holds it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn compute_kernel_by_id(
        &self,
        kernel_id: &str,
    ) -> Option<Arc<crate::vulkan::rhi::VulkanComputeKernel>> {
        self.host_inner().compute_kernel_by_id(kernel_id)
    }

    /// [`GpuContext::dispatch_compute_kernel_batch`] — N dispatches, one
    /// submission, one fence wait.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn dispatch_compute_kernel_batch(
        &self,
        batch: &[BatchedComputeKernelDispatch],
    ) -> Result<()> {
        self.host_inner().dispatch_compute_kernel_batch(batch)
    }

    /// Runs the host's [`GpuContext::create_or_reuse_graphics_kernel`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_or_reuse_graphics_kernel(
        &self,
        label: &str,
        stages: &[crate::core::rhi::GraphicsStage<'_>],
        declared_push_constants: crate::core::rhi::GraphicsPushConstants,
        pipeline_state: &crate::core::rhi::GraphicsPipelineState,
        descriptor_sets_in_flight: u32,
        declared_bindings: &[crate::core::rhi::GraphicsBindingDeclaration],
    ) -> Result<(String, Arc<crate::vulkan::rhi::VulkanGraphicsKernel>)> {
        CallTooSlowForAFrame::CreatesAKernel
            .refused_while_a_frame_runs("create_or_reuse_graphics_kernel")?;
        self.host_inner().create_or_reuse_graphics_kernel(
            label,
            stages,
            declared_push_constants,
            pipeline_state,
            descriptor_sets_in_flight,
            declared_bindings,
        )
    }

    /// [`GpuContext::graphics_kernel_by_id_held_for_one_draw`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn graphics_kernel_by_id_held_for_one_draw(
        &self,
        kernel_id: &str,
    ) -> Option<GraphicsKernelHeldForOneDraw<'_>> {
        self.host_inner()
            .graphics_kernel_by_id_held_for_one_draw(kernel_id)
    }

    /// Runs the host's [`GpuContext::create_or_reuse_ray_tracing_kernel`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_or_reuse_ray_tracing_kernel(
        &self,
        label: &str,
        stages: &[crate::core::rhi::RayTracingStage<'_>],
        groups: &[crate::core::rhi::RayTracingShaderGroup],
        declared_push_constants: crate::core::rhi::RayTracingPushConstants,
        max_recursion_depth: u32,
        declared_bindings: &[crate::core::rhi::RayTracingBindingDeclaration],
    ) -> Result<(String, Arc<crate::vulkan::rhi::VulkanRayTracingKernel>)> {
        CallTooSlowForAFrame::CreatesAKernel
            .refused_while_a_frame_runs("create_or_reuse_ray_tracing_kernel")?;
        self.host_inner().create_or_reuse_ray_tracing_kernel(
            label,
            stages,
            groups,
            declared_push_constants,
            max_recursion_depth,
            declared_bindings,
        )
    }

    /// [`GpuContext::ray_tracing_kernel_by_id_held_for_one_trace`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn ray_tracing_kernel_by_id_held_for_one_trace(
        &self,
        kernel_id: &str,
    ) -> Option<RayTracingKernelHeldForOneTrace<'_>> {
        self.host_inner()
            .ray_tracing_kernel_by_id_held_for_one_trace(kernel_id)
    }

    /// Runs the host's [`GpuContext::register_acceleration_structure`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn register_acceleration_structure(
        &self,
        acceleration_structure: crate::vulkan::rhi::VulkanAccelerationStructure,
    ) -> String {
        self.host_inner()
            .register_acceleration_structure(acceleration_structure)
    }

    /// Look up an acceleration structure a prior
    /// `register_acceleration_structure` returned the id of.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acceleration_structure_by_id(
        &self,
        acceleration_structure_id: &str,
    ) -> Option<Arc<crate::vulkan::rhi::VulkanAccelerationStructure>> {
        self.host_inner()
            .acceleration_structure_by_id(acceleration_structure_id)
    }

    /// Runs the host's [`GpuContext::release_acceleration_structure`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn release_acceleration_structure(&self, acceleration_structure_id: &str) -> bool {
        self.host_inner()
            .release_acceleration_structure(acceleration_structure_id)
    }
}

impl std::fmt::Debug for GpuContextFullAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContextFullAccess")
            .field("handle", &self.handle)
            .finish()
    }
}
