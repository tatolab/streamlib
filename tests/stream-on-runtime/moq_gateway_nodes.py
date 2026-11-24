# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A source that counts in `MoqRecord`s, and a sink that reports what reached it."""

import json
from typing import Annotated

from tatolab.moq import MoqRecord
from tatolab.stream import InputPin, OutputPin, StreamDelivery, log, node

#: The marker the sink logs each record it reads under, with the record's count.
RECORD_RECEIVED_THROUGH_THE_RELAY_MARKER = "RECORD_RECEIVED_THROUGH_THE_RELAY"


@node(execution="continuous", interval_ms=20)
class CountingRecordsSource:
    """Writes `MoqRecord({"count": n})`, n rising from 0, every tick."""

    records_to_downstream: OutputPin[MoqRecord]

    def __init__(self) -> None:
        self.count = 0

    def process(self, ctx) -> None:
        del ctx
        self.records_to_downstream.write(MoqRecord({"count": self.count}))
        self.count += 1


@node
class ReportsEachRecordReceivedSink:
    """Logs each record it reads under `RECORD_RECEIVED_THROUGH_THE_RELAY_MARKER`."""

    records_from_upstream: InputPin[Annotated[MoqRecord, StreamDelivery("ordered")]]

    def process(self, ctx) -> None:
        del ctx
        while (record := self.records_from_upstream.read()) is not None:
            log.info(
                f"MARKER:{RECORD_RECEIVED_THROUGH_THE_RELAY_MARKER} "
                + json.dumps({"count": record.members["count"]})
            )
