# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Processors that report the interpreter they run in and the runtime it borrowed.

Imported by the app that registers them and by the processor interpreters that
host them, which run from a venv holding only `tatolab-stream` — so this module
imports nothing but `tatolab.stream` and the standard library at module scope.
"""

import sys

from tatolab.stream import log, node

BAGS_THE_SINK_COUNTS_BEFORE_IT_REPORTS = 20


@node(execution="continuous", interval_ms=10)
class ReportsItsInterpreterSource:
    """Announces its interpreter and the lent runtime's file, then emits a bag per tick."""

    def __init__(self) -> None:
        self.announced = False

    @node.output()
    def bags_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        if not self.announced:
            import tatolab.runtime

            log.info(f"MARKER:PROCESSOR_INTERPRETER={sys.executable}")
            log.info(f"MARKER:LENT_RUNTIME_FILE={tatolab.runtime.__file__}")
            self.announced = True
        ctx.outputs.write("bags_to_downstream", {"produced_by_interpreter": sys.executable})


@node
class CountsBagsFromUpstreamSink:
    """Announces once it has processed enough bags to call the stream live."""

    def __init__(self) -> None:
        self.bags_processed = 0

    @node.input(delivery_profile="ordered")
    def bags_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        bag = ctx.inputs.read("bags_from_upstream")
        if bag is None:
            return
        self.bags_processed += 1
        if self.bags_processed == BAGS_THE_SINK_COUNTS_BEFORE_IT_REPORTS:
            log.info(
                f"MARKER:BAGS_PROCESSED={self.bags_processed} "
                f"SINK_INTERPRETER={sys.executable} "
                f"UPSTREAM_INTERPRETER={bag['produced_by_interpreter']}"
            )
