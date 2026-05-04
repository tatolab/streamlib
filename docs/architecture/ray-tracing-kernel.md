# Ray-tracing kernels in the RHI

streamlib's RHI exposes one canonical abstraction for GPU ray-tracing
work: `VulkanRayTracingKernel` (Linux) plus the public stage / group /
binding types in `core::rhi` (`RayTracingKernelDescriptor`,
`RayTracingStage`, `RayTracingShaderGroup`, `RayTracingBindingSpec`,
…) and the build-time-required `VulkanAccelerationStructure` primitive
that backs every TLAS instance. **Every new ray-tracing service uses
this abstraction — do not hand-roll a `VkRayTracingPipelineKHR`,
shader-binding-table, descriptor set, descriptor pool, command buffer,
or pipeline layout.**

This is the third kernel in the same engine-model lineage as
[`compute-kernel.md`](compute-kernel.md) and
[`graphics-kernel.md`](graphics-kernel.md). Same rule: the RHI is the
single gateway to the GPU, and the kernel abstraction is the single
gateway for ray-tracing dispatch.

Ray-tracing pipelines are their own Vulkan object kind —
`VkRayTracingPipelineKHR` rather than the graphics or compute
`VkPipeline` flavor — with their own shader stages (RayGen / Miss /
ClosestHit / AnyHit / Intersection / Callable) and a shader-binding-
table requirement that has no equivalent in the graphics or compute
paths. They cannot share `VulkanGraphicsKernel`'s shape; the kernel
records `vkCmdTraceRaysKHR` with four `VkStridedDeviceAddressRegionKHR`
SBT regions instead of `vkCmdDraw` / `vkCmdDispatch`.

## What the abstraction does for you

Given a list of shader stages, a shader-group layout, and a small typed
binding declaration, the kernel:

1. **Reflects every stage's SPIR-V** at construction time via
   [`rspirv-reflect`](https://docs.rs/rspirv-reflect), merges the
   per-stage descriptor sets, and validates that:
   - Declared `bindings` match the merged shader declaration (kind +
     stage visibility, including the new
     `RayTracingBindingKind::AccelerationStructure` variant).
   - Push-constant size matches the largest declared push-constant
     range across all stages.
   - Only descriptor set 0 is used (multi-set is out of scope).
   - Shader-group layout is internally consistent — `General` groups
     reference RayGen / Miss / Callable stages, hit groups reference
     ClosestHit / AnyHit / Intersection stages, etc.

   Mismatches surface as `Result::Err` at *kernel creation*, not as
   undefined GPU behavior at first trace.

2. **Builds the descriptor-set layout, descriptor pool, descriptor
   set, pipeline layout, ray-tracing pipeline, and shader-binding
   table.** None of this is your code anymore. The SBT is laid out
   per `VkPhysicalDeviceRayTracingPipelinePropertiesKHR`'s
   `shaderGroupHandleSize` / `shaderGroupHandleAlignment` /
   `shaderGroupBaseAlignment` — handles for each region (raygen /
   miss / hit / callable) are packed at the right stride and the
   region base addresses are aligned to `shaderGroupBaseAlignment`.

3. **Stages bindings as data** through `set_storage_buffer`,
   `set_uniform_buffer`, `set_sampled_texture`, `set_storage_image`,
   `set_acceleration_structure`, `set_push_constants`. Each setter
   takes RHI-level types (`RhiPixelBuffer`, `StreamTexture`,
   `Arc<VulkanAccelerationStructure>`, `&[u8]`).

4. **Records bind + push + trace + waits** in `trace_rays(width,
   height, depth)`. The fence is pre-signaled so the first dispatch
   doesn't block; consecutive `trace_rays` calls against the same
   kernel are serial (one in flight at a time), matching the compute
   kernel's shape. The four SBT regions are passed to
   `vkCmdTraceRaysKHR` from cached `VkStridedDeviceAddressRegionKHR`
   values; empty regions correctly carry `device_address = 0` and
   `stride = 0` per spec.

## Adding a new ray-tracing kernel — the recipe

1. **Write the GLSL** in `libs/streamlib/src/vulkan/rhi/shaders/<name>.{rgen,rmiss,rchit,rahit,rint,rcall}`.
   Use descriptor set 0; multi-set kernels are not supported. Each
   shader needs `#version 460` and `#extension GL_EXT_ray_tracing :
   require`. Acceleration-structure bindings declare as
   `accelerationStructureEXT`; output images as `image2D` with a
   format qualifier (`rgba8`, etc.).

2. **Wire the shaders into `build.rs`.** Add an entry per stage to
   the `rt_shaders` array in `libs/streamlib/build.rs`. RT shaders
   are compiled with `--target-env=vulkan1.2 --target-spv=spv1.4`
   so `SPV_KHR_ray_tracing` opcodes are available; the helper
   handles the per-stage `-fshader-stage=rgen|rmiss|rchit|...`
   flag. SPIR-V is read via
   `include_bytes!(concat!(env!("OUT_DIR"), "/<name>.spv"))`.

3. **Declare stages, groups, bindings, and push constants as data.**
   Stage indices index into `stages: &[RayTracingStage]`; groups
   reference those indices via the `RayTracingShaderGroup` enum:

   ```rust
   const STAGES: &[RayTracingStage] = &[
       RayTracingStage::ray_gen(RGEN_SPV),
       RayTracingStage::miss(RMISS_SPV),
       RayTracingStage::closest_hit(RCHIT_SPV),
   ];
   const GROUPS: &[RayTracingShaderGroup] = &[
       RayTracingShaderGroup::General { general: 0 },
       RayTracingShaderGroup::General { general: 1 },
       RayTracingShaderGroup::TrianglesHit {
           closest_hit: Some(2), any_hit: None,
       },
   ];
   const BINDINGS: &[RayTracingBindingSpec] = &[
       RayTracingBindingSpec::acceleration_structure(0, RayTracingShaderStageFlags::RAYGEN),
       RayTracingBindingSpec::storage_image(1, RayTracingShaderStageFlags::RAYGEN),
   ];
   ```

4. **Build the acceleration structures.** Triangle BLASes come from
   `GpuContext::build_triangles_blas(label, vertices, indices)` —
   vertices are interleaved `[x, y, z, x, y, z, …]` (R32G32B32_SFLOAT,
   stride 12), indices are u32 triples per triangle. TLASes come from
   `GpuContext::build_tlas(label, &[TlasInstanceDesc])`. Use
   `TlasInstanceDesc::identity(blas)` for the common case.

5. **Create the kernel via `GpuContext::create_ray_tracing_kernel`**
   at setup time and store the `Arc<VulkanRayTracingKernel>` on your
   processor:

   ```rust
   let kernel = gpu_ctx.create_ray_tracing_kernel(
       &RayTracingKernelDescriptor {
           label: "my_kernel",
           stages: STAGES,
           groups: GROUPS,
           bindings: BINDINGS,
           push_constants: RayTracingPushConstants {
               size: std::mem::size_of::<MyPushConstants>() as u32,
               stages: RayTracingShaderStageFlags::RAYGEN,
           },
           max_recursion_depth: 1,
       },
   )?;
   ```

6. **Dispatch from your hot path.** Bind every declared binding,
   then trace:

   ```rust
   self.kernel.set_acceleration_structure(0, &tlas)?;
   self.kernel.set_storage_image(1, &output_tex)?;
   self.kernel.set_push_constants_value(&MyPushConstants { … })?;
   self.kernel.trace_rays(width, height, 1)?;
   ```

   The output image must be in `VK_IMAGE_LAYOUT_GENERAL` when the
   kernel records — issue an UNDEFINED → GENERAL barrier on the
   storage image before the first trace (mirrors what compute
   kernels with storage-image outputs need).

7. **Test the shape.** Add a parameterized test alongside
   `vulkan_ray_tracing_kernel.rs::tests`. Use the
   `try_ray_tracing_device` helper that skips when no Vulkan device
   is available *or* when the device does not advertise
   `VK_KHR_ray_tracing_pipeline`. The default trace-rays smoke test
   (a 1-triangle BLAS + 1-instance TLAS) is a good template.

## What's deliberately not covered

- **Acceleration-structure compaction, refit, BVH rebuild.** The v1
  `VulkanAccelerationStructure` lifecycle is build-once / use /
  destroy. When a real consumer needs in-place updates or
  compaction, lift those methods onto the same primitive — don't
  add a parallel AS type.
- **Procedural geometry (AABBs).** The
  `RayTracingShaderGroup::ProceduralHit` shape is in place and the
  intersection-shader stage is supported by the kernel, but
  `VulkanAccelerationStructure::build_triangles_blas` is the only
  BLAS constructor today. Add an AABB BLAS constructor when the
  first procedural-hit consumer arrives.
- **Indirect trace-rays (`vkCmdTraceRaysIndirect2KHR`).** Add when
  the first kernel needs it.
- **Multi-set kernels.** Set 0 only. If you need set-0 = per-frame
  and set-1 = per-batch, push it now and we'll extend the
  abstraction.
- **Bindless / descriptor indexing.** The public API is shaped so
  the backend can migrate later without breaking callers — the
  descriptor pool is an internal detail today.
- **Concurrent traces against the same kernel.** Each kernel has
  one fence and one descriptor set; consecutive `trace_rays` calls
  block on the prior one. For parallel dispatch, hold one kernel
  handle per in-flight slot.
- **Ray pipelines compiled from libraries (`VK_KHR_pipeline_library`
  with stitched stages).** The dependency extension is enabled so
  the pipeline create call is well-formed, but the kernel builds
  every pipeline as monolithic — extend when the first consumer
  actually uses libraries.
- **Subprocess support.** The kernel is host-only; subprocess
  customers (Python / Deno cdylibs) escalate via IPC, mirroring the
  compute / graphics escalate ops. The follow-up issue captures
  the IPC surface.

## Why this shape

Production realtime engines (Granite, Unreal RDG, NVIDIA OptiX-like
APIs, bgfx, wgpu's RT extensions) converge on the same answer for RT:
typed-stage list + typed-group list + reflection-validated bindings,
backed by SBT machinery the engine owns. The kernel surfaces only the
slot-based binding API and the four-region trace dispatch; SBT
alignment, handle fetch, and pipeline build live below the public
API.

The relevant trade-off discussion lives on issue
[#610](https://github.com/tatolab/streamlib/issues/610).
