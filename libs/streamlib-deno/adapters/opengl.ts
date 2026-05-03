// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * OpenGL/EGL surface adapter — Deno customer-facing API.
 *
 * Mirrors the Rust crate `streamlib-adapter-opengl` (#512). The
 * subprocess's actual EGL+GL handling lives in the runtime's
 * native binding; this module provides:
 *
 *  - `OpenGLReadView` / `OpenGLWriteView` — typed views the
 *    subprocess sees inside `acquireRead` / `acquireWrite` scopes;
 *    expose a single `glTextureId` (a `number` GL handle) and the
 *    constant `target = GL_TEXTURE_2D`.
 *  - `OpenGLContext` interface — the runtime hands one out,
 *    customers use TC39 `using` blocks for scoped acquire/release.
 *
 * Customers never see DMA-BUF FDs, fourcc codes, plane offsets,
 * strides, or DRM modifiers. Per the NVIDIA EGL DMA-BUF
 * render-target learning, the host allocator picks a tiled,
 * render-target-capable modifier so the resulting GL texture is
 * always a regular `GL_TEXTURE_2D` — never `GL_TEXTURE_EXTERNAL_OES`.
 */

import {
  STREAMLIB_ADAPTER_ABI_VERSION,
  type StreamlibSurface,
  type SurfaceAccessGuard,
} from "../surface_adapter.ts";
import { VulkanContext } from "./vulkan.ts";

export { STREAMLIB_ADAPTER_ABI_VERSION };

/** `GL_TEXTURE_2D` enumerant — re-exported so customers don't have
 * to import a GL binding just to compare `view.target`. Matches the
 * Rust crate's `GL_TEXTURE_2D` constant. */
export const GL_TEXTURE_2D = 0x0DE1 as const;

/** `GL_TEXTURE_EXTERNAL_OES` enumerant from `GL_OES_EGL_image_external`.
 * Returned in `view.target` for surfaces acquired via
 * `OpenGLContext.acquireReadExternalOes` — the consumer's GLSL must
 * `#extension GL_OES_EGL_image_external : require` and sample via
 * `texture2D(samplerExternalOES, vec2)` (the adapter's desktop-GL
 * context does not provide the ESSL3 unified `texture(...)` overload).
 * See `OpenGLContext.acquireReadExternalOes` for the full reasoning. */
export const GL_TEXTURE_EXTERNAL_OES = 0x8D65 as const;

/** Read-side view inside an `acquireRead` / `acquireReadExternalOes`
 * scope. `target` is `GL_TEXTURE_2D` for the host render-target path
 * or `GL_TEXTURE_EXTERNAL_OES` for the sampler-only EGL DMA-BUF path
 * (linear surfaces, NVIDIA `external_only=TRUE` modifiers). */
export interface OpenGLReadView {
  /** GL texture id the customer feeds into their GL stack. */
  readonly glTextureId: number;
  /** Either `GL_TEXTURE_2D` or `GL_TEXTURE_EXTERNAL_OES`. */
  readonly target: typeof GL_TEXTURE_2D | typeof GL_TEXTURE_EXTERNAL_OES;
}

/** Write-side view inside an `acquireWrite` scope. */
export interface OpenGLWriteView {
  readonly glTextureId: number;
  readonly target: typeof GL_TEXTURE_2D;
}

/** Public OpenGL adapter contract. */
export interface OpenGLSurfaceAdapter {
  acquireRead(surface: StreamlibSurface): SurfaceAccessGuard<OpenGLReadView>;
  acquireWrite(
    surface: StreamlibSurface,
  ): SurfaceAccessGuard<OpenGLWriteView>;
  tryAcquireRead(
    surface: StreamlibSurface,
  ): SurfaceAccessGuard<OpenGLReadView> | null;
  tryAcquireWrite(
    surface: StreamlibSurface,
  ): SurfaceAccessGuard<OpenGLWriteView> | null;
}

/** Async-disposable guard returned by acquire ops. `using` (synchronous
 * disposable) suffices because the OpenGL adapter's per-acquire path is
 * fully synchronous — no IPC roundtrip on the hot path. */
export interface OpenGLAccessGuard<V> extends Disposable {
  readonly view: V;
}

/** Minimal subset of `GpuContextLimitedAccess` the OpenGL adapter
 * runtime needs. The full shape lives in `context.ts`; we type against
 * a structural subset here so tests can stub it without dragging the
 * whole FFI surface. */
export interface OpenGLGpuLimitedAccess {
  resolveSurface(poolId: string): {
    readonly nativeHandlePtr: Deno.PointerObject | null;
    release(): void;
  };
  // deno-lint-ignore no-explicit-any
  readonly nativeLib: { readonly symbols: any };
}

let _SURFACE_ID_COUNTER = 0n;
function nextSurfaceId(): bigint {
  _SURFACE_ID_COUNTER += 1n;
  return _SURFACE_ID_COUNTER;
}

let _SHARED_INSTANCE: OpenGLContext | null = null;

/** Subprocess-side OpenGL adapter runtime (#530, Linux).
 *
 * Brings up `streamlib-adapter-opengl::EglRuntime` +
 * `OpenGlSurfaceAdapter` inside this subprocess and exposes scoped
 * acquire/release that hands the customer a real `GL_TEXTURE_2D` id.
 * The adapter's EGL context is current on the calling thread for the
 * lifetime of an `acquire*` scope — any GL library that latches onto
 * the current EGL context (raw `Deno.dlopen` against `libGLESv2.so`,
 * a Deno-FFI game-engine binding, etc.) sees the texture id as live.
 *
 * Construct via `OpenGLContext.fromRuntime(ctx)` — single instance per
 * subprocess. Repeat calls return the cached instance.
 *
 * Acquire / release MUST happen on the same thread. Deno's default is
 * a single-threaded async event loop, so this is the natural shape.
 */
export class OpenGLContext {
  private readonly gpu: OpenGLGpuLimitedAccess;
  // deno-lint-ignore no-explicit-any
  private readonly symbols: any;
  private readonly rt: Deno.PointerObject;
  private readonly surfaceIds = new Map<string, bigint>();
  /** Tracks the GL target each surface was registered under so a
   * surface registered as `GL_TEXTURE_2D` can't be silently re-used as
   * `GL_TEXTURE_EXTERNAL_OES` (or vice versa) — the underlying
   * `SurfaceState` only carries one binding. */
  private readonly surfaceTargets = new Map<string, number>();
  private readonly resolvedHandles = new Map<
    string,
    ReturnType<OpenGLGpuLimitedAccess["resolveSurface"]>
  >();

  constructor(gpu: OpenGLGpuLimitedAccess) {
    this.gpu = gpu;
    this.symbols = gpu.nativeLib.symbols;
    const rtPtr = this.symbols.sldn_opengl_runtime_new();
    if (rtPtr === null) {
      throw new Error(
        "OpenGLContext: sldn_opengl_runtime_new returned NULL — the " +
          "subprocess could not bring up an EGL display + GL context. " +
          "Check that libEGL.so.1 is installed and the driver supports " +
          "EGL_EXT_image_dma_buf_import_modifiers.",
      );
    }
    this.rt = rtPtr;
  }

  /** Build (or fetch the cached) `OpenGLContext` for this subprocess.
   * The subprocess hosts at most one EGL display + GL context — calling
   * this twice returns the same instance. */
  static fromRuntime(
    ctx: { readonly gpuLimitedAccess: OpenGLGpuLimitedAccess },
  ): OpenGLContext {
    if (_SHARED_INSTANCE === null) {
      _SHARED_INSTANCE = new OpenGLContext(ctx.gpuLimitedAccess);
    }
    return _SHARED_INSTANCE;
  }

  private resolveAndRegister(
    poolId: string,
    target: number = GL_TEXTURE_2D,
  ): bigint {
    const cached = this.surfaceIds.get(poolId);
    if (cached !== undefined) {
      const cachedTarget = this.surfaceTargets.get(poolId);
      if (cachedTarget !== target) {
        throw new Error(
          `OpenGLContext: surface '${poolId}' was already registered ` +
            `under target 0x${cachedTarget?.toString(16).toUpperCase()}; ` +
            `refusing to re-register under target 0x${
              target.toString(16).toUpperCase()
            }. Use the matching acquire method (acquireRead[ExternalOes]).`,
        );
      }
      return cached;
    }
    const handle = this.gpu.resolveSurface(poolId);
    const handlePtr = handle.nativeHandlePtr;
    if (handlePtr === null) {
      throw new Error(
        `OpenGLContext: resolveSurface('${poolId}') returned a handle with a null native pointer`,
      );
    }
    const surfaceId = nextSurfaceId();
    let rc: number;
    let registerName: string;
    if (target === GL_TEXTURE_EXTERNAL_OES) {
      registerName = "register_external_oes_surface";
      rc = this.symbols.sldn_opengl_register_external_oes_surface(
        this.rt,
        surfaceId,
        handlePtr,
      );
    } else if (target === GL_TEXTURE_2D) {
      registerName = "register_surface";
      rc = this.symbols.sldn_opengl_register_surface(
        this.rt,
        surfaceId,
        handlePtr,
      );
    } else {
      throw new Error(
        `OpenGLContext: unsupported registration target 0x${
          target.toString(16).toUpperCase()
        } (expected GL_TEXTURE_2D or GL_TEXTURE_EXTERNAL_OES)`,
      );
    }
    if (rc !== 0) {
      throw new Error(
        `OpenGLContext: ${registerName} failed for pool_id ` +
          `'${poolId}' (rc=${rc}). Check the subprocess log for ` +
          `EGL/DMA-BUF import errors — typically a wrong DRM modifier ` +
          `or an unsupported pixel format.`,
      );
    }
    this.surfaceIds.set(poolId, surfaceId);
    this.surfaceTargets.set(poolId, target);
    // Hold the SDK handle so its FDs stay alive for the runtime's life.
    this.resolvedHandles.set(poolId, handle);
    return surfaceId;
  }

  private static surfacePoolId(
    surface: StreamlibSurface | string | bigint,
  ): string {
    if (typeof surface === "string") return surface;
    if (typeof surface === "bigint") return surface.toString();
    const id = (surface as { id?: bigint | string | number }).id;
    if (id === undefined) {
      throw new TypeError(
        `OpenGLContext: expected StreamlibSurface, string pool_id, or bigint — got ${
          typeof surface
        }`,
      );
    }
    return String(id);
  }

  /** Acquire write access. Returns a `using`-disposable guard whose
   * `view.glTextureId` is a `GL_TEXTURE_2D` valid in the adapter's EGL
   * context, which is current on the calling thread for the guard's
   * scope. On dispose the adapter drains GL (`glFinish`). */
  acquireWrite(
    surface: StreamlibSurface | string | bigint,
  ): OpenGLAccessGuard<OpenGLWriteView> {
    const poolId = OpenGLContext.surfacePoolId(surface);
    const surfaceId = this.resolveAndRegister(poolId);
    const textureId = Number(
      this.symbols.sldn_opengl_acquire_write(this.rt, surfaceId),
    );
    if (textureId === 0) {
      throw new Error(
        `OpenGLContext.acquireWrite: sldn_opengl_acquire_write returned 0 for surface '${poolId}'`,
      );
    }
    const symbols = this.symbols;
    const rt = this.rt;
    return {
      view: { glTextureId: textureId, target: GL_TEXTURE_2D },
      [Symbol.dispose]() {
        symbols.sldn_opengl_release_write(rt, surfaceId);
      },
    };
  }

  /** Acquire read access. Same shape as `acquireWrite`, but the
   * resulting texture is sample-only (multiple readers may coexist; no
   * writer can be active). */
  acquireRead(
    surface: StreamlibSurface | string | bigint,
  ): OpenGLAccessGuard<OpenGLReadView> {
    const poolId = OpenGLContext.surfacePoolId(surface);
    const surfaceId = this.resolveAndRegister(poolId, GL_TEXTURE_2D);
    const textureId = Number(
      this.symbols.sldn_opengl_acquire_read(this.rt, surfaceId),
    );
    if (textureId === 0) {
      throw new Error(
        `OpenGLContext.acquireRead: sldn_opengl_acquire_read returned 0 for surface '${poolId}'`,
      );
    }
    const symbols = this.symbols;
    const rt = this.rt;
    return {
      view: { glTextureId: textureId, target: GL_TEXTURE_2D },
      [Symbol.dispose]() {
        symbols.sldn_opengl_release_read(rt, surfaceId);
      },
    };
  }

  /** Publish a producer-side cross-process release for a surface this
   * OpenGL context has been writing to via `acquireWrite`.
   *
   * OpenGL writes don't touch the underlying `VkImage`'s Vulkan
   * tracker — and the OpenGL adapter has no Vulkan device handle of
   * its own. The engine-model answer (per
   * `docs/architecture/adapter-authoring.md` → Cross-process producer
   * composition) is to let the canonical Vulkan adapter own the QFOT
   * release barrier: the customer dual-registers the same surface
   * with both adapters, and OpenGL's release path delegates to
   * `VulkanContext.releaseForCrossProcess`.
   *
   * `vulkanCtx` is the subprocess's `VulkanContext` — typically
   * obtained alongside this `OpenGLContext` from the same runtime
   * context inside `setup`. The runtime must have been started with
   * surface-share registration carrying an exportable timeline
   * OPAQUE_FD per the host-side
   * `register_texture(... Some(timeline.as_ref()), ...)` pattern.
   *
   * Sequencing — call this *after* the `acquireWrite` `using` block
   * has exited so the adapter's `glFinish` on release has drained GL
   * writes through the DMA-BUF kernel backing. The Vulkan adapter's
   * QFOT release then issues a barrier on its imported `VkImage`
   * (bound to the same DMA-BUF) and publishes the layout via
   * surface-share, so any subsequent host-side consumer reaching
   * this surface through `GpuContext::resolve_videoframe_registration`
   * Path 2 sees the right source layout.
   *
   * `postReleaseLayout` is a Vulkan `VkImageLayout` enumerant as a
   * number. `GENERAL` is the right choice for OpenGL-backed surfaces.
   *
   * See `docs/learnings/cross-process-vkimage-layout.md` for the QFOT
   * vs bridging-fallback story; on NVIDIA Linux the host consumer
   * side rides the bridging fallback. */
  releaseForCrossProcess(
    surface: StreamlibSurface | string | bigint,
    vulkanCtx: VulkanContext,
    postReleaseLayout: number,
  ): void {
    // Delegate to VulkanContext.releaseForCrossProcess — that's where
    // release_to_foreign + surface-share update_layout already live
    // (#647). VulkanContext lazily registers the surface on its first
    // reference, so the customer doesn't need a no-op acquire first.
    vulkanCtx.releaseForCrossProcess(surface, postReleaseLayout);
  }

  /** Acquire read access against a surface registered under
   * `GL_TEXTURE_EXTERNAL_OES` — the path for sampler-only DMA-BUFs
   * (camera ring textures, linear surfaces on NVIDIA per
   * `docs/learnings/nvidia-egl-dmabuf-render-target.md`).
   *
   * First call for a `pool_id` registers the surface as EXTERNAL_OES;
   * subsequent calls return the same texture id. Mixing this with
   * `acquireRead` for the same `pool_id` is rejected (one binding
   * target per registration).
   *
   * `view.target` is `GL_TEXTURE_EXTERNAL_OES` — bind manually via
   * `glBindTexture(GL_TEXTURE_EXTERNAL_OES, view.glTextureId)` and
   * sample via `samplerExternalOES` in GLSL. On the adapter's desktop-
   * GL context, declare `#extension GL_OES_EGL_image_external : require`
   * and use the `texture2D(samplerExternalOES, vec2)` overload —
   * NVIDIA's desktop-GL driver does NOT register the unified
   * `texture(...)` overload for `samplerExternalOES` in
   * `#version 330 core`; that overload comes from
   * `GL_OES_EGL_image_external_essl3`, which requires a GLES context
   * (which this adapter does not create). */
  acquireReadExternalOes(
    surface: StreamlibSurface | string | bigint,
  ): OpenGLAccessGuard<OpenGLReadView> {
    const poolId = OpenGLContext.surfacePoolId(surface);
    const surfaceId = this.resolveAndRegister(poolId, GL_TEXTURE_EXTERNAL_OES);
    const textureId = Number(
      this.symbols.sldn_opengl_acquire_read(this.rt, surfaceId),
    );
    if (textureId === 0) {
      throw new Error(
        `OpenGLContext.acquireReadExternalOes: sldn_opengl_acquire_read ` +
          `returned 0 for surface '${poolId}'`,
      );
    }
    const symbols = this.symbols;
    const rt = this.rt;
    return {
      view: { glTextureId: textureId, target: GL_TEXTURE_EXTERNAL_OES },
      [Symbol.dispose]() {
        symbols.sldn_opengl_release_read(rt, surfaceId);
      },
    };
  }
}
