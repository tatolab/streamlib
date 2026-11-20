// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// Cache key for a compute kernel built from a pre-compiled blob.
///
/// The blob fixes stage, entry point and target environment on its own, so the
/// bytes plus the declared push-constant size are the whole key. A Slang source
/// contract keys on the compiler's version too, because then the engine is what
/// turns source into bytes.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn compute_kernel_cache_key(
    spv: &[u8],
    push_constant_size: u32,
    entry_point: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(spv);
    hasher.update(push_constant_size.to_le_bytes());
    // Two pipelines built from one module against different entry points are
    // different kernels, so the id has to tell them apart. The length prefix is
    // defensive: the name is last in the digest today, so nothing can run into
    // it yet, but a field appended below would merge with it silently.
    hasher.update((entry_point.len() as u64).to_le_bytes());
    hasher.update(entry_point.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Digest a variable-length part of a cache key.
///
/// The length prefix is what keeps two different splits of the same
/// concatenated bytes from hashing the same.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn digest_length_prefixed(hasher: &mut sha2::Sha256, bytes: &[u8]) {
    use sha2::Digest as _;
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// Cache key for a graphics kernel.
///
/// Everything that changes the pipeline the driver builds is in the digest.
/// The fixed-function state goes in through its `Debug` rendering because that
/// is total: a field added to `GraphicsPipelineState` joins the key without
/// anyone remembering to add it, which a hand-enumerated digest cannot promise.
/// The key never leaves this process, so its stability across builds buys
/// nothing that would justify the alternative.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn graphics_kernel_cache_key(
    stages: &[crate::core::rhi::GraphicsStage<'_>],
    push_constants: crate::core::rhi::GraphicsPushConstants,
    pipeline_state: &crate::core::rhi::GraphicsPipelineState,
    descriptor_sets_in_flight: u32,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update((stages.len() as u64).to_le_bytes());
    for stage in stages {
        hasher.update((stage.stage as u32).to_le_bytes());
        digest_length_prefixed(&mut hasher, stage.spv);
        digest_length_prefixed(&mut hasher, stage.entry_point.as_bytes());
    }
    hasher.update(push_constants.size.to_le_bytes());
    hasher.update(push_constants.stages.bits().to_le_bytes());
    hasher.update(descriptor_sets_in_flight.to_le_bytes());
    digest_length_prefixed(&mut hasher, format!("{pipeline_state:?}").as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Cache key for a ray-tracing kernel.
///
/// Same shape as the graphics key; the shader-group layout and the recursion
/// depth take the place of the fixed-function state, since those are what the
/// driver builds the pipeline and its binding table from.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn ray_tracing_kernel_cache_key(
    stages: &[crate::core::rhi::RayTracingStage<'_>],
    groups: &[crate::core::rhi::RayTracingShaderGroup],
    push_constants: crate::core::rhi::RayTracingPushConstants,
    max_recursion_depth: u32,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update((stages.len() as u64).to_le_bytes());
    for stage in stages {
        hasher.update((stage.stage as u32).to_le_bytes());
        digest_length_prefixed(&mut hasher, stage.spv);
        digest_length_prefixed(&mut hasher, stage.entry_point.as_bytes());
    }
    hasher.update(push_constants.size.to_le_bytes());
    hasher.update(push_constants.stages.bits().to_le_bytes());
    hasher.update(max_recursion_depth.to_le_bytes());
    digest_length_prefixed(&mut hasher, format!("{groups:?}").as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Settle a caller's push-constant declaration against what the shaders
/// reflect, for a kernel kind whose push-constant range carries a stage mask.
///
/// The size must agree outright. The stage mask follows the same rule a
/// binding's does — a declaration may widen visibility past what the shaders
/// read, never narrow it below — and an empty mask asserts nothing, so the
/// reflected one stands.
///
/// Generic over the mask rather than written once per kernel kind, for the same
/// reason the shared binding reconciliation is: graphics and ray tracing differ
/// only in which unrelated `u32` newtype they name, and
/// [`KernelShaderStageMask`] is the seam that already spans both.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn reconciled_push_constant_stages<Stages: KernelShaderStageMask>(
    kernel_kind_label: &str,
    declared_size: u32,
    declared_stages: Stages,
    reflected_size: u32,
    reflected_stages: Stages,
) -> Result<Stages> {
    if declared_size != reflected_size {
        return Err(Error::GpuError(format!(
            "{kernel_kind_label} kernel declares {declared_size} push-constant bytes but its \
             shaders reflect {reflected_size}"
        )));
    }
    if declared_stages.names_no_stage() {
        return Ok(reflected_stages);
    }
    if !declared_stages.contains_every_stage_in(reflected_stages) {
        return Err(Error::GpuError(format!(
            "{kernel_kind_label} kernel declares its push constants for {} but its shaders also \
             read them from {}",
            crate::core::rhi::quote_shader_stage_names(&declared_stages.named_stages()),
            crate::core::rhi::quote_shader_stage_names(
                &reflected_stages.stages_missing_from(declared_stages)
            )
        )));
    }
    Ok(declared_stages)
}

/// A cached graphics kernel with the context's draw recorder locked: its staged
/// bindings are this holder's until it drops.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub struct GraphicsKernelHeldForOneDraw<'context> {
    kernel: Arc<crate::vulkan::rhi::VulkanGraphicsKernel>,
    input_transition_recorder_slot:
        parking_lot::MutexGuard<'context, Option<crate::vulkan::rhi::RhiCommandRecorder>>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl GraphicsKernelHeldForOneDraw<'_> {
    /// The kernel whose bindings this draw stages, borrowed no longer than
    /// the lock is held.
    pub fn kernel(&self) -> &crate::vulkan::rhi::VulkanGraphicsKernel {
        &self.kernel
    }

    /// The kernel, with the draw family's input-transition recorder — `None`
    /// until first built — borrowed alongside it.
    pub(crate) fn kernel_and_input_transition_recorder_slot(
        &mut self,
    ) -> (
        &crate::vulkan::rhi::VulkanGraphicsKernel,
        &mut Option<crate::vulkan::rhi::RhiCommandRecorder>,
    ) {
        (&self.kernel, &mut self.input_transition_recorder_slot)
    }
}

/// A cached ray-tracing kernel with the context's trace recorder locked: its
/// staged bindings are this holder's until it drops.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub struct RayTracingKernelHeldForOneTrace<'context> {
    kernel: Arc<crate::vulkan::rhi::VulkanRayTracingKernel>,
    input_transition_recorder_slot:
        parking_lot::MutexGuard<'context, Option<crate::vulkan::rhi::RhiCommandRecorder>>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl RayTracingKernelHeldForOneTrace<'_> {
    /// The kernel whose bindings this trace stages, borrowed no longer than
    /// the lock is held.
    pub fn kernel(&self) -> &crate::vulkan::rhi::VulkanRayTracingKernel {
        &self.kernel
    }

    /// The kernel, with the trace family's input-transition recorder — `None`
    /// until first built — borrowed alongside it.
    pub(crate) fn kernel_and_input_transition_recorder_slot(
        &mut self,
    ) -> (
        &crate::vulkan::rhi::VulkanRayTracingKernel,
        &mut Option<crate::vulkan::rhi::RhiCommandRecorder>,
    ) {
        (&self.kernel, &mut self.input_transition_recorder_slot)
    }
}

impl GpuContext {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn color_converter(&self, src: PixelFormat, dst: PixelFormat) -> Result<RhiColorConverter> {
        // Fast path: read lock; cache stores Arc<Inner> so we can build
        // a fresh handle via from_arc_into_raw per request.
        {
            let cache = self.color_converter_cache.read();
            if let Some(c) = cache.get(&(src, dst)) {
                return Ok(RhiColorConverter::from_arc_into_raw(Arc::clone(c)));
            }
        }
        // Slow path: build under write lock with double-check.
        let mut cache = self.color_converter_cache.write();
        if let Some(c) = cache.get(&(src, dst)) {
            return Ok(RhiColorConverter::from_arc_into_raw(Arc::clone(c)));
        }
        let vulkan_device = &self.device.inner;
        let inner = crate::vulkan::rhi::VulkanColorConverter::new(vulkan_device, src, dst)?;
        let inner_arc = Arc::new(crate::core::rhi::RhiColorConverterInner { inner });
        cache.insert((src, dst), Arc::clone(&inner_arc));
        tracing::debug!(
            rhi_op = "color_converter",
            ?src,
            ?dst,
            "GpuContext::color_converter — converter constructed"
        );
        Ok(RhiColorConverter::from_arc_into_raw(inner_arc))
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_color_converter(
        &self,
        src: PixelFormat,
        dst: PixelFormat,
    ) -> Result<RhiColorConverter> {
        let vulkan_device = &self.device.inner;
        let inner = crate::vulkan::rhi::VulkanColorConverter::new(vulkan_device, src, dst)?;
        tracing::debug!(
            rhi_op = "create_color_converter",
            ?src,
            ?dst,
            "GpuContext::create_color_converter — owned converter constructed"
        );
        Ok(RhiColorConverter::from_arc_into_raw(Arc::new(
            crate::core::rhi::RhiColorConverterInner { inner },
        )))
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_compute_kernel(
        &self,
        descriptor: &crate::core::rhi::ComputeKernelDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::VulkanComputeKernel> {
        tracing::debug!(
            rhi_op = "create_compute_kernel",
            label = descriptor.label,
            bindings = descriptor.bindings.len(),
            push_constant_size = descriptor.push_constant_size,
            "GpuContext::create_compute_kernel"
        );
        let vulkan_device = &self.device.inner;
        crate::vulkan::rhi::VulkanComputeKernel::new_caching_its_pipeline_in(
            vulkan_device,
            descriptor,
            self.pipeline_cache_directory_of_its_stream(),
        )
    }

    /// Build a [`crate::vulkan::rhi::VulkanOffscreenCompositor`] for
    /// `attachment_format`.
    ///
    /// A caller that composes every frame into its own destinations builds one
    /// here and keeps it. A caller that just wants one draw uses
    /// [`Self::compose_texture_onto_offscreen_texture`] instead, which shares
    /// a cached compositor rather than compiling a pipeline per call.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_offscreen_compositor(
        &self,
        attachment_format: crate::core::rhi::TextureFormat,
    ) -> Result<crate::vulkan::rhi::VulkanOffscreenCompositor> {
        tracing::debug!(
            rhi_op = "create_offscreen_compositor",
            ?attachment_format,
            "GpuContext::create_offscreen_compositor"
        );
        crate::vulkan::rhi::VulkanOffscreenCompositor::new(&self.device.inner, attachment_format)
    }

    /// Draw `source` onto `destination` — scaled and aspect-managed — through a
    /// compositor cached on this context for `destination`'s format.
    ///
    /// Submits and waits: `destination` comes back in
    /// `COLOR_ATTACHMENT_OPTIMAL` and `source` in `SHADER_READ_ONLY_OPTIMAL`.
    /// Concurrent callers serialize on the cached compositor's lock, which is
    /// also what makes one descriptor-ring slot enough.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn compose_texture_onto_offscreen_texture(
        &self,
        destination: &Texture,
        source: &Texture,
        source_current_layout: VulkanLayout,
        scaling: crate::vulkan::rhi::OffscreenCompositorScalingMode,
    ) -> Result<()> {
        let attachment_format = destination.format();
        let compositor = {
            let mut cache = self.offscreen_compositor_cache.lock();
            match cache.get(&attachment_format) {
                Some(cached) => Arc::clone(cached),
                None => {
                    let built = Arc::new(parking_lot::Mutex::new(
                        self.create_offscreen_compositor(attachment_format)?,
                    ));
                    cache.insert(attachment_format, Arc::clone(&built));
                    built
                }
            }
        };
        compositor.lock().compose_to_offscreen_texture(
            destination,
            source,
            source_current_layout,
            scaling,
        )
    }

    /// Create a graphics kernel from a multi-stage SPIR-V set + binding
    /// declaration + pipeline state. Graphics counterpart to
    /// [`Self::create_compute_kernel`].
    ///
    /// Reflects every stage's SPIR-V at creation time and validates that
    /// the declared bindings + push constants + stage visibility match the
    /// shaders; mismatches surface as a clear error rather than at first
    /// draw.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_graphics_kernel(
        &self,
        descriptor: &crate::core::rhi::GraphicsKernelDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::VulkanGraphicsKernel> {
        tracing::debug!(
            rhi_op = "create_graphics_kernel",
            label = descriptor.label,
            stages = descriptor.stages.len(),
            bindings = descriptor.bindings.len(),
            push_constant_size = descriptor.push_constants.size,
            descriptor_sets_in_flight = descriptor.descriptor_sets_in_flight,
            "GpuContext::create_graphics_kernel"
        );
        let vulkan_device = &self.device.inner;
        crate::vulkan::rhi::VulkanGraphicsKernel::new_caching_its_pipeline_in(
            vulkan_device,
            descriptor,
            self.pipeline_cache_directory_of_its_stream(),
        )
    }

    /// Create a ray-tracing kernel from shader stages, shader-group
    /// layout, binding declaration, and push-constant range. Mirror of
    /// [`Self::create_compute_kernel`] / [`Self::create_graphics_kernel`]
    /// for `VkRayTracingPipelineKHR`-backed work.
    ///
    /// Validates every stage's SPIR-V against the declared bindings +
    /// push-constants at creation time, builds the pipeline, fetches
    /// shader-group handles, lays out the shader-binding table, and
    /// returns a kernel ready for `set_*` + `trace_rays` dispatch.
    /// Returns a clean error when the device lacks RT support.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_ray_tracing_kernel(
        &self,
        descriptor: &crate::core::rhi::RayTracingKernelDescriptor<'_>,
    ) -> Result<crate::vulkan::rhi::VulkanRayTracingKernel> {
        tracing::debug!(
            rhi_op = "create_ray_tracing_kernel",
            label = descriptor.label,
            stages = descriptor.stages.len(),
            groups = descriptor.groups.len(),
            bindings = descriptor.bindings.len(),
            push_constant_size = descriptor.push_constants.size,
            max_recursion_depth = descriptor.max_recursion_depth,
            "GpuContext::create_ray_tracing_kernel"
        );
        let vulkan_device = &self.device.inner;
        crate::vulkan::rhi::VulkanRayTracingKernel::new(vulkan_device, descriptor)
    }

    // =========================================================================
    // Compute kernels for the escalate ops — always present, never installed
    // =========================================================================

    /// Compile Slang kernel source to SPIR-V, reusing what an identical earlier
    /// request compiled.
    ///
    /// `label` is what the compiler's diagnostics name the source as, so it is
    /// the prefix an author sees on a syntax error.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn compile_slang_shader_source_to_spirv(
        &self,
        source: &str,
        stage: crate::core::rhi::ShaderCompilationTargetStage,
        entry_point: &str,
        label: &str,
    ) -> Result<Arc<[u8]>> {
        self.slang_shader_source_compiler
            .compile_or_reuse(source, stage, entry_point, label)
    }

    /// Build a compute kernel for a caller that named it by SPIR-V, reusing an
    /// identical one if this context already built it.
    ///
    /// The cache key covers everything that changes the compiled output. For a
    /// pre-compiled blob that is the blob itself plus the declared
    /// push-constant size — the bytes already fix stage, entry point and target
    /// environment. A Slang source contract adds the compiler's own version to
    /// the key, because then the engine is what produces the bytes.
    ///
    /// Returns the cache key as the kernel id: a caller re-registering the same
    /// kernel gets the same id back, and pays no compilation for it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_or_reuse_compute_kernel(
        &self,
        spv: &[u8],
        push_constant_size: u32,
        declared_bindings: &[crate::core::rhi::ComputeBindingDeclaration],
        entry_point: &str,
    ) -> Result<(String, Arc<crate::vulkan::rhi::VulkanComputeKernel>)> {
        let kernel_id = compute_kernel_cache_key(spv, push_constant_size, entry_point);
        let cached_kernel = self
            .compute_kernel_cache
            .lock()
            .get(&kernel_id)
            .map(Arc::clone);
        if let Some(cached) = cached_kernel {
            // The declaration is checked on the hit path too: the cache key
            // covers the blob, not the caller's assertion, and a wrong
            // assertion must refuse identically whether or not somebody
            // registered this blob first.
            crate::core::rhi::reconcile_compute_binding_declarations(
                declared_bindings,
                &cached.bindings(),
            )?;
            tracing::debug!(
                rhi_op = "create_or_reuse_compute_kernel",
                kernel_id,
                "GpuContext::create_or_reuse_compute_kernel — cache hit"
            );
            return Ok((kernel_id, Arc::clone(&cached)));
        }

        // Reflection is the source of truth for the binding shape; a caller's
        // declaration is checked against it rather than replacing it.
        let (reflected, reflected_push_size) = crate::core::rhi::derive_bindings_from_spirv(spv)?;
        crate::core::rhi::reconcile_compute_binding_declarations(declared_bindings, &reflected)?;
        if reflected_push_size != push_constant_size {
            return Err(Error::GpuError(format!(
                "compute kernel declares {push_constant_size} push-constant bytes but its \
                 SPIR-V reflects {reflected_push_size}"
            )));
        }

        let kernel = Arc::new(self.create_compute_kernel(
            &crate::core::rhi::ComputeKernelDescriptor {
                entry_point,
                label: "escalate-compute-kernel",
                spv,
                bindings: &reflected,
                push_constant_size,
                specialization_constants: &[],
            },
        )?);

        Ok((
            kernel_id.clone(),
            Arc::clone(
                self.compute_kernel_cache
                    .lock()
                    .entry(kernel_id)
                    .or_insert(kernel),
            ),
        ))
    }

    /// Look up a compute kernel a prior `create_or_reuse_compute_kernel`
    /// returned.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn compute_kernel_by_id(
        &self,
        kernel_id: &str,
    ) -> Option<Arc<crate::vulkan::rhi::VulkanComputeKernel>> {
        self.compute_kernel_cache
            .lock()
            .get(kernel_id)
            .map(Arc::clone)
    }

    /// Build a graphics kernel for the `register_graphics_kernel` escalate op,
    /// or hand back one an identical earlier registration already built.
    ///
    /// The twin of [`Self::create_or_reuse_compute_kernel`], and the same
    /// contract: reflection is the source of truth for the binding shape, the
    /// caller's declaration is checked against it by name rather than
    /// replacing it, and the cache key is what the caller gets back as the
    /// kernel id.
    ///
    /// `label` names the pipeline in RHI errors and validation-layer messages,
    /// and is deliberately outside the cache key — two registrations differing
    /// only in label are one pipeline, and the first one's label is the one the
    /// driver keeps.
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
        use crate::core::rhi::GraphicsShaderStageFlags;

        // Both stages are always present — `VulkanGraphicsKernel::new` refuses
        // anything else — so every graphics kernel is built from both.
        let stages_the_kernel_was_built_from = GraphicsShaderStageFlags::VERTEX_FRAGMENT;
        let kernel_id = graphics_kernel_cache_key(
            stages,
            declared_push_constants,
            pipeline_state,
            descriptor_sets_in_flight,
        );
        let cached_kernel = self
            .graphics_kernel_cache
            .lock()
            .get(&kernel_id)
            .map(Arc::clone);
        if let Some(cached) = cached_kernel {
            // The declaration is checked on the hit path too: the cache key
            // covers the shaders and the pipeline, not the caller's assertion,
            // and a wrong assertion must refuse identically whether or not
            // somebody registered this kernel first.
            crate::core::rhi::reconcile_graphics_binding_declarations(
                declared_bindings,
                &cached.bindings(),
                stages_the_kernel_was_built_from,
            )?;
            tracing::debug!(
                rhi_op = "create_or_reuse_graphics_kernel",
                kernel_id,
                "GpuContext::create_or_reuse_graphics_kernel — cache hit"
            );
            return Ok((kernel_id, Arc::clone(&cached)));
        }

        let (reflected, reflected_push_constants) =
            crate::core::rhi::derive_bindings_from_spirv_multistage(stages)?;
        crate::core::rhi::reconcile_graphics_binding_declarations(
            declared_bindings,
            &reflected,
            stages_the_kernel_was_built_from,
        )?;
        let push_constants = crate::core::rhi::GraphicsPushConstants {
            size: reflected_push_constants.size,
            stages: reconciled_push_constant_stages(
                "graphics",
                declared_push_constants.size,
                declared_push_constants.stages,
                reflected_push_constants.size,
                reflected_push_constants.stages,
            )?,
        };

        let kernel = Arc::new(self.create_graphics_kernel(
            &crate::core::rhi::GraphicsKernelDescriptor {
                label,
                stages,
                bindings: &reflected,
                push_constants,
                pipeline_state: pipeline_state.clone(),
                descriptor_sets_in_flight,
            },
        )?);

        Ok((
            kernel_id.clone(),
            Arc::clone(
                self.graphics_kernel_cache
                    .lock()
                    .entry(kernel_id)
                    .or_insert(kernel),
            ),
        ))
    }

    /// Look up a graphics kernel a prior `create_or_reuse_graphics_kernel`
    /// returned, holding the draw recorder for as long as the caller keeps it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn graphics_kernel_by_id_held_for_one_draw(
        &self,
        kernel_id: &str,
    ) -> Option<GraphicsKernelHeldForOneDraw<'_>> {
        let input_transition_recorder_slot = self.graphics_draw_input_transition_recorder.lock();
        let kernel = self
            .graphics_kernel_cache
            .lock()
            .get(kernel_id)
            .map(Arc::clone)?;
        Some(GraphicsKernelHeldForOneDraw {
            kernel,
            input_transition_recorder_slot,
        })
    }

    /// Build a ray-tracing kernel for the `register_ray_tracing_kernel`
    /// escalate op, or hand back one an identical earlier registration already
    /// built.
    ///
    /// The twin of [`Self::create_or_reuse_compute_kernel`]. Unlike graphics,
    /// a ray-tracing kernel's stage set varies per kernel, so the stages it was
    /// actually built from are what a declaration naming a stage is checked
    /// against.
    ///
    /// `label` names the pipeline in RHI errors and validation-layer messages,
    /// and is deliberately outside the cache key — two registrations differing
    /// only in label are one pipeline, and the first one's label is the one the
    /// driver keeps.
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
        let stages_the_kernel_was_built_from =
            crate::core::rhi::ray_tracing_stages_covered_by(stages);
        let kernel_id = ray_tracing_kernel_cache_key(
            stages,
            groups,
            declared_push_constants,
            max_recursion_depth,
        );
        let cached_kernel = self
            .ray_tracing_kernel_cache
            .lock()
            .get(&kernel_id)
            .map(Arc::clone);
        if let Some(cached) = cached_kernel {
            crate::core::rhi::reconcile_ray_tracing_binding_declarations(
                declared_bindings,
                &cached.bindings(),
                stages_the_kernel_was_built_from,
            )?;
            tracing::debug!(
                rhi_op = "create_or_reuse_ray_tracing_kernel",
                kernel_id,
                "GpuContext::create_or_reuse_ray_tracing_kernel — cache hit"
            );
            return Ok((kernel_id, Arc::clone(&cached)));
        }

        let (reflected, reflected_push_constants) =
            crate::core::rhi::derive_ray_tracing_bindings_from_spirv_multistage(stages)?;
        crate::core::rhi::reconcile_ray_tracing_binding_declarations(
            declared_bindings,
            &reflected,
            stages_the_kernel_was_built_from,
        )?;
        let push_constants = crate::core::rhi::RayTracingPushConstants {
            size: reflected_push_constants.size,
            stages: reconciled_push_constant_stages(
                "ray-tracing",
                declared_push_constants.size,
                declared_push_constants.stages,
                reflected_push_constants.size,
                reflected_push_constants.stages,
            )?,
        };

        let kernel = Arc::new(self.create_ray_tracing_kernel(
            &crate::core::rhi::RayTracingKernelDescriptor {
                label,
                stages,
                groups,
                bindings: &reflected,
                push_constants,
                max_recursion_depth,
            },
        )?);

        Ok((
            kernel_id.clone(),
            Arc::clone(
                self.ray_tracing_kernel_cache
                    .lock()
                    .entry(kernel_id)
                    .or_insert(kernel),
            ),
        ))
    }

    /// Look up a ray-tracing kernel a prior
    /// `create_or_reuse_ray_tracing_kernel` returned, holding the trace recorder
    /// for as long as the caller keeps it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn ray_tracing_kernel_by_id_held_for_one_trace(
        &self,
        kernel_id: &str,
    ) -> Option<RayTracingKernelHeldForOneTrace<'_>> {
        let input_transition_recorder_slot =
            self.ray_tracing_trace_input_transition_recorder.lock();
        let kernel = self
            .ray_tracing_kernel_cache
            .lock()
            .get(kernel_id)
            .map(Arc::clone)?;
        Some(RayTracingKernelHeldForOneTrace {
            kernel,
            input_transition_recorder_slot,
        })
    }
}
