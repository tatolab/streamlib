# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The `@graph_generator` functions `test_node_run_identity.py` runs from the suite project."""

from identity_stable_node import IdentityStableNode
from second_identity_stable_node import SecondIdentityStableNode
from tatolab.stream import StreamBuilder, graph_generator


@graph_generator
def two_identity_stable_nodes(stream_builder: StreamBuilder) -> None:
    """Both nodes, in one stream."""
    stream_builder.add(IdentityStableNode)
    stream_builder.add(SecondIdentityStableNode)
