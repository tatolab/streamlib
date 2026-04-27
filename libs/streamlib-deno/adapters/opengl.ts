// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * OpenGL/EGL surface adapter — Deno customer-facing API.
 *
 * Mirrors the Rust crate `streamlib-adapter-opengl` (#512). The
 * subprocess's actual EGL+GL handling lives in the runtime's
 * native binding; this module provides:
 *
 *  - `OpenGLReadView` / `OpenGLWriteView` — typed views the
 *    subprocess sees inside `acquireRead` / `acquireWrite` scopes;
 *    expose a single `glTextureId` (a `number` GL handle) and the
 *    constant `target = GL_TEXTURE_2D`.
 *  - `OpenGLContext` interface — the runtime hands one out,
 *    customers use TC39 `using` blocks for scoped acquire/release.
 *
 * Customers never see DMA-BUF FDs, fourcc codes, plane offsets,
 * strides, or DRM modifiers. Per the NVIDIA EGL DMA-BUF
 * render-target learning, the host allocator picks a tiled,
 * render-target-capable modifier so the resulting GL texture is
 * always a regular `GL_TEXTURE_2D` — never `GL_TEXTURE_EXTERNAL_OES`.
 */

import {
  STREAMLIB_ADAPTER_ABI_VERSION,
  type StreamlibSurface,
  type SurfaceAccessGuard,
} from "../surface_adapter.ts";

export { STREAMLIB_ADAPTER_ABI_VERSION };

/** `GL_TEXTURE_2D` enumerant — re-exported so customers don't have
 * to import a GL binding just to compare `view.target`. Matches the
 * Rust crate's `GL_TEXTURE_2D` constant. */
export const GL_TEXTURE_2D = 0x0DE1 as const;

/** Read-side view inside an `acquireRead` scope. */
export interface OpenGLReadView {
  /** GL texture id the customer feeds into their GL stack. */
  readonly glTextureId: number;
  /** Always `GL_TEXTURE_2D` — never `GL_TEXTURE_EXTERNAL_OES`. */
  readonly target: typeof GL_TEXTURE_2D;
}

/** Write-side view inside an `acquireWrite` scope. */
export interface OpenGLWriteView {
  readonly glTextureId: number;
  readonly target: typeof GL_TEXTURE_2D;
}

/** Public OpenGL adapter contract. */
export interface OpenGLSurfaceAdapter {
  acquireRead(surface: StreamlibSurface): SurfaceAccessGuard<OpenGLReadView>;
  acquireWrite(
    surface: StreamlibSurface,
  ): SurfaceAccessGuard<OpenGLWriteView>;
  tryAcquireRead(
    surface: StreamlibSurface,
  ): SurfaceAccessGuard<OpenGLReadView> | null;
  tryAcquireWrite(
    surface: StreamlibSurface,
  ): SurfaceAccessGuard<OpenGLWriteView> | null;
}

/** Customer-facing context. Same shape as the adapter — the runtime
 * wraps the adapter and hands the context out. Mirrors the Rust
 * `OpenGlContext`. */
export type OpenGLContext = OpenGLSurfaceAdapter;
