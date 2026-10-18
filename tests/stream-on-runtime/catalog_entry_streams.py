# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@stream` function `test_catalog_entries.py` runs from the suite project."""

from tatolab.stream import StreamBuilder, stream

from catalog_entry_probes import (
    KeepsItsDeclaredCatalogEntry,
    ReplacesItsCatalogEntryOnItsFirstTick,
    ReportsItsUpstreamsCatalogEntry,
)

KEEPING_SOURCE_NAME = "keeper"
REPLACING_SOURCE_NAME = "replacer"


@stream
def a_keeping_source_and_a_replacing_one_read_downstream(stream_builder: StreamBuilder) -> None:
    """The replacing source feeds a reader; the keeping source feeds none."""
    stream_builder.add(KeepsItsDeclaredCatalogEntry, name=KEEPING_SOURCE_NAME)
    replacer = stream_builder.add(ReplacesItsCatalogEntryOnItsFirstTick, name=REPLACING_SOURCE_NAME)
    reader = stream_builder.add(ReportsItsUpstreamsCatalogEntry)
    stream_builder.connect(replacer.output("records"), reader.input("records_from_upstream"))
