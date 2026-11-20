# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The declaration of a pin written `Annotated[object, PinTypeFromSetting("...")]`, of the type
that setting chooses, which only the runtime's own nodes declare yet."""

from __future__ import annotations

from collections.abc import Callable, Sequence
from typing import TYPE_CHECKING

from ._contract_pin_types import PinDeclarationOnTheWire
from ._description import Description
from ._formerly_named import FormerlyNamed
from ._pin_type_choice import PinTypeFromSetting

if TYPE_CHECKING:
    from ._pin_declaration import NodeDeclarationFindingCode

__all__ = ["declaration_of_a_pin_a_setting_types"]


def declaration_of_a_pin_a_setting_types(
    *,
    pin_named: str,
    written_type: object,
    metadata: Sequence[object],
    type_settings: Sequence[PinTypeFromSetting],
    an_output: bool,
    optional: bool,
    default: object,
    refusal: Callable[[str, NodeDeclarationFindingCode], Exception],
) -> PinDeclarationOnTheWire:
    """The declaration of `pin_named`, typed by the one setting `type_settings` names: a value,
    unset while it holds none when `optional`, whose default, if any, is `None`. `refusal` makes
    the refusal of `words` under a finding code; the caller states its former names."""
    if len(type_settings) > 1:
        raise refusal(
            f"{pin_named} carries {len(type_settings)} `PinTypeFromSetting`s; one setting "
            f"chooses its type. To fix: keep one.",
            "pin_annotation_repeated",
        )
    setting = type_settings[0]
    if written_type is not object:
        raise refusal(
            f"{pin_named} carries `{setting!r}` on {written_type!r}, and the setting chooses its "
            f"whole type. To fix: write it `object`.",
            "pin_annotation_not_taken",
        )
    for entry in metadata:
        if not isinstance(entry, (Description, FormerlyNamed, PinTypeFromSetting)):
            raise refusal(
                f"{pin_named} is typed by the setting `{setting.setting}`, and carries "
                f"{entry!r}, which only a pin of one type takes. To fix: drop it.",
                "pin_annotation_not_taken",
            )
    declaration: PinDeclarationOnTheWire = {
        "pin_type": {"kind": "set_by_input", "input_name": setting.setting},
        "flow": "value",
    }
    if an_output or not optional:
        return declaration
    if default is not None:
        raise refusal(
            f"{pin_named} is typed `T | None`, so it reads `None` while unset, and it defaults "
            f"to {default!r}. To fix: `= None`.",
            "unset_input_with_a_default",
        )
    declaration["may_be_unset"] = True
    return declaration
