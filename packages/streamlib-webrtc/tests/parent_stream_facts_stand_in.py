# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The parent's graph, as far as a helper's stream-facts escalates reach it."""

from typing import Any


class ParentHoldingStreamFacts:
    """Answers the two stream-facts escalate ops a helper context sends its parent.

    A port's stream facts live on the parent's graph, so a context opened
    directly on wired links reaches them only through
    `escalate_request_to_parent`. This holds what an upstream port states, in
    its wire form keyed by the inbound link a reader names, and keeps every set
    of facts a node's output port states, in order.
    """

    def __init__(self) -> None:
        self.stream_facts_by_inbound_link: "dict[str, dict[str, Any]]" = {}
        self.stream_facts_stated: "list[tuple[str, dict[str, Any]]]" = []

    def escalate_request_to_parent(self, op: "dict[str, Any]") -> "dict[str, Any]":
        if op["op"] == "read_inbound_link_stream_facts":
            return {"stream_facts": self.stream_facts_by_inbound_link.get(op["inbound_link"])}
        if op["op"] == "set_output_port_stream_facts":
            self.stream_facts_stated.append((op["port"], dict(op["stream_facts"])))
            return {}
        raise AssertionError(f"an escalate op the parent stand-in does not answer: {op}")
