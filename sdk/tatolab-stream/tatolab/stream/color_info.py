# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""An image's H.273 colour description, axis by axis."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal

__all__ = ["ColorInfo"]

Primaries = Literal[
    "bt2020", "bt470_bg", "bt470_m", "bt709", "ebu3213", "film",
    "smpte170m", "smpte240m", "smpte428", "smpte431", "smpte432",
]
Transfer = Literal[
    "arib_std_b67", "bt1361", "bt2020_ten_bit", "bt2020_twelve_bit", "bt709",
    "gamma22", "gamma28", "linear", "log100", "log100_sqrt10", "smpte170m",
    "smpte2084", "smpte240m", "smpte428", "srgb", "xvycc",
]
Matrix = Literal[
    "bt2020_cl", "bt2020_ncl", "bt470_bg", "bt709", "chroma_cl", "chroma_ncl",
    "fcc", "ictcp", "identity", "smpte170m", "smpte2085", "smpte240m", "ycgco",
]
Range = Literal["full", "limited"]


@dataclass(frozen=True)
class ColorInfo:
    """H.273 / ITU-T VUI four-tuple. ``None`` means unspecified."""

    primaries: Primaries | None = None
    transfer: Transfer | None = None
    matrix: Matrix | None = None
    range: Range | None = None
