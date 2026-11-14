# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""The frame an Array port's read returns: each named attribute a borrowed DLPack tensor.

Built from the message's Array descriptor and the attribute table its payload
starts with. A CPU attribute borrows the sample it arrived in; a GPU attribute
holds its pool slot against producer reuse while it lives.
"""

from __future__ import annotations

from collections.abc import Iterator, Mapping
from dataclasses import dataclass
from typing import TYPE_CHECKING, Union

from .claimed_surface_pixel_access import ClaimedSurfacePixelAccess

if TYPE_CHECKING:
    from ._node_context_protocols import ArrayFrameCpuAttribute

__all__ = ["ArrayFrame", "ArrayFrameGpuAttribute"]


@dataclass(frozen=True, init=False)
class ArrayFrameGpuAttribute(ClaimedSurfacePixelAccess):
    """One GPU-resident attribute of an Array frame: a claimed tensor surface."""

    surface_id: str
    dtype: str
    shape: "tuple[int, ...]"

    def __init__(self, surface_id: str, dtype: str, shape: "tuple[int, ...]") -> None:
        """Hold the row the attribute table states, then claim the surface."""
        super().__init__(surface_id=surface_id, dtype=dtype, shape=tuple(shape))


ArrayFrameAttribute = Union["ArrayFrameCpuAttribute", ArrayFrameGpuAttribute]


class ArrayFrame(Mapping[str, ArrayFrameAttribute]):
    """An Array frame, as `ctx.inputs.read(self.tensors)` returns it: name to tensor.

    Each value speaks `__dlpack__` and states its `dtype` and `shape`, so
    `torch.from_dlpack(frame["boxes"])` reads it in place.
    """

    __slots__ = ("_attributes_by_name", "_timestamp_ns")

    def __init__(
        self, attributes: "Mapping[str, ArrayFrameAttribute]", timestamp_ns: int
    ) -> None:
        """Hold the attributes a read built, in their table's order."""
        self._attributes_by_name = dict(attributes)
        self._timestamp_ns = timestamp_ns

    @property
    def timestamp_ns(self) -> int:
        """The stream time the frame was stamped with, in nanoseconds."""
        return self._timestamp_ns

    def __getitem__(self, attribute_name: str) -> ArrayFrameAttribute:
        return self._attributes_by_name[attribute_name]

    def __iter__(self) -> Iterator[str]:
        return iter(self._attributes_by_name)

    def __len__(self) -> int:
        return len(self._attributes_by_name)

    def __repr__(self) -> str:
        attributes = ", ".join(
            f"{name}: {attribute.dtype}{list(attribute.shape)}"
            for name, attribute in self._attributes_by_name.items()
        )
        return f"ArrayFrame({attributes})"
