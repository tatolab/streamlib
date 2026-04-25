// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Halftone dot pattern processor — WebGPU compute shader via TypeGPU.
 *
 * Demonstrates the canonical polyglot allocation path: subprocess holds a
 * limited-access GPU capability, and asks the host to allocate output pixel
 * buffers via `escalateAcquirePixelBuffer`. The returned `handle_id` is then
 * resolved locally with `gpuLimitedAccess.resolveSurface` for zero-copy
 * write access — the same shape the input frame takes.
 */

import tgpu, { type TgpuRoot } from "npm:typegpu@0.8.2";
import * as d from "npm:typegpu@0.8.2/data";
import type {
  GpuSurface,
  ReactiveProcessor,
  RuntimeContextFullAccess,
  RuntimeContextLimitedAccess,
} from "../../../libs/streamlib-deno/mod.ts";
import type { Videoframe } from "../../../libs/streamlib-deno/_generated_/com_tatolab_videoframe.ts";

const HALFTONE_WGSL = /* wgsl */`
@group(0) @binding(0) var<storage, read> inputPixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> outputPixels: array<u32>;
@group(0) @binding(2) var<uniform> params: vec2<u32>; // (width, height)

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let width = params.x;
    let height = params.y;
    let index = gid.x;
    if (index >= width * height) {
        return;
    }

    let x = index % width;
    let y = index / width;

    let cellSize = 8u;
    let cellX = x / cellSize;
    let cellY = y / cellSize;
    let centerX = cellX * cellSize + cellSize / 2u;
    let centerY = cellY * cellSize + cellSize / 2u;

    let cx = min(centerX, width - 1u);
    let cy = min(centerY, height - 1u);

    let centerBgra = inputPixels[cy * width + cx];
    let b = f32(centerBgra & 0xFFu) / 255.0;
    let g = f32((centerBgra >> 8u) & 0xFFu) / 255.0;
    let r = f32((centerBgra >> 16u) & 0xFFu) / 255.0;
    let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;

    let maxRadius = f32(cellSize) * 0.55;
    let radius = maxRadius * lum;

    let dx = f32(x) - f32(cx);
    let dy = f32(y) - f32(cy);
    let dist = sqrt(dx * dx + dy * dy);

    if (dist <= radius) {
        let boost = 1.3;
        let ob = u32(clamp(b * boost, 0.0, 1.0) * 255.0);
        let og = u32(clamp(g * boost, 0.0, 1.0) * 255.0);
        let or_ = u32(clamp(r * boost, 0.0, 1.0) * 255.0);
        outputPixels[index] = ob | (og << 8u) | (or_ << 16u) | 0xFF000000u;
    } else {
        outputPixels[index] = 0xFF101010u;
    }
}
`;

const halftoneBindGroupLayout = tgpu.bindGroupLayout({
  inputPixels: { storage: d.arrayOf(d.u32), access: "readonly" },
  outputPixels: { storage: d.arrayOf(d.u32), access: "mutable" },
  params: { uniform: d.vec2u },
});

interface GpuResources {
  root: TgpuRoot;
  device: GPUDevice;
  pipeline: GPUComputePipeline;
  // Sized lazily on first frame. TypeGPU buffers carry their own type info,
  // so they're stored untyped here and validated at runtime.
  // deno-lint-ignore no-explicit-any
  inputBuffer: any;
  // deno-lint-ignore no-explicit-any
  outputBuffer: any;
  // deno-lint-ignore no-explicit-any
  paramsBuffer: any;
  readbackBuffer: GPUBuffer;
  pixelCount: number;
  width: number;
  height: number;
}

const OUTPUT_POOL_SIZE = 3;

interface PoolSlot {
  handleId: string;
}

export default class HalftoneProcessor implements ReactiveProcessor {
  private gpu: GpuResources | null = null;
  private outputPool: PoolSlot[] = [];
  private outputPoolIndex = 0;
  private outputPoolWidth = 0;
  private outputPoolHeight = 0;
  private frameIndex = 0;

  async setup(ctx: RuntimeContextFullAccess): Promise<void> {
    console.error("[HalftoneProcessor] setup — config:", JSON.stringify(ctx.config));

    const root = await tgpu.init();
    const device = root.device;

    const shaderModule = device.createShaderModule({ code: HALFTONE_WGSL });
    const pipeline = device.createComputePipeline({
      layout: device.createPipelineLayout({
        bindGroupLayouts: [root.unwrap(halftoneBindGroupLayout)],
      }),
      compute: { module: shaderModule, entryPoint: "main" },
    });

    this.gpu = {
      root,
      device,
      pipeline,
      inputBuffer: null,
      outputBuffer: null,
      paramsBuffer: null,
      readbackBuffer: null!,
      pixelCount: 0,
      width: 0,
      height: 0,
    };
  }

  private initGpuResources(width: number, height: number): void {
    if (!this.gpu) return;

    const root = this.gpu.root;
    const device = this.gpu.device;
    const pixelCount = width * height;
    const bufferSize = pixelCount * 4;

    if (this.gpu.inputBuffer) {
      this.gpu.inputBuffer.destroy();
      this.gpu.outputBuffer.destroy();
      this.gpu.paramsBuffer.destroy();
      this.gpu.readbackBuffer.destroy();
    }

    this.gpu.inputBuffer = root
      .createBuffer(d.arrayOf(d.u32, pixelCount))
      .$usage("storage");

    this.gpu.outputBuffer = root
      .createBuffer(d.arrayOf(d.u32, pixelCount))
      .$usage("storage");

    this.gpu.paramsBuffer = root
      .createBuffer(d.vec2u, d.vec2u(width, height))
      .$usage("uniform");

    // Raw readback buffer for fast GPU→CPU transfer (avoids TypeGPU serialization).
    this.gpu.readbackBuffer = device.createBuffer({
      size: bufferSize,
      usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST,
    });

    this.gpu.pixelCount = pixelCount;
    this.gpu.width = width;
    this.gpu.height = height;

    console.error(`[HalftoneProcessor] GPU resources initialized: ${width}x${height}`);
  }

  private async ensureOutputPool(
    ctx: RuntimeContextLimitedAccess,
    width: number,
    height: number,
  ): Promise<void> {
    if (
      this.outputPool.length === OUTPUT_POOL_SIZE &&
      this.outputPoolWidth === width &&
      this.outputPoolHeight === height
    ) {
      return;
    }

    await this.releaseOutputPool(ctx);

    for (let i = 0; i < OUTPUT_POOL_SIZE; i++) {
      const ok = await ctx.escalateAcquirePixelBuffer(width, height, "bgra");
      this.outputPool.push({ handleId: ok.handle_id });
    }
    this.outputPoolWidth = width;
    this.outputPoolHeight = height;
    this.outputPoolIndex = 0;
    console.error(
      `[HalftoneProcessor] output pool ready: ${OUTPUT_POOL_SIZE}× ${width}x${height}`,
    );
  }

  private async releaseOutputPool(
    ctx: RuntimeContextLimitedAccess | RuntimeContextFullAccess,
  ): Promise<void> {
    const drained = this.outputPool;
    this.outputPool = [];
    this.outputPoolWidth = 0;
    this.outputPoolHeight = 0;
    this.outputPoolIndex = 0;
    for (const slot of drained) {
      try {
        await ctx.escalateReleaseHandle(slot.handleId);
      } catch (e) {
        console.error(
          `[HalftoneProcessor] escalateReleaseHandle(${slot.handleId}) failed:`,
          e,
        );
      }
    }
  }

  async process(ctx: RuntimeContextLimitedAccess): Promise<void> {
    const result = ctx.inputs.read<Videoframe>("video_in");
    if (!result || !this.gpu) return;

    const { value: frame, timestampNs } = result;
    const width = frame.width;
    const height = frame.height;

    if (width !== this.gpu.width || height !== this.gpu.height) {
      this.initGpuResources(width, height);
    }
    await this.ensureOutputPool(ctx, width, height);

    const root = this.gpu.root;
    const device = this.gpu.device;
    const pixelCount = this.gpu.pixelCount;

    // --- Read input surface pixels (zero-copy lock + typed-array view) ---
    const inputSurface = ctx.gpuLimitedAccess.resolveSurface(frame.surface_id);
    inputSurface.lock(true);
    const inputBytes = new Uint8Array(inputSurface.asBuffer());
    const inputBytesPerRow = inputSurface.bytesPerRow;
    const tightRowBytes = width * 4;

    const packedInput = new Uint32Array(pixelCount);
    if (inputBytesPerRow === tightRowBytes) {
      // Fast path — tightly packed; one aligned u32 view, no per-row copy.
      packedInput.set(
        new Uint32Array(inputBytes.buffer, inputBytes.byteOffset, pixelCount),
      );
    } else {
      for (let row = 0; row < height; row++) {
        const srcOffset = inputBytes.byteOffset + row * inputBytesPerRow;
        const rowSlice = new Uint32Array(inputBytes.buffer, srcOffset, width);
        packedInput.set(rowSlice, row * width);
      }
    }
    inputSurface.unlock(true);
    inputSurface.release();

    // --- Upload + dispatch + readback ---
    device.queue.writeBuffer(
      root.unwrap(this.gpu.inputBuffer) as unknown as GPUBuffer,
      0,
      packedInput,
    );

    const workgroupCount = Math.ceil(pixelCount / 256);
    const bindGroup = root.createBindGroup(halftoneBindGroupLayout, {
      inputPixels: this.gpu.inputBuffer,
      outputPixels: this.gpu.outputBuffer,
      params: this.gpu.paramsBuffer,
    });

    const encoder = device.createCommandEncoder();
    const pass = encoder.beginComputePass();
    pass.setPipeline(this.gpu.pipeline);
    pass.setBindGroup(0, root.unwrap(bindGroup));
    pass.dispatchWorkgroups(workgroupCount);
    pass.end();

    encoder.copyBufferToBuffer(
      root.unwrap(this.gpu.outputBuffer) as unknown as GPUBuffer, 0,
      this.gpu.readbackBuffer, 0,
      pixelCount * 4,
    );
    device.queue.submit([encoder.finish()]);

    await this.gpu.readbackBuffer.mapAsync(GPUMapMode.READ);
    const outputData = new Uint32Array(this.gpu.readbackBuffer.getMappedRange().slice(0));
    this.gpu.readbackBuffer.unmap();

    // --- Write to next pool slot ---
    const slot = this.outputPool[this.outputPoolIndex];
    this.outputPoolIndex = (this.outputPoolIndex + 1) % this.outputPool.length;

    const outputSurface: GpuSurface = ctx.gpuLimitedAccess.resolveSurface(slot.handleId);
    outputSurface.lock(false);
    const outputBytes = new Uint8Array(outputSurface.asBuffer());
    const outputBytesPerRow = outputSurface.bytesPerRow;

    if (outputBytesPerRow === tightRowBytes) {
      outputBytes.set(new Uint8Array(outputData.buffer));
    } else {
      for (let row = 0; row < height; row++) {
        const srcOffset = row * width * 4;
        const dstOffset = row * outputBytesPerRow;
        outputBytes.set(
          new Uint8Array(outputData.buffer, srcOffset, tightRowBytes),
          dstOffset,
        );
      }
    }
    outputSurface.unlock(false);
    outputSurface.release();

    // --- Forward downstream ---
    this.frameIndex++;
    const outputFrame: Videoframe = {
      surface_id: slot.handleId,
      width,
      height,
      timestamp_ns: String(timestampNs),
      frame_index: String(this.frameIndex),
    };
    ctx.outputs.write("video_out", outputFrame, timestampNs);
  }

  async teardown(ctx: RuntimeContextFullAccess): Promise<void> {
    console.error("[HalftoneProcessor] teardown");
    await this.releaseOutputPool(ctx);
    if (this.gpu) {
      if (this.gpu.inputBuffer) this.gpu.inputBuffer.destroy();
      if (this.gpu.outputBuffer) this.gpu.outputBuffer.destroy();
      if (this.gpu.paramsBuffer) this.gpu.paramsBuffer.destroy();
      if (this.gpu.readbackBuffer) this.gpu.readbackBuffer.destroy();
      this.gpu.root.destroy();
      this.gpu = null;
    }
  }
}
