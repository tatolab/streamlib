# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The Texture reader `test_graph_variables.py` reads a variable with, twice."""

import json
import os
from typing import Annotated, Callable

from tatolab.stream import Input, InputPin, RuntimeContextLimitedAccess, StreamDelivery, Texture, log, node

READ_MARKER_PREFIX = "MARKER:GRAPH_VARIABLE_READ_"


@node(execution="reactive")
def ImageVariableReader(
    *,
    image_from_the_variable: InputPin[Annotated[Texture, StreamDelivery("ordered")]],
    reader: Input[str] = Input("reader"),
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports the first Texture frame it reads, under a marker named for `reader`."""

    reported = False

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal reported
        frame = image_from_the_variable.read()
        if frame is None or reported:
            return
        reported = True
        log.info(
            f"{READ_MARKER_PREFIX}{reader.get().upper()} "
            + json.dumps(
                {"pid": os.getpid(), "surface_id": frame.surface_id, "width": frame.width}
            )
        )

    return each_frame
