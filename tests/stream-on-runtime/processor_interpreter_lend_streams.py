# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@stream` functions `test_processor_interpreter_lend.py` runs from the suite project."""

from processor_interpreter_lend_probes import (
    CountsBagsFromUpstreamSink,
    ReportsItsInterpreterSource,
)
from tatolab.stream import StreamBuilder, stream


@stream
def interpreter_reporting_source_into_counting_sink(stream_builder: StreamBuilder) -> None:
    """A source announcing its interpreter into a sink counting what it received."""
    source = stream_builder.add(ReportsItsInterpreterSource)
    sink = stream_builder.add(CountsBagsFromUpstreamSink)
    stream_builder.connect(source.output("bags_to_downstream"), sink.input("bags_from_upstream"))
