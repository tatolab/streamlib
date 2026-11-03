# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The parent's graph, as far as a helper's catalog-entry escalates reach it."""

from typing import Any


class ParentHoldingCatalogEntries:
    """Answers the two catalog-entry escalate ops a helper context sends its parent.

    A port's entry lives on the parent's graph, so a context opened directly on
    wired links reaches one only through `escalate_request_to_parent`. An entry an
    output port states becomes what the input port it is linked to reads, through
    `downstream_input_port_by_output_port`; each input here is fed by one link.
    """

    def __init__(self) -> None:
        self.downstream_input_port_by_output_port: "dict[str, str]" = {}
        self.catalog_entry_by_input_port: "dict[str, dict[str, Any]]" = {}
        self.catalog_entries_stated: "list[tuple[str, str, dict[str, Any]]]" = []

    def escalate_request_to_parent(self, op: "dict[str, Any]") -> "dict[str, Any]":
        if op["op"] == "read_inbound_link_catalog_entry":
            return {"catalog_entry": self.catalog_entry_by_input_port.get(op["port"])}
        if op["op"] == "set_output_port_catalog_entry":
            entry = dict(op["entry"])
            self.catalog_entries_stated.append((op["port"], op["section"], entry))
            input_port = self.downstream_input_port_by_output_port.get(op["port"])
            if input_port is not None:
                self.catalog_entry_by_input_port[input_port] = {
                    "section": op["section"],
                    "entry": entry,
                }
            return {}
        raise AssertionError(f"an escalate op the parent stand-in does not answer: {op}")
