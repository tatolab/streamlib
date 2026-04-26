# NVIDIA EGL DMA-BUF render-target: linear modifier is sampler-only, tiled modifiers are render-target-capable

## Symptom

Importing a DMA-BUF as an EGLImage via `EGL_LINUX_DMA_BUF_EXT` and
binding it to a `GL_TEXTURE_2D` on NVIDIA Linux:

- `glEGLImageTargetTexture2DOES(GL_TEXTURE_2D, image)` returns
  `GL_INVALID_OPERATION` (`0x0502`) via `glGetError`. The call does
  not raise an exception in ctypes; the error is asynchronous.
- Attaching that texture to an FBO via
  `glFramebufferTexture2D(... GL_COLOR_ATTACHMENT0 ...)` does not
  error, but `glCheckFramebufferStatus(GL_FRAMEBUFFER)` returns
  `GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT` (`0x8CD6`).
- The same texture *can* be sampled fine via a `samplerExternalOES`
  in a fragment shader — sampling works, rendering doesn't.

## Root cause

NVIDIA's EGL implementation classifies DMA-BUF imports per
*format×modifier* pair. Query
`eglQueryDmaBufModifiersEXT(display, fourcc, …)` and you get an
`external_only` flag for each supported modifier. On
`570.211.01` / RTX 3090 for `DRM_FORMAT_ABGR8888` (fourcc `'AB24'`):

| modifier | external_only | meaning |
|---|---|---|
| `0x0` (DRM_FORMAT_MOD_LINEAR) | `TRUE` | sampler-only |
| `0x300000000606010..15` (NVIDIA tiled) | `FALSE` | full GL_TEXTURE_2D, render-target-capable |
| `0x300000000e08010..15` (NVIDIA tiled) | `FALSE` | full GL_TEXTURE_2D, render-target-capable |

`external_only=TRUE` means the imported EGLImage has
`GL_TEXTURE_EXTERNAL_OES` semantics: it can only be bound via
`glEGLImageTargetTexture2DOES(GL_TEXTURE_EXTERNAL_OES, image)`, sampled
in shaders via `samplerExternalOES`, and **cannot** be a color
attachment. Binding it to `GL_TEXTURE_2D` triggers the
`GL_INVALID_OPERATION` above.

This is **not** a bug or a missing extension. NVIDIA does not publish
an OS-side path that exposes a linearly-laid-out DMA-BUF as a
render-target-capable `GL_TEXTURE_2D`. Linear DMA-BUFs are
sampler-only on this driver family; tiled ones are full textures.

Mesa drivers (Intel / AMD via `iris` / `radeonsi`) generally mark
LINEAR `external_only=FALSE`, which is why this is invisible until
you try the path on NVIDIA.

## What this rules out

- **Passing `DRM_FORMAT_MOD_LINEAR` explicitly** — same result. The
  attribute is correctly accepted; the import succeeds; the resulting
  texture is still external-only.
- **Choosing a different format** (`ARGB8888`, `XRGB8888`, etc.) —
  the `external_only` distinction tracks the modifier, not the
  format. Linear is sampler-only across formats on NVIDIA.
- **Adding `EGL_IMAGE_PRESERVED_KHR`** or similar attribute hints —
  these don't change render-target capability for an external-only
  modifier.

## What works

**Allocate the DMA-BUF as a tiled VkImage instead of a linear
VkBuffer.** A `VkImage` created with `VK_IMAGE_TILING_OPTIMAL` (or,
better, `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT` from
`VK_EXT_image_drm_format_modifier` with one of the modifiers the
EGL query reports as `external_only=FALSE`) and exported via
`VkExportMemoryAllocateInfo` lands a DMA-BUF FD that EGL imports as
a real `GL_TEXTURE_2D` — no `GL_INVALID_OPERATION`, FBO completes,
shader can render to it.

The streamlib RHI already has `VulkanTexture` (a `VkImage`) for
render-target use; the gap is escalate-IPC plumbing for subprocess
processors to acquire one. The host-side allocation already needs to
happen pre-swapchain to avoid NVIDIA's per-process DMA-BUF cap (see
`docs/learnings/nvidia-dma-buf-after-swapchain.md`) — same pattern
the camera processor already uses for its buffer pool.

`VulkanPixelBuffer` (a `VkBuffer`, linear by definition) is the
right shape for CPU-touchable / MMAP / readback paths — but it is
not the right shape for "GL renders into it." Treat the buffer
vs. image distinction as a load-bearing one for cross-API
interop, not a freely-substitutable detail.

## How to detect this in the field

Before assuming a DMA-BUF will work as a GL render target on Linux,
query supported modifiers and inspect `external_only`:

```c
/* PFNEGLQUERYDMABUFMODIFIERSEXTPROC eglQueryDmaBufModifiersEXT;
   resolve via eglGetProcAddress("eglQueryDmaBufModifiersEXT"). */
EGLint count = 0;
eglQueryDmaBufModifiersEXT(display, fourcc, 0, NULL, NULL, &count);
EGLuint64KHR mods[count];
EGLBoolean external_only[count];
eglQueryDmaBufModifiersEXT(display, fourcc, count, mods, external_only, &count);
/* external_only[i] == EGL_TRUE  →  modifier mods[i] is sampler-only.
   external_only[i] == EGL_FALSE →  modifier mods[i] is full GL_TEXTURE_2D. */
```

Pair this with `glCheckFramebufferStatus` after attaching — a
`0x8CD6` (`GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT`) on a successfully-
imported EGL DMA-BUF texture is the red flag for this exact
mismatch.

## Reference

- NVIDIA driver: `570.211.01`, RTX 3090.
- Probe captured during issue #481 PR review (see PR body for the
  research run that established the `external_only` table).
- Spec: `EGL_EXT_image_dma_buf_import_modifiers`, in particular the
  `external_only` semantics —
  https://registry.khronos.org/EGL/extensions/EXT/EGL_EXT_image_dma_buf_import_modifiers.txt
- Spec: `GL_OES_EGL_image_external` for the texture-external
  semantics that `external_only=TRUE` opts the import into.
