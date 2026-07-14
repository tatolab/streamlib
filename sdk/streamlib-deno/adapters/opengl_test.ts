// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Smoke test for the Deno OpenGL adapter wrapper module.
 *
 * Confirms the module loads, the `GL_TEXTURE_2D` constant matches
 * the Rust side, and the type shapes (`OpenGLReadView`,
 * `OpenGLWriteView`, `OpenGLContext`) are present. End-to-end
 * cross-process GL verification lives in the Rust integration tests
 * in `streamlib-adapter-opengl` — Deno code that consumes this
 * contract uses the FFI binding in `streamlib-deno-native`.
 */

import { assertEquals } from "@std/assert";
import {
  GL_TEXTURE_2D,
  type OpenGLReadView,
  type OpenGLWriteView,
  STREAMLIB_ADAPTER_ABI_VERSION,
} from "./opengl.ts";

Deno.test("ABI version matches Rust", () => {
  assertEquals(STREAMLIB_ADAPTER_ABI_VERSION, 1);
});

Deno.test("GL_TEXTURE_2D matches the GL spec value 0x0DE1", () => {
  assertEquals(GL_TEXTURE_2D, 0x0DE1);
});

Deno.test("OpenGLReadView shape carries glTextureId and target", () => {
  const view: OpenGLReadView = { glTextureId: 42, target: GL_TEXTURE_2D };
  assertEquals(view.glTextureId, 42);
  assertEquals(view.target, GL_TEXTURE_2D);
});

Deno.test("OpenGLWriteView shape carries glTextureId and target", () => {
  const view: OpenGLWriteView = { glTextureId: 77, target: GL_TEXTURE_2D };
  assertEquals(view.glTextureId, 77);
  assertEquals(view.target, GL_TEXTURE_2D);
});
