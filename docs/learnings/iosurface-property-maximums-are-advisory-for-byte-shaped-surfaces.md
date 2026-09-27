# IOSurface's property maximums are advisory for a byte-shaped surface

## Symptom

- Sizing a tensor storage buffer's IOSurface from `IOSurfaceGetPropertyMaximum`
  suggests a 256 MiB ceiling (16384 rows of 16384 bytes) that the allocation
  does not actually have.
- A byte-shaped surface wider than 16384 elements, or taller than 16384 rows,
  is created without complaint, even though the reported maximums say it
  should not be.

## Constraint

Measured on Apple M1 Pro, macOS 26.3 (25D125), for a surface with
`kIOSurfaceBytesPerElement = 1`, an explicit `kIOSurfaceBytesPerRow`, and no
`kIOSurfacePixelFormat`:

| query | answer |
|---|---|
| `IOSurfaceGetPropertyMaximum(kIOSurfaceWidth)` | 16384 |
| `IOSurfaceGetPropertyMaximum(kIOSurfaceHeight)` | 16384 |
| `IOSurfaceGetPropertyMaximum(kIOSurfaceBytesPerRow)` | 32768 |
| `IOSurfaceGetPropertyMaximum(kIOSurfaceBytesPerElement)` / `(kIOSurfaceAllocSize)` | 0 (no limit reported) |
| `IOSurfaceGetPropertyAlignment(kIOSurfaceBytesPerRow)` | 128 |
| `IOSurfaceGetPropertyAlignment(kIOSurfaceWidth)` | 1 |

What `IOSurfaceCreate` actually accepted and allocated:

| width x height | bytes_per_row | alloc_size |
|---|---|---|
| 16384 x 1 | 16384 | 16384 |
| 16385 x 1 | 16385 | 32768 |
| 16384 x 16385 | 16384 | 268451840 |
| 16384 x 262144 | 16384 | 4294967296 (4 GiB) |
| 1073741824 x 1 | — | created |

- A 16384-byte row is honoured exactly: it is a multiple of the 128-byte
  row-pitch alignment, so rows pack back to back.
- The allocation is rounded up to whole 16 KiB pages, and the base address is
  page-aligned. That keeps `VK_EXT_external_memory_host`'s import alignment
  inside the surface's own pages.
- The reported maximums look like the GPU's texture limits. They do not bound
  a surface that is only ever imported as a buffer.

## Fix pattern

- A tensor storage buffer's IOSurface is 16384-byte rows, with
  `height = ceil(len / 16384)`. Do not clamp the height to the reported
  maximum; a model-sized tensor needs only a few hundred rows.
- The real ceiling on a tensor is the buffer side: MoltenVK's
  `min(maxBufferLength, u32::MAX)`. Nothing on the IOSurface side limits it
  before that.
- Never create a Metal texture over one of these surfaces. The texture limits
  that the maximums describe would then apply.

## References

- `runtime/streamlib-engine/src/vulkan/rhi/vulkan_imported_iosurface_storage_buffer.rs`
  (`new_iosurface_backed_storage_buffer`)
- `runtime/streamlib-engine/src/apple/iosurface.rs`
  (`create_private_iosurface_with_packed_rows`)
