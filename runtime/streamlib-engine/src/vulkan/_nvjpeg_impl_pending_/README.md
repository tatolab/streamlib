# Parked nvJPEG backend (disabled)

This directory holds the NVIDIA `libnvjpeg` (CUDA-based) JPEG-decode
backend that used to ship behind `vulkan-jpeg`'s `JpegDecodeBackend`
trait. It was split out of `vulkan-jpeg` during the plugin-SDK
extraction and parked here, **disabled** — there is no
`mod _nvjpeg_impl_pending_;` anywhere in the engine, so nothing in this
directory is in the module tree and none of it compiles. It is
reference code only, mirroring the `_apple_impl_pending_/` convention
used elsewhere in the codebase (e.g.
`packages/h264/src/_apple_impl_pending_/`).

## Why it was split out (and not shipped in `plugin/vulkan-jpeg`)

`vulkan-jpeg` moved to the engine-free `plugin/` zone. The Vulkan-compute
backend is cdylib-safe: it builds every GPU resource through the
FullAccess primitives (`create_compute_kernel`, `acquire_storage_buffer`,
`create_texture_ring`, `create_command_recorder`) that return `#[repr(C)]`
handles.

The nvJPEG backend is **not** cdylib-safe. It reaches the raw
`HostVulkanDevice` (via `GpuContextFullAccess::host_vulkan_device_arc()`)
and allocates OPAQUE_FD-exportable `VkBuffer`s / timeline semaphores
directly so it can import them into CUDA (`cudaImportExternalMemory`,
`cudaImportExternalSemaphore`). Those primitives are engine-internal and
have **no cdylib-safe FullAccess form yet** — transiting a
non-`#[repr(C)]` `HostVulkanDevice` across the plugin ABI is unsound for a
separately-built `.slpkg` (see
`docs/learnings/slpkg-raw-device-rhi-construction.md`). So the nvJPEG
backend cannot live in `plugin/vulkan-jpeg`; it has to come back as an
engine-resident backend with a cdylib-safe exposure path.

## Files

- `backend.rs` — `NvJpegBackend` (`JpegDecodeBackend` impl). Was
  `nvjpeg_backend.rs`.
- `resources.rs` — OPAQUE_FD buffer / timeline allocation + CUDA import +
  per-frame `vkCmdCopyBufferToImage` plumbing.
- `ffi.rs` — `libnvjpeg.so.12` symbol bindings resolved via `libloading`.
- `tests_pending/` — the host-RHI tests that exercised this backend and
  the in-process Vulkan-compute path (`gpu_decode.rs`,
  `nvjpeg_backend.rs`, `simple_decoder.rs`, `cuda_vulkan_repro.rs`). They
  reference `HostVulkanDevice` and so cannot compile engine-free; they are
  parked here as reference rather than deleted.

## Re-integration notes (for issue #1206)

- This code retains its `streamlib::sdk::*` imports from when it lived in
  `vulkan-jpeg`. When it is wired back into the engine, repoint those to
  the engine-internal paths (`crate::vulkan::rhi::*` for
  `HostVulkanBuffer`, `HostVulkanDevice`, `RhiCommandRecorder`,
  `ImageCopyRegion`, `VulkanAccess`, `VulkanStage`, etc.; the engine's
  `core`/`sdk` modules for `GpuContextFullAccess`, `TextureRing`, the
  color types, and `Error`/`Result`).
- The historical claim in this code that
  `host_vulkan_device_arc()` "is the cdylib-safe bridge" is **WRONG** —
  it transits a non-`#[repr(C)]` device by raw pointer and is only sound
  when the plugin and host share an identical compilation. The
  cdylib-safe path is to add a FullAccess primitive that does the
  OPAQUE_FD allocation + CUDA-import host-side and returns a `#[repr(C)]`
  handle. Re-integration + cdylib-safe plugin exposure is tracked in
  issue #1206.
- Do NOT add a `mod _nvjpeg_impl_pending_;` declaration to try to make
  this compile. It is intentionally out of the module tree.
