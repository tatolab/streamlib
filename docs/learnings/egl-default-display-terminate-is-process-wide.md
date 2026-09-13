# EGL: `eglTerminate` on the default display is process-wide

## Symptom

Any of these, intermittently, in a process where more than one piece of code
opens `EGL_DEFAULT_DISPLAY`:

- glibc `corrupted size vs. prev_size while consolidating` (SIGABRT, exit 134),
  with no Rust panic and no test summary — a test binary simply dies;
- a hang;
- `eglMakeCurrent` failing with `EGL_NOT_INITIALIZED` ("EGL is not initialized,
  or could not be initialized, for the specified EGL display connection") on a
  context that was created successfully and never destroyed.

The crash only shows up when the holders run concurrently. The
`EGL_NOT_INITIALIZED` failure is deterministic whenever one holder finishes
before another uses its display.

## Root cause

`eglGetDisplay(EGL_DEFAULT_DISPLAY)` returns the **same** `EGLDisplay` to every
caller in the process. `eglInitialize` on an already-initialized display is a
no-op, but `eglTerminate` is not reference-counted: the first caller to
terminate tears the display down for everyone. Only
`EGL_KHR_display_reference` (opted into through `eglGetPlatformDisplay` with
`EGL_TRACK_REFERENCES_KHR`) makes terminate counted, and NVIDIA 595.84 does not
advertise it.

- **Serial holders:** the survivor's next `eglMakeCurrent` returns
  `EGL_NOT_INITIALIZED`.
- **Concurrent holders:** one thread terminates while another is still
  querying the display (`eglQueryString`, `eglQueryDmaBufModifiersEXT`), which
  corrupts the driver's heap.

A C harness isolated it (NVIDIA 595.84, RTX 3090, `DISPLAY` set, two threads
× ten probes, twenty runs per variant):

| Per probe | Aborted |
|---|---|
| dlopen + init + query + **terminate** + dlclose | 18 / 20 |
| libEGL loaded once, init + query + **terminate** | 20 / 20 |
| dlopen + init + query + dlclose, **no terminate** | 0 / 20 |
| the whole probe under one mutex | 0 / 20 |

Loading and unloading libEGL is not the trigger. `eglTerminate` is.

## Fix

Treat an initialized default display as process state. Nobody terminates it:

- Never call `eglTerminate` on a display obtained from `EGL_DEFAULT_DISPLAY`.
  Release your own context and images, and leave the display initialized for
  the life of the process.
- Keep libEGL loaded for as long as that display stays initialized.
- If an answer comes from the display and not from a particular caller, such as
  the DRM modifiers the driver advertises, compute it once per process and share
  it.

Adding only a lock around terminate stops the concurrent crash, but the next
holder still gets `EGL_NOT_INITIALIZED`.

## Where this lives

- `runtime/streamlib-engine/src/vulkan/rhi/drm_modifier_probe.rs` — probed once
  per process, and the display never terminated.
- `adapters/streamlib-adapter-opengl/src/egl.rs` — `EglRuntime`'s drop releases
  its context and leaves the display alone.
