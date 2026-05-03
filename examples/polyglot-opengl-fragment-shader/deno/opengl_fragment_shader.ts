// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Polyglot OpenGL fragment-shader processor — Deno twin of
 * `examples/polyglot-opengl-fragment-shader/python/opengl_fragment_shader.py`.
 *
 * End-to-end gate for the subprocess `OpenGLContext` runtime (#530).
 * The host pre-allocates a render-target-capable DMA-BUF surface and
 * registers it with surface-share. This processor receives a trigger
 * Videoframe, opens the host surface through `OpenGLContext.acquireWrite`
 * (which imports the DMA-BUF as an `EGLImage` + `GL_TEXTURE_2D` and
 * makes the adapter's EGL context current on the calling thread), uses
 * `Deno.dlopen` against `libGL.so.1` to compile a plasma-effect
 * fragment shader, attaches an FBO to the imported texture, draws a
 * fullscreen quad, and releases — the adapter's `glFinish` on release
 * ensures the host's DMA-BUF readback sees the writes.
 *
 * Different effect from the Python twin so the two output PNGs are
 * visually distinct: Python renders a Mandelbrot zoom; Deno renders
 * sine-interference plasma waves.
 *
 * Real Deno customers can use any GL library (a community FFI binding,
 * a game-engine wrapper, etc.) — the SDK is library-agnostic and just
 * makes the EGL context current. This processor uses raw `Deno.dlopen`
 * to keep the dep surface minimal and parallel to the Python twin.
 *
 * Config keys:
 *   opengl_surface_uuid (string, required)
 *     Surface-share UUID the host registered the render-target image
 *     under. Passed to `OpenGLContext.acquireWrite`.
 *   width (number, required)
 *     Surface width in pixels — the FBO viewport is set to this.
 *   height (number, required)
 *     Surface height in pixels.
 */

import type {
  ReactiveProcessor,
  RuntimeContextFullAccess,
  RuntimeContextLimitedAccess,
} from "../../../libs/streamlib-deno/mod.ts";
import { OpenGLContext } from "../../../libs/streamlib-deno/adapters/opengl.ts";
import {
  VkImageLayout,
  VulkanContext,
} from "../../../libs/streamlib-deno/adapters/vulkan.ts";

// =============================================================================
// Minimal libGL.so.1 binding via Deno.dlopen
// =============================================================================

const GL_FRAGMENT_SHADER = 0x8B30;
const GL_VERTEX_SHADER = 0x8B31;
const GL_COMPILE_STATUS = 0x8B81;
const GL_LINK_STATUS = 0x8B82;
const GL_INFO_LOG_LENGTH = 0x8B84;
const GL_FRAMEBUFFER = 0x8D40;
const GL_COLOR_ATTACHMENT0 = 0x8CE0;
const GL_FRAMEBUFFER_COMPLETE = 0x8CD5;
const GL_TEXTURE_2D = 0x0DE1;
const GL_TRIANGLE_STRIP = 0x0005;
const GL_NO_ERROR = 0;

const glSymbols = {
  glCreateShader: { parameters: ["u32"], result: "u32" },
  glShaderSource: {
    parameters: ["u32", "i32", "buffer", "buffer"],
    result: "void",
  },
  glCompileShader: { parameters: ["u32"], result: "void" },
  glGetShaderiv: {
    parameters: ["u32", "u32", "buffer"],
    result: "void",
  },
  glGetShaderInfoLog: {
    parameters: ["u32", "i32", "buffer", "buffer"],
    result: "void",
  },
  glDeleteShader: { parameters: ["u32"], result: "void" },
  glCreateProgram: { parameters: [], result: "u32" },
  glAttachShader: { parameters: ["u32", "u32"], result: "void" },
  glLinkProgram: { parameters: ["u32"], result: "void" },
  glGetProgramiv: {
    parameters: ["u32", "u32", "buffer"],
    result: "void",
  },
  glGetProgramInfoLog: {
    parameters: ["u32", "i32", "buffer", "buffer"],
    result: "void",
  },
  glDeleteProgram: { parameters: ["u32"], result: "void" },
  glUseProgram: { parameters: ["u32"], result: "void" },
  glGetUniformLocation: {
    parameters: ["u32", "buffer"],
    result: "i32",
  },
  glUniform2f: {
    parameters: ["i32", "f32", "f32"],
    result: "void",
  },
  glGenFramebuffers: {
    parameters: ["i32", "buffer"],
    result: "void",
  },
  glDeleteFramebuffers: {
    parameters: ["i32", "buffer"],
    result: "void",
  },
  glBindFramebuffer: {
    parameters: ["u32", "u32"],
    result: "void",
  },
  glFramebufferTexture2D: {
    parameters: ["u32", "u32", "u32", "u32", "i32"],
    result: "void",
  },
  glCheckFramebufferStatus: {
    parameters: ["u32"],
    result: "u32",
  },
  glGenVertexArrays: {
    parameters: ["i32", "buffer"],
    result: "void",
  },
  glDeleteVertexArrays: {
    parameters: ["i32", "buffer"],
    result: "void",
  },
  glBindVertexArray: { parameters: ["u32"], result: "void" },
  glViewport: {
    parameters: ["i32", "i32", "i32", "i32"],
    result: "void",
  },
  glDrawArrays: {
    parameters: ["u32", "i32", "i32"],
    result: "void",
  },
  glFinish: { parameters: [], result: "void" },
  glGetError: { parameters: [], result: "u32" },
} as const;

// deno-lint-ignore no-explicit-any
let glLib: { symbols: any } | null = null;

function loadLibGL(): { symbols: typeof glSymbols extends infer _ ? any : never } {
  if (glLib === null) {
    glLib = Deno.dlopen("libGL.so.1", glSymbols);
  }
  // deno-lint-ignore no-explicit-any
  return glLib as any;
}

// =============================================================================
// Shaders — sine-interference plasma waves
// =============================================================================

/**
 * Encode a string into a `Uint8Array<ArrayBuffer>` (not
 * `Uint8Array<ArrayBufferLike>` which `TextEncoder` returns) — Deno FFI
 * APIs require the concrete `ArrayBuffer` parameterization.
 */
function encodeUtf8(s: string): Uint8Array<ArrayBuffer> {
  const tmp = new TextEncoder().encode(s);
  const out = new Uint8Array(new ArrayBuffer(tmp.byteLength));
  out.set(tmp);
  return out;
}

const VERTEX_SHADER = encodeUtf8(`#version 330 core
const vec2 positions[4] = vec2[4](
    vec2(-1.0, -1.0), vec2( 1.0, -1.0),
    vec2(-1.0,  1.0), vec2( 1.0,  1.0)
);
void main() {
    gl_Position = vec4(positions[gl_VertexID], 0.0, 1.0);
}
\0`);

// Classic demoscene plasma — superimposed sines hashed through a cosine
// palette. Different visual fingerprint from the Python Mandelbrot.
const FRAGMENT_SHADER = encodeUtf8(`#version 330 core
out vec4 fragColor;
uniform vec2 resolution;
void main() {
    vec2 uv = gl_FragCoord.xy / resolution;
    vec2 p = uv * 6.0 - 3.0;
    float v = 0.0;
    v += sin(p.x * 1.7);
    v += sin(p.y * 2.3 + 1.1);
    v += sin((p.x + p.y) * 1.3 + 2.7);
    v += sin(length(p) * 3.7);
    v *= 0.25;
    // Cosine palette in HSV-ish space (Inigo Quilez style).
    vec3 a = vec3(0.5, 0.5, 0.5);
    vec3 b = vec3(0.5, 0.5, 0.5);
    vec3 c = vec3(2.0, 1.0, 0.0);
    vec3 d = vec3(0.5, 0.20, 0.25);
    vec3 col = a + b * cos(6.28318 * (c * v + d));
    fragColor = vec4(col, 1.0);
}
\0`);

// =============================================================================
// GL helpers
// =============================================================================

// deno-lint-ignore no-explicit-any
function compileShader(gl: any, source: Uint8Array<ArrayBuffer>, kind: number, name: string): number {
  const sh = gl.symbols.glCreateShader(kind);
  if (sh === 0) throw new Error(`glCreateShader(${name}) returned 0`);
  // glShaderSource takes (count, **strings, *lengths). We pack a single
  // pointer-to-pointer via a u64-aligned buffer.
  const srcPtr = Deno.UnsafePointer.of(source);
  if (srcPtr === null) throw new Error("null shader source pointer");
  const ptrBuf = new BigUint64Array(1);
  ptrBuf[0] = BigInt(Deno.UnsafePointer.value(srcPtr));
  gl.symbols.glShaderSource(sh, 1, new Uint8Array(ptrBuf.buffer), null);
  gl.symbols.glCompileShader(sh);
  const status = new Int32Array(1);
  gl.symbols.glGetShaderiv(sh, GL_COMPILE_STATUS, new Uint8Array(status.buffer));
  if (status[0] === 0) {
    const logLen = new Int32Array(1);
    gl.symbols.glGetShaderiv(
      sh,
      GL_INFO_LOG_LENGTH,
      new Uint8Array(logLen.buffer),
    );
    const len = Math.max(logLen[0], 1024);
    const log = new Uint8Array(len);
    const actual = new Int32Array(1);
    gl.symbols.glGetShaderInfoLog(sh, len, new Uint8Array(actual.buffer), log);
    gl.symbols.glDeleteShader(sh);
    const msg = new TextDecoder().decode(log.subarray(0, actual[0]));
    throw new Error(`${name} shader compile failed: ${msg}`);
  }
  return sh;
}

// deno-lint-ignore no-explicit-any
function linkProgram(gl: any, vs: number, fs: number): number {
  const prog = gl.symbols.glCreateProgram();
  if (prog === 0) throw new Error("glCreateProgram returned 0");
  gl.symbols.glAttachShader(prog, vs);
  gl.symbols.glAttachShader(prog, fs);
  gl.symbols.glLinkProgram(prog);
  const status = new Int32Array(1);
  gl.symbols.glGetProgramiv(prog, GL_LINK_STATUS, new Uint8Array(status.buffer));
  if (status[0] === 0) {
    const logLen = new Int32Array(1);
    gl.symbols.glGetProgramiv(
      prog,
      GL_INFO_LOG_LENGTH,
      new Uint8Array(logLen.buffer),
    );
    const len = Math.max(logLen[0], 1024);
    const log = new Uint8Array(len);
    const actual = new Int32Array(1);
    gl.symbols.glGetProgramInfoLog(prog, len, new Uint8Array(actual.buffer), log);
    gl.symbols.glDeleteProgram(prog);
    const msg = new TextDecoder().decode(log.subarray(0, actual[0]));
    throw new Error(`program link failed: ${msg}`);
  }
  return prog;
}

// =============================================================================
// Processor
// =============================================================================

export default class OpenGlFragmentShaderProcessor implements ReactiveProcessor {
  private uuid = "";
  private width = 0;
  private height = 0;
  private opengl: OpenGLContext | null = null;
  // Dual-register the surface with the Vulkan adapter too so the
  // producer-side QFOT release barrier (#644) can publish layout to
  // host consumers via OpenGLContext.releaseForCrossProcess. OpenGL
  // itself does GL writes; the Vulkan adapter owns the cross-process
  // release barrier — engine-model composition per
  // docs/architecture/adapter-authoring.md.
  private vulkan: VulkanContext | null = null;
  private rendered = false;
  private errorMessage: string | null = null;

  setup(ctx: RuntimeContextFullAccess): void {
    const cfg = ctx.config;
    this.uuid = String(cfg["opengl_surface_uuid"]);
    this.width = Number(cfg["width"] ?? 0);
    this.height = Number(cfg["height"] ?? 0);
    this.opengl = OpenGLContext.fromRuntime(ctx);
    this.vulkan = VulkanContext.fromRuntime(ctx);
    console.error(
      `[OpenGlFragmentShader/deno] setup uuid=${this.uuid} ` +
        `size=${this.width}x${this.height}`,
    );
  }

  process(ctx: RuntimeContextLimitedAccess): void {
    const result = ctx.inputs.read("video_in");
    if (!result) return;
    if (this.rendered) return;
    try {
      this.renderOnce();
      this.rendered = true;
      console.error(
        `[OpenGlFragmentShader/deno] plasma rendered into surface '${this.uuid}'`,
      );
    } catch (e) {
      this.errorMessage = e instanceof Error ? e.message : String(e);
      console.error(
        `[OpenGlFragmentShader/deno] render failed: ${this.errorMessage}`,
      );
    }
  }

  private renderOnce(): void {
    if (this.opengl === null || this.vulkan === null) {
      throw new Error(
        "OpenGLContext / VulkanContext were not initialized in setup",
      );
    }
    // Wrap the GL block in `{ … }` so the `using guard` disposes
    // (running the adapter's `end_write_access` → `glFinish`) BEFORE
    // the cross-process release call below — the QFOT release barrier
    // assumes producer-side hazard coverage upstream.
    {
      using guard = this.opengl.acquireWrite(this.uuid);
      const textureId = guard.view.glTextureId;
      const gl = loadLibGL();

      // Empty VAO — desktop core requires one bound; geometry comes
      // from gl_VertexID in the vertex shader.
      const vao = new Uint32Array(1);
      gl.symbols.glGenVertexArrays(1, new Uint8Array(vao.buffer));
      gl.symbols.glBindVertexArray(vao[0]);
      try {
        const vs = compileShader(
          gl,
          VERTEX_SHADER,
          GL_VERTEX_SHADER,
          "vertex",
        );
        let program: number;
        try {
          const fs = compileShader(
            gl,
            FRAGMENT_SHADER,
            GL_FRAGMENT_SHADER,
            "fragment",
          );
          try {
            program = linkProgram(gl, vs, fs);
          } finally {
            gl.symbols.glDeleteShader(fs);
          }
        } finally {
          gl.symbols.glDeleteShader(vs);
        }

        try {
          // FBO with the imported texture as color attachment.
          const fbo = new Uint32Array(1);
          gl.symbols.glGenFramebuffers(1, new Uint8Array(fbo.buffer));
          gl.symbols.glBindFramebuffer(GL_FRAMEBUFFER, fbo[0]);
          gl.symbols.glFramebufferTexture2D(
            GL_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_TEXTURE_2D,
            textureId,
            0,
          );
          const status = gl.symbols.glCheckFramebufferStatus(GL_FRAMEBUFFER);
          if (status !== GL_FRAMEBUFFER_COMPLETE) {
            throw new Error(
              `FBO incomplete (status=0x${
                status.toString(16)
              }) — the imported texture may have been bound with an ` +
                `external_only modifier; the host allocator should pick ` +
                `a tiled, render-target-capable DRM modifier`,
            );
          }

          gl.symbols.glViewport(0, 0, this.width, this.height);
          gl.symbols.glUseProgram(program);

          const resolutionName = new TextEncoder().encode("resolution\0");
          const loc = gl.symbols.glGetUniformLocation(program, resolutionName);
          if (loc >= 0) {
            gl.symbols.glUniform2f(loc, this.width, this.height);
          }

          gl.symbols.glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
          gl.symbols.glFinish();

          const glErr = gl.symbols.glGetError();
          if (glErr !== GL_NO_ERROR) {
            throw new Error(
              `GL error 0x${glErr.toString(16)} after draw — see ` +
                `docs/learnings/nvidia-egl-dmabuf-render-target.md`,
            );
          }

          gl.symbols.glBindFramebuffer(GL_FRAMEBUFFER, 0);
          gl.symbols.glDeleteFramebuffers(1, new Uint8Array(fbo.buffer));
        } finally {
          gl.symbols.glDeleteProgram(program);
        }
      } finally {
        gl.symbols.glBindVertexArray(0);
        gl.symbols.glDeleteVertexArrays(1, new Uint8Array(vao.buffer));
      }
      // `using guard` runs adapter `end_write_access` at scope exit
      // which drains GL via glFinish so the host's DMA-BUF readback
      // sees a fully-flushed image AND the cross-process release
      // barrier below sees coherent contents through the DMA-BUF.
    }

    // Producer-side cross-process release (#644). The OpenGL adapter
    // has no Vulkan device of its own and never issues a release
    // barrier on the imported VkImage — delegate to the Vulkan
    // adapter (dual-registration). General is the right choice for
    // OpenGL-backed surfaces (the host's pre-stop readback also reads
    // with TextureSourceLayout::General). Pairs with any future host
    // consumer's `acquire_from_foreign` via the bridging fallback on
    // NVIDIA / QFOT-acquire on Mesa.
    this.opengl.releaseForCrossProcess(
      this.uuid,
      this.vulkan,
      VkImageLayout.General,
    );
    console.error(
      `[OpenGlFragmentShader/deno] published cross-process release ` +
        `layout=General for surface '${this.uuid}'`,
    );
  }

  teardown(_ctx: RuntimeContextFullAccess): void {
    console.error(
      `[OpenGlFragmentShader/deno] teardown rendered=${this.rendered} ` +
        `error=${this.errorMessage}`,
    );
  }
}
