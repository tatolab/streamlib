# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`SetVariable` and `GetVariable`: one value named once and read with no drawn wire."""

from __future__ import annotations

from typing import ClassVar, NotRequired, Required, TypedDict

from ._built_in_node import BuiltInNode
from ._stream_graph_builder import NodeReference

__all__ = [
    "GetVariable",
    "GetVariableConfig",
    "SetVariable",
    "SetVariableConfig",
]


class SetVariableConfig(TypedDict):
    """The variable a `SetVariable` names, and the node it holds when it holds one."""

    variable: Required[str]
    node_reference: NotRequired[NodeReference]
    """The node the variable holds; left out, the variable holds what links into `value`."""


class SetVariable(BuiltInNode[SetVariableConfig]):
    """Names one value under `variable`: the port's output linked into `value`, or the node
    in `node_reference`. It runs nothing; every `GetVariable` of the name on its level, the
    stream's or a fragment's, or on a fragment inside that level, reads the value.

    Inputs:
        `value`: The port whose output the variable holds.

    Config: `SetVariableConfig`.
    """

    type: ClassVar[str] = "tatolab.stream:SetVariable"


class GetVariableConfig(TypedDict):
    """The variable a `GetVariable` reads."""

    variable: Required[str]


class GetVariable(BuiltInNode[GetVariableConfig]):
    """Reads the variable `variable` from the setter on its own level, or else from the
    nearest enclosing level's. A variable holding a port's output is read by linking
    `value`, which runs as a link from the setter's source with that port's family; one
    holding a node is read by passing this node's reference as a config value, which names
    the setter's node.

    Outputs:
        `value`: The port's output the variable holds.

    Config: `GetVariableConfig`.
    """

    type: ClassVar[str] = "tatolab.stream:GetVariable"
