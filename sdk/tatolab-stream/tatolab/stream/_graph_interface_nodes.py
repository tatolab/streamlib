# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`GraphInput` and `GraphOutput`: the ports a graph offers, each named, and of the type its
`pin_type` setting chooses or, unset, of the port first linked to it."""

from __future__ import annotations

from typing import Annotated, ClassVar, Literal

from ._built_in_node import BuiltInNode
from ._class_node_pins import InputPin
from ._description import Description
from ._formerly_named import FormerlyNamed
from ._input_declaration_annotations import PublishedOnAnOutput, HandedToItsNode
from ._pin_type_choice import PinTypeChoice, PinTypeFromSetting

__all__ = [
    "GraphInput",
    "GraphOutput",
]


class GraphOutput(BuiltInNode):
    """Declares one Output of the graph: the port linked into `value`. A stream's top-level
    Outputs are what Tato Studio displays. It runs nothing and sends nothing off the machine.
    """

    type: ClassVar[str] = "tatolab.stream:GraphOutput"

    port: Annotated[str, HandedToItsNode(), Description("The Output's name, cast as every exposed name is")]
    pin_type: Annotated[
        PinTypeChoice | None,
        Description("The type of what leaves the Output; unset, the type of the port linked into it"),
    ] = None
    value: InputPin[
        Annotated[
            object,
            PinTypeFromSetting("pin_type"),
            Description("The port the Output offers, of the type `pin_type` chooses"),
        ]
    ]


class GraphInput(BuiltInNode):
    """Declares one Input of the graph: every port `value` is linked to reads what enters it.
    A stream's top-level Input of a type that holds a value, a Tensor or Audio, runs, and carries the value a client sets with `set_node_input_value`
    naming the Input and no node, kept as its `lifetime` says, else its saved `value`; a
    fragment's Input carries what the graph that adds the fragment wires into it. Its
    `range`, `step`, `unit`, `meaning` and `control` are the annotations of its `value`, each
    one the chosen `pin_type` takes, such as `control="dial"` on a Float.
    """

    type: ClassVar[str] = "tatolab.stream:GraphInput"

    port: Annotated[str, HandedToItsNode(), Description("The Input's name, cast as every exposed name is")]
    pin_type: Annotated[
        PinTypeChoice | None,
        Description("The type of what enters the Input; unset, the type of the first port linked to it"),
    ] = None
    range: Annotated[
        list[float] | None,
        HandedToItsNode(),
        Description("`[lowest, highest]`, the span a control moves the Input's number in"),
    ] = None
    step: Annotated[
        float | None,
        HandedToItsNode(),
        Description("The step a control moves the Input's number by"),
    ] = None
    unit: Annotated[
        str | None,
        HandedToItsNode(),
        Description("The unit the Input's number is in, such as `Hz`"),
    ] = None
    meaning: Annotated[
        str | None,
        HandedToItsNode(),
        Description("What the Input's value stands for, such as `frequency`"),
    ] = None
    control: Annotated[
        str | None,
        HandedToItsNode(),
        Description(
            "The control that sets the Input's value, such as `dial`: one of the Studio's, or "
            "a module's qualified name"
        ),
    ] = None
    lifetime: Annotated[
        Literal["kept", "reset"],
        HandedToItsNode(),
        Description(
            "How long a value a client sets lasts: kept through Stop and a restart, or reset "
            "at Stop"
        ),
    ] = "kept"
    value: Annotated[
        object | None,
        PinTypeFromSetting("pin_type"),
        FormerlyNamed("default"),
        HandedToItsNode(),
        PublishedOnAnOutput(),
        Description(
            "What the Input's readers see until a client sets it, of the type `pin_type` "
            "chooses: a stream's top-level Input only"
        ),
    ] = None
