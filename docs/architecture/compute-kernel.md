# Compute kernels in the RHI

streamlib's RHI exposes one canonical abstraction for GPU compute work:
`VulkanComputeKernel` (Linux) plus the public binding-shape types in
`core::rhi` (`ComputeKernelDescriptor`, `ComputeBindingSpec`,
`ComputeBindingKind`). **Every new compute service uses this abstraction —
do not hand-roll a descriptor set, descriptor pool, command buffer, fence,
or pipeline layout for a kernel.**

For graphics-pipeline (vertex + fragment) work, see the sibling
[graphics-kernel.md](graphics-kernel.md). For ray-tracing-pipeline work
(`VkRayTracingPipelineKHR`), see [ray-tracing-kernel.md](ray-tracing-kernel.md).
All three kernels share the SPIR-V-reflection-validated, descriptor-managed,
pipeline-cached shape; the asymmetry is that compute and ray-tracing own
their own command buffer + fence (serial dispatch) while graphics records
into a caller-owned command buffer with a descriptor-set ring sized to
frames-in-flight.

This is engine-model territory: the RHI is the single gateway to the GPU,
and the kernel abstraction is the single gateway for compute dispatch.
Adding a third shape every time a new kernel arrives is the failure mode
this doc exists to prevent.

## What the abstraction does for you

Given a SPIR-V blob and a small typed declaration, the kernel:

1. **Reflects the SPIR-V** at construction time via
   [`rspirv-reflect`](https://docs.rs/rspirv-reflect) and validates that
   your declared bindings match the shader's. Mismatches (wrong kind,
   missing binding, extra binding, wrong push-constant size) become a
   `Result::Err` at *kernel creation*, not a corrupted GPU dispatch.
2. **Builds the descriptor-set layout, descriptor pool, descriptor set,
   pipeline layout, compute pipeline, command pool, command buffer, and
   fence.** None of this is your code anymore.
3. **Stages bindings as data** through `set_storage_buffer`,
   `set_uniform_buffer`, `set_sampled_texture`, `set_storage_image`,
   and `set_push_constants`. Each setter takes RHI-level types
   (`RhiPixelBuffer`, `StreamTexture`, `&[u8]`).
4. **Flushes + dispatches + waits** in `dispatch(x, y, z)`. The fence is
   pre-signaled so the first dispatch doesn't block, and consecutive
   dispatches against the same kernel are serial (one in flight at a time).

## Adding a new compute kernel — the recipe

1. **Write the GLSL** in `libs/streamlib/src/vulkan/rhi/shaders/<name>.comp`.
   Use descriptor set 0; multi-set kernels are not supported. Keep
   binding indices in declaration order.

2. **Wire the shader into `build.rs`.** Append an entry to the `shaders`
   array in `libs/streamlib/build.rs`. The build script invokes
   `glslc -O` and writes the SPIR-V into `OUT_DIR`. SPIR-V is read at
   compile time via
   `include_bytes!(concat!(env!("OUT_DIR"), "/<name>.spv"))`. Do not
   commit `.spv` files to the source tree — they're build artifacts.

3. **Declare the binding shape as data.** Match the shader's bindings
   exactly:

   ```rust
   const BINDINGS: &[ComputeBindingSpec] = &[
       ComputeBindingSpec::storage_buffer(0),  // input
       ComputeBindingSpec::storage_image(1),   // output
   ];
   ```

4. **Create the kernel via `GpuContext::create_compute_kernel`** at setup
   time and store the `Arc<VulkanComputeKernel>` on your processor:

   ```rust
   let kernel = gpu_ctx.create_compute_kernel(&ComputeKernelDescriptor {
       label: "my_kernel",
       spv: include_bytes!(concat!(env!("OUT_DIR"), "/my_kernel.spv")),
       bindings: BINDINGS,
       push_constant_size: std::mem::size_of::<MyPushConstants>() as u32,
   })?;
   ```

5. **Dispatch from your hot path.** No GpuContext access required; the
   kernel is the dispatch primitive:

   ```rust
   self.kernel.set_storage_buffer(0, &input)?;
   self.kernel.set_storage_image(1, &output_tex)?;
   self.kernel.set_push_constants_value(&MyPushConstants { … })?;
   self.kernel.dispatch(group_x, group_y, group_z)?;
   ```

6. **Test the shape.** Add a parameterized test alongside the existing
   ones in `vulkan_compute_kernel.rs::tests` or write a focused test for
   the kernel's specific math (CPU reference vs GPU output is the
   pattern — see `nv12_full_range_to_bgra_matches_cpu_reference` in
   `vulkan_format_converter.rs`).

## What's deliberately not covered

- **Multi-set kernels.** Set 0 only. If you need set-0 = per-frame and
  set-1 = per-batch, push it now and we'll extend the abstraction.
- **Bindless / descriptor indexing** (`VK_EXT_descriptor_indexing`,
  `VK_EXT_descriptor_buffer`). The public API is shaped so the backend
  can migrate later without breaking callers — the descriptor pool is
  an internal detail today.
- **Concurrent dispatches against the same kernel.** Each kernel has one
  fence and one descriptor set; consecutive `dispatch()` calls block on
  the prior one. For parallel dispatch, hold one kernel handle per
  in-flight slot.
- **Custom samplers.** `set_sampled_texture` uses a default linear-clamp
  sampler created on first use. If a kernel needs anisotropic / nearest
  / different addressing, extend the abstraction; do not work around it.
- **Indirect dispatch** (`vkCmdDispatchIndirect`). Add when first kernel
  needs it.

## Why this shape

Production realtime engines (Granite, Unreal RDG, Slang, bgfx) all
converge on the same answer: typed-struct or slot-based binding API
backed by shader reflection. We picked the slot-based API because it's
the smallest surface that solves the problem and matches Granite's shape
exactly — the engine that's closest to streamlib's scale and goals. The
SPIR-V reflection layer lifts the abstraction above "user must mirror
the shader's binding layout in code" (the wgpu/raw-Vulkan model) into
"the shader is the source of truth and the kernel refuses to load if
you got the layout wrong." That's the engine-grade invariant.

The relevant trade-off discussion lives on issue
[#480](https://github.com/tatolab/streamlib/issues/480).
