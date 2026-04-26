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
    Size: 32,
    Align: 8,
    Offsets: {
      timelineSemaphore: 0,
      lastAcquireValue: 8,
      lastReleaseValue: 16,
      currentImageLayout: 24,
      pad: 28,
    },
  },
  /** StreamlibSurface (top-level) */
  Surface: {
    Size: 152,
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

/** Public ABI for a Deno streamlib surface adapter. */
export interface SurfaceAdapter<RView = ReadView, WView = WriteView> {
  acquireRead(surface: StreamlibSurface): SurfaceAccessGuard<RView>;
  acquireWrite(surface: StreamlibSurface): SurfaceAccessGuard<WView>;
  traitVersion(): number;
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
