# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The parent's graph, as far as a helper's catalog-entry escalates reach it."""

from typing import Any


class ParentHoldingCatalogEntries:
    """Answers the two catalog-entry escalate ops a helper context sends its parent.

    A port's entry lives on the parent's graph, so a context opened directly on
    wired links reaches one only through `escalate_request_to_parent`. This
    holds what an upstream port states, keyed by the inbound link a reader
    names, and keeps every entry a node's output port states, in order.
    """

    def __init__(self) -> None:
        self.catalog_entry_by_inbound_link: "dict[str, dict[str, Any]]" = {}
        self.catalog_entries_stated: "list[tuple[str, str, dict[str, Any]]]" = []

    def escalate_request_to_parent(self, op: "dict[str, Any]") -> "dict[str, Any]":
        if op["op"] == "read_inbound_link_catalog_entry":
            return {
                "catalog_entry": self.catalog_entry_by_inbound_link.get(
                    op["inbound_link"]
                )
            }
        if op["op"] == "set_output_port_catalog_entry":
            self.catalog_entries_stated.append(
                (op["port"], op["section"], dict(op["entry"]))
            )
            return {}
        raise AssertionError(f"an escalate op the parent stand-in does not answer: {op}")
