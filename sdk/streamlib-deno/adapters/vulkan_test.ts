// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Smoke test for the Deno Vulkan adapter wrapper module.
 *
 * Confirms the module loads, layout constants match the Rust side,
 * and the type shapes are present. End-to-end cross-process Vulkan
 * verification lives in the Rust integration tests in
 * `streamlib-adapter-vulkan` — Deno code that consumes this contract
 * uses the FFI binding in `streamlib-deno-native`.
 */

import { assertEquals, assertExists } from "@std/assert";
import {
  RawVulkanHandles,
  STREAMLIB_ADAPTER_ABI_VERSION,
  VkImageLayout,
} from "./vulkan.ts";

Deno.test("ABI version matches Rust", () => {
  assertEquals(STREAMLIB_ADAPTER_ABI_VERSION, 1);
});

Deno.test("VkImageLayout constants match Rust enum values", () => {
  assertEquals(VkImageLayout.Undefined, 0);
  assertEquals(VkImageLayout.General, 1);
  assertEquals(VkImageLayout.ColorAttachmentOptimal, 2);
  assertEquals(VkImageLayout.ShaderReadOnlyOptimal, 5);
  assertEquals(VkImageLayout.TransferSrcOptimal, 6);
  assertEquals(VkImageLayout.TransferDstOptimal, 7);
});

Deno.test("RawVulkanHandles round-trip preserves bigint and number fields", () => {
  const h: RawVulkanHandles = {
    vkInstance: 0xdead_beef_cafe_0001n,
    vkPhysicalDevice: 0x0010_0001n,
    vkDevice: 0x0010_0002n,
    vkQueue: 0x0010_0003n,
    vkQueueFamilyIndex: 0,
    apiVersion: (1 << 22) | (4 << 12),
  };
  assertEquals(h.vkInstance, 0xdead_beef_cafe_0001n);
  assertEquals(h.vkQueueFamilyIndex, 0);
  assertEquals(h.apiVersion >> 22, 1);
  assertExists(h.vkDevice);
});
