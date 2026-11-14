# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The `@graph_generator` functions `test_processor_identity.py` runs from the suite project."""

from identity_stable_processor import IdentityStableProcessor
from second_identity_stable_processor import SecondIdentityStableProcessor
from tatolab.stream import StreamBuilder, graph_generator


@graph_generator
def two_identity_stable_processors(stream_builder: StreamBuilder) -> None:
    """Both processors, in one stream."""
    stream_builder.add(IdentityStableProcessor)
    stream_builder.add(SecondIdentityStableProcessor)
