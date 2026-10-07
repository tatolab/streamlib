# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The built-in node classes `tatolab.stream` publishes are exactly the
built-ins the runtime registers. The classes themselves are the stream suite's
(`sdk/tatolab-stream/tests/test_built_in_node_classes.py`)."""

import inspect
import sys

from tatolab.runtime._engine import processor_class_import_paths_in_this_processes_catalog
from tatolab.stream import _built_in_nodes
from tatolab.stream._built_in_node import BuiltInNode

BUILT_IN_NODE_TYPE_PREFIX = "tatolab.stream:"


def test_the_generated_types_are_exactly_the_built_ins_the_runtime_registers():
    registered_built_in_node_types = {
        registered
        for registered in processor_class_import_paths_in_this_processes_catalog()
        if registered.startswith(BUILT_IN_NODE_TYPE_PREFIX)
    }
    generated_built_in_node_types = {
        exported.type
        for exported in vars(_built_in_nodes).values()
        if inspect.isclass(exported)
        and issubclass(exported, BuiltInNode)
        and exported is not BuiltInNode
    }
    if sys.platform == "linux":
        assert generated_built_in_node_types == registered_built_in_node_types
    else:
        # A floor compiles some built-ins out; the module still names every one,
        # and the runtime refuses an absent one at load naming its floors.
        assert registered_built_in_node_types <= generated_built_in_node_types
