# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`SetVariable` and `GetVariable`: one value named once and read with no drawn wire."""

from __future__ import annotations

from typing import Annotated, ClassVar

from ._built_in_node import BuiltInNode
from ._class_node_pins import InputPin, OutputPin
from ._description import Description
from ._input_declaration_annotations import HandedToItsNode
from ._stream_graph_builder import NodeReference

__all__ = [
    "GetVariable",
    "SetVariable",
]


class SetVariable(BuiltInNode):
    """Names one value under `variable`: the port's output linked into `value`, or the node
    in `node_reference`. It runs nothing; every `GetVariable` of the name on its level, the
    stream's or a fragment's, or on a fragment inside that level, reads the value.
    """

    type: ClassVar[str] = "tatolab.stream:SetVariable"

    variable: Annotated[
        str, HandedToItsNode(), Description("The variable's name, which every `GetVariable` of it reads")
    ]
    node_reference: Annotated[
        NodeReference | None,
        Description(
            "The node the variable holds, of any type; unset, the variable holds the port "
            "linked into `value`"
        ),
    ] = None
    value: InputPin[
        Annotated[
            object,
            Description("The port whose output the variable holds, of that port's type"),
        ]
    ]


class GetVariable(BuiltInNode):
    """Reads the variable `variable` from the setter on its own level, or else from the
    nearest enclosing level's. A variable holding a port's output is read by linking
    `value`, which runs as a link from the setter's source with that port's type; one
    holding a node is read by passing this node's reference as an input value, which names
    the setter's node.
    """

    type: ClassVar[str] = "tatolab.stream:GetVariable"

    variable: Annotated[
        str,
        HandedToItsNode(),
        Description("The variable it reads, set on its level or an enclosing one"),
    ]
    value: OutputPin[
        Annotated[
            object,
            Description("The port's output the variable holds, of that port's type"),
        ]
    ]
