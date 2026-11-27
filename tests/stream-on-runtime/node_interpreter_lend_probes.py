# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Nodes that report the interpreter they run in and the runtime it borrowed.

The node interpreters hosting them run from a venv holding only
`tatolab-stream`, so this module imports nothing but `tatolab.stream` and the
standard library at module scope.
"""

import json
import os
import sys
from typing import Annotated, Callable

from tatolab.stream import EncodedVideo, InputPin, OutputPin, RuntimeContextLimitedAccess, StreamDelivery, log, node

BAGS_THE_SINK_COUNTS_BEFORE_IT_REPORTS = 20


@node(execution="continuous", interval_ms=10)
def ReportsItsInterpreterSource(
    *,
    bags_to_downstream: OutputPin[EncodedVideo],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Announces its interpreter and the lent runtime's file, then emits a JSON bag per tick."""

    announced = False

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal announced
        if not announced:
            # Lent by the runtime unit; the venv this runs from has none of its own.
            import tatolab.runtime  # pyright: ignore[reportMissingImports]

            log.info(
                "MARKER:NODE_INTERPRETER "
                + json.dumps(
                    {
                        "node_interpreter": sys.executable,
                        "node_interpreter_process_id": os.getpid(),
                        "lent_runtime_file": tatolab.runtime.__file__,
                    }
                )
            )
            announced = True
        bags_to_downstream.write(
            json.dumps({"produced_by_interpreter": sys.executable}).encode()
        )

    return each_frame


@node(execution="reactive")
def CountsBagsFromUpstreamSink(
    *,
    bags_from_upstream: InputPin[Annotated[EncodedVideo, StreamDelivery("ordered")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Announces once it has processed enough messages to call the stream live."""

    bags_processed = 0

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal bags_processed
        unit = bags_from_upstream.read()
        if unit is None:
            return
        payload = json.loads(bytes(unit.payload))
        bags_processed += 1
        if bags_processed == BAGS_THE_SINK_COUNTS_BEFORE_IT_REPORTS:
            log.info(
                "MARKER:BAGS_PROCESSED "
                + json.dumps(
                    {
                        "bags_processed": bags_processed,
                        "sink_interpreter": sys.executable,
                        "upstream_interpreter": payload["produced_by_interpreter"],
                    }
                )
            )

    return each_frame
