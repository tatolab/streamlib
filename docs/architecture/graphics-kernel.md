# Graphics kernels in the RHI

streamlib's RHI exposes one canonical abstraction for GPU graphics-pipeline
work: `VulkanGraphicsKernel` (Linux) plus the public binding-shape and
fixed-function-state types in `core::rhi` (`GraphicsKernelDescriptor`,
`GraphicsBindingSpec`, `GraphicsPipelineState`, …). **Every new graphics
service uses this abstraction — do not hand-roll a `VkPipeline`,
descriptor set, descriptor pool, command buffer, or pipeline layout for
a graphics shader.**

This is the graphics-side counterpart to
[`compute-kernel.md`](compute-kernel.md). Same engine-model rule: the
RHI is the single gateway to the GPU, and the kernel abstraction is the
single gateway for graphics dispatch.

## What the abstraction does for you

Given a multi-stage SPIR-V set (vertex + fragment) and a small typed
declaration, the kernel:

1. **Reflects every stage's SPIR-V** at construction time via
   [`rspirv-reflect`](https://docs.rs/rspirv-reflect), merges the
   per-stage descriptor sets, and validates that:
   - Declared `bindings` match the merged shader declaration (kind +
     stage visibility).
   - Push-constant size and stage visibility match.
   - Only descriptor set 0 is used (multi-set is out of scope).
   - Stage classification matches the SPIR-V (a Vertex stage's blob
     must declare `EntryPoint Vertex`, etc.).

   Mismatches surface as a `Result::Err` at *kernel creation*, not as
   undefined GPU behavior at first draw.

2. **Builds the descriptor-set layout, descriptor pool, descriptor-set
   ring, pipeline layout, graphics pipeline (with on-disk pipeline
   cache), and a default linear-clamp sampler.** None of this is your
   code anymore.

3. **Stages bindings as data** through `set_sampled_texture`,
   `set_storage_buffer`, `set_uniform_buffer`, `set_storage_image`,
   `set_push_constants`, `set_vertex_buffer`, `set_index_buffer`. Each
   setter takes RHI-level types (`StreamTexture`, `RhiPixelBuffer`,
   `&[u8]`).

4. **Records bind + push + draw into the caller's command buffer.**
   Render-pass scope (`vkCmdBeginRendering` / `vkCmdEndRendering`)
   stays caller-side because the same pass typically dispatches
   multiple kernels — the kernel only owns the shape its handle
   represents.

## Render-loop shape (canonical)

```rust
// Per frame, caller indexes into the descriptor-set ring:
let frame = current_frame % MAX_FRAMES_IN_FLIGHT;

kernel.set_sampled_texture(frame, 0, &texture)?;
kernel.set_push_constants_value(frame, &push)?;

device.cmd_begin_rendering(cmd, &rendering_info);  // caller-managed
kernel.cmd_bind_and_draw(cmd, frame, &DrawCall {
    vertex_count: 3, instance_count: 1,
    first_vertex: 0, first_instance: 0,
    viewport: Some(Viewport::full(width, height)),
    scissor: Some(ScissorRect::full(width, height)),
})?;
device.cmd_end_rendering(cmd);                     // caller-managed
```

The descriptor-set ring is sized by `descriptor_sets_in_flight` at
construction; render-loop callers pass `frame_index ∈ [0, ring_depth)`.
Caller is responsible for ensuring slot N isn't referenced by an
in-flight command buffer before updating it (typically via a per-frame
fence or timeline semaphore wait — the same pattern used to gate
command-buffer reuse).

## Offscreen / one-shot rendering

For unit tests, ad-hoc renderers, and procedural-texture generation
the kernel ships an `offscreen_render` convenience that owns its own
command buffer + fence + render-pass scope:

```rust
kernel.offscreen_render(
    /* frame_index */ 0,
    &[OffscreenColorTarget {
        texture: &output_texture,
        clear_color: Some([0.0, 0.0, 0.0, 1.0]),
    }],
    /* extent */ (width, height),
    OffscreenDraw::Draw(DrawCall { vertex_count: 3, ..default() }),
)?;
```

Submits + waits before returning. Single in-flight per kernel
(serialized by the kernel's owned fence, mirroring `VulkanComputeKernel`'s
serial-dispatch contract).

## Adding a new graphics kernel — the recipe

1. **Write the GLSL** in
   `libs/streamlib/src/vulkan/rhi/shaders/<name>.{vert,frag}`. Use
   descriptor set 0; multi-set kernels are not supported. Keep
   binding indices in declaration order.

2. **Wire the shaders into `build.rs`.** Append vertex + fragment
   entries to the `shaders` array in `libs/streamlib/build.rs`. The
   build script invokes `glslc -O` per-stage and writes SPIR-V into
   `OUT_DIR`. SPIR-V is read at compile time via
   `include_bytes!(concat!(env!("OUT_DIR"), "/<name>.<stage>.spv"))`.
   Do not commit `.spv` files to the source tree — they're build
   artifacts.

3. **Declare the binding shape and pipeline state as data.** Match
   the shaders' bindings exactly:

   ```rust
   const BINDINGS: &[GraphicsBindingSpec] = &[
       GraphicsBindingSpec::sampled_texture(0, GraphicsShaderStageFlags::FRAGMENT),
   ];

   let pipeline_state = GraphicsPipelineState {
       topology: PrimitiveTopology::TriangleList,
       vertex_input: VertexInputState::None, // gl_VertexIndex
       rasterization: RasterizationState::default(),
       multisample: MultisampleState::default(),
       depth_stencil: DepthStencilState::Disabled,
       color_blend: ColorBlendState::Disabled {
           color_write_mask: ColorWriteMask::RGBA,
       },
       attachment_formats: AttachmentFormats::color_only(TextureFormat::Bgra8Unorm),
       dynamic_state: GraphicsDynamicState::ViewportScissor,
   };
   ```

4. **Create the kernel via `GpuContext::create_graphics_kernel`** at
   setup time and store the `Arc<VulkanGraphicsKernel>`:

   ```rust
   let kernel = gpu_ctx.create_graphics_kernel(&GraphicsKernelDescriptor {
       label: "my_kernel",
       stages: &[
           GraphicsStage::vertex(include_bytes!(concat!(env!("OUT_DIR"), "/my_kernel.vert.spv"))),
           GraphicsStage::fragment(include_bytes!(concat!(env!("OUT_DIR"), "/my_kernel.frag.spv"))),
       ],
       bindings: BINDINGS,
       push_constants: GraphicsPushConstants {
           size: std::mem::size_of::<MyPushConstants>() as u32,
           stages: GraphicsShaderStageFlags::FRAGMENT,
       },
       pipeline_state,
       descriptor_sets_in_flight: MAX_FRAMES_IN_FLIGHT as u32,
   })?;
   ```

5. **Drive draws from your render loop** — see the canonical shape
   above. The caller manages render-pass scope (`cmd_begin_rendering`
   / `cmd_end_rendering`) and the per-frame fence/timeline that
   gates descriptor-set reuse.

6. **Test the shape.** Reflection-rejection tests run on host
   architecture (no GPU required); pipeline-construction tests need
   a Vulkan device. See `vulkan_graphics_kernel.rs::tests` for the
   parameterized pattern.

## What's deliberately not covered (yet)

These are out of scope for the v1 surface and are tracked as
follow-ups under the **Graphics Kernel Buildout** milestone:

- **Optional shader stages** — geometry, tessellation control /
  evaluation, mesh, task. The `GraphicsShaderStage` enum is open;
  adding a stage variant is mostly enum + flag plumbing. File a
  consumer first, then extend.
- **Indirect draw** (`vkCmdDrawIndirect` / `vkCmdDrawIndexedIndirect`).
  GPU-driven rendering pattern; add when first consumer needs it.
- **Multi-attachment color blending (MRT).** The
  `ColorBlendState` enum today carries a single attachment shape;
  multi-attachment compositing is the next extension.
- **MSAA / sample count > 1.** `MultisampleState::samples` is fixed
  at 1; the kernel rejects other values up front.
- **Custom samplers.** `set_sampled_texture` uses a default
  linear-clamp sampler created on first use. If a kernel needs
  anisotropic / nearest / different addressing, extend the
  abstraction; do not work around it.
- **Multi-set kernels.** Set 0 only — same constraint as
  `VulkanComputeKernel`.
- **Bindless / descriptor indexing**
  (`VK_EXT_descriptor_indexing`, `VK_EXT_descriptor_buffer`). The
  public API is shaped so the backend can migrate later without
  breaking callers; the descriptor pool is an internal detail today.
- **Depth attachment allocation.** Depth `TextureFormat` variants
  and `StreamTexture` allocation for depth attachments are not yet
  in `streamlib-consumer-rhi` — depth-stencil pipeline-creation is
  validated by unit test, but depth-correctness rendering tests are
  pending depth-format support.

## Why this shape

Production realtime engines (Granite, Unreal RDG, bgfx, wgpu) all
converge on the same pattern: typed-struct or slot-based binding
API backed by shader reflection, with the pipeline as a long-lived
object the renderer binds into a caller-owned command buffer.
streamlib's compute kernel chose the slot-based API for the same
reasons; the graphics kernel mirrors that shape so callers see one
consistent RHI dispatch model.

The descriptor-set ring (vs. compute kernel's single set + serial
dispatch) is the single asymmetry between the two kernels — it
exists because graphics dispatches are integrated into a render
loop with multiple frames in flight, where compute dispatches in
streamlib are typically synchronous (format conversion, blending,
etc.). The asymmetry is documented at trait birth rather than
papered over.

The relevant trade-off discussion lives on issue
[#609](https://github.com/tatolab/streamlib/issues/609).
