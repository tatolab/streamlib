# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The parent's graph, as far as a helper's stream-facts escalates reach it."""

from typing import Any


class ParentHoldingStreamFacts:
    """Answers the two stream-facts escalate ops a helper context sends its parent.

    A port's stream facts live on the parent's graph, so a context opened
    directly on wired links reaches them only through
    `escalate_request_to_parent`. Facts an output port states become what the
    input port it is linked to reads, through
    `downstream_input_port_by_output_port`; each input here is fed by one link.
    """

    def __init__(self) -> None:
        self.downstream_input_port_by_output_port: "dict[str, str]" = {}
        self.stream_facts_by_input_port: "dict[str, dict[str, Any]]" = {}
        self.stream_facts_stated: "list[tuple[str, dict[str, Any]]]" = []

    def escalate_request_to_parent(self, op: "dict[str, Any]") -> "dict[str, Any]":
        if op["op"] == "read_inbound_link_stream_facts":
            return {"stream_facts": self.stream_facts_by_input_port.get(op["port"])}
        if op["op"] == "set_output_port_stream_facts":
            stream_facts = dict(op["stream_facts"])
            self.stream_facts_stated.append((op["port"], stream_facts))
            input_port = self.downstream_input_port_by_output_port.get(op["port"])
            if input_port is not None:
                self.stream_facts_by_input_port[input_port] = stream_facts
            return {}
        raise AssertionError(f"an escalate op the parent stand-in does not answer: {op}")
