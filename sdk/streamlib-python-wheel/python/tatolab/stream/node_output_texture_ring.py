# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A node's output textures, one engine-pooled slot per frame.

The cross-process sibling of the engine's `TextureRing` (which is
same-process-only by design — its slots are non-exportable and Path-1-only, so
no helper can resolve them). This ring names a node output pool the engine
owns: each frame asks it for the next slot, and the answer is a fresh
`<slot>#<generation>` id, registered with the surface-share service so a
consumer in another process can resolve it.

The engine, not this class, decides which slot is next: one a consumer still
holds — claimed by a typed cast, or resolved — is skipped and never rewritten,
so a downstream node holding an earlier output keeps seeing its pixels.
The pool rotates through `depth` slots while nobody holds anything, grows while
consumers hold frames, and at its cap refuses by name: the producer drops its
own frame, and never waits on a consumer. A frame nobody claimed stays
resolvable for `depth` publishes; after that its id is refused as recycled,
never answered with newer pixels.
"""

from __future__ import annotations

import uuid
from typing import Union

from tatolab.runtime._engine import (
    GpuContextFullAccess,
    GpuContextLimitedAccess,
    GpuSurfaceHandle,
)

__all__ = ["NodeOutputTextureRing"]

STANDARD_RING_DEPTH = 2

_GpuContextWithAcquireTexture = Union[GpuContextLimitedAccess, GpuContextFullAccess]


class NodeOutputTextureRing:
    """Output textures a node publishes frames from, one slot per frame."""

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
        self._processor_output_pool_key = f"processor-output-texture-ring-{uuid.uuid4().hex}"

    @property
    def depth(self) -> int:
        """How many published frames nobody claimed stay resolvable behind the newest one."""
        return self._depth

    def next_texture_for_this_frame(
        self,
        gpu_context: _GpuContextWithAcquireTexture,
        width: int,
        height: int,
    ) -> GpuSurfaceHandle:
        """The slot this frame publishes into, from the engine's pool.

        The extent is asked per frame because it is usually the upstream
        producer's answer — whatever a camera negotiated arrives with its first
        frame. An extent change replaces the pool's slots with ones the new
        size, rather than publishing frames into slots the wrong size.

        Raises when every slot the pool may grow to is held by a consumer: the
        frame is dropped, and the next one asks again.
        """
        return gpu_context.acquire_texture_from_node_output_pool(
            self._processor_output_pool_key,
            self._depth,
            width,
            height,
            self._texture_format,
            self._texture_usage,
        )
