# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Nodes that report the interpreter they run in and the runtime it borrowed.

The processor interpreters hosting them run from a venv holding only
`tatolab-stream`, so this module imports nothing but `tatolab.stream` and the
standard library at module scope.
"""

import json
import os
import sys

from tatolab.stream import Input, Output, Records, log, node

BAGS_THE_SINK_COUNTS_BEFORE_IT_REPORTS = 20


@node.process(execution="continuous", interval_ms=10)
class ReportsItsInterpreterSource:
    """Announces its interpreter and the lent runtime's file, then emits a bag per tick."""

    def __init__(self) -> None:
        self.announced = False

    bags_to_downstream = Output(Records)

    def process(self, ctx) -> None:
        if not self.announced:
            # Lent by the runtime unit; the venv this runs from has none of its own.
            import tatolab.runtime  # pyright: ignore[reportMissingImports]

            log.info(
                "MARKER:PROCESSOR_INTERPRETER "
                + json.dumps(
                    {
                        "processor_interpreter": sys.executable,
                        "processor_interpreter_process_id": os.getpid(),
                        "lent_runtime_file": tatolab.runtime.__file__,
                    }
                )
            )
            self.announced = True
        ctx.outputs.write(
            self.bags_to_downstream, {"produced_by_interpreter": sys.executable}
        )


@node.process
class CountsBagsFromUpstreamSink:
    """Announces once it has processed enough messages to call the stream live."""

    def __init__(self) -> None:
        self.bags_processed = 0

    bags_from_upstream = Input(Records, delivery="ordered")

    def process(self, ctx) -> None:
        payload = ctx.inputs.read(self.bags_from_upstream)
        if payload is None:
            return
        self.bags_processed += 1
        if self.bags_processed == BAGS_THE_SINK_COUNTS_BEFORE_IT_REPORTS:
            log.info(
                "MARKER:BAGS_PROCESSED "
                + json.dumps(
                    {
                        "bags_processed": self.bags_processed,
                        "sink_interpreter": sys.executable,
                        "upstream_interpreter": payload["produced_by_interpreter"],
                    }
                )
            )
