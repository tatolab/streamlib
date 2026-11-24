# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The declaration of a pin carrying `PinTypeFromSetting("...")`, of the type that setting
chooses: on the runtime's own node a value written `object`, and on a module's node an output of
several stream types, each one the setting may choose."""

from __future__ import annotations

import typing
from collections.abc import Callable, Sequence
from typing import TYPE_CHECKING

from ._contract_pin_types import PinDeclarationOnTheWire, PinType
from ._description import Description
from ._formerly_named import FormerlyNamed
from ._pin_declaration_refusal_words import (
    PIN_TYPES_AN_INPUT_PIN_OF_SEVERAL_TAKES,
    words_refusing_an_input_pin_of_several_types_taking,
)
from ._pin_spelling_words import is_a_union, name_of
from ._pin_type_choice import PinTypeFromSetting

if TYPE_CHECKING:
    from ._pin_declaration import NodeDeclarationFindingCode, PinDirection, PinForm

__all__ = ["declaration_of_a_pin_a_setting_types"]

type _Refusal = Callable[[str, "NodeDeclarationFindingCode"], Exception]


def declaration_of_a_pin_a_setting_types(
    *,
    pin_named: str,
    written_type: object,
    metadata: Sequence[object],
    type_settings: Sequence[PinTypeFromSetting],
    of_the_runtimes_own_node: bool,
    direction: PinDirection,
    form: PinForm,
    optional: bool,
    default: object,
    member_pin_type: Callable[[object], PinType],
    refusal: _Refusal,
) -> tuple[PinDeclarationOnTheWire, tuple[PinType, ...]]:
    """The declaration of `pin_named`, typed by the one setting `type_settings` names, and each
    type that setting may choose: on a module's node, the members of the union an `OutputPin` is
    written as, each read by `member_pin_type`, carried as a stream; on the runtime's own node,
    which chooses among every type, a value written `object`, unset while it holds none when
    `optional`, whose default, if any, is `None`. `refusal` makes the refusal of `words` under a
    finding code; the caller states its former names."""
    if len(type_settings) > 1:
        raise refusal(
            f"{pin_named} carries {len(type_settings)} `PinTypeFromSetting`s; one setting "
            f"chooses its type. To fix: keep one.",
            "pin_annotation_repeated",
        )
    setting = type_settings[0]
    if not of_the_runtimes_own_node:
        return _declaration_of_an_output_pin_a_setting_chooses_among(
            pin_named=pin_named,
            written_type=written_type,
            metadata=metadata,
            setting=setting,
            an_output_pin=form == "pin" and direction == "output",
            member_pin_type=member_pin_type,
            refusal=refusal,
        )
    if written_type is not object:
        raise refusal(
            f"{pin_named} carries `{setting!r}` on {written_type!r}, and the setting chooses its "
            f"whole type. To fix: write it `object`.",
            "pin_annotation_not_taken",
        )
    _refuse_what_only_a_pin_of_one_type_takes(pin_named, setting, metadata, refusal, "drop it.")
    declaration: PinDeclarationOnTheWire = {
        "pin_type": {"kind": "set_by_input", "input_name": setting.setting},
        "flow": "value",
    }
    if direction == "output" or not optional:
        return declaration, ()
    if default is not None:
        raise refusal(
            f"{pin_named} is typed `T | None`, so it reads `None` while unset, and it defaults "
            f"to {default!r}. To fix: `= None`.",
            "unset_input_with_a_default",
        )
    declaration["may_be_unset"] = True
    return declaration, ()


def _declaration_of_an_output_pin_a_setting_chooses_among(
    *,
    pin_named: str,
    written_type: object,
    metadata: Sequence[object],
    setting: PinTypeFromSetting,
    an_output_pin: bool,
    member_pin_type: Callable[[object], PinType],
    refusal: _Refusal,
) -> tuple[PinDeclarationOnTheWire, tuple[PinType, ...]]:
    """A module's `OutputPin[Annotated[A | B, PinTypeFromSetting("...")]]`: a stream of the one
    type of `A`, `B`, ... the setting chooses."""
    if not an_output_pin:
        raise refusal(
            f"{pin_named} carries `{setting!r}`, and on a module's node a setting chooses the "
            f"type of an `OutputPin` alone. To fix: type it as one of several types its link "
            f"finds, `InputPin[A | B]`, or as one type.",
            "pin_type_not_read_yet",
        )
    if not is_a_union(written_type):
        raise refusal(
            f"{pin_named} carries `{setting!r}` on {name_of(written_type)}, one type, so the "
            f"setting has nothing to choose. To fix: write each type it may choose, "
            f"`OutputPin[Annotated[EncodedVideo | EncodedAudio, {setting!r}]]`, or drop it.",
            "pin_annotation_not_taken",
        )
    _refuse_what_only_a_pin_of_one_type_takes(
        pin_named,
        setting,
        metadata,
        refusal,
        'drop it, or narrow each type it may choose, `Annotated[EncodedVideo, EncodedCodecs("h264")]`.',
    )
    chosen_among: list[PinType] = []
    for member in typing.get_args(written_type):
        pin_type = member_pin_type(member)
        if pin_type["type"] not in PIN_TYPES_AN_INPUT_PIN_OF_SEVERAL_TAKES:
            raise refusal(
                words_refusing_an_input_pin_of_several_types_taking(
                    pin_named, pin_type["type"], "OutputPin"
                ),
                "pin_type_not_read_yet",
            )
        if pin_type in chosen_among:
            raise refusal(
                f"{pin_named} is typed one of several types, and names {name_of(member)} twice. "
                f"To fix: name each type once.",
                "pin_annotation_repeated",
            )
        chosen_among.append(pin_type)
    declaration: PinDeclarationOnTheWire = {
        "pin_type": {"kind": "set_by_input", "input_name": setting.setting},
        "flow": "stream",
    }
    return declaration, tuple(chosen_among)


def _refuse_what_only_a_pin_of_one_type_takes(
    pin_named: str, setting: PinTypeFromSetting, metadata: Sequence[object], refusal: _Refusal, fix: str
) -> None:
    for entry in metadata:
        if not isinstance(entry, (Description, FormerlyNamed, PinTypeFromSetting)):
            raise refusal(
                f"{pin_named} is typed by the setting `{setting.setting}`, and carries "
                f"{entry!r}, which only a pin of one type takes. To fix: {fix}",
                "pin_annotation_not_taken",
            )
