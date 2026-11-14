# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The frame rate the upstream runs at, measured from two consecutive frames' stamps."""

from __future__ import annotations

import dataclasses

NANOSECONDS_PER_SECOND = 1_000_000_000


@dataclasses.dataclass(frozen=True)
class FirstFrameOfAnExtentAwaitingItsRate:
    """The first frame of an extent the camera has not yet been configured for."""

    width: int
    height: int
    timestamp_ns: int

    def is_the_extent_of(self, width: int, height: int) -> bool:
        """Whether a later frame carries this frame's extent."""
        return (self.width, self.height) == (width, height)


def frames_per_second_between(
    earlier_frame_timestamp_ns: int, later_frame_timestamp_ns: int
) -> "int | None":
    """The whole frame rate one inter-frame interval runs at, or `None` when the
    stamps do not advance or the interval is too long to round to one frame a second."""
    interval_ns = later_frame_timestamp_ns - earlier_frame_timestamp_ns
    if interval_ns <= 0:
        return None
    frames_per_second = round(NANOSECONDS_PER_SECOND / interval_ns)
    return frames_per_second if frames_per_second >= 1 else None
