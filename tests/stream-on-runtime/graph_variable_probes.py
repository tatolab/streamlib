# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The Image reader `test_graph_variables.py` reads a variable with, twice."""

import dataclasses
import json
import os

from tatolab.stream import Image, Input, log, node

READ_MARKER_PREFIX = "MARKER:GRAPH_VARIABLE_READ_"


@dataclasses.dataclass
class ImageVariableReaderConfig:
    reader: str = "reader"


@node.process
class ImageVariableReader:
    """Reports the first Image frame it reads, under a marker named for `reader`."""

    image_from_the_variable = Input(Image, delivery="ordered")

    def __init__(self, config: ImageVariableReaderConfig) -> None:
        self.reader = config.reader
        self.reported = False

    def process(self, ctx) -> None:
        frame = ctx.inputs.read(self.image_from_the_variable)
        if frame is None or self.reported:
            return
        self.reported = True
        log.info(
            f"{READ_MARKER_PREFIX}{self.reader.upper()} "
            + json.dumps(
                {"pid": os.getpid(), "surface_id": frame.surface_id, "width": frame.width}
            )
        )
