# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Output textures allocated once and rotated per frame.

The Python spelling of the engine's own `TextureRing`, which
`docs/architecture/texture-ring.md` names the canonical shape for a producer's
per-frame output: allocate the slots up front, rotate through them, publish a
stable per-slot surface id, and never release. The engine's Rust constructor is
not reachable from a helper process, but the discipline is the point and the
primitive is — `acquire_texture` is on the Limited capability.

Two things make it the right shape rather than acquiring a texture per frame,
and both were measured on an RTX 3090 at 1920x1080:

- **Cost.** A per-frame `acquire_texture` plus the copy into it runs 7.2 ms;
  the copy into a slot the ring already holds runs 2.3 ms. The ~4.9 ms
  difference is the escalate round trip, the pool work, and the surface-share
  registration — paid once here instead of once per frame per processor.
- **Lifetime.** An acquired texture's registration *is* its handle, so a
  producer that lets go at the end of `process()` unregisters the surface id a
  consumer one process away was handed a millisecond earlier. A ring never lets
  go, so the question does not arise.

Depth follows the engine's own ring at 2: the producer draws into the next slot
while a consumer still reads the last one, and comes back around a frame later.
"""

from __future__ import annotations

from streamlib import GpuContextLimitedAccess, GpuSurfaceHandle

from .gpu_surface_conventions import TEXTURE_FORMAT

__all__ = ["PUBLISHED_TEXTURE_RING_DEPTH", "PublishedTextureRing"]

PUBLISHED_TEXTURE_RING_DEPTH = 2


class PublishedTextureRing:
    """A processor's own output textures, rotated one per published frame."""

    def __init__(
        self,
        texture_usage: "list[str]",
        depth: int = PUBLISHED_TEXTURE_RING_DEPTH,
    ) -> None:
        self.texture_usage = texture_usage
        self.depth = depth
        self._slots: "list[GpuSurfaceHandle]" = []
        self._extent: "tuple[int, int] | None" = None
        self._next_slot_index = 0

    def next_texture_for_this_frame(
        self, gpu_limited_access: GpuContextLimitedAccess, width: int, height: int
    ) -> GpuSurfaceHandle:
        """The slot this frame publishes into, allocating the ring on first use.

        Allocated here rather than in `setup()` because the extent is the
        camera's answer, not the app's: whatever it negotiated arrives with the
        first frame. A later change of extent — a different capture format, a
        different source — reallocates rather than publishing frames into
        slots the wrong size.
        """
        if self._extent != (width, height):
            # Dropped before the new ones are asked for, so the old slots'
            # memory is back in the pool before it has to hold both.
            self._slots = []
            self._slots = [
                gpu_limited_access.acquire_texture(
                    width, height, TEXTURE_FORMAT, self.texture_usage
                )
                for _ in range(self.depth)
            ]
            self._extent = (width, height)
            self._next_slot_index = 0

        slot = self._slots[self._next_slot_index]
        self._next_slot_index = (self._next_slot_index + 1) % self.depth
        return slot
