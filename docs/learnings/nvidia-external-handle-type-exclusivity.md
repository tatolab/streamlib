# NVIDIA: one allocation cannot export both OPAQUE_FD and DMA-BUF

## Symptom

- A DMA-BUF-flavoured allocation (pixel-buffer pool, camera imports) has
  no OPAQUE_FD export path — `vkGetMemoryFdKHR` with
  `OPAQUE_FD` fails on memory allocated for `DMA_BUF_EXT`, and vice
  versa. `SurfaceStore::register_texture` works around the runtime
  symptom by branching on the allocation's flavour.
- CUDA cannot consume a graph frame directly: `cudaExternalMemoryHandleType`
  has no dma-buf member at all (OpaqueFd, Win32×2, D3D×3, NvSciBuf — that
  is the whole enum). NVIDIA's dma-buf support in CUDA
  (`cuMemGetHandleForAddressRange`) *exports CUDA memory as* dma-buf for
  GPUDirect; it does not import Vulkan dma-bufs. The only dma-buf→CUDA
  route is an EGLImage detour with a GL context in the loop.

## Root cause (measured, not inferred)

A `vkGetPhysicalDeviceExternalBufferProperties` probe on the MVP floor
platform (RTX 3090, driver 595.84) reports the two handle types in
**disjoint** `compatibleHandleTypes` sets:

    OPAQUE_FD:    compatibleHandleTypes = { OPAQUE_FD }
    DMA_BUF_EXT:  compatibleHandleTypes = { DMA_BUF }

VUID-VkExportMemoryAllocateInfo-handleTypes-00656 requires every
requested handle type to appear in the others' compatibility set, so a
single allocation requesting `OPAQUE_FD | DMA_BUF_EXT` is spec-invalid on
this driver. Note this is **not** VMA's one-export-info-per-pool
limitation — `handleTypes` is a bitmask and one pool could legally chain
both; the driver's compatibility report is what refuses. (Mesa
Intel/AMD report the types mutually compatible — both are dma-bufs
underneath — but the plan's platform floor is Linux + NVIDIA.)

## Consequence for design

An allocation carries exactly one external-handle flavour, chosen at
creation, and each consumer class needs its own: EGL/GL import is
dma-buf-only (no OPAQUE_FD EGLImage exists), CUDA import is
OPAQUE_FD-only. Bridging a frame from one flavour's world to the other's
therefore costs one GPU copy into a staging allocation of the target
flavour — this is why the device-export path blits into an OPAQUE_FD
staging buffer rather than re-flavouring the pool
(`core/context/device_export_staging.rs`), and why "give the pool both
flavours" is not an available option, only a choice of which consumer
class to strand.
