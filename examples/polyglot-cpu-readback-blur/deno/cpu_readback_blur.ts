// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Polyglot cpu-readback blur processor — Deno twin of
 * `python/cpu_readback_blur.py` (#529).
 *
 * Receives a trigger Videoframe, opens the host-pre-allocated
 * cpu-readback surface through `CpuReadbackContext.acquireWrite`,
 * applies a hand-rolled separable Gaussian blur to the BGRA bytes,
 * and releases — the host adapter flushes CPU→GPU. After the runtime
 * stops, the host reads the surface back and writes a PNG that
 * should visually show the blurred input.
 *
 * Deno has no numpy / cv2 binding ecosystem; the Gaussian is
 * separable so a pure-TS implementation runs in O(N·k) per axis pass
 * (vs. O(N·k²) for a naive 2D convolution) without external deps.
 *
 * Config keys:
 *     cpu_readback_surface_id (number, required)
 *         Host-assigned u64 surface id the host pre-registered with
 *         the cpu-readback adapter.
 *     kernel_size (number, default 11)
 *         Gaussian kernel side length (odd integer).
 *     sigma (number, default 4.0)
 *         Gaussian sigma.
 */

import type {
  ReactiveProcessor,
  RuntimeContextFullAccess,
  RuntimeContextLimitedAccess,
} from "../../../libs/streamlib-deno/mod.ts";
import { CpuReadbackContext } from "../../../libs/streamlib-deno/adapters/cpu_readback.ts";

export default class CpuReadbackBlurProcessor implements ReactiveProcessor {
  private surfaceId: bigint = 0n;
  private kernelSize = 11;
  private sigma = 4.0;
  private cpuReadback: CpuReadbackContext | null = null;
  private blurCount = 0;
  private errorCount = 0;
  private lastError: string | null = null;

  setup(ctx: RuntimeContextFullAccess): void {
    const cfg = ctx.config;
    const sidRaw = cfg["cpu_readback_surface_id"];
    if (typeof sidRaw !== "number" && typeof sidRaw !== "bigint") {
      throw new Error(
        `[CpuReadbackBlur/deno] cpu_readback_surface_id must be a number, got ${typeof sidRaw}`,
      );
    }
    this.surfaceId = typeof sidRaw === "bigint" ? sidRaw : BigInt(sidRaw);
    const ksRaw = cfg["kernel_size"];
    // Force odd
    this.kernelSize = Math.max(
      3,
      ((typeof ksRaw === "number" ? Math.floor(ksRaw) : 11) | 1),
    );
    const sigmaRaw = cfg["sigma"];
    this.sigma = typeof sigmaRaw === "number" ? sigmaRaw : 4.0;
    this.cpuReadback = CpuReadbackContext.fromRuntime(ctx);
    console.error(
      `[CpuReadbackBlur/deno] setup surface_id=${this.surfaceId} ` +
        `kernel=${this.kernelSize} sigma=${this.sigma}`,
    );
  }

  async process(ctx: RuntimeContextLimitedAccess): Promise<void> {
    // Drain the trigger frame so the upstream port doesn't backpressure.
    const _frame = ctx.inputs.read("video_in");
    if (!_frame) return;

    if (this.blurCount > 0) return;
    if (!this.cpuReadback) {
      throw new Error(
        "[CpuReadbackBlur/deno] cpu-readback context not initialized",
      );
    }

    try {
      await this.applyBlurOnce();
      this.blurCount += 1;
      console.error(
        `[CpuReadbackBlur/deno] blur applied (kernel=${this.kernelSize}, ` +
          `sigma=${this.sigma})`,
      );
    } catch (e) {
      this.errorCount += 1;
      this.lastError = String(e);
      console.error(
        `[CpuReadbackBlur/deno] blur failed (count=${this.errorCount}): ${e}`,
      );
    }
  }

  private async applyBlurOnce(): Promise<void> {
    await using guard = await this.cpuReadback!.acquireWrite(this.surfaceId);
    const plane = guard.view.plane(0);
    const w = plane.width;
    const h = plane.height;
    const bpp = plane.bytesPerPixel; // 4 for BGRA8
    const kernel = buildGaussianKernel(this.kernelSize, this.sigma);

    // Horizontal pass: bytes → tmp. Vertical pass: tmp → bytes.
    // BGRA: blur B, G, R; leave A alone so the PNG alpha doesn't
    // softed at the borders.
    const tmp = new Uint8ClampedArray(plane.bytes.length);
    separablePassHorizontal(
      plane.bytes,
      tmp,
      w,
      h,
      bpp,
      kernel,
    );
    separablePassVertical(
      tmp,
      plane.bytes,
      w,
      h,
      bpp,
      kernel,
    );
  }

  teardown(_ctx: RuntimeContextFullAccess): void {
    console.error(
      `[CpuReadbackBlur/deno] teardown blurs=${this.blurCount} ` +
        `errors=${this.errorCount} last_error=${this.lastError}`,
    );
  }
}

/** 1D Gaussian kernel — normalized to sum to 1.0. */
function buildGaussianKernel(ks: number, sigma: number): Float32Array {
  const k = new Float32Array(ks);
  const center = (ks - 1) / 2;
  let sum = 0;
  for (let i = 0; i < ks; i += 1) {
    const x = i - center;
    const v = Math.exp(-(x * x) / (2 * sigma * sigma));
    k[i] = v;
    sum += v;
  }
  for (let i = 0; i < ks; i += 1) k[i] /= sum;
  return k;
}

/** Apply the kernel along the horizontal axis. Edge pixels replicate.
 * BGRA: blur channels 0/1/2 (B/G/R), pass channel 3 (A) through. */
function separablePassHorizontal(
  src: Uint8Array,
  dst: Uint8ClampedArray,
  w: number,
  h: number,
  bpp: number,
  kernel: Float32Array,
): void {
  const ks = kernel.length;
  const half = (ks - 1) >> 1;
  const stride = w * bpp;
  for (let y = 0; y < h; y += 1) {
    const row = y * stride;
    for (let x = 0; x < w; x += 1) {
      let sumB = 0, sumG = 0, sumR = 0;
      for (let i = 0; i < ks; i += 1) {
        let xi = x + i - half;
        if (xi < 0) xi = 0;
        else if (xi >= w) xi = w - 1;
        const off = row + xi * bpp;
        const wgt = kernel[i];
        sumB += src[off] * wgt;
        sumG += src[off + 1] * wgt;
        sumR += src[off + 2] * wgt;
      }
      const dstOff = row + x * bpp;
      dst[dstOff] = sumB;
      dst[dstOff + 1] = sumG;
      dst[dstOff + 2] = sumR;
      dst[dstOff + 3] = src[row + x * bpp + 3]; // A passthrough
    }
  }
}

/** Apply the kernel along the vertical axis. Edge pixels replicate. */
function separablePassVertical(
  src: Uint8ClampedArray,
  dst: Uint8Array,
  w: number,
  h: number,
  bpp: number,
  kernel: Float32Array,
): void {
  const ks = kernel.length;
  const half = (ks - 1) >> 1;
  const stride = w * bpp;
  for (let y = 0; y < h; y += 1) {
    for (let x = 0; x < w; x += 1) {
      let sumB = 0, sumG = 0, sumR = 0;
      for (let i = 0; i < ks; i += 1) {
        let yi = y + i - half;
        if (yi < 0) yi = 0;
        else if (yi >= h) yi = h - 1;
        const off = yi * stride + x * bpp;
        const wgt = kernel[i];
        sumB += src[off] * wgt;
        sumG += src[off + 1] * wgt;
        sumR += src[off + 2] * wgt;
      }
      const dstOff = y * stride + x * bpp;
      dst[dstOff] = Math.round(sumB);
      dst[dstOff + 1] = Math.round(sumG);
      dst[dstOff + 2] = Math.round(sumR);
      dst[dstOff + 3] = src[y * stride + x * bpp + 3];
    }
  }
}
