// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

/**
 * Vulkan-native surface adapter — Deno customer-facing API.
 *
 * Mirrors the Rust crate `streamlib-adapter-vulkan` (#511). The
 * subprocess's actual Vulkan handling lives in
 * `streamlib-deno-native`'s `SurfaceShareVulkanDevice`; this module
 * provides:
 *
 *  - `VulkanReadView` / `VulkanWriteView` — typed views the subprocess
 *    sees inside `acquireRead` / `acquireWrite` scopes; expose
 *    `vkImage` (a `bigint` Vulkan handle) plus the current
 *    `vkImageLayout`.
 *  - `VulkanContext` interface — the runtime hands one out, customers
 *    use TC39 `using` blocks for scoped acquire/release.
 *  - `RawVulkanHandles` + `rawHandles()` shape — escape hatch for
 *    customers driving Vulkan directly.
 *
 * Note: the issue body specifies the path
 * `streamlib-deno/types/adapters/vulkan.ts`. The existing layout
 * keeps top-level modules (`surface_adapter.ts`, `escalate.ts`, etc.)
 * directly under `streamlib-deno/`, so we put the new module under
 * `streamlib-deno/adapters/vulkan.ts` to match. If a future
 * refactor introduces `types/`, this file moves with it.
 */

import {
  STREAMLIB_ADAPTER_ABI_VERSION,
  type StreamlibSurface,
  type SurfaceAccessGuard,
} from "../surface_adapter.ts";

export { STREAMLIB_ADAPTER_ABI_VERSION };

/** Mirror of `vk::ImageLayout` enumerant values used in views. */
export const VkImageLayout = {
  Undefined: 0,
  General: 1,
  ColorAttachmentOptimal: 2,
  ShaderReadOnlyOptimal: 5,
  TransferSrcOptimal: 6,
  TransferDstOptimal: 7,
} as const;
export type VkImageLayout = (typeof VkImageLayout)[keyof typeof VkImageLayout];

/** Read-side view inside an `acquireRead` scope. */
export interface VulkanReadView {
  readonly vkImage: bigint;
  readonly vkImageLayout: VkImageLayout;
}

/** Write-side view inside an `acquireWrite` scope. */
export interface VulkanWriteView {
  readonly vkImage: bigint;
  readonly vkImageLayout: VkImageLayout;
}

/**
 * Power-user escape hatch — raw Vulkan handles as `bigint`s the
 * customer feeds into their preferred Vulkan binding (e.g. through
 * `Deno.UnsafePointer.create`). Valid for the lifetime of the
 * runtime; using them after shutdown is undefined.
 */
export interface RawVulkanHandles {
  readonly vkInstance: bigint;
  readonly vkPhysicalDevice: bigint;
  readonly vkDevice: bigint;
  readonly vkQueue: bigint;
  readonly vkQueueFamilyIndex: number;
  readonly apiVersion: number;
}

/** Public Vulkan adapter contract. */
export interface VulkanSurfaceAdapter {
  acquireRead(surface: StreamlibSurface): SurfaceAccessGuard<VulkanReadView>;
  acquireWrite(
    surface: StreamlibSurface,
  ): SurfaceAccessGuard<VulkanWriteView>;
  tryAcquireRead(
    surface: StreamlibSurface,
  ): SurfaceAccessGuard<VulkanReadView> | null;
  tryAcquireWrite(
    surface: StreamlibSurface,
  ): SurfaceAccessGuard<VulkanWriteView> | null;
  rawHandles(): RawVulkanHandles;
}

/** Customer-facing context. Same shape as the adapter — the runtime
 * wraps the adapter and hands the context out. Mirrors the Rust
 * `VulkanContext`. */
export type VulkanContext = VulkanSurfaceAdapter;
