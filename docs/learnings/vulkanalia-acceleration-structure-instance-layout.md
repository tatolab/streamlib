# vulkanalia-sys: VkAccelerationStructureInstanceKHR field order disagrees with the Vulkan spec

## Symptom

Building a TLAS from `vk::AccelerationStructureInstanceKHR` records and
binding it to a ray-tracing kernel: every `traceRayEXT` call returns
the miss-shader output, never the closest-hit. No validation errors;
no driver warnings. The acceleration-structure device addresses look
plausible (non-zero, in VRAM range). The shader binding table layout,
descriptor write, and pipeline construction are all spec-clean.

## Root cause

The Vulkan spec (`VkAccelerationStructureInstanceKHR`) defines the
instance layout as:

```c
typedef struct VkAccelerationStructureInstanceKHR {
    VkTransformMatrixKHR          transform;                              // bytes 0..48
    uint32_t                      instanceCustomIndex:24;                 // bytes 48..52 (low 24 bits)
    uint32_t                      mask:8;                                 //              (high 8 bits)
    uint32_t                      instanceShaderBindingTableRecordOffset:24; // bytes 52..56 (low 24)
    VkGeometryInstanceFlagsKHR    flags:8;                                //              (high 8)
    uint64_t                      accelerationStructureReference;         // bytes 56..64
} VkAccelerationStructureInstanceKHR;
```

`vulkanalia-sys` 0.35.0 (and the streamlib `tatolab/vulkanalia` fork at
the rev pinned in workspace `Cargo.toml` as of 2026-05-03) defines it
with `acceleration_structure_reference` BETWEEN `transform` and the
two bitfields, not at the end:

```rust
#[repr(C)]
pub struct AccelerationStructureInstanceKHR {
    pub transform: TransformMatrixKHR,                  // 0..48
    pub acceleration_structure_reference: u64,          // 48..56  <-- WRONG
    pub bitfields0: AccelerationStructureInstanceKHRBitfields0,  // 56..60
    pub bitfields1: AccelerationStructureInstanceKHRBitfields1,  // 60..64
}
```

Because the struct is `#[repr(C)]`, writing through the Rust fields
puts `acceleration_structure_reference` at offset 48 (where the spec
expects the bitfields) and the bitfields at offsets 56–60 (where the
spec expects the device-address u64).

When the GPU consumes the instance buffer it reads in spec order. So:

- Bytes 48..52 — driver reads `instanceCustomIndex/mask`. Gets the
  low 32 bits of the BLAS device address (looks like a giant custom
  index + a non-zero mask, garbage).
- Bytes 52..56 — driver reads `sbtRecordOffset/flags`. Gets the high
  32 bits of the BLAS device address (typically zero on consumer
  GPUs, sometimes a small flag-shaped pattern).
- Bytes 56..64 — driver reads the BLAS reference. Gets two `u32`
  bitfield values catted together — a totally bogus device address.

The TLAS BVH then points every instance at garbage memory; rays
traverse the TLAS, fail to enter any BLAS, and the miss shader fires
for every pixel.

There is **no validation error** on either the build or the trace.
Validation layers don't second-guess the bytes in the instance buffer
— the spec gives the host total responsibility for the layout.

## Workaround

Don't use `vk::AccelerationStructureInstanceKHR` directly. Serialize
the instance bytes by hand in spec order into a flat `[u8; 64]`, and
write that into the instance buffer:

```rust
const INSTANCE_BYTES: usize = 64;

fn instance_bytes(desc: &TlasInstanceDesc) -> [u8; INSTANCE_BYTES] {
    let mut out = [0u8; INSTANCE_BYTES];
    // bytes 0..48 — transform: row-major 3×4 floats.
    let mut off = 0;
    for row in 0..3 {
        for col in 0..4 {
            out[off..off + 4].copy_from_slice(&desc.transform[row][col].to_ne_bytes());
            off += 4;
        }
    }
    // bytes 48..52 — instanceCustomIndex (24) + mask (8) packed u32.
    let custom_index = desc.custom_index & 0x00ff_ffff;
    let mask = (desc.mask as u32) << 24;
    out[48..52].copy_from_slice(&(custom_index | mask).to_ne_bytes());
    // bytes 52..56 — instanceShaderBindingTableRecordOffset (24) + flags (8).
    let sbt = desc.sbt_record_offset & 0x00ff_ffff;
    let flags = (desc.flags.bits() & 0xff) << 24;
    out[52..56].copy_from_slice(&(sbt | flags).to_ne_bytes());
    // bytes 56..64 — accelerationStructureReference (BLAS device address).
    out[56..64].copy_from_slice(&desc.blas.device_address.to_ne_bytes());
    out
}
```

Used by `streamlib::vulkan::rhi::VulkanAccelerationStructure::build_tlas`.
The instance buffer is a HOST_VISIBLE+COHERENT mapping; we write the
serialized bytes directly through the mapped pointer (no staging copy).

## Where else this might bite

- `VkAccelerationStructureMatrixMotionInstanceNV` and
  `VkAccelerationStructureSRTMotionInstanceNV` have the same
  bitfields-tail-with-accel_ref-trailing shape. If we ever use motion
  instances, audit those too.
- Any future packed-bitfield struct where vulkanalia's field order
  diverges from the C spec. The `bitfields32!` macro layout tail
  matters: field declaration order in the Rust struct determines
  `#[repr(C)]` offsets, and there's no compile-time check tying it
  back to the spec.

## Reference

- Issue: streamlib #610 (VulkanRayTracingKernel) — bug surfaced when
  every frame of the showcase example came back as pure miss color
  even though the AS device addresses looked correct and validation
  was silent.
- Vulkan spec: `VkAccelerationStructureInstanceKHR`
  https://registry.khronos.org/vulkan/specs/latest/man/html/VkAccelerationStructureInstanceKHR.html
- vulkanalia-sys: 0.35.0 / `tatolab/vulkanalia` fork at rev
  `982d32d293e5425753d595978776ee4fa4ded3a1`.
- Implementation:
  `libs/streamlib/src/vulkan/rhi/vulkan_acceleration_structure.rs::instance_bytes`.

## Upstream report

To the best of our current knowledge as of 2026-05-03 this is an
upstream bug in `vulkanalia-sys` rather than something specific to the
fork. Worth filing an issue / PR upstream — the trivial fix is to
reorder the struct fields to match the spec. If accepted, the
workaround in `instance_bytes` can be retired.
