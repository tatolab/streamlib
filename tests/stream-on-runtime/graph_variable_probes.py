# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The Texture reader `test_graph_variables.py` reads a variable with, twice."""

import json
import os
from typing import Annotated

from tatolab.stream import InputPin, StreamDelivery, Texture, log, node

READ_MARKER_PREFIX = "MARKER:GRAPH_VARIABLE_READ_"


@node
class ImageVariableReader:
    """Reports the first Texture frame it reads, under a marker named for `reader`."""

    image_from_the_variable: InputPin[Annotated[Texture, StreamDelivery("ordered")]]

    reader: str = "reader"

    def __init__(self) -> None:
        self._reported = False

    def process(self, ctx) -> None:
        frame = self.image_from_the_variable.read()
        if frame is None or self._reported:
            return
        self._reported = True
        log.info(
            f"{READ_MARKER_PREFIX}{self.reader.upper()} "
            + json.dumps(
                {"pid": os.getpid(), "surface_id": frame.surface_id, "width": frame.width}
            )
        )
