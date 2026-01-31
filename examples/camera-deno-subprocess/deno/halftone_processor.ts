// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Halftone dot pattern processor — WebGPU compute shader via TypeGPU.
 *
 * Converts live camera frames into a newspaper/pop-art style dot pattern.
 * The image is divided into 8x8 cells; each cell becomes a colored circle
 * whose radius is proportional to the luminance of the center pixel.
 */

import tgpu from "npm:typegpu@0.8.2";
import * as d from "npm:typegpu@0.8.2/data";
import type { ReactiveProcessor, ProcessorContext, GpuSurface } from "../../../libs/streamlib-deno/mod.ts";
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

    // Cell grid (8px cells)
    let cellSize = 8u;
    let cellX = x / cellSize;
    let cellY = y / cellSize;
    let centerX = cellX * cellSize + cellSize / 2u;
    let centerY = cellY * cellSize + cellSize / 2u;

    // Clamp center to image bounds
    let cx = min(centerX, width - 1u);
    let cy = min(centerY, height - 1u);

    // Sample center pixel — get original color and luminance
    let centerBgra = inputPixels[cy * width + cx];
    let b = f32(centerBgra & 0xFFu) / 255.0;
    let g = f32((centerBgra >> 8u) & 0xFFu) / 255.0;
    let r = f32((centerBgra >> 16u) & 0xFFu) / 255.0;
    let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;

    // Dot radius proportional to luminance (brighter = bigger)
    let maxRadius = f32(cellSize) * 0.55;
    let radius = maxRadius * lum;

    // Distance from pixel to cell center
    let dx = f32(x) - f32(cx);
    let dy = f32(y) - f32(cy);
    let dist = sqrt(dx * dx + dy * dy);

    // Inside circle: use boosted original color. Outside: near-black.
    if (dist <= radius) {
        // Boost saturation slightly for pop-art look
        let boost = 1.3;
        let ob = u32(clamp(b * boost, 0.0, 1.0) * 255.0);
        let og = u32(clamp(g * boost, 0.0, 1.0) * 255.0);
        let or_ = u32(clamp(r * boost, 0.0, 1.0) * 255.0);
        outputPixels[index] = ob | (og << 8u) | (or_ << 16u) | 0xFF000000u;
    } else {
        outputPixels[index] = 0xFF101010u; // near-black background
    }
}
`;

interface GpuResources {
  device: GPUDevice;
  pipeline: GPUComputePipeline;
  bindGroupLayout: GPUBindGroupLayout;
  inputBuffer: GPUBuffer;
  outputBuffer: GPUBuffer;
  paramsBuffer: GPUBuffer;
  readbackBuffer: GPUBuffer;
  bindGroup: GPUBindGroup;
  pixelCount: number;
  width: number;
  height: number;
}

export default class HalftoneProcessor implements ReactiveProcessor {
  private gpu: GpuResources | null = null;
  private outputSurface: GpuSurface | null = null;
  private outputPoolId: string | null = null;
  private frameIndex = 0;

  async setup(ctx: ProcessorContext): Promise<void> {
    console.error("[HalftoneProcessor] setup — config:", JSON.stringify(ctx.config));

    // WebGPU init via TypeGPU (gets adapter + device)
    const root = await tgpu.init();
    const device = root.device;
    console.error("[HalftoneProcessor] WebGPU device acquired via TypeGPU");

    // Pipeline will be created on first frame when we know dimensions
    this.gpu = {
      device,
      pipeline: null!,
      bindGroupLayout: null!,
      inputBuffer: null!,
      outputBuffer: null!,
      paramsBuffer: null!,
      readbackBuffer: null!,
      bindGroup: null!,
      pixelCount: 0,
      width: 0,
      height: 0,
    };
  }

  private initGpuResources(width: number, height: number): void {
    if (!this.gpu) return;

    const device = this.gpu.device;
    const pixelCount = width * height;
    const bufferSize = pixelCount * 4; // u32 per pixel

    // Destroy old buffers if resizing
    if (this.gpu.inputBuffer) {
      this.gpu.inputBuffer.destroy();
      this.gpu.outputBuffer.destroy();
      this.gpu.paramsBuffer.destroy();
      this.gpu.readbackBuffer.destroy();
    }

    // Create shader module + pipeline (only once, or on first call)
    if (!this.gpu.pipeline) {
      const shaderModule = device.createShaderModule({ code: HALFTONE_WGSL });

      this.gpu.bindGroupLayout = device.createBindGroupLayout({
        entries: [
          { binding: 0, visibility: GPUShaderStage.COMPUTE, buffer: { type: "read-only-storage" } },
          { binding: 1, visibility: GPUShaderStage.COMPUTE, buffer: { type: "storage" } },
          { binding: 2, visibility: GPUShaderStage.COMPUTE, buffer: { type: "uniform" } },
        ],
      });

      this.gpu.pipeline = device.createComputePipeline({
        layout: device.createPipelineLayout({
          bindGroupLayouts: [this.gpu.bindGroupLayout],
        }),
        compute: { module: shaderModule, entryPoint: "main" },
      });
    }

    this.gpu.inputBuffer = device.createBuffer({
      size: bufferSize,
      usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST,
    });

    this.gpu.outputBuffer = device.createBuffer({
      size: bufferSize,
      usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC,
    });

    this.gpu.paramsBuffer = device.createBuffer({
      size: 8, // vec2<u32> = 8 bytes
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });

    this.gpu.readbackBuffer = device.createBuffer({
      size: bufferSize,
      usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST,
    });

    this.gpu.bindGroup = device.createBindGroup({
      layout: this.gpu.bindGroupLayout,
      entries: [
        { binding: 0, resource: { buffer: this.gpu.inputBuffer } },
        { binding: 1, resource: { buffer: this.gpu.outputBuffer } },
        { binding: 2, resource: { buffer: this.gpu.paramsBuffer } },
      ],
    });

    // Write params (width, height)
    const params = new Uint32Array([width, height]);
    device.queue.writeBuffer(this.gpu.paramsBuffer, 0, params);

    this.gpu.pixelCount = pixelCount;
    this.gpu.width = width;
    this.gpu.height = height;

    console.error(`[HalftoneProcessor] GPU resources initialized: ${width}x${height}`);
  }

  async process(ctx: ProcessorContext): Promise<void> {
    const result = ctx.inputs.read<Videoframe>("video_in");
    if (!result || !this.gpu) return;

    const { value: frame, timestampNs } = result;
    const width = frame.width;
    const height = frame.height;

    // Re-init GPU resources if dimensions changed
    if (width !== this.gpu.width || height !== this.gpu.height) {
      this.initGpuResources(width, height);
    }

    // Create output surface on first frame or dimension change
    if (!this.outputSurface || this.outputSurface.width !== width || this.outputSurface.height !== height) {
      if (this.outputSurface) {
        this.outputSurface.release();
      }
      const { poolId, surface } = ctx.gpu.createSurface(width, height, "BGRA");
      this.outputSurface = surface;
      this.outputPoolId = poolId;
    }

    const device = this.gpu.device;
    const pixelCount = this.gpu.pixelCount;

    // --- Read input IOSurface pixels ---
    const inputSurface = ctx.gpu.resolveSurface(frame.surface_id);
    inputSurface.lock(true);
    const inputRawBuffer = inputSurface.asBuffer();
    const bytesPerRow = inputSurface.bytesPerRow;
    const srcRowBytes = width * 4;

    // Strip bytesPerRow padding → packed pixel array
    const packedInput = new Uint32Array(pixelCount);
    const inputBytes = new Uint8Array(inputRawBuffer);
    for (let row = 0; row < height; row++) {
      const srcOffset = row * bytesPerRow;
      const dstOffset = row * width;
      const rowSlice = new Uint32Array(inputBytes.buffer, srcOffset, width);
      packedInput.set(rowSlice, dstOffset);
    }
    inputSurface.unlock(true);
    inputSurface.release();

    // --- Upload to GPU ---
    device.queue.writeBuffer(this.gpu.inputBuffer, 0, packedInput);

    // --- Dispatch compute shader ---
    const workgroupCount = Math.ceil(pixelCount / 256);
    const encoder = device.createCommandEncoder();
    const pass = encoder.beginComputePass();
    pass.setPipeline(this.gpu.pipeline);
    pass.setBindGroup(0, this.gpu.bindGroup);
    pass.dispatchWorkgroups(workgroupCount);
    pass.end();

    // Copy output to readback buffer
    encoder.copyBufferToBuffer(
      this.gpu.outputBuffer, 0,
      this.gpu.readbackBuffer, 0,
      pixelCount * 4,
    );
    device.queue.submit([encoder.finish()]);

    // --- Read back results ---
    await this.gpu.readbackBuffer.mapAsync(GPUMapMode.READ);
    const outputData = new Uint32Array(this.gpu.readbackBuffer.getMappedRange().slice(0));
    this.gpu.readbackBuffer.unmap();

    // --- Write to output IOSurface ---
    this.outputSurface.lock(false);
    const outputRawBuffer = this.outputSurface.asBuffer();
    const outBytesPerRow = this.outputSurface.bytesPerRow;
    const outputBytes = new Uint8Array(outputRawBuffer);

    // Add bytesPerRow padding back
    for (let row = 0; row < height; row++) {
      const srcOffset = row * width;
      const dstOffset = row * outBytesPerRow;
      const rowData = new Uint8Array(outputData.buffer, srcOffset * 4, srcRowBytes);
      outputBytes.set(rowData, dstOffset);
    }
    this.outputSurface.unlock(false);

    // --- Write output frame ---
    this.frameIndex++;
    const outputFrame: Videoframe = {
      surface_id: this.outputPoolId!,
      width: width,
      height: height,
      timestamp_ns: String(timestampNs),
      frame_index: String(this.frameIndex),
    };
    ctx.outputs.write("video_out", outputFrame, timestampNs);
  }

  teardown(_ctx: ProcessorContext): void {
    console.error("[HalftoneProcessor] teardown");
    if (this.gpu) {
      if (this.gpu.inputBuffer) this.gpu.inputBuffer.destroy();
      if (this.gpu.outputBuffer) this.gpu.outputBuffer.destroy();
      if (this.gpu.paramsBuffer) this.gpu.paramsBuffer.destroy();
      if (this.gpu.readbackBuffer) this.gpu.readbackBuffer.destroy();
      this.gpu = null;
    }
    if (this.outputSurface) {
      this.outputSurface.release();
      this.outputSurface = null;
    }
  }
}
