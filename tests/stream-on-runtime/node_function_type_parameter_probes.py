# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A node function whose output is typed by its type parameter, and the reader past it.

`HandsOnEachPicture` states no format on its output: the output carries the type linked into
its input, so the pictures the test pattern renders reach the reader as they left it.
"""

import json
from typing import Annotated, Any, Callable

from tatolab.stream import (
    InputPin,
    OutputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    log,
    node,
)

RESULT_MARKER = "MARKER:PICTURES_HANDED_ON "
PICTURES_REPORTED = 2


@node(execution="reactive")
def HandsOnEachPicture[Frame: Texture](
    *,
    picture_in: InputPin[Annotated[Frame, StreamDelivery("ordered")]],
    picture_out: OutputPin[Frame],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Hands on each picture it reads, unchanged."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        frame = picture_in.read()
        if frame is not None:
            picture_out.write(frame)

    return each_frame


@node(execution="reactive")
def ReportsThePicturesItReads(
    *,
    pictures: InputPin[Annotated[Texture, StreamDelivery("ordered")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports the first pictures it reads."""

    seen: "list[dict[str, Any]]" = []

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        if len(seen) >= PICTURES_REPORTED:
            return
        frame = pictures.read()
        if frame is None:
            return
        seen.append(
            {
                "surface_id": frame.surface_id,
                "width": frame.width,
                "height": frame.height,
                "format": frame.format,
                "timestamp_ns": frame.timestamp_ns,
            }
        )
        if len(seen) == PICTURES_REPORTED:
            log.info(RESULT_MARKER + json.dumps(seen))

    return each_frame
