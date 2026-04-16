# vulkanalia: empty slices require explicit type casts

## Symptom

Cryptic Rust compiler error mentioning `Cast` trait bounds when passing
`&[]` to vulkanalia Vulkan wrapper methods:

```
error[E0283]: type annotations needed
  --> camera.rs:1340
   |
   |     device.cmd_pipeline_barrier(cmd, ..., &[], &[], &[barrier]);
   |                                           ^^^
   |
   = note: cannot satisfy `_: Cast`
```

The error appears on any vulkanalia method that accepts `&[impl Cast<Target=T>]`
when one or more arguments is an empty slice. Common triggers:
- `cmd_pipeline_barrier(cmd, src_stage, dst_stage, deps, memory_barriers, buffer_barriers, image_barriers)`
- `cmd_copy_buffer_to_image(cmd, src, dst, layout, &[region])` (fine — non-empty)
  but passing `&[]` for any other barrier parameter in the same call fails

## Root cause

vulkanalia's Vulkan wrapper methods use generic `impl Cast<Target=T>` bounds
for slice parameters instead of concrete types. When Rust sees `&[]`, it has
zero elements to infer `T` from. ash avoided this because its methods took
concrete `&[vk::MemoryBarrier]` parameters directly.

## Fix

Explicit cast on every empty slice:

```rust
// ❌ Fails — Rust can't infer T for empty slices
device.cmd_pipeline_barrier(
    cmd, src_stage, dst_stage, vk::DependencyFlags::empty(),
    &[], &[], &[barrier],
);

// ✅ Works — explicit types resolve the Cast bounds
device.cmd_pipeline_barrier(
    cmd, src_stage, dst_stage, vk::DependencyFlags::empty(),
    &[] as &[vk::MemoryBarrier],
    &[] as &[vk::BufferMemoryBarrier],
    &[barrier],
);
```

Only the empty slices need the cast. Non-empty slices infer correctly from
their elements.

## Where this hits in streamlib

Any file calling Vulkan commands with mixed empty/non-empty barrier arrays.
Currently:
- `libs/streamlib/src/linux/processors/camera.rs` — two `cmd_pipeline_barrier`
  calls during image layout transitions (capture → transfer, transfer → present)
- `libs/streamlib/src/vulkan/rhi/vulkan_format_converter.rs` — compute dispatch
  barriers

## Reference
- Migration PR: #252 (ash → vulkanalia)
- vulkanalia `Cast` trait: defined in `vulkanalia::bytecode` — the generic bound
  that replaces ash's concrete parameter types
