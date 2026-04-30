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
 * The view is valid only inside the `await using` scope — after the
 * scope exits, the adapter releases its guard and the host pipeline
 * is free to overwrite the buffer. If you need to retain the tensor
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

/** Async-disposable guard returned by `acquireRead` / `acquireWrite`.
 * `await using` runs `[Symbol.asyncDispose]` at scope exit, which
 * releases the adapter guard (so the timeline can advance) and
 * conditionally calls the DLPack deleter (only if `consume()` was
 * NOT invoked on the view). */
export interface CudaAccessGuard<V> extends AsyncDisposable {
  readonly view: V;
}

/** Minimal subset of `GpuContextLimitedAccess` the cuda runtime needs. */
export interface CudaGpuLimitedAccess {
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
 *     await using guard = await ctx.acquireRead(surface);
 *     // Hand guard.view.dlpackPtr to a native consumer if one exists.
 *     // Otherwise leave it to the dispose path to clean up.
 */
export class CudaContext {
  private readonly gpu: CudaGpuLimitedAccess;
  // deno-lint-ignore no-explicit-any
  private readonly symbols: any;
  private readonly rt: Deno.PointerObject;
  private readonly surfaceIds = new Map<string, bigint>();
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
  ): Promise<CudaAccessGuard<CudaReadView>> {
    return this._acquire(surface, false, true) as Promise<
      CudaAccessGuard<CudaReadView>
    >;
  }

  acquireWrite(
    surface: StreamlibSurface | string | bigint | number,
  ): Promise<CudaAccessGuard<CudaWriteView>> {
    return this._acquire(surface, true, true) as Promise<
      CudaAccessGuard<CudaWriteView>
    >;
  }

  tryAcquireRead(
    surface: StreamlibSurface | string | bigint | number,
  ): Promise<CudaAccessGuard<CudaReadView> | null> {
    return this._acquire(surface, false, false) as Promise<
      CudaAccessGuard<CudaReadView> | null
    >;
  }

  tryAcquireWrite(
    surface: StreamlibSurface | string | bigint | number,
  ): Promise<CudaAccessGuard<CudaWriteView> | null> {
    return this._acquire(surface, true, false) as Promise<
      CudaAccessGuard<CudaWriteView> | null
    >;
  }

  private _acquire(
    surface: StreamlibSurface | string | bigint | number,
    write: boolean,
    blocking: boolean,
  ): Promise<CudaAccessGuard<CudaReadView | CudaWriteView> | null> {
    // The cdylib's acquire is synchronous (no per-acquire IPC) — wrap
    // in a Promise so the API mirrors cpu_readback's `await using` shape.
    return new Promise((resolve, reject) => {
      try {
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
          resolve(null);
          return;
        }
        if (rc !== RC_OK) {
          reject(
            new Error(
              `CudaContext.${blocking ? "" : "try_"}acquire_${
                write ? "write" : "read"
              }: rc=${rc} for surface '${poolId}'`,
            ),
          );
          return;
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

        resolve({
          view,
          [Symbol.asyncDispose]: () => {
            // Free the DLManagedTensor heap allocation only if the
            // consumer didn't claim it. After `consume()`, the consumer
            // is responsible — calling the deleter here would
            // double-free the producer-side state.
            if (!consumed) {
              try {
                callDlpackDeleter(dlpackPtr);
              } catch (_e) {
                // Deleter failure is logged on the cdylib side; we
                // can't propagate from a dispose path without breaking
                // the scope contract.
              }
            }
            if (writeMode) {
              symbols.sldn_cuda_release_write(rt, surfaceIdSnapshot);
            } else {
              symbols.sldn_cuda_release_read(rt, surfaceIdSnapshot);
            }
            return Promise.resolve();
          },
        });
      } catch (e) {
        reject(e);
      }
    });
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
}
