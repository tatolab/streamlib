# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The stream `test_connection_wires.py` starts on `tatolabd`."""

from tatolab.stream import GetVariable, SetVariable, StreamBuilder, graph_generator

from connection_wire_nodes import Hub, Spoke

HUB_NAME = "the-hub"


@graph_generator
def a_hub_wired_into_one_spoke_and_through_a_variable_into_another(
    stream_builder: StreamBuilder,
) -> None:
    hub = stream_builder.add(Hub, name=HUB_NAME)
    wired = stream_builder.add(Spoke, name="wired", input_values={"spoke": "wired"})
    stream_builder.connect(hub.output("connection"), wired.input("hub"))
    setter = stream_builder.add(SetVariable, input_values={"variable": "hub"})
    stream_builder.connect(hub.output("connection"), setter.input("value"))
    getter = stream_builder.add(GetVariable, input_values={"variable": "hub"})
    through_the_variable = stream_builder.add(
        Spoke, name="through-the-variable", input_values={"spoke": "variable"}
    )
    stream_builder.connect(getter.output("value"), through_the_variable.input("hub"))
