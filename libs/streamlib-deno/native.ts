// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Deno FFI bindings to libstreamlib_deno_native.
 *
 * Wraps all `sldn_*` C ABI functions with TypeScript-friendly APIs.
 */

// FFI symbol definitions
const symbols = {
  // Context
  sldn_context_create: {
    parameters: ["buffer"] as const,
    result: "pointer" as const,
  },
  sldn_context_destroy: {
    parameters: ["pointer"] as const,
    result: "void" as const,
  },
  sldn_monotonic_now_ns: {
    parameters: [] as const,
    result: "u64" as const,
  },

  // Periodic monotonic timer via timerfd (Linux). `wait` is nonblocking so
  // the JS event loop stays responsive while a worker thread blocks on
  // `epoll_wait`. Used by subprocess_runner's continuous-mode dispatch to
  // replace `setTimeout`-based pacing.
  sldn_timerfd_create: {
    parameters: ["u64"] as const,
    result: "pointer" as const,
  },
  sldn_timerfd_wait: {
    parameters: ["pointer", "i32"] as const,
    result: "i64" as const,
    nonblocking: true,
  },
  sldn_timerfd_close: {
    parameters: ["pointer"] as const,
    result: "void" as const,
  },

  // Input
  sldn_input_subscribe: {
    parameters: ["pointer", "buffer"] as const,
    result: "i32" as const,
  },
  sldn_input_poll: {
    parameters: ["pointer"] as const,
    result: "i32" as const,
  },
  sldn_input_read: {
    parameters: ["pointer", "buffer", "pointer", "u32", "pointer", "pointer"] as const,
    result: "i32" as const,
  },
  sldn_input_set_read_mode: {
    parameters: ["pointer", "buffer", "i32"] as const,
    result: "i32" as const,
  },

  // Output
  sldn_output_publish: {
    parameters: ["pointer", "buffer", "buffer", "buffer", "buffer", "usize", "buffer"] as const,
    result: "i32" as const,
  },
  sldn_output_write: {
    parameters: ["pointer", "buffer", "pointer", "u32", "i64"] as const,
    result: "i32" as const,
  },

  // Event service (fd-multiplexed wakeups). sldn_event_wait is nonblocking
  // so the JS event loop can stay responsive while we wait in a worker thread.
  sldn_event_subscribe: {
    parameters: ["pointer", "buffer"] as const,
    result: "i32" as const,
  },
  sldn_event_wait: {
    parameters: ["pointer", "u32"] as const,
    result: "i32" as const,
    nonblocking: true,
  },

  // GPU Surface
  sldn_gpu_surface_lookup: {
    parameters: ["u32"] as const,
    result: "pointer" as const,
  },
  sldn_gpu_surface_lock: {
    parameters: ["pointer", "i32"] as const,
    result: "i32" as const,
  },
  sldn_gpu_surface_unlock: {
    parameters: ["pointer", "i32"] as const,
    result: "i32" as const,
  },
  sldn_gpu_surface_base_address: {
    parameters: ["pointer"] as const,
    result: "pointer" as const,
  },
  sldn_gpu_surface_width: {
    parameters: ["pointer"] as const,
    result: "u32" as const,
  },
  sldn_gpu_surface_height: {
    parameters: ["pointer"] as const,
    result: "u32" as const,
  },
  sldn_gpu_surface_bytes_per_row: {
    parameters: ["pointer"] as const,
    result: "u32" as const,
  },
  sldn_gpu_surface_create: {
    parameters: ["u32", "u32", "u32"] as const,
    result: "pointer" as const,
  },
  sldn_gpu_surface_get_id: {
    parameters: ["pointer"] as const,
    result: "u32" as const,
  },
  sldn_gpu_surface_release: {
    parameters: ["pointer"] as const,
    result: "void" as const,
  },

  // Surface-share client
  sldn_surface_connect: {
    parameters: ["buffer"] as const,
    result: "pointer" as const,
  },
  sldn_surface_disconnect: {
    parameters: ["pointer"] as const,
    result: "void" as const,
  },
  sldn_surface_resolve_surface: {
    parameters: ["pointer", "buffer"] as const,
    result: "pointer" as const,
  },
  sldn_surface_acquire_surface: {
    parameters: ["pointer", "u32", "u32", "u32", "pointer", "u32"] as const,
    result: "pointer" as const,
  },
  sldn_surface_unregister_surface: {
    parameters: ["pointer", "buffer"] as const,
    result: "void" as const,
  },

  // Per-plane surface-share accessors (#530). The OpenGL adapter needs
  // `plane_stride` (EGL_DMA_BUF_PLANE{N}_PITCH_EXT) and
  // `drm_format_modifier` (EGL_DMA_BUF_PLANE0_MODIFIER_LO/HI_EXT) for
  // EGL DMA-BUF import. The base accessors above (`width`, `height`,
  // `bytes_per_row`) are tightly-packed-CPU-mmap-shaped and don't carry
  // the modifier-aware row pitch the host allocator chose.
  sldn_gpu_surface_plane_stride: {
    parameters: ["pointer", "u32"] as const,
    result: "u64" as const,
  },
  sldn_gpu_surface_plane_offset: {
    parameters: ["pointer", "u32"] as const,
    result: "u64" as const,
  },
  sldn_gpu_surface_plane_fd: {
    parameters: ["pointer", "u32"] as const,
    result: "i32" as const,
  },
  sldn_gpu_surface_drm_format_modifier: {
    parameters: ["pointer"] as const,
    result: "u64" as const,
  },
  // Producer-declared `VkImageLayout` from the surface-share lookup
  // response (#633). Adapter `register_host_surface` paths read it
  // from the SurfaceHandle and pass it into
  // `HostSurfaceRegistration::initial_layout` so the consumer-side
  // `current_layout` matches the producer's claim.
  sldn_gpu_surface_initial_image_layout: {
    parameters: ["pointer"] as const,
    result: "i32" as const,
  },

  // OpenGL adapter runtime (#530, Linux). Brings up
  // `streamlib-adapter-opengl::EglRuntime` + `OpenGlSurfaceAdapter`
  // inside the subprocess; exposes scoped acquire/release returning a
  // `GL_TEXTURE_2D` id the customer's GL stack renders into.
  sldn_opengl_runtime_new: {
    parameters: [] as const,
    result: "pointer" as const,
  },
  sldn_opengl_runtime_free: {
    parameters: ["pointer"] as const,
    result: "void" as const,
  },
  sldn_opengl_register_surface: {
    parameters: ["pointer", "u64", "pointer"] as const,
    result: "i32" as const,
  },
  sldn_opengl_register_external_oes_surface: {
    parameters: ["pointer", "u64", "pointer"] as const,
    result: "i32" as const,
  },
  sldn_opengl_unregister_surface: {
    parameters: ["pointer", "u64"] as const,
    result: "i32" as const,
  },
  sldn_opengl_acquire_write: {
    parameters: ["pointer", "u64"] as const,
    result: "u32" as const,
  },
  sldn_opengl_release_write: {
    parameters: ["pointer", "u64"] as const,
    result: "i32" as const,
  },
  sldn_opengl_acquire_read: {
    parameters: ["pointer", "u64"] as const,
    result: "u32" as const,
  },
  sldn_opengl_release_read: {
    parameters: ["pointer", "u64"] as const,
    result: "i32" as const,
  },

  // Vulkan adapter runtime (#531, Linux). Same shape as `sldn_opengl_*`
  // but wraps `streamlib_adapter_vulkan::VulkanSurfaceAdapter` against a
  // subprocess-local `VulkanDevice` from the RHI.  Acquire returns the
  // imported `VkImage` + layout via an out-pointer struct (the symbol
  // matches `streamlib_deno_native::vulkan::SldnVulkanView`).
  sldn_vulkan_runtime_new: {
    parameters: [] as const,
    result: "pointer" as const,
  },
  sldn_vulkan_runtime_free: {
    parameters: ["pointer"] as const,
    result: "void" as const,
  },
  sldn_vulkan_register_surface: {
    parameters: ["pointer", "u64", "pointer"] as const,
    result: "i32" as const,
  },
  sldn_vulkan_unregister_surface: {
    parameters: ["pointer", "u64"] as const,
    result: "i32" as const,
  },
  sldn_vulkan_acquire_write: {
    parameters: ["pointer", "u64", "pointer"] as const,
    result: "i32" as const,
  },
  sldn_vulkan_release_write: {
    parameters: ["pointer", "u64"] as const,
    result: "i32" as const,
  },
  sldn_vulkan_acquire_read: {
    parameters: ["pointer", "u64", "pointer"] as const,
    result: "i32" as const,
  },
  sldn_vulkan_release_read: {
    parameters: ["pointer", "u64"] as const,
    result: "i32" as const,
  },
  sldn_vulkan_raw_handles: {
    parameters: ["pointer", "pointer"] as const,
    result: "i32" as const,
  },
  // Compute dispatch routes through escalate IPC (`register_compute_kernel`
  // + `run_compute_kernel`) — no cdylib FFI for compute. See
  // `adapters/vulkan.ts::dispatchCompute`.

  // cpu-readback adapter runtime (#562, Linux). Same shape as
  // `sldn_vulkan_*` — adapter generic over device flavor; this cdylib
  // instantiates it against `ConsumerVulkanDevice`. Per-acquire copy
  // runs host-side via a `run_cpu_readback_copy` escalate IPC; the
  // SDK installs a trigger callback that wraps that call.
  sldn_cpu_readback_runtime_new: {
    parameters: [] as const,
    result: "pointer" as const,
  },
  sldn_cpu_readback_runtime_free: {
    parameters: ["pointer"] as const,
    result: "void" as const,
  },
  // The callback parameter is typed as `pointer` here because Deno
  // FFI doesn't have a dedicated `function` slot for received-from-JS
  // callbacks — the SDK creates a `Deno.UnsafeCallback` and passes
  // its `.pointer` field. Callback signature on the cdylib side is
  // `(*mut c_void user_data, u64 surface_id, u32 direction) -> u64`
  // (timeline value; 0 sentinel for failure).
  sldn_cpu_readback_set_trigger_callback: {
    parameters: ["pointer", "pointer", "pointer"] as const,
    result: "i32" as const,
  },
  sldn_cpu_readback_register_surface: {
    parameters: ["pointer", "u64", "pointer", "u32"] as const,
    result: "i32" as const,
  },
  sldn_cpu_readback_unregister_surface: {
    parameters: ["pointer", "u64"] as const,
    result: "i32" as const,
  },
  sldn_cpu_readback_acquire_read: {
    parameters: ["pointer", "u64", "pointer"] as const,
    result: "i32" as const,
  },
  sldn_cpu_readback_acquire_write: {
    parameters: ["pointer", "u64", "pointer"] as const,
    result: "i32" as const,
  },
  sldn_cpu_readback_try_acquire_read: {
    parameters: ["pointer", "u64", "pointer"] as const,
    result: "i32" as const,
  },
  sldn_cpu_readback_try_acquire_write: {
    parameters: ["pointer", "u64", "pointer"] as const,
    result: "i32" as const,
  },
  sldn_cpu_readback_release_read: {
    parameters: ["pointer", "u64"] as const,
    result: "i32" as const,
  },
  sldn_cpu_readback_release_write: {
    parameters: ["pointer", "u64"] as const,
    result: "i32" as const,
  },

  // cuda adapter runtime (#590, Linux). The cdylib instantiates
  // `CudaSurfaceAdapter<ConsumerVulkanDevice>` plus the CUDA driver
  // imports (`cudaImportExternalMemory(OPAQUE_FD)` /
  // `cudaImportExternalSemaphore`). Per-acquire control flow is the
  // adapter's Vulkan-side timeline wait + a `cudaWaitExternalSemaphoresAsync`
  // sync; no host-side bridge / IPC / trigger callback is needed.
  // Surfaced end-to-end by #591's polyglot scenario; the symbol set
  // was missing here in #590 and added when #591 first tried to
  // dlopen it.
  sldn_cuda_runtime_new: {
    parameters: [] as const,
    result: "pointer" as const,
  },
  sldn_cuda_runtime_free: {
    parameters: ["pointer"] as const,
    result: "void" as const,
  },
  sldn_cuda_register_surface: {
    parameters: ["pointer", "u64", "pointer"] as const,
    result: "i32" as const,
  },
  sldn_cuda_unregister_surface: {
    parameters: ["pointer", "u64"] as const,
    result: "i32" as const,
  },
  sldn_cuda_acquire_read: {
    parameters: ["pointer", "u64", "pointer"] as const,
    result: "i32" as const,
  },
  sldn_cuda_acquire_write: {
    parameters: ["pointer", "u64", "pointer"] as const,
    result: "i32" as const,
  },
  sldn_cuda_try_acquire_read: {
    parameters: ["pointer", "u64", "pointer"] as const,
    result: "i32" as const,
  },
  sldn_cuda_try_acquire_write: {
    parameters: ["pointer", "u64", "pointer"] as const,
    result: "i32" as const,
  },
  sldn_cuda_release_read: {
    parameters: ["pointer", "u64"] as const,
    result: "i32" as const,
  },
  sldn_cuda_release_write: {
    parameters: ["pointer", "u64"] as const,
    result: "i32" as const,
  },
} as const;

export type NativeLib = Deno.DynamicLibrary<typeof symbols>;

/**
 * Load the native StreamLib Deno library.
 */
export function loadNativeLib(path: string): NativeLib {
  return Deno.dlopen(path, symbols);
}

/**
 * Encode a string to a null-terminated UTF-8 buffer.
 */
export function cString(str: string): Uint8Array<ArrayBuffer> {
  const encoder = new TextEncoder();
  const encoded = encoder.encode(str);
  const buf = new Uint8Array(new ArrayBuffer(encoded.length + 1));
  buf.set(encoded);
  buf[encoded.length] = 0; // null terminator
  return buf;
}
