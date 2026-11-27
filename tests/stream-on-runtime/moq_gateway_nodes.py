# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A source that counts in `MoqRecord`s, and a sink that reports what reached it."""

import json
from typing import Annotated, Callable

from tatolab.moq import MoqRecord
from tatolab.stream import InputPin, OutputPin, RuntimeContextLimitedAccess, StreamDelivery, log, node

#: The marker the sink logs each record it reads under, with the record's count.
RECORD_RECEIVED_THROUGH_THE_RELAY_MARKER = "RECORD_RECEIVED_THROUGH_THE_RELAY"


@node(execution="continuous", interval_ms=20)
def CountingRecordsSource(
    *,
    records_to_downstream: OutputPin[MoqRecord],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Writes `MoqRecord({"count": n})`, n rising from 0, every tick."""

    count = 0

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal count
        del ctx
        records_to_downstream.write(MoqRecord({"count": count}))
        count += 1

    return each_frame


@node(execution="reactive")
def ReportsEachRecordReceivedSink(
    *,
    records_from_upstream: InputPin[Annotated[MoqRecord, StreamDelivery("ordered")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Logs each record it reads under `RECORD_RECEIVED_THROUGH_THE_RELAY_MARKER`."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        del ctx
        while (record := records_from_upstream.read()) is not None:
            log.info(
                f"MARKER:{RECORD_RECEIVED_THROUGH_THE_RELAY_MARKER} "
                + json.dumps({"count": record.members["count"]})
            )

    return each_frame
