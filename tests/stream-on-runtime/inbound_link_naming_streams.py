# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@stream` function `test_inbound_link_naming.py` runs from the suite project."""

from tatolab.stream import StreamBuilder, stream

from inbound_link_naming_processors import (
    FeedsOneValueSource,
    ReportsEachAttributionSink,
    ReportsWhichLinkEachBagCameFrom,
)


FEEDER_VALUES = {"firstfeeder": "from-the-first", "secondfeeder": "from-the-second"}


@stream
def two_feeders_into_one_port(stream_builder: StreamBuilder) -> None:
    """Both feeders linked into the one `tracks` port, a reporter on its output."""
    sink = stream_builder.add(ReportsWhichLinkEachBagCameFrom)
    for feeder_name, value in FEEDER_VALUES.items():
        feeder = stream_builder.add(FeedsOneValueSource, name=feeder_name, config={"value": value})
        stream_builder.connect(feeder.output("bags_to_downstream"), sink.input("tracks"))
    reporter = stream_builder.add(ReportsEachAttributionSink)
    stream_builder.connect(
        sink.output("attributions_to_downstream"), reporter.input("attributions_from_upstream")
    )
