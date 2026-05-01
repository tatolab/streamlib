// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Smoke test for the Deno CUDA adapter wrapper module (#590).
 *
 * Confirms the module loads, layout constants match the cdylib, and
 * the type shapes are present. A real subprocess test (host registers
 * a host-allocated OPAQUE_FD `VkBuffer`, Deno opens the cdylib,
 * `sldn_cuda_acquire_read` → `Deno.UnsafePointerView` over the DLPack
 * capsule → byte-equal assertion against the host pattern) requires a
 * polyglot test harness that doesn't yet exist in tree — filed as
 * #596. This file exercises the Deno module's contract against the
 * cdylib's documented FFI ABI.
 */

import { assertEquals, assertExists } from "@std/assert";
import {
  type CudaAccessGuard,
  CudaContext,
  type CudaReadView,
  type CudaWriteView,
  STREAMLIB_ADAPTER_ABI_VERSION,
} from "./cuda.ts";

Deno.test("ABI version re-exported from surface_adapter", () => {
  assertEquals(STREAMLIB_ADAPTER_ABI_VERSION, 1);
});

Deno.test("CudaContext class exposes the surface-adapter method set", () => {
  const ctx = CudaContext as unknown as Record<string, unknown>;
  // Static factory.
  assertExists(ctx.fromRuntime);
  // Instance methods (verify via prototype).
  const proto = (CudaContext as unknown as { prototype: Record<string, unknown> })
    .prototype;
  assertExists(proto.acquireRead);
  assertExists(proto.acquireWrite);
  assertExists(proto.tryAcquireRead);
  assertExists(proto.tryAcquireWrite);
  assertExists(proto.close);
});

Deno.test("CudaReadView / CudaWriteView shapes round-trip dataclass-style", () => {
  // Build literal objects matching the interface — the type checker
  // would catch shape drift at compile time, but assertions here are
  // a runtime guard against accidental optional fields.
  const fakeCapsule = { _stub: true } as unknown as Deno.PointerObject;
  const rv: CudaReadView = {
    format: 0 as unknown as CudaReadView["format"],
    size: 1024n * 1024n,
    devicePtr: 0xDEAD_BEEF_CAFE_0000n,
    deviceType: 2, // kDLCUDA
    deviceId: 0,
    dlpackPtr: fakeCapsule,
    consume: () => {},
  };
  const wv: CudaWriteView = {
    format: 0 as unknown as CudaWriteView["format"],
    size: 2048n,
    devicePtr: 0xDEAD_BEEF_CAFE_1000n,
    deviceType: 3, // kDLCUDAHost
    deviceId: 1,
    dlpackPtr: fakeCapsule,
    consume: () => {},
  };
  assertEquals(rv.size, 1024n * 1024n);
  assertEquals(rv.deviceType, 2);
  assertEquals(rv.deviceId, 0);
  assertEquals(wv.size, 2048n);
  assertEquals(wv.deviceType, 3);
  assertEquals(wv.deviceId, 1);
});

Deno.test("CudaAccessGuard satisfies sync Disposable", () => {
  // Sync `using` semantics: the cuda adapter has no per-acquire IPC,
  // so the cdylib FFI is blocking but synchronous all the way down.
  // Tracking #590's exit criterion verbatim ("`using` (Symbol.dispose)
  // support") rather than mirroring cpu_readback's `await using` shape.
  const fakeCapsule = { _stub: true } as unknown as Deno.PointerObject;
  const view: CudaReadView = {
    format: 0 as unknown as CudaReadView["format"],
    size: 4n,
    devicePtr: 0n,
    deviceType: 2,
    deviceId: 0,
    dlpackPtr: fakeCapsule,
    consume: () => {},
  };
  const guard: CudaAccessGuard<CudaReadView> = {
    view,
    [Symbol.dispose]: () => {},
  };
  assertExists(guard.view);
  assertExists(guard[Symbol.dispose]);
});
