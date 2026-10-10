# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@stream` functions `test_processor_identity.py` runs from the suite project."""

from identity_stable_processor import IdentityStableProcessor
from second_identity_stable_processor import SecondIdentityStableProcessor
from tatolab.stream import StreamBuilder, stream


@stream
def two_identity_stable_processors(stream_builder: StreamBuilder) -> None:
    """Both processors, in one stream."""
    stream_builder.add(IdentityStableProcessor)
    stream_builder.add(SecondIdentityStableProcessor)
