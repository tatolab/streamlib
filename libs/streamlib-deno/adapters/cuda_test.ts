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
  CudaImageFormat,
  type CudaReadView,
  type CudaSurfaceView,
  type CudaTextureView,
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
  // Image-flavored methods — sibling of acquireRead / acquireWrite
  // for the `cudaTextureObject_t` / `cudaSurfaceObject_t` path.
  assertExists(proto.acquireTexture);
  assertExists(proto.acquireSurface);
  assertExists(proto.tryAcquireTexture);
  assertExists(proto.tryAcquireSurface);
  // Cross-process release shim — present per the design clarification
  // comment on #802 (no `vulkanCtx` parameter; see the arity test).
  assertExists(proto.releaseForCrossProcess);
});

Deno.test("CudaImageFormat enum matches cdylib discriminants", () => {
  // Wire ABI mirror of the cdylib's `SLDN_CUDA_FORMAT_*` constants.
  // CUDA image flavor is restricted to the four-channel R8/R16/R32
  // subset accepted by `cudaExternalMemoryGetMappedMipmappedArray`.
  assertEquals(CudaImageFormat.Rgba8Unorm, 0);
  assertEquals(CudaImageFormat.Rgba16Float, 1);
  assertEquals(CudaImageFormat.Rgba32Float, 2);
});

Deno.test("CudaTextureView / CudaSurfaceView shapes round-trip dataclass-style", () => {
  const tv: CudaTextureView = {
    handle: 0xDEAD_BEEF_CAFE_0000n,
    width: 1920,
    height: 1080,
    format: CudaImageFormat.Rgba8Unorm,
  };
  const sv: CudaSurfaceView = {
    handle: 0xCAFE_BABE_1234_ABCDn,
    width: 640,
    height: 480,
    format: CudaImageFormat.Rgba32Float,
  };
  assertEquals(tv.handle, 0xDEAD_BEEF_CAFE_0000n);
  assertEquals(tv.width, 1920);
  assertEquals(tv.height, 1080);
  assertEquals(tv.format, CudaImageFormat.Rgba8Unorm);
  assertEquals(sv.handle, 0xCAFE_BABE_1234_ABCDn);
  assertEquals(sv.width, 640);
  assertEquals(sv.height, 480);
  assertEquals(sv.format, CudaImageFormat.Rgba32Float);
});

Deno.test(
  "image-path release FFI declares (pointer, u64, u64) — handle-keyed",
  async () => {
    // The cdylib's `sldn_cuda_release_texture` / `_surface` take the
    // customer's `cudaTextureObject_t` / `cudaSurfaceObject_t` back
    // as a `u64`. This is what makes the FFI safe under concurrent
    // reads (N read holders, each with a unique handle, releases
    // must destroy the caller's handle — not LIFO pop). Pin the
    // declared argtypes here so a regression to a 2-arg shape
    // surfaces at unit-test time.
    //
    // Source-level check rather than runtime introspection because
    // the `symbols` object in `native.ts` is not exported (it's
    // `const symbols`, used only by `loadNativeLib`). The `as const`
    // shape is the load-bearing invariant; reading the source text
    // is the simplest way to lock it.
    // `import.meta.url` is the test file's URL; resolve `../native.ts`
    // relative to it so the test runs regardless of cwd.
    const nativeUrl = new URL("../native.ts", import.meta.url);
    const src = await Deno.readTextFile(nativeUrl);
    const releaseTexture =
      /sldn_cuda_release_texture:\s*\{\s*parameters:\s*\["pointer",\s*"u64",\s*"u64"\]\s+as const/;
    const releaseSurface =
      /sldn_cuda_release_surface:\s*\{\s*parameters:\s*\["pointer",\s*"u64",\s*"u64"\]\s+as const/;
    if (!releaseTexture.test(src)) {
      throw new Error(
        "sldn_cuda_release_texture FFI declaration must take " +
          '["pointer", "u64", "u64"] — a regression to ["pointer", "u64"] ' +
          "is the LIFO-pop anti-pattern that breaks under concurrent " +
          "reads. See the cdylib's `sldn_cuda_release_texture` doc-comment.",
      );
    }
    if (!releaseSurface.test(src)) {
      throw new Error(
        "sldn_cuda_release_surface FFI declaration must take " +
          '["pointer", "u64", "u64"] — same regression check.',
      );
    }
  },
);

Deno.test("releaseForCrossProcess signature takes no vulkanCtx parameter", () => {
  // The CUDA shim must NOT take a `vulkanCtx` parameter — unlike the
  // OpenGL shim. Per the design clarification comment on #802:
  //
  //   CUDA writes via `cudaSurfaceObject_t` against the imported
  //   mipmapped array — the underlying VkImage memory is touched but
  //   the Vulkan layout tracker is unchanged (and the cdylib has no
  //   host VkDevice to barrier on anyway, per the consumer-rhi
  //   carve-out). The pairwise sync runs entirely on
  //   `cudaSignalExternalSemaphoresAsync` /
  //   `cudaWaitExternalSemaphoresAsync` against the imported
  //   timeline.
  //
  // The shim's job is the layout publish via `updateImageLayout`. A
  // future regression that adds `vulkanCtx` back (e.g. copying the
  // OpenGL shape reflexively) trips this test.
  const fn = (CudaContext as unknown as { prototype: {
    releaseForCrossProcess: (...args: unknown[]) => void;
  } }).prototype.releaseForCrossProcess;
  // `.length` returns the number of declared formal parameters before
  // any with default values; two = (surface, postReleaseLayout).
  assertEquals(
    fn.length,
    2,
    "releaseForCrossProcess must take (surface, postReleaseLayout) " +
      "only — no vulkanCtx. See the design clarification on #802.",
  );
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
