# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A pin type of the sample's own: `Sweep`, a class under `@named_pin_type`, named by its import
path `nodes.sweep:Sweep`.

`SweepAt` writes a `Sweep` and `SweepPosition` reads it, so a named type links between two
nodes, to a pin of the same name or to an input of any type. `SweepPosition` states that it
converts a `Sweep` to a Float, so a link from a `Sweep` into a Float input is refused naming it,
with the call that places it between the two. A `Sweep` travels as the bytes
`to_bytes` packs and is built back with `from_bytes`. The sample's `node_ui/` registers the type's default control,
which only the module that declares the type may register.
"""

from __future__ import annotations

import struct
from collections.abc import Buffer
from dataclasses import dataclass
from typing import Annotated, Self

from tatolab.stream import Description, NumberRange, NumberStep, named_pin_type, node

#: A `Sweep` as bytes: its position and its width, each a little-endian f64.
SWEEP_PACKING = struct.Struct("<2d")


@named_pin_type(description="A band sweeping across the picture: where it is, 0 to 1, and how wide it is.")
@dataclass(frozen=True)
class Sweep:
    """A band across the picture, at `position` from its left edge and `width` wide, each a fraction of its width."""

    position: float
    width: float

    @classmethod
    def from_bytes(cls, payload: Buffer) -> Self:
        position, width = SWEEP_PACKING.unpack(bytes(payload))
        return cls(position, width)

    def to_bytes(self) -> bytes:
        return SWEEP_PACKING.pack(self.position, self.width)


@node.meta(
    category="Every pin kind",
    description="A band at one place across the picture, as the sample's own named type.",
)
@node
def SweepAt(
    position: Annotated[
        float,
        NumberRange(minimum=0.0, maximum=1.0),
        NumberStep(0.01),
        Description("Where the band is, from the picture's left edge."),
    ] = 0.5,
    width: Annotated[
        float,
        NumberRange(minimum=0.0, maximum=1.0),
        NumberStep(0.01),
        Description("How wide the band is."),
    ] = 0.1,
) -> Sweep:
    """The band at `position`, `width` wide."""
    return Sweep(position, width)


@node.meta(
    category="Every pin kind",
    description="Where a band is across the picture, read from the sample's own named type.",
    conversion=True,
)
@node
def SweepPosition(sweep: Sweep) -> float:
    """Where `sweep` is, from the picture's left edge."""
    return sweep.position
