// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Unit tests for the Deno cpu-readback subprocess runtime (#529).
 *
 * The escalate channel and `gpuLimitedAccess` are stubbed so these
 * tests assert the wire-protocol glue and view assembly without
 * spawning a real subprocess or touching a GPU.
 *
 * Real subprocess+GPU end-to-end testing ships with the polyglot
 * E2E harness in `examples/deno-cpu-readback-numpy-blur/`.
 */

import { assertEquals, assertRejects } from "@std/assert";
import {
  CpuReadbackContext,
  type CpuReadbackGpuLimitedAccess,
} from "./cpu_readback.ts";
import {
  EscalateChannel,
  EscalateError,
  type EscalateOkResponse,
} from "../escalate.ts";

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/** Stub `GpuSurface` exposing the subset `CpuReadbackGpuLimitedAccess`
 * needs. Tracks lock/unlock/release calls for assertion. */
class _FakeStagingHandle {
  width: number;
  height: number;
  bytesPerPixel: number;
  bytesPerRow: number;
  buffer: Uint8Array;
  locks: boolean[] = [];
  unlocks: boolean[] = [];
  released = false;

  constructor(width: number, height: number, bytesPerPixel: number) {
    this.width = width;
    this.height = height;
    this.bytesPerPixel = bytesPerPixel;
    this.bytesPerRow = width * bytesPerPixel;
    this.buffer = new Uint8Array(this.bytesPerRow * height);
  }

  lock(readOnly: boolean): void {
    this.locks.push(readOnly);
  }

  unlock(readOnly: boolean): void {
    this.unlocks.push(readOnly);
  }

  asBuffer(): ArrayBuffer {
    // Hand out the underlying buffer so the view can mutate it. Deno
    // typing requires we narrow ArrayBufferLike → ArrayBuffer; this
    // is safe because Uint8Array always allocates over an ArrayBuffer
    // unless explicitly given a SharedArrayBuffer (we don't).
    return this.buffer.buffer as ArrayBuffer;
  }

  release(): void {
    this.released = true;
  }
}

class _FakeGpu implements CpuReadbackGpuLimitedAccess {
  resolved: string[] = [];
  constructor(private readonly handles: Record<string, _FakeStagingHandle>) {}

  resolveSurface(stagingId: string) {
    this.resolved.push(stagingId);
    const h = this.handles[stagingId];
    if (!h) {
      throw new Error(`fake host: unknown staging surface ${stagingId}`);
    }
    return h;
  }
}

interface _RecordedRequest {
  op: string;
  surfaceId?: string;
  mode?: string;
  handleId?: string;
}

class _FakeEscalate {
  readonly requests: _RecordedRequest[] = [];

  constructor(
    private readonly acquireResponse: EscalateOkResponse,
    private readonly acquireError?: Error,
  ) {}

  async acquireCpuReadback(
    surfaceId: bigint,
    mode: "read" | "write",
  ): Promise<EscalateOkResponse> {
    this.requests.push({
      op: "acquire_cpu_readback",
      surfaceId: surfaceId.toString(),
      mode,
    });
    if (this.acquireError) throw this.acquireError;
    return await Promise.resolve(this.acquireResponse);
  }

  async releaseHandle(handleId: string): Promise<EscalateOkResponse> {
    this.requests.push({ op: "release_handle", handleId });
    return await Promise.resolve({
      result: "ok" as const,
      request_id: "release",
      handle_id: handleId,
    });
  }

  // Methods we don't exercise but the EscalateChannel type expects.
  acquirePixelBuffer = unused;
  acquireTexture = unused;
  logFireAndForget = unused;
  request = unused;
  handleIncoming = unused;
  cancelAll = unused;
}

// deno-lint-ignore no-explicit-any
const unused: any = () => {
  throw new Error("not implemented in test stub");
};

// ---------------------------------------------------------------------------
// Response builders
// ---------------------------------------------------------------------------

function bgraAcquireResponse(
  handleId = "host-handle-bgra",
): EscalateOkResponse {
  return {
    result: "ok" as const,
    request_id: "req-1",
    handle_id: handleId,
    width: 4,
    height: 2,
    format: "bgra8",
    cpu_readback_planes: [
      {
        staging_surface_id: "stg-bgra-0",
        width: 4,
        height: 2,
        bytes_per_pixel: 4,
      },
    ],
  };
}

function nv12AcquireResponse(
  handleId = "host-handle-nv12",
): EscalateOkResponse {
  return {
    result: "ok" as const,
    request_id: "req-nv12",
    handle_id: handleId,
    width: 8,
    height: 4,
    format: "nv12",
    cpu_readback_planes: [
      {
        staging_surface_id: "stg-nv12-y",
        width: 8,
        height: 4,
        bytes_per_pixel: 1,
      },
      {
        staging_surface_id: "stg-nv12-uv",
        width: 4,
        height: 2,
        bytes_per_pixel: 2,
      },
    ],
  };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

Deno.test("acquireWrite BGRA — view aliases staging buffer; release fires on dispose", async () => {
  const handle = new _FakeStagingHandle(4, 2, 4);
  const gpu = new _FakeGpu({ "stg-bgra-0": handle });
  const escalate = new _FakeEscalate(bgraAcquireResponse());
  const ctx = new CpuReadbackContext(
    gpu,
    escalate as unknown as EscalateChannel,
  );

  {
    await using guard = await ctx.acquireWrite(42n);
    assertEquals(guard.handleId, "host-handle-bgra");
    assertEquals(guard.view.planeCount, 1);
    const plane = guard.view.plane(0);
    assertEquals(plane.width, 4);
    assertEquals(plane.height, 2);
    assertEquals(plane.bytesPerPixel, 4);
    assertEquals(plane.rowStride, 16);
    assertEquals(plane.bytes.byteLength, 32); // 4 cols × 2 rows × 4 bytes
    // Mutate via the alias; the staging buffer sees the write.
    plane.bytes.fill(0xff);
    assertEquals(handle.buffer[0], 0xff);
    assertEquals(handle.buffer[31], 0xff);
  }

  // Order: acquire → release. surface_id marshalled as decimal string.
  assertEquals(escalate.requests.length, 2);
  assertEquals(escalate.requests[0].op, "acquire_cpu_readback");
  assertEquals(escalate.requests[0].surfaceId, "42");
  assertEquals(escalate.requests[0].mode, "write");
  assertEquals(escalate.requests[1].op, "release_handle");
  assertEquals(escalate.requests[1].handleId, "host-handle-bgra");
  // Lifecycle: locked-for-write, unlocked-for-write, released.
  assertEquals(handle.locks, [false]);
  assertEquals(handle.unlocks, [false]);
  assertEquals(handle.released, true);
});

Deno.test("acquireRead uses read-only lock", async () => {
  const handle = new _FakeStagingHandle(4, 2, 4);
  const gpu = new _FakeGpu({ "stg-bgra-0": handle });
  const escalate = new _FakeEscalate(bgraAcquireResponse("read-h"));
  const ctx = new CpuReadbackContext(
    gpu,
    escalate as unknown as EscalateChannel,
  );

  {
    await using _guard = await ctx.acquireRead(7);
    // body deliberately empty — assertion is on the lifecycle below.
  }

  assertEquals(escalate.requests[0].mode, "read");
  assertEquals(handle.locks, [true]);
  assertEquals(handle.unlocks, [true]);
  assertEquals(handle.released, true);
});

Deno.test("acquireWrite NV12 exposes Y + UV planes with correct geometry", async () => {
  const y = new _FakeStagingHandle(8, 4, 1);
  const uv = new _FakeStagingHandle(4, 2, 2);
  const gpu = new _FakeGpu({ "stg-nv12-y": y, "stg-nv12-uv": uv });
  const escalate = new _FakeEscalate(nv12AcquireResponse());
  const ctx = new CpuReadbackContext(
    gpu,
    escalate as unknown as EscalateChannel,
  );

  {
    await using guard = await ctx.acquireWrite(99n);
    assertEquals(guard.view.planeCount, 2);
    const yp = guard.view.plane(0);
    const uvp = guard.view.plane(1);
    assertEquals([yp.width, yp.height, yp.bytesPerPixel], [8, 4, 1]);
    assertEquals(yp.bytes.byteLength, 32);
    assertEquals([uvp.width, uvp.height, uvp.bytesPerPixel], [4, 2, 2]);
    assertEquals(uvp.bytes.byteLength, 16);
    // Independent backing — write to Y must not touch UV.
    yp.bytes.fill(200);
    let sum = 0;
    for (let i = 0; i < uv.buffer.length; i += 1) sum += uv.buffer[i];
    assertEquals(sum, 0);
  }

  assertEquals(gpu.resolved, ["stg-nv12-y", "stg-nv12-uv"]);
  assertEquals(y.released, true);
  assertEquals(uv.released, true);
});

Deno.test("release_handle still fires when view assembly fails mid-acquire", async () => {
  const y = new _FakeStagingHandle(8, 4, 1);
  // Deliberately omit stg-nv12-uv to force resolveSurface to throw.
  const gpu = new _FakeGpu({ "stg-nv12-y": y });
  const escalate = new _FakeEscalate(nv12AcquireResponse("nv12-h"));
  const ctx = new CpuReadbackContext(
    gpu,
    escalate as unknown as EscalateChannel,
  );

  await assertRejects(
    () => ctx.acquireWrite(99n),
    Error,
    "unknown staging surface",
  );

  // Y plane was locked then unlocked + released on the unwind path.
  assertEquals(y.locks, [false]);
  assertEquals(y.unlocks, [false]);
  assertEquals(y.released, true);
  // release_handle still fired even though acquire raised.
  const releaseCalls = escalate.requests.filter((r) =>
    r.op === "release_handle"
  );
  assertEquals(releaseCalls.length, 1);
  assertEquals(releaseCalls[0].handleId, "nv12-h");
});

Deno.test("acquireWrite propagates EscalateError from the host", async () => {
  const escalate = new _FakeEscalate(
    bgraAcquireResponse(),
    new EscalateError("host returned err: surface 42 not registered"),
  );
  const ctx = new CpuReadbackContext(
    new _FakeGpu({}),
    escalate as unknown as EscalateChannel,
  );

  await assertRejects(
    () => ctx.acquireWrite(42n),
    EscalateError,
    "not registered",
  );
  // No release_handle should fire when the acquire never succeeded.
  assertEquals(
    escalate.requests.filter((r) => r.op === "release_handle").length,
    0,
  );
});

Deno.test("acquireCpuReadback wire format encodes surface_id as decimal string", async () => {
  // EscalateChannel.acquireCpuReadback marshals bigint to a decimal
  // string per the JTD wire format. Capture the writer payload and
  // feed back a synthetic ok response so the promise resolves cleanly.
  let captured: Record<string, unknown> | null = null;
  let channel: EscalateChannel | null = null;
  const writer = (msg: Record<string, unknown>) => {
    captured = msg;
    // Schedule the synthetic response on the next microtask so the
    // pending map has the entry by the time we deliver it.
    queueMicrotask(() => {
      channel!.handleIncoming({
        rpc: "escalate_response",
        result: "ok",
        request_id: msg.request_id,
        handle_id: "captured",
      });
    });
    return Promise.resolve();
  };
  channel = new EscalateChannel(writer);
  await channel.acquireCpuReadback(0xdeadbeefn, "write");
  assertEquals(captured!.op, "acquire_cpu_readback");
  assertEquals(captured!.surface_id, "3735928559");
  assertEquals(captured!.mode, "write");
});

Deno.test("acquireCpuReadback rejects invalid mode locally", async () => {
  const channel = new EscalateChannel(() => Promise.resolve());
  await assertRejects(
    // deno-lint-ignore no-explicit-any
    () => channel.acquireCpuReadback(1n, "read-only" as any),
    EscalateError,
    "must be 'read' or 'write'",
  );
});
