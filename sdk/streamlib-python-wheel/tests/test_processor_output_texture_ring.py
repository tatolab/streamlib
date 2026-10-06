# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The output ring's request discipline, with the capability stood in for.

The real capability needs a running engine, and the engine is what decides
which slot is next (`test_texture_ring_producer.py` proves that against one).
What these tests own is what the class adds over the capability: every frame
asks the engine, under one pool key the ring owns alone, with its depth, format,
usage and the frame's extent.
"""

from __future__ import annotations

from typing import cast

import pytest

from tatolab.stream import GpuContextLimitedAccess, NodeOutputTextureRing

RING_FORMAT = "rgba8_unorm"
RING_USAGE = ["render_attachment", "texture_binding"]


class SurfaceHandleStandIn:
    def __init__(self, surface_id: str) -> None:
        self.surface_id = surface_id


class GpuContextStandIn:
    """Records every processor output pool acquire and answers a numbered frame."""

    def __init__(self) -> None:
        self.acquires: "list[tuple[object, ...]]" = []

    def acquire_texture_from_node_output_pool(
        self,
        pool_key: str,
        rotation_depth: int,
        width: int,
        height: int,
        texture_format: str,
        usage: "list[str]",
    ) -> SurfaceHandleStandIn:
        self.acquires.append(
            (pool_key, rotation_depth, f"{width}x{height}", texture_format, *usage)
        )
        return SurfaceHandleStandIn(f"stand-in#{len(self.acquires)}")


def capability(stand_in: GpuContextStandIn) -> GpuContextLimitedAccess:
    """The stand-in, worn as the capability the ring's signature names."""
    return cast(GpuContextLimitedAccess, stand_in)


def test_every_frame_asks_the_engine_for_its_slot() -> None:
    """The engine decides reuse; a ring that rotated on its own would publish
    into a slot a consumer still holds."""
    gpu = GpuContextStandIn()
    ring = NodeOutputTextureRing(RING_FORMAT, RING_USAGE, depth=3)
    published = [
        ring.next_texture_for_this_frame(capability(gpu), 640, 360).surface_id
        for _ in range(5)
    ]
    assert published == [f"stand-in#{frame}" for frame in range(1, 6)]
    assert len(gpu.acquires) == 5


def test_one_ring_asks_under_one_pool_key_and_two_rings_never_share_one() -> None:
    gpu = GpuContextStandIn()
    first_ring = NodeOutputTextureRing(RING_FORMAT, RING_USAGE)
    second_ring = NodeOutputTextureRing(RING_FORMAT, RING_USAGE)
    for _ in range(3):
        first_ring.next_texture_for_this_frame(capability(gpu), 64, 64)
    second_ring.next_texture_for_this_frame(capability(gpu), 64, 64)
    pool_keys = [acquire[0] for acquire in gpu.acquires]
    assert len(set(pool_keys[:3])) == 1
    assert pool_keys[3] != pool_keys[0]


def test_the_depth_format_usage_and_extent_reach_every_acquire_as_given() -> None:
    gpu = GpuContextStandIn()
    ring = NodeOutputTextureRing("bgra8_unorm", ["texture_binding"], depth=3)
    ring.next_texture_for_this_frame(capability(gpu), 64, 32)
    ring.next_texture_for_this_frame(capability(gpu), 1920, 1080)
    assert [acquire[1:] for acquire in gpu.acquires] == [
        (3, "64x32", "bgra8_unorm", "texture_binding"),
        (3, "1920x1080", "bgra8_unorm", "texture_binding"),
    ]


def test_a_depthless_ring_is_refused_naming_the_depth() -> None:
    with pytest.raises(ValueError, match="depth must be at least 1"):
        NodeOutputTextureRing(RING_FORMAT, RING_USAGE, depth=0)


def test_a_fractional_or_boolean_depth_is_refused_at_construction() -> None:
    """A fractional depth names no whole number of slots to rotate through,
    and `True` would silently become a one-deep ring."""
    with pytest.raises(ValueError, match="whole"):
        NodeOutputTextureRing(RING_FORMAT, RING_USAGE, depth=1.5)  # type: ignore[arg-type]
    with pytest.raises(ValueError, match="whole"):
        NodeOutputTextureRing(RING_FORMAT, RING_USAGE, depth=True)


def test_the_standard_depth_matches_the_engines_own_ring() -> None:
    assert NodeOutputTextureRing(RING_FORMAT, RING_USAGE).depth == 2
