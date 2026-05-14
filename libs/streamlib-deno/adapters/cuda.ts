// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * CUDA surface adapter — Deno customer-facing API.
 *
 * Mirrors the Rust crate `streamlib-adapter-cuda` (#587 / #588). The
 * Deno subprocess delegates to `streamlib-deno-native`'s `sldn_cuda_*`
 * FFI surface, which wraps `CudaSurfaceAdapter<ConsumerVulkanDevice>`
 * plus `cudaImportExternalMemory` / `cudaImportExternalSemaphore`
 * against the host-allocated OPAQUE_FD `VkBuffer` + timeline semaphore.
 * Per-acquire control flow:
 *
 * 1. The Deno SDK looks the host's pre-registered cuda surface up via
 *    surface-share once (`sldn_cuda_register_surface`). The OPAQUE_FD
 *    memory + timeline FDs enter the cdylib's address space, get
 *    imported into Vulkan via `streamlib-consumer-rhi` AND re-imported
 *    into CUDA via `cudaImportExternalMemory` /
 *    `cudaImportExternalSemaphore`. The CUDA device pointer
 *    (`cudaExternalMemoryGetMappedBuffer`) is cached for the surface's
 *    lifetime.
 * 2. Every `acquireRead` / `acquireWrite` waits on the imported timeline
 *    (Vulkan-side via the adapter; CUDA-side via
 *    `cudaWaitExternalSemaphoresAsync_v2` so CUDA driver state is in
 *    sync with Vulkan's view of the kernel timeline) and hands back a
 *    raw `*mut DLManagedTensor` pointer plus a `consume()` helper.
 *
 * **Deno's DLPack story is less mature than Python's.** PyTorch /
 * NumPy / JAX expose `from_dlpack` consumers; Deno's ML ecosystem
 * (TensorFlow.js, ONNX-Runtime-Web, third-party WebGPU bindings) does
 * not yet have a native `from_dlpack` that takes a `DLManagedTensor*`
 * shape. This wrapper still emits the spec-compliant capsule pointer
 * so future consumers can plug in zero-copy; today the typical use is
 * to hand the pointer to a custom native module (a separate Deno
 * `dlopen`'d cdylib that knows how to consume DLPack) or to drive
 * host-side validation.
 *
 * There is no per-acquire IPC — the host's pipeline is expected to
 * write into the OPAQUE_FD buffer and signal the shared timeline
 * ambiently. Customers who need an explicit host-side trigger should
 * use `streamlib.adapters.cpu_readback` instead.
 */

import {
  STREAMLIB_ADAPTER_ABI_VERSION,
  type StreamlibSurface,
  type SurfaceFormat,
} from "../surface_adapter.ts";

export { STREAMLIB_ADAPTER_ABI_VERSION };

/** `sldn_cuda_*` return values — must match the cdylib's
 * `SLDN_CUDA_OK` / `_ERR` / `_CONTENDED` constants. */
const RC_OK = 0;
const RC_CONTENDED = 1;

/** DLPack `DLDeviceType` discriminants — kDLCUDA = 2, kDLCUDAHost = 3. */
const DEVICE_TYPE_CUDA = 2;
const DEVICE_TYPE_CUDA_HOST = 3;

/** Image-path format discriminants on
 * [`SldnCudaImageView::format`] — wire ABI mirroring the cdylib's
 * `SLDN_CUDA_FORMAT_*` constants. The CUDA image flavor is constrained
 * to the four-channel R8/R16/R32 subset accepted by
 * `cudaExternalMemoryGetMappedMipmappedArray`. */
export enum CudaImageFormat {
  Rgba8Unorm = 0,
  Rgba16Float = 1,
  Rgba32Float = 2,
}

/** `SlpnCudaView` `#[repr(C)]` layout, pinned by the cdylib's
 * `sldn_cuda_view_layout_matches_spec_64bit` test:
 *   size                 : u64    @ 0   (8 bytes)
 *   device_ptr           : u64    @ 8   (8)
 *   device_type          : i32    @ 16  (4)
 *   device_id            : i32    @ 20  (4)
 *   dlpack_managed_tensor: ptr    @ 24  (8 bytes on 64-bit)
 * Total = 32 bytes. */
const VIEW_STRUCT_SIZE = 32;

/** `DLManagedTensor` deleter offset — pinned by the layout regression
 * test in `streamlib-adapter-cuda::dlpack`:
 *   dl_tensor   : 48 bytes @ 0
 *   manager_ctx : 8 bytes @ 48
 *   deleter     : 8 bytes @ 56  ← function pointer void(*)(DLManagedTensor*)
 */
const DLPACK_DELETER_OFFSET = 56;

/** Read-side view inside an `acquireRead` scope.
 *
 * `dlpackPtr` is a raw `*mut DLManagedTensor` — pass it to a native
 * DLPack consumer for zero-copy access, or call `consume()` to mark
 * that ownership has transferred (which prevents the dispose path
 * from double-freeing). The capsule's underlying CUDA device memory
 * lives until either (a) the consumer calls the deleter or (b) the
 * dispose path calls it on consumer-less drops.
 *
 * The view is valid only inside the `using` scope — after the scope
 * exits, the adapter releases its guard and the host pipeline is
 * free to overwrite the buffer. If you need to retain the tensor
 * beyond the scope, hand the `dlpackPtr` to a consumer that copies
 * the data out (or that itself outlives the scope and accepts
 * ownership).
 */
export interface CudaReadView {
  readonly format: SurfaceFormat;
  /** Buffer size in bytes. */
  readonly size: bigint;
  /** CUDA device pointer (`CUdeviceptr` cast to `u64`). */
  readonly devicePtr: bigint;
  /** DLPack `DLDeviceType` discriminant (`kDLCUDA` = 2 or
   * `kDLCUDAHost` = 3). */
  readonly deviceType: number;
  /** CUDA device ordinal. Single-GPU rigs always see `0`. */
  readonly deviceId: number;
  /** Raw `*mut DLManagedTensor`. Pass to a native DLPack consumer or
   * call [`consume`] before scope exit. */
  readonly dlpackPtr: Deno.PointerObject;
  /** Mark that an external consumer has taken ownership of the DLPack
   * capsule (and is responsible for calling the deleter). After
   * `consume()`, the dispose path will NOT call the deleter — calling
   * `consume()` on a capsule the consumer did NOT claim will leak the
   * `DLManagedTensor` heap allocation. */
  consume(): void;
}

/** Write-side view. Same shape as [`CudaReadView`]; CUDA writes land
 * in the OPAQUE_FD buffer and become visible to the host pipeline
 * after the adapter guard is released. */
export interface CudaWriteView {
  readonly format: SurfaceFormat;
  readonly size: bigint;
  readonly devicePtr: bigint;
  readonly deviceType: number;
  readonly deviceId: number;
  readonly dlpackPtr: Deno.PointerObject;
  consume(): void;
}

/** `SldnCudaImageView` `#[repr(C)]` layout, pinned by the cdylib's
 * `sldn_cuda_image_view_layout_matches_spec_64bit` test:
 *   cuda_object_handle: u64    @ 0   (8 bytes)
 *   width             : u32    @ 8   (4)
 *   height            : u32    @ 12  (4)
 *   format            : i32    @ 16  (4)
 *   _reserved         : [u8;12]@ 20  (12 bytes)
 * Total = 32 bytes — same wire width as `SldnCudaView`. */
const IMAGE_VIEW_STRUCT_SIZE = 32;

/** Read-side view inside an `acquireTexture` scope.
 *
 * `handle` is a raw `cudaTextureObject_t` — pass it to a native module
 * (a separate `dlopen`'d cdylib that knows how to sample CUDA
 * textures) or drive host-side validation. There's no DLPack capsule
 * on this path because the underlying `cudaMipmappedArray_t` is opaque
 * and not DLPack-shaped.
 *
 * The view is valid only inside the `using` scope. After scope exit,
 * the cdylib calls `cudaDestroyTextureObject` on `handle` and
 * releases the adapter's read guard — do NOT retain `handle` past
 * the scope.
 */
export interface CudaTextureView {
  /** Raw `cudaTextureObject_t` (typedef'd to `c_ulonglong`). */
  readonly handle: bigint;
  /** Image width in pixels. */
  readonly width: number;
  /** Image height in pixels. */
  readonly height: number;
  /** Format discriminant from [`CudaImageFormat`] — the cdylib has
   * already validated the registration; this is informational for
   * customer kernels that need to know element size for sampling. */
  readonly format: CudaImageFormat;
}

/** Write-side view inside an `acquireSurface` scope.
 *
 * Same shape as [`CudaTextureView`] but `handle` is a raw
 * `cudaSurfaceObject_t`. Kernels that produce new frames can
 * `surf2Dwrite` against this handle and the writes land in the
 * host-shared OPAQUE_FD `VkImage`'s backing memory.
 *
 * Same lifetime rules as [`CudaTextureView`] — `handle` is destroyed
 * at scope exit.
 */
export interface CudaSurfaceView {
  readonly handle: bigint;
  readonly width: number;
  readonly height: number;
  readonly format: CudaImageFormat;
}

/** Disposable guard returned by `acquireRead` / `acquireWrite`.
 * `using` runs `[Symbol.dispose]` at scope exit, which releases the
 * adapter guard (so the timeline can advance) and conditionally calls
 * the DLPack deleter (only if `consume()` was NOT invoked on the view).
 *
 * Sync-disposable rather than async-disposable because the cuda
 * adapter has no per-acquire IPC — the cdylib's `sldn_cuda_*` calls
 * are synchronous all the way down. (Contrast with `cpu_readback`,
 * which goes async because every acquire round-trips an escalate-IPC
 * trigger.) */
export interface CudaAccessGuard<V> extends Disposable {
  readonly view: V;
}

/** Minimal subset of `GpuContextLimitedAccess` the cuda runtime needs. */
export interface CudaGpuLimitedAccess {
  resolveSurface(poolId: string): {
    readonly nativeHandlePtr: Deno.PointerObject | null;
    release(): void;
  };
  /** Publish the producer-side post-release `VkImageLayout` for
   * `poolId` via the surface-share `update_layout` op. Called by
   * [`CudaContext.releaseForCrossProcess`] after a CUDA-side write
   * cycle to publish the next layout so the host consumer's
   * `acquire_from_foreign` Path 2 sees the right source. */
  updateImageLayout(poolId: string, layout: number): void;
  // deno-lint-ignore no-explicit-any
  readonly nativeLib: { readonly symbols: any };
}

let _SURFACE_ID_COUNTER = 0n;
function nextSurfaceId(): bigint {
  _SURFACE_ID_COUNTER += 1n;
  return _SURFACE_ID_COUNTER;
}

let _SHARED_INSTANCE: CudaContext | null = null;

function surfacePoolId(
  surface: StreamlibSurface | string | bigint | number,
): string {
  if (typeof surface === "string") return surface;
  if (typeof surface === "bigint") return surface.toString();
  if (typeof surface === "number") return String(Math.trunc(surface));
  const id = (surface as { id?: bigint | string | number }).id;
  if (id === undefined) {
    throw new TypeError(
      `CudaContext: expected StreamlibSurface or pool_id, got ${typeof surface}`,
    );
  }
  return String(id);
}

function surfaceFormatFrom(
  surface: StreamlibSurface | string | bigint | number,
): SurfaceFormat {
  if (typeof surface === "object" && surface !== null) {
    const fmt = (surface as { format?: number }).format;
    if (fmt !== undefined) return fmt as SurfaceFormat;
  }
  return 0 as SurfaceFormat; // BGRA8 default
}

/** Function-pointer signature for DLPack deleters. */
const DLPACK_DELETER_DEFINITION = {
  parameters: ["pointer"] as const,
  result: "void" as const,
} as const;

/** Read the function pointer at offset 56 inside a DLManagedTensor and
 * call it. The deleter frees the producer-side state (shape, strides,
 * manager_ctx, the ManagedTensor itself); after the call the pointer
 * is invalidated. */
function callDlpackDeleter(dlpackPtr: Deno.PointerObject): void {
  const view = new Deno.UnsafePointerView(dlpackPtr);
  const deleterAddr = view.getBigUint64(DLPACK_DELETER_OFFSET);
  if (deleterAddr === 0n) return;
  const deleterPtr = Deno.UnsafePointer.create(deleterAddr);
  if (deleterPtr === null) return;
  // `Deno.UnsafeFnPointer` infers its argument type from the pointer's
  // type parameter; cast through `unknown` to satisfy the inferred
  // generic since `Deno.PointerObject` defaults to `<unknown>`.
  const deleterFn = new Deno.UnsafeFnPointer(
    deleterPtr as Deno.PointerObject<typeof DLPACK_DELETER_DEFINITION>,
    DLPACK_DELETER_DEFINITION,
  );
  // SAFETY: `dlpackPtr` is the same pointer the deleter expects. The
  // deleter is `void(*)(DLManagedTensor*)` per DLPack spec.
  deleterFn.call(dlpackPtr);
}

/** Customer-facing context for the Deno subprocess SDK.
 *
 *     using guard = ctx.acquireRead(surface);
 *     // Hand guard.view.dlpackPtr to a native consumer if one exists.
 *     // Otherwise leave it to the dispose path to clean up.
 */
export class CudaContext {
  private readonly gpu: CudaGpuLimitedAccess;
  // deno-lint-ignore no-explicit-any
  private readonly symbols: any;
  private readonly rt: Deno.PointerObject;
  private readonly surfaceIds = new Map<string, bigint>();
  /** Image-flavored registration map. Separate from
   * [`surfaceIds`] because a given pool_id is exclusively one
   * flavor — the cdylib's adapter rejects mixing acquire paths
   * against a single surface. */
  private readonly imageSurfaceIds = new Map<string, bigint>();
  private readonly resolvedHandles = new Map<
    string,
    ReturnType<CudaGpuLimitedAccess["resolveSurface"]>
  >();

  constructor(gpu: CudaGpuLimitedAccess) {
    this.gpu = gpu;
    this.symbols = gpu.nativeLib.symbols;
    const rtPtr = this.symbols.sldn_cuda_runtime_new();
    if (rtPtr === null) {
      throw new Error(
        "CudaContext: sldn_cuda_runtime_new returned NULL — the subprocess " +
          "could not bring up a Vulkan device + CUDA context. Check that " +
          "libvulkan.so.1, libcuda.so.1, and libcudart.so are installed and " +
          "that the driver supports VK_KHR_external_memory_fd, " +
          "VK_EXT_external_memory_dma_buf, and VK_KHR_external_semaphore_fd. " +
          "See the subprocess log for the underlying error.",
      );
    }
    this.rt = rtPtr;
  }

  static fromRuntime(
    ctx: { readonly gpuLimitedAccess: CudaGpuLimitedAccess },
  ): CudaContext {
    if (_SHARED_INSTANCE === null) {
      _SHARED_INSTANCE = new CudaContext(ctx.gpuLimitedAccess);
    }
    return _SHARED_INSTANCE;
  }

  /** Close the cdylib runtime. After `close()` the context is unusable;
   * mostly for tests. */
  close(): void {
    this.symbols.sldn_cuda_runtime_free(this.rt);
    _SHARED_INSTANCE = null;
  }

  private resolveAndRegister(poolId: string): bigint {
    const cached = this.surfaceIds.get(poolId);
    if (cached !== undefined) return cached;
    const handle = this.gpu.resolveSurface(poolId);
    const handlePtr = handle.nativeHandlePtr;
    if (handlePtr === null) {
      throw new Error(
        `CudaContext: resolveSurface('${poolId}') returned a handle with a null native pointer`,
      );
    }
    const surfaceId = nextSurfaceId();
    const rc: number = this.symbols.sldn_cuda_register_surface(
      this.rt,
      surfaceId,
      handlePtr,
    );
    if (rc !== RC_OK) {
      throw new Error(
        `CudaContext: register_surface failed for pool_id ` +
          `'${poolId}' (rc=${rc}). Common causes: host registered the ` +
          `surface as DMA-BUF rather than OPAQUE_FD (cuda requires ` +
          `handle_type=opaque_fd); host did not attach an exportable ` +
          `timeline semaphore (cuda requires sync_fd); or libcudart / ` +
          `libcuda.so missing.`,
      );
    }
    this.surfaceIds.set(poolId, surfaceId);
    this.resolvedHandles.set(poolId, handle);
    return surfaceId;
  }

  acquireRead(
    surface: StreamlibSurface | string | bigint | number,
  ): CudaAccessGuard<CudaReadView> {
    return this._acquire(surface, false, true) as CudaAccessGuard<
      CudaReadView
    >;
  }

  acquireWrite(
    surface: StreamlibSurface | string | bigint | number,
  ): CudaAccessGuard<CudaWriteView> {
    return this._acquire(surface, true, true) as CudaAccessGuard<
      CudaWriteView
    >;
  }

  tryAcquireRead(
    surface: StreamlibSurface | string | bigint | number,
  ): CudaAccessGuard<CudaReadView> | null {
    return this._acquire(surface, false, false) as
      | CudaAccessGuard<CudaReadView>
      | null;
  }

  tryAcquireWrite(
    surface: StreamlibSurface | string | bigint | number,
  ): CudaAccessGuard<CudaWriteView> | null {
    return this._acquire(surface, true, false) as
      | CudaAccessGuard<CudaWriteView>
      | null;
  }

  private _acquire(
    surface: StreamlibSurface | string | bigint | number,
    write: boolean,
    blocking: boolean,
  ): CudaAccessGuard<CudaReadView | CudaWriteView> | null {
    const poolId = surfacePoolId(surface);
    const format = surfaceFormatFrom(surface);
    const surfaceId = this.resolveAndRegister(poolId);
    const buf = new Uint8Array(VIEW_STRUCT_SIZE);
    const fn = blocking
      ? (write
        ? this.symbols.sldn_cuda_acquire_write
        : this.symbols.sldn_cuda_acquire_read)
      : (write
        ? this.symbols.sldn_cuda_try_acquire_write
        : this.symbols.sldn_cuda_try_acquire_read);
    const rc: number = fn(this.rt, surfaceId, Deno.UnsafePointer.of(buf));
    if (rc === RC_CONTENDED) {
      return null;
    }
    if (rc !== RC_OK) {
      throw new Error(
        `CudaContext.${blocking ? "" : "try_"}acquire_${
          write ? "write" : "read"
        }: rc=${rc} for surface '${poolId}'`,
      );
    }

    const view = this.parseView(buf, format, write);
    const surfaceIdSnapshot = surfaceId;
    const symbols = this.symbols;
    const rt = this.rt;
    const writeMode = write;
    const dlpackPtr = view.dlpackPtr;
    let consumed = false;
    // The view's `consume()` flips the consumed flag — must be set
    // BEFORE constructing the returned guard so JS code that does
    // `view.consume()` inside the scope flows correctly through the
    // dispose path.
    (view as { consume: () => void }).consume = () => {
      consumed = true;
    };

    return {
      view,
      [Symbol.dispose]: () => {
        // Free the DLManagedTensor heap allocation only if the
        // consumer didn't claim it. After `consume()`, the consumer
        // is responsible — calling the deleter here would double-free
        // the producer-side state.
        if (!consumed) {
          try {
            callDlpackDeleter(dlpackPtr);
          } catch (_e) {
            // Deleter failure is logged on the cdylib side; we can't
            // propagate from a dispose path without breaking the
            // scope contract.
          }
        }
        if (writeMode) {
          symbols.sldn_cuda_release_write(rt, surfaceIdSnapshot);
        } else {
          symbols.sldn_cuda_release_read(rt, surfaceIdSnapshot);
        }
      },
    };
  }

  private parseView(
    buf: Uint8Array,
    format: SurfaceFormat,
    writable: boolean,
  ): CudaReadView | CudaWriteView {
    const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    const size = dv.getBigUint64(0, true);
    const devicePtr = dv.getBigUint64(8, true);
    const deviceType = dv.getInt32(16, true);
    const deviceId = dv.getInt32(20, true);
    const dlpackAddrLow = dv.getUint32(24, true);
    const dlpackAddrHigh = dv.getUint32(28, true);
    const dlpackAddr = (BigInt(dlpackAddrHigh) << 32n) | BigInt(dlpackAddrLow);
    if (dlpackAddr === 0n) {
      throw new Error(
        "CudaContext: cdylib returned null DLPack managed tensor pointer",
      );
    }
    const dlpackPtr = Deno.UnsafePointer.create(dlpackAddr);
    if (dlpackPtr === null) {
      throw new Error(
        "CudaContext: DLPack managed tensor pointer could not be wrapped",
      );
    }
    if (deviceType !== DEVICE_TYPE_CUDA && deviceType !== DEVICE_TYPE_CUDA_HOST) {
      throw new Error(
        `CudaContext: cdylib returned unexpected deviceType=${deviceType} ` +
          `(expected kDLCUDA=2 or kDLCUDAHost=3)`,
      );
    }
    // `consume` is filled in by `_acquire` once the closure has scope
    // over the `consumed` flag. Stub here keeps the type checker happy.
    const view = {
      format,
      size,
      devicePtr,
      deviceType,
      deviceId,
      dlpackPtr,
      consume: () => {
        throw new Error(
          "CudaContext: consume() must be called on the view returned by acquire, not the parsed bytes",
        );
      },
    };
    return writable
      ? (view as unknown as CudaWriteView)
      : (view as unknown as CudaReadView);
  }

  // ----- Image-flavored API ----------------------------------------

  /** Acquire read access to an image-flavored CUDA surface as a
   * `cudaTextureObject_t`. The texture object is constructed fresh
   * inside the cdylib on every call (per-acquire — first-cut design
   * per the issue body's AI-Agent-Notes) and destroyed on scope exit.
   *
   *     using guard = ctx.acquireTexture(surface);
   *     const handle = guard.view.handle; // pass into a native module
   */
  acquireTexture(
    surface: StreamlibSurface | string | bigint | number,
  ): CudaAccessGuard<CudaTextureView> {
    return this._acquireImage(surface, false, true) as CudaAccessGuard<
      CudaTextureView
    >;
  }

  /** Acquire write access to an image-flavored CUDA surface as a
   * `cudaSurfaceObject_t`. */
  acquireSurface(
    surface: StreamlibSurface | string | bigint | number,
  ): CudaAccessGuard<CudaSurfaceView> {
    return this._acquireImage(surface, true, true) as CudaAccessGuard<
      CudaSurfaceView
    >;
  }

  /** Non-blocking variant of [`acquireTexture`] — returns `null` on
   * contention rather than blocking. */
  tryAcquireTexture(
    surface: StreamlibSurface | string | bigint | number,
  ): CudaAccessGuard<CudaTextureView> | null {
    return this._acquireImage(surface, false, false) as
      | CudaAccessGuard<CudaTextureView>
      | null;
  }

  /** Non-blocking variant of [`acquireSurface`]. */
  tryAcquireSurface(
    surface: StreamlibSurface | string | bigint | number,
  ): CudaAccessGuard<CudaSurfaceView> | null {
    return this._acquireImage(surface, true, false) as
      | CudaAccessGuard<CudaSurfaceView>
      | null;
  }

  /** Publish the post-release `VkImageLayout` to surface-share so the
   * next cross-process consumer's `acquire_from_foreign` Path 2 sees
   * the right source layout.
   *
   * Unlike `OpenGLContext.releaseForCrossProcess` — which takes a
   * `VulkanContext` and delegates to its `releaseForCrossProcess`
   * because OpenGL writes don't touch the underlying `VkImage`'s
   * Vulkan tracker — the CUDA shim takes **no** `vulkanCtx`
   * parameter. CUDA writes via `cudaSurfaceObject_t` against the
   * imported mipmapped array; the cdylib has no host `VkDevice` to
   * issue a QFOT release barrier against (per the consumer-rhi
   * carve-out), and the pairwise sync runs entirely on
   * `cudaSignalExternalSemaphoresAsync` /
   * `cudaWaitExternalSemaphoresAsync` against the imported timeline.
   * The host consumer's
   * `GpuContext::resolve_videoframe_registration` Path 2 acquire
   * handles its own barriers via QFOT-acquire (Mesa) or
   * bridging-from-UNDEFINED (NVIDIA) — independent of what the CUDA
   * producer did.
   *
   * What this shim does is just the **layout publish**: update the
   * surface-share daemon's per-surface `current_image_layout` field
   * so the next consumer sees the right source layout. The timeline
   * signal happens naturally on scope exit (the cdylib's adapter
   * advances the timeline as part of releasing the write guard).
   *
   * Call this *after* the matching `acquireSurface` `using` scope
   * has exited so the CUDA stream's writes have drained through to
   * the GPU and the timeline has been signaled.
   *
   * Customers running scenario (a) from the issue body's design
   * clarification (pure-CUDA AI consumer that just reads, drops the
   * guard) do NOT call this method — the host is the producer and
   * handles its own release barriers via `VulkanSurfaceAdapter`.
   *
   * `postReleaseLayout` is a Vulkan `VkImageLayout` enumerant as a
   * number. `GENERAL` is the safest default for cross-process
   * handoffs — the consumer's `acquire_from_foreign` re-transitions
   * to whatever layout it actually needs. */
  releaseForCrossProcess(
    surface: StreamlibSurface | string | bigint | number,
    postReleaseLayout: number,
  ): void {
    const poolId = surfacePoolId(surface);
    this.gpu.updateImageLayout(poolId, postReleaseLayout);
  }

  private resolveAndRegisterImage(poolId: string): bigint {
    const cached = this.imageSurfaceIds.get(poolId);
    if (cached !== undefined) return cached;
    const handle = this.gpu.resolveSurface(poolId);
    const handlePtr = handle.nativeHandlePtr;
    if (handlePtr === null) {
      throw new Error(
        `CudaContext: resolveSurface('${poolId}') returned a handle with a null native pointer`,
      );
    }
    const surfaceId = nextSurfaceId();
    const rc: number = this.symbols.sldn_cuda_register_image_surface(
      this.rt,
      surfaceId,
      handlePtr,
    );
    if (rc !== RC_OK) {
      throw new Error(
        `CudaContext: register_image_surface failed for pool_id ` +
          `'${poolId}' (rc=${rc}). Common causes: host registered the ` +
          `surface as a buffer (DLPack path), not an image; host's ` +
          `image format is outside the CUDA-mappable subset ` +
          `(Rgba8Unorm / Rgba16Float / Rgba32Float); host did not ` +
          `attach an exportable timeline semaphore.`,
      );
    }
    this.imageSurfaceIds.set(poolId, surfaceId);
    this.resolvedHandles.set(poolId, handle);
    return surfaceId;
  }

  private _acquireImage(
    surface: StreamlibSurface | string | bigint | number,
    write: boolean,
    blocking: boolean,
  ): CudaAccessGuard<CudaTextureView | CudaSurfaceView> | null {
    const poolId = surfacePoolId(surface);
    const surfaceId = this.resolveAndRegisterImage(poolId);
    const buf = new Uint8Array(IMAGE_VIEW_STRUCT_SIZE);
    const fn = blocking
      ? (write
        ? this.symbols.sldn_cuda_acquire_surface
        : this.symbols.sldn_cuda_acquire_texture)
      : (write
        ? this.symbols.sldn_cuda_try_acquire_surface
        : this.symbols.sldn_cuda_try_acquire_texture);
    const rc: number = fn(this.rt, surfaceId, Deno.UnsafePointer.of(buf));
    if (rc === RC_CONTENDED) {
      return null;
    }
    if (rc !== RC_OK) {
      throw new Error(
        `CudaContext.${blocking ? "" : "try_"}acquire_${
          write ? "surface" : "texture"
        }: rc=${rc} for surface '${poolId}'`,
      );
    }

    const view = this.parseImageView(buf, write);
    const surfaceIdSnapshot = surfaceId;
    const handleSnapshot = view.handle;
    const symbols = this.symbols;
    const rt = this.rt;
    const writeMode = write;

    return {
      view,
      [Symbol.dispose]: () => {
        // Thread the customer's handle back so the cdylib destroys
        // this view's cudaTextureObject_t / cudaSurfaceObject_t —
        // not some other concurrent reader's.
        if (writeMode) {
          symbols.sldn_cuda_release_surface(
            rt,
            surfaceIdSnapshot,
            handleSnapshot,
          );
        } else {
          symbols.sldn_cuda_release_texture(
            rt,
            surfaceIdSnapshot,
            handleSnapshot,
          );
        }
      },
    };
  }

  private parseImageView(
    buf: Uint8Array,
    writable: boolean,
  ): CudaTextureView | CudaSurfaceView {
    const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    const handle = dv.getBigUint64(0, true);
    const width = dv.getUint32(8, true);
    const height = dv.getUint32(12, true);
    const format = dv.getInt32(16, true);
    if (handle === 0n) {
      throw new Error(
        "CudaContext: cdylib returned null cuda object handle",
      );
    }
    if (
      format !== CudaImageFormat.Rgba8Unorm &&
      format !== CudaImageFormat.Rgba16Float &&
      format !== CudaImageFormat.Rgba32Float
    ) {
      throw new Error(
        `CudaContext: cdylib returned unexpected format discriminant ` +
          `${format} on the image view — the wire ABI may have drifted`,
      );
    }
    const view = {
      handle,
      width,
      height,
      format: format as CudaImageFormat,
    };
    return writable
      ? (view as unknown as CudaSurfaceView)
      : (view as unknown as CudaTextureView);
  }
}
