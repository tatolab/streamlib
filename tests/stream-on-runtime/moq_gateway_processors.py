# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""A records source that counts, and a sink that reports what reached it."""

import json

from tatolab.stream import Input, Output, Records, log, node

#: The marker the sink logs each record it reads under, with the record's count.
RECORD_RECEIVED_THROUGH_THE_RELAY_MARKER = "RECORD_RECEIVED_THROUGH_THE_RELAY"


@node.process(execution="continuous", interval_ms=20)
class CountingRecordsSource:
    """Writes `{"count": n}`, n rising from 0, every tick."""

    def __init__(self) -> None:
        self.count = 0

    records_to_downstream = Output(Records)

    def process(self, ctx) -> None:
        ctx.outputs.write(self.records_to_downstream, {"count": self.count})
        self.count += 1


@node.process
class ReportsEachRecordReceivedSink:
    """Logs each record it reads under `RECORD_RECEIVED_THROUGH_THE_RELAY_MARKER`."""

    records_from_upstream = Input(Records, delivery="ordered")

    def process(self, ctx) -> None:
        while (record := ctx.inputs.read(self.records_from_upstream)) is not None:
            log.info(
                f"MARKER:{RECORD_RECEIVED_THROUGH_THE_RELAY_MARKER} "
                + json.dumps({"count": record["count"]})
            )
