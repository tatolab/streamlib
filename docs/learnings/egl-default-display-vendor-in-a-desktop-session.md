# EGL: the default display's vendor in a desktop session may not be the GPU's

## Symptom

On an NVIDIA machine, a process started inside a desktop session (with
`DISPLAY` or `WAYLAND_DISPLAY` set) gets back only `DRM_FORMAT_MOD_LINEAR` from
`eglQueryDmaBufModifiersEXT` for `DRM_FORMAT_ABGR8888`/`ARGB8888`, and Mesa
marks it render-target-capable (`external_only=FALSE`). A Vulkan render target
allocated from that list is LINEAR, and NVIDIA's display shows a LINEAR import
as black. The same process started with neither variable set gets NVIDIA's 12
tiled render-target modifiers, with LINEAR as sampler-only.

Measured on an RTX 3090 with driver 595.99.02 on Ubuntu 26 under GNOME Wayland,
with GLVND vendors `10_nvidia.json` and `50_mesa.json`:

- `DISPLAY` and/or `WAYLAND_DISPLAY` set: `EGL_VENDOR` "Mesa Project", which logs
  `pci id for fd …: 10de:2204, driver (null)` and `failed to create dri2 screen`
  and falls back to software rendering.
- Neither set: `EGL_VENDOR` "NVIDIA".
- `__EGL_VENDOR_LIBRARY_FILENAMES` forced to NVIDIA with the variables set:
  `eglInitialize` on the default display fails.

## Root cause

`eglGetDisplay(EGL_DEFAULT_DISPLAY)` picks a platform from the environment (X11
or Wayland when a display variable is set), and GLVND hands the display to the
first vendor that accepts that platform. NVIDIA's EGL declines the default
display there, so Mesa takes it, and Mesa cannot drive the NVIDIA GPU. The
display answers for a different device from the one the process allocates on.

## Fix shape

Never ask the default display about modifiers a specific device will allocate.
Enumerate EGL devices (`EGL_EXT_device_enumeration`), match the one whose
`EGL_DRM_RENDER_NODE_FILE_EXT` has the Vulkan device's render node number
(`VK_EXT_physical_device_drm`), and open it with
`eglGetPlatformDisplayEXT(EGL_PLATFORM_DEVICE_EXT, …)`. Two EGL devices can
share a render node, and only one may initialize, so try each match in turn.
That display reports NVIDIA's table whatever the environment holds.
