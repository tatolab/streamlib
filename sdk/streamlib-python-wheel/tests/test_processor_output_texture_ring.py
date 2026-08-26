# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The output ring's slot discipline, with the capability stood in for.

The real capability needs a running engine; what these tests own is the pure
contract the class adds over it — allocate once, rotate in order, hold every
slot alive, and reallocate on an extent change with the releases ordered ahead
of the new acquires. That ordering is the part a GPU run cannot assert: it
shows up there only as pool pressure.
"""

from __future__ import annotations

from typing import cast

import pytest

from streamlib import GpuContextLimitedAccess, ProcessorOutputTextureRing

RING_FORMAT = "rgba8_unorm"
RING_USAGE = ["render_attachment", "texture_binding"]


class SurfaceHandleStandIn:
    """Records its own release, which CPython's refcounting makes immediate."""

    def __init__(self, surface_id: str, events: "list[tuple[str, ...]]") -> None:
        self.surface_id = surface_id
        self._events = events

    def __del__(self) -> None:
        self._events.append(("released", self.surface_id))


class GpuContextStandIn:
    """Hands out numbered stand-in handles and records every acquire."""

    def __init__(self) -> None:
        self.events: "list[tuple[str, ...]]" = []
        self._minted = 0

    def acquire_texture(
        self, width: int, height: int, texture_format: str, usage: "list[str]"
    ) -> SurfaceHandleStandIn:
        self._minted += 1
        surface_id = f"stand-in-{self._minted}"
        self.events.append(
            ("acquired", surface_id, f"{width}x{height}", texture_format, *usage)
        )
        return SurfaceHandleStandIn(surface_id, self.events)


def acquires_in(events: "list[tuple[str, ...]]") -> "list[tuple[str, ...]]":
    return [event for event in events if event[0] == "acquired"]


def capability(stand_in: GpuContextStandIn) -> GpuContextLimitedAccess:
    """The stand-in, worn as the capability the ring's signature names."""
    return cast(GpuContextLimitedAccess, stand_in)


def test_the_ring_rotates_through_its_slots_in_order_and_wraps() -> None:
    gpu = GpuContextStandIn()
    ring = ProcessorOutputTextureRing(RING_FORMAT, RING_USAGE, depth=3)
    published = [
        ring.next_texture_for_this_frame(capability(gpu), 640, 360).surface_id
        for _ in range(7)
    ]
    assert published == [
        "stand-in-1", "stand-in-2", "stand-in-3",
        "stand-in-1", "stand-in-2", "stand-in-3",
        "stand-in-1",
    ]


def test_a_stable_extent_allocates_exactly_once() -> None:
    gpu = GpuContextStandIn()
    ring = ProcessorOutputTextureRing(RING_FORMAT, RING_USAGE)
    for _ in range(20):
        ring.next_texture_for_this_frame(capability(gpu), 1920, 1080)
    assert len(acquires_in(gpu.events)) == ring.depth


def test_the_format_and_usage_reach_every_acquire_as_given() -> None:
    gpu = GpuContextStandIn()
    ring = ProcessorOutputTextureRing("bgra8_unorm", ["texture_binding"])
    ring.next_texture_for_this_frame(capability(gpu), 64, 64)
    assert acquires_in(gpu.events) == [
        ("acquired", "stand-in-1", "64x64", "bgra8_unorm", "texture_binding"),
        ("acquired", "stand-in-2", "64x64", "bgra8_unorm", "texture_binding"),
    ]


def test_a_slots_surface_id_is_stable_across_rotations() -> None:
    """The stability is what lets a consumer's resolve outlive one frame."""
    gpu = GpuContextStandIn()
    ring = ProcessorOutputTextureRing(RING_FORMAT, RING_USAGE, depth=2)
    first_pass = ring.next_texture_for_this_frame(capability(gpu), 320, 240)
    ring.next_texture_for_this_frame(capability(gpu), 320, 240)
    same_slot_again = ring.next_texture_for_this_frame(capability(gpu), 320, 240)
    assert same_slot_again is first_pass


def test_an_extent_change_releases_the_old_slots_before_acquiring_new_ones() -> None:
    """The pool must never be asked to hold both extents at once."""
    gpu = GpuContextStandIn()
    ring = ProcessorOutputTextureRing(RING_FORMAT, RING_USAGE, depth=2)
    ring.next_texture_for_this_frame(capability(gpu), 1280, 720)
    ring.next_texture_for_this_frame(capability(gpu), 1920, 1080)

    # The order among the two releases is interpreter detail; what the pool
    # needs is that both land before either new acquire.
    event_kinds = [event[0] for event in gpu.events]
    assert event_kinds == [
        "acquired", "acquired", "released", "released", "acquired", "acquired",
    ]
    assert {event[1] for event in gpu.events if event[0] == "released"} == {
        "stand-in-1", "stand-in-2",
    }
    assert [event[2] for event in acquires_in(gpu.events)] == [
        "1280x720", "1280x720", "1920x1080", "1920x1080",
    ]


def test_a_depthless_ring_is_refused_naming_the_depth() -> None:
    with pytest.raises(ValueError, match="depth must be at least 1"):
        ProcessorOutputTextureRing(RING_FORMAT, RING_USAGE, depth=0)


def test_a_fractional_or_boolean_depth_is_refused_at_construction() -> None:
    """`range(1.5)` would raise a bare TypeError at the first frame instead,
    and `True` would silently become a one-deep ring."""
    with pytest.raises(ValueError, match="whole"):
        ProcessorOutputTextureRing(RING_FORMAT, RING_USAGE, depth=1.5)  # type: ignore[arg-type]
    with pytest.raises(ValueError, match="whole"):
        ProcessorOutputTextureRing(RING_FORMAT, RING_USAGE, depth=True)


def test_the_standard_depth_matches_the_engines_own_ring() -> None:
    assert ProcessorOutputTextureRing(RING_FORMAT, RING_USAGE).depth == 2
