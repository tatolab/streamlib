// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Deno mirror of streamlib_adapter_abi.
 *
 * Provides SurfaceFormat / SurfaceUsage / AccessMode constants, the
 * SurfaceAdapter interface (scoped acquireRead / acquireWrite returning
 * `using`-disposable guards via [Symbol.dispose]), and the
 * StreamlibSurface descriptor offsets — locked against the Rust
 * `#[repr(C)]` layout via the twin test in surface_adapter_test.ts.
 */

/** ABI version major. Mirrors STREAMLIB_ADAPTER_ABI_VERSION in lib.rs. */
export const STREAMLIB_ADAPTER_ABI_VERSION = 1;

/** Maximum DMA-BUF planes the descriptor carries. */
export const MAX_DMA_BUF_PLANES = 4;

/** Mirror of Rust `SurfaceFormat` (#[repr(u32)]). */
export const SurfaceFormat = {
  Bgra8: 0,
  Rgba8: 1,
  Nv12: 2,
} as const;
export type SurfaceFormat = (typeof SurfaceFormat)[keyof typeof SurfaceFormat];

/** Mirror of Rust `SurfaceUsage` bitflags. */
export const SurfaceUsage = {
  RenderTarget: 1 << 0,
  Sampled: 1 << 1,
  CpuReadback: 1 << 2,
} as const;
export type SurfaceUsage = number;

/** Wire-format access mode used by the IPC and polyglot mirrors. */
export const AccessMode = {
  Read: 0,
  Write: 1,
} as const;
export type AccessMode = (typeof AccessMode)[keyof typeof AccessMode];

/**
 * Byte offsets and sizes of the `#[repr(C)] StreamlibSurface` struct
 * and its components. Locked against the Rust unit test in
 * libs/streamlib-adapter-abi/src/surface.rs and the Python mirror.
 *
 * Subprocess adapters use these to read fields out of a surface
 * descriptor passed across FFI via Deno.UnsafePointerView.
 */
export const SurfaceLayout = {
  /** SurfaceTransportHandle */
  TransportHandle: {
    Size: 96,
    Align: 8,
    Offsets: {
      planeCount: 0,
      dmaBufFds: 4,
      planeOffsets: 24,
      planeStrides: 56,
      drmFormatModifier: 88,
    },
  },
  /** SurfaceSyncState */
  SyncState: {
    Size: 56,
    Align: 8,
    Offsets: {
      timelineSemaphoreHandle: 0,
      timelineSemaphoreSyncFd: 8,
      padA: 12,
      lastAcquireValue: 16,
      lastReleaseValue: 24,
      currentImageLayout: 32,
      padB: 36,
      reserved: 40,
    },
  },
  /** StreamlibSurface (top-level) */
  Surface: {
    Size: 176,
    Align: 8,
    Offsets: {
      id: 0,
      width: 8,
      height: 12,
      format: 16,
      usage: 20,
      transport: 24,
      sync: 120,
    },
  },
} as const;

/** Customer-visible surface descriptor (the public fields only). */
export interface StreamlibSurface {
  readonly id: bigint;
  readonly width: number;
  readonly height: number;
  readonly format: SurfaceFormat;
  readonly usage: SurfaceUsage;
}

/**
 * DMA-BUF transport handle — fds, plane layout, modifier — read out of a
 * surface descriptor for adapter-internal import on the subprocess side.
 *
 * Customers never see this; only adapter implementations.
 */
export interface SurfaceTransportHandle {
  readonly planeCount: number;
  readonly dmaBufFds: readonly number[]; // length MAX_DMA_BUF_PLANES
  readonly planeOffsets: readonly bigint[]; // length MAX_DMA_BUF_PLANES
  readonly planeStrides: readonly bigint[]; // length MAX_DMA_BUF_PLANES
  readonly drmFormatModifier: bigint;
}

/**
 * Host-side timeline-semaphore + initial layout. Subprocess adapters
 * import `timelineSemaphoreSyncFd` via `vkImportSemaphoreFdKHR`.
 */
export interface SurfaceSyncState {
  readonly timelineSemaphoreHandle: bigint;
  readonly timelineSemaphoreSyncFd: number;
  readonly lastAcquireValue: bigint;
  readonly lastReleaseValue: bigint;
  readonly currentImageLayout: number;
}

/** Read-side view returned by `acquireRead`. Adapter-typed. */
export type ReadView = unknown;

/** Write-side view returned by `acquireWrite`. Adapter-typed. */
export type WriteView = unknown;

/**
 * RAII-style guard used with TC39 `using` blocks.
 *
 *   {
 *     using guard = adapter.acquireWrite(surface);
 *     guard.view.draw(...);
 *   }
 *   // [Symbol.dispose] runs here, releasing the surface.
 */
export interface SurfaceAccessGuard<V> extends Disposable {
  readonly view: V;
  readonly surfaceId: bigint;
}

/** Public ABI for a Deno streamlib surface adapter.
 *
 * Two acquisition flavors mirror the Rust trait:
 * - `acquireRead` / `acquireWrite` block until the timeline semaphore
 *   wait completes.
 * - `tryAcquireRead` / `tryAcquireWrite` return `null` immediately
 *   when the surface is contended; never block. Right shape for
 *   processor-graph nodes that must not stall their thread runner.
 */
export interface SurfaceAdapter<RView = ReadView, WView = WriteView> {
  acquireRead(surface: StreamlibSurface): SurfaceAccessGuard<RView>;
  acquireWrite(surface: StreamlibSurface): SurfaceAccessGuard<WView>;
  tryAcquireRead(surface: StreamlibSurface): SurfaceAccessGuard<RView> | null;
  tryAcquireWrite(surface: StreamlibSurface): SurfaceAccessGuard<WView> | null;
}

/**
 * Read a `StreamlibSurface` descriptor's public fields out of a
 * Deno.UnsafePointerView at `ptr`. Adapter authors use this when
 * receiving a descriptor from native code via FFI.
 */
export function readStreamlibSurface(
  view: Deno.UnsafePointerView,
): StreamlibSurface {
  const o = SurfaceLayout.Surface.Offsets;
  return {
    id: view.getBigUint64(o.id),
    width: view.getUint32(o.width),
    height: view.getUint32(o.height),
    format: view.getUint32(o.format) as SurfaceFormat,
    usage: view.getUint32(o.usage) as SurfaceUsage,
  };
}

/**
 * Read the embedded `SurfaceTransportHandle` out of a `StreamlibSurface`
 * descriptor. Adapter implementations use this when they need DMA-BUF fds
 * and the modifier to import the backing.
 */
export function readSurfaceTransportHandle(
  view: Deno.UnsafePointerView,
): SurfaceTransportHandle {
  const base = SurfaceLayout.Surface.Offsets.transport;
  const o = SurfaceLayout.TransportHandle.Offsets;
  const fds: number[] = [];
  const offs: bigint[] = [];
  const strides: bigint[] = [];
  for (let i = 0; i < MAX_DMA_BUF_PLANES; i++) {
    fds.push(view.getInt32(base + o.dmaBufFds + i * 4));
    offs.push(view.getBigUint64(base + o.planeOffsets + i * 8));
    strides.push(view.getBigUint64(base + o.planeStrides + i * 8));
  }
  return {
    planeCount: view.getUint32(base + o.planeCount),
    dmaBufFds: fds,
    planeOffsets: offs,
    planeStrides: strides,
    drmFormatModifier: view.getBigUint64(base + o.drmFormatModifier),
  };
}

/**
 * Read the embedded `SurfaceSyncState` out of a `StreamlibSurface`
 * descriptor. Adapter implementations import the sync-fd to participate
 * in the host-side timeline.
 */
export function readSurfaceSyncState(
  view: Deno.UnsafePointerView,
): SurfaceSyncState {
  const base = SurfaceLayout.Surface.Offsets.sync;
  const o = SurfaceLayout.SyncState.Offsets;
  return {
    timelineSemaphoreHandle: view.getBigUint64(
      base + o.timelineSemaphoreHandle,
    ),
    timelineSemaphoreSyncFd: view.getInt32(base + o.timelineSemaphoreSyncFd),
    lastAcquireValue: view.getBigUint64(base + o.lastAcquireValue),
    lastReleaseValue: view.getBigUint64(base + o.lastReleaseValue),
    currentImageLayout: view.getInt32(base + o.currentImageLayout),
  };
}
