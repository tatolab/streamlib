# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Vulkan-native surface adapter — Python customer-facing API.

Mirrors the Rust crate ``streamlib-adapter-vulkan`` (#511). The
subprocess's actual Vulkan handling lives in
``streamlib-python-native``'s ``SurfaceShareVulkanDevice``; this
module provides:

  * Typed views the subprocess sees inside ``acquire_*`` scopes —
    ``VulkanReadView`` / ``VulkanWriteView`` exposing ``vk_image`` (an
    integer handle) plus the current ``vk_image_layout``.
  * A ``VulkanContext`` Protocol the subprocess runtime implements —
    customers don't construct one, they receive it via the runtime
    and call ``acquire_write(surface)`` inside a ``with`` block.
  * ``raw_handles()`` — escape hatch returning the underlying
    ``vk_instance``, ``vk_device``, ``vk_queue``, etc. as integer
    handles for power-user callers that want to drive Vulkan
    directly.

Subprocess Vulkan adapters MUST NOT call ``vkCreateDevice`` themselves;
the binding's ``SurfaceShareVulkanDevice`` already creates one at
init. Per ``docs/learnings/nvidia-dual-vulkan-device-crash.md``, a
second ``VkDevice`` while the first has active GPU work crashes on
NVIDIA — same-process safety; subprocesses are independent processes.
"""

from __future__ import annotations

import ctypes
from contextlib import AbstractContextManager
from dataclasses import dataclass
from typing import Iterator, Optional, Protocol, runtime_checkable

from streamlib.surface_adapter import (
    STREAMLIB_ADAPTER_ABI_VERSION,
    StreamlibSurface,
    SurfaceAdapter,
    SurfaceFormat,
    SurfaceUsage,
)

__all__ = [
    "STREAMLIB_ADAPTER_ABI_VERSION",
    "RawVulkanHandles",
    "VulkanReadView",
    "VulkanWriteView",
    "VulkanSurfaceAdapter",
    "VulkanContext",
]


# Vulkan layout integer values mirroring vk::ImageLayout. Used so
# customer code doesn't need to import a Vulkan binding just to read
# `view.vk_image_layout`.
class VkImageLayout:
    UNDEFINED = 0
    GENERAL = 1
    COLOR_ATTACHMENT_OPTIMAL = 2
    SHADER_READ_ONLY_OPTIMAL = 5
    TRANSFER_SRC_OPTIMAL = 6
    TRANSFER_DST_OPTIMAL = 7


@dataclass(frozen=True)
class RawVulkanHandles:
    """Power-user escape hatch — raw Vulkan handles as integers.

    Customers wrap with their preferred Python Vulkan binding (e.g.
    ``vulkan.vkInstance(handle=...)``). Returned handles are valid
    for the lifetime of the streamlib runtime; using them after
    runtime shutdown is undefined.
    """

    vk_instance: int
    vk_physical_device: int
    vk_device: int
    vk_queue: int
    vk_queue_family_index: int
    api_version: int


@dataclass(frozen=True)
class VulkanReadView:
    """View handed back inside an ``acquire_read`` scope.

    ``vk_image`` is the integer Vulkan handle the customer feeds into
    their binding (``vulkan.VkImage(value=view.vk_image)`` or
    equivalent). ``vk_image_layout`` is the layout the adapter just
    transitioned the image to (``SHADER_READ_ONLY_OPTIMAL`` on read).
    """

    vk_image: int
    vk_image_layout: int


@dataclass(frozen=True)
class VulkanWriteView:
    """View handed back inside an ``acquire_write`` scope.

    Layout is ``GENERAL`` so the customer can use it as a transfer
    destination, color attachment, or compute storage image without
    re-transitioning.
    """

    vk_image: int
    vk_image_layout: int


@runtime_checkable
class VulkanSurfaceAdapter(Protocol):
    """Protocol an in-process Python Vulkan adapter implements.

    ``surface_id`` based, so the subprocess can pass just the
    ``StreamlibSurface.id`` from a descriptor it received over the
    surface-share IPC.
    """

    def acquire_read(
        self, surface: StreamlibSurface
    ) -> AbstractContextManager[VulkanReadView]: ...

    def acquire_write(
        self, surface: StreamlibSurface
    ) -> AbstractContextManager[VulkanWriteView]: ...

    def try_acquire_read(
        self, surface: StreamlibSurface
    ) -> Optional[AbstractContextManager[VulkanReadView]]: ...

    def try_acquire_write(
        self, surface: StreamlibSurface
    ) -> Optional[AbstractContextManager[VulkanWriteView]]: ...

    def raw_handles(self) -> RawVulkanHandles: ...


@runtime_checkable
class VulkanContext(Protocol):
    """Customer-facing handle the subprocess runtime hands out.

    Equivalent shape to the Rust ``VulkanContext`` — thin wrapper over
    a ``VulkanSurfaceAdapter`` so customer code can write::

        with ctx.acquire_write(surface) as view:
            do_vulkan_work(view.vk_image, view.vk_image_layout)
    """

    def acquire_read(
        self, surface: StreamlibSurface
    ) -> AbstractContextManager[VulkanReadView]: ...

    def acquire_write(
        self, surface: StreamlibSurface
    ) -> AbstractContextManager[VulkanWriteView]: ...

    def try_acquire_read(
        self, surface: StreamlibSurface
    ) -> Optional[AbstractContextManager[VulkanReadView]]: ...

    def try_acquire_write(
        self, surface: StreamlibSurface
    ) -> Optional[AbstractContextManager[VulkanWriteView]]: ...

    def raw_handles(self) -> RawVulkanHandles: ...
