# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@graph_generator` function `test_stream_facts.py` runs from the suite project."""

from tatolab.stream import StreamBuilder, graph_generator

from stream_facts_probes import (
    DeclaresItsFrameRateAndWritesRecords,
    ReplacesItsStreamFactsOnItsFirstTick,
    ReportsItsUpstreamsStreamFacts,
)

DECLARING_SOURCE_NAME = "declarer"
REPLACING_SOURCE_NAME = "replacer"


@graph_generator
def a_declaring_source_and_a_replacing_one_read_downstream(stream_builder: StreamBuilder) -> None:
    """The replacing source feeds a reader; the declaring source feeds none."""
    stream_builder.add(DeclaresItsFrameRateAndWritesRecords, name=DECLARING_SOURCE_NAME)
    replacer = stream_builder.add(ReplacesItsStreamFactsOnItsFirstTick, name=REPLACING_SOURCE_NAME)
    reader = stream_builder.add(ReportsItsUpstreamsStreamFacts)
    stream_builder.connect(replacer.output("encoded"), reader.input("encoded_from_upstream"))
