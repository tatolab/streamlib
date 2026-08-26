# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A processor's output textures, allocated once and rotated per frame.

The cross-process sibling of the engine's `TextureRing` (which is
same-process-only by design — its slots are non-exportable and Path-1-only, so
no helper can resolve them). This ring composes `acquire_texture`, whose slots
the engine allocates cross-process-importable and registers with the
surface-share service — which is exactly what a helper-placed producer's
published ids need.

Two facts make the ring the right shape rather than acquiring a texture inside
`process()`:

- An acquired texture's registration *is* its handle. A producer that lets the
  handle go at the end of `process()` unregisters the surface id a consumer in
  another process was handed a moment earlier, and that consumer's resolve is
  refused by name. A ring never lets go, so the question does not arise.
- A per-frame acquire pays an escalate round trip, pool work and a
  surface-share registration on every frame; a ring pays them once per slot.

What depth bounds is how far behind a consumer may fall, not how fast anything
runs: a slot is redrawn when its turn comes around again, so a consumer still
sampling a frame `depth` publishes old reads the producer's newer pixels. The
engine's own ring runs two slots, and so does this one by default.
"""

from __future__ import annotations

from typing import Union

from ._engine import (
    GpuContextFullAccess,
    GpuContextLimitedAccess,
    GpuSurfaceHandle,
)

__all__ = ["ProcessorOutputTextureRing"]

STANDARD_RING_DEPTH = 2

_GpuContextWithAcquireTexture = Union[GpuContextLimitedAccess, GpuContextFullAccess]


class ProcessorOutputTextureRing:
    """Output textures a processor publishes frames from, one slot per frame."""

    def __init__(
        self,
        texture_format: str,
        texture_usage: "list[str]",
        depth: int = STANDARD_RING_DEPTH,
    ) -> None:
        # `bool` is an `int` subclass; a ring of depth `True` is a bug.
        if not isinstance(depth, int) or isinstance(depth, bool):
            raise ValueError(
                f"depth must be an int, got {depth!r} — a ring holds a whole "
                f"number of textures"
            )
        if depth < 1:
            raise ValueError(
                f"a ring of depth {depth} holds no texture to publish from — "
                f"depth must be at least 1, and the standard depth is "
                f"{STANDARD_RING_DEPTH}"
            )
        self._texture_format = texture_format
        self._texture_usage = texture_usage
        self._depth = depth
        self._slots: "list[GpuSurfaceHandle]" = []
        self._extent_the_slots_were_allocated_for: "tuple[int, int] | None" = None
        self._next_slot_index = 0

    @property
    def depth(self) -> int:
        """How many published frames stay resolvable behind the newest one."""
        return self._depth

    def next_texture_for_this_frame(
        self,
        gpu_context: _GpuContextWithAcquireTexture,
        width: int,
        height: int,
    ) -> GpuSurfaceHandle:
        """The slot this frame publishes into, allocating the ring on first use.

        Allocated here rather than at construction because the extent is
        usually the upstream producer's answer — whatever a camera negotiated
        arrives with its first frame. An extent change releases the old slots
        and allocates fresh ones, rather than publishing frames into slots the
        wrong size.
        """
        if self._extent_the_slots_were_allocated_for != (width, height):
            # Emptied first, so the old slots' releases reach the engine before
            # the new acquires ask the pool to hold both extents at once.
            self._slots = []
            self._slots = [
                gpu_context.acquire_texture(
                    width, height, self._texture_format, self._texture_usage
                )
                for _ in range(self._depth)
            ]
            self._extent_the_slots_were_allocated_for = (width, height)
            self._next_slot_index = 0

        slot_for_this_frame = self._slots[self._next_slot_index]
        self._next_slot_index = (self._next_slot_index + 1) % self._depth
        return slot_for_this_frame
