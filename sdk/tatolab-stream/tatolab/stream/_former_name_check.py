# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The check of the names a node's pins had, across all its pins."""

from __future__ import annotations

from collections.abc import Sequence

from ._contract_pin_types import PinDeclarationOnTheWire
from ._pin_declaration import NodeDeclarationRefusal

__all__ = ["refuse_former_names_that_find_no_one_pin"]


def refuse_former_names_that_find_no_one_pin(
    node: str, pins: "Sequence[tuple[str, str, PinDeclarationOnTheWire]]"
) -> None:
    """Refuse a name a pin of `node` had that would not find that pin alone.

    `pins` is each pin of the node, inputs and outputs, as its name as written, its port name
    and its declaration. A former name is refused when it is a pin's name now, its own pin's
    included, or when two pins list it, as the runtime refuses it at registration.
    """
    written_name_of_each_port = {port_name: written_name for written_name, port_name, _ in pins}
    written_name_listing_each_former_name: dict[str, str] = {}
    for written_name, _, declaration in pins:
        for former_name in declaration.get("former_names", []):
            holder = written_name_of_each_port.get(former_name)
            if holder == written_name:
                words = (
                    f"`{node}`'s pin `{written_name}` lists `{former_name}` as a name it had, "
                    f"and it is named that now. To fix: drop `{former_name}` from its "
                    f"`FormerlyNamed`."
                )
            elif holder is not None:
                words = (
                    f"`{node}`'s pin `{written_name}` lists `{former_name}` as a name it had, "
                    f"and pin `{holder}` is named that now, so a project saved under "
                    f"`{former_name}` would not find one pin. To fix: drop `{former_name}` "
                    f"from its `FormerlyNamed`, or rename `{holder}`."
                )
            elif former_name in written_name_listing_each_former_name:
                other = written_name_listing_each_former_name[former_name]
                words = (
                    f"`{node}`'s pins `{other}` and `{written_name}` both list `{former_name}` "
                    f"as a name they had, so a project saved under it would not find one pin. "
                    f"To fix: keep `{former_name}` on the pin that had it."
                )
            else:
                written_name_listing_each_former_name[former_name] = written_name
                continue
            raise NodeDeclarationRefusal(
                words, finding_code="former_name_finds_no_one_pin", node=node, pin=written_name
            )
