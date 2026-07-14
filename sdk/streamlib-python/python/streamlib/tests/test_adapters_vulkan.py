# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Smoke test for the Python Vulkan adapter wrapper module.

Confirms the module imports, the Protocol shapes match what
adapter authors implement, and `RawVulkanHandles` round-trips
through dataclass construction (mirrors the Rust struct fields).

A real subprocess Python smoke test (vulkano-or-equivalent reading
a host-produced surface zero-copy) lives in the round-trip Rust
integration tests in ``streamlib-adapter-vulkan``; this file
exercises the Python module's contract.
"""

from __future__ import annotations

from streamlib.adapters import vulkan as v
from streamlib.surface_adapter import (
    STREAMLIB_ADAPTER_ABI_VERSION,
    SurfaceFormat,
    SurfaceUsage,
)


def test_module_re_exports_abi_version_constant():
    assert v.STREAMLIB_ADAPTER_ABI_VERSION == STREAMLIB_ADAPTER_ABI_VERSION


def test_raw_vulkan_handles_round_trip():
    h = v.RawVulkanHandles(
        vk_instance=0xdead_beef_cafe_0001,
        vk_physical_device=0x0000_0000_0010_0001,
        vk_device=0x0000_0000_0010_0002,
        vk_queue=0x0000_0000_0010_0003,
        vk_queue_family_index=0,
        api_version=(1 << 22) | (4 << 12),  # vk::make_version(1, 4, 0)
    )
    assert h.vk_instance == 0xdead_beef_cafe_0001
    assert h.vk_queue_family_index == 0
    assert h.api_version >> 22 == 1


def test_views_carry_image_handle_and_layout():
    rv = v.VulkanReadView(vk_image=0x100, vk_image_layout=v.VkImageLayout.SHADER_READ_ONLY_OPTIMAL)
    wv = v.VulkanWriteView(vk_image=0x200, vk_image_layout=v.VkImageLayout.GENERAL)
    assert rv.vk_image == 0x100
    assert rv.vk_image_layout == v.VkImageLayout.SHADER_READ_ONLY_OPTIMAL
    assert wv.vk_image == 0x200
    assert wv.vk_image_layout == v.VkImageLayout.GENERAL


def test_protocols_describe_expected_method_set():
    # `runtime_checkable` Protocols only check method NAMES, not full
    # signatures — but that's what we want here: any subprocess-side
    # adapter implementing acquire_read / acquire_write / etc. should
    # satisfy the Protocol structurally.
    assert hasattr(v.VulkanSurfaceAdapter, "acquire_read")
    assert hasattr(v.VulkanSurfaceAdapter, "acquire_write")
    assert hasattr(v.VulkanSurfaceAdapter, "try_acquire_read")
    assert hasattr(v.VulkanSurfaceAdapter, "try_acquire_write")
    assert hasattr(v.VulkanSurfaceAdapter, "raw_handles")
    assert hasattr(v.VulkanContext, "acquire_write")
    assert hasattr(v.VulkanContext, "raw_handles")
