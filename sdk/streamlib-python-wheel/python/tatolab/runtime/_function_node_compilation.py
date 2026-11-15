# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Compile `@node.function` bodies to their evaluate entry and feed each island to its port."""

from __future__ import annotations

from typing import Any, Callable

from tatolab.stream import NodeLinkDataAccess

from tatolab.runtime._function_node_carried_values import (
    FunctionNodeCompiledToItsEvaluateEntry,
    FunctionNodeDoesNotCompile,
    FunctionNodeReadyToEvaluate,
    FunctionNodeRunAsPythonInItsReader,
    UNREAD_MESSAGE_VALUE_TYPE,
)
from tatolab.runtime._function_node_evaluate_entry_compiler import (
    compile_function_node_to_its_evaluate_entry,
)
from tatolab.runtime._function_node_body_run_as_python import (
    FunctionNodeBodyRunAsPython,
    the_value_a_config_field_takes,
)
from tatolab.runtime._function_node_preparation import (
    config_values_of_one_function_node,
    function_node_run_as_python_in_its_reader,
    how_one_function_node_is_called,
    prepare_function_node_to_evaluate,
)
from tatolab.runtime._function_node_islands_fed_to_their_ports import (
    FunctionNodeIslandsFedToTheirPorts,
)


def feed_each_input_port_from_its_function_node_island(
    link_data_access: NodeLinkDataAccess,
    islands: "list[dict[str, Any]]",
    load_function_node: Callable[[str], Any],
) -> FunctionNodeIslandsFedToTheirPorts:
    """Prepare every function node the setup command's `islands` carry and feed each to its port.

    A node is compiled when its body compiles, and run as Python otherwise.
    """
    fed = FunctionNodeIslandsFedToTheirPorts(link_data_access, load_function_node)
    for island in islands:
        fed.feed_an_input_port_from_its_island(island)
    return fed
