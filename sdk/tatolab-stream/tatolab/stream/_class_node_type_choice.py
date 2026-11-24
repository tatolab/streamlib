# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A module's class node choosing an output's type with a setting: the `PinTypeChoice` field an
`OutputPin[Annotated[A | B, PinTypeFromSetting("...")]]` names may choose `A`, `B`, ..., and the
output travels on the family of the one it chooses."""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from typing import Any

from ._contract_pin_types import PinDeclarationOnTheWire, PinType
from ._node_declaration_refusal import NodeDeclarationRefusal
from ._pin_family import family_a_pin_type_travels_on
from ._pin_type_choice import pin_type_in_words

__all__ = [
    "family_declaration_of_an_output_a_setting_types",
    "give_each_type_choice_what_it_may_choose",
]


def family_declaration_of_an_output_a_setting_types(
    declaration: PinDeclarationOnTheWire, chosen_among: Sequence[PinType]
) -> dict[str, Any]:
    """The family declaration of an output of the type the setting its `declaration` names
    chooses among `chosen_among`: the family of the first until the runtime reads the setting,
    then the family, and the codec, of the type it holds."""
    setting: str = declaration["pin_type"]["input_name"]  # pyright: ignore[reportGeneralTypeIssues, reportTypedDictNotRequiredAccess]
    return {
        "family": family_a_pin_type_travels_on(chosen_among[0]),
        "family_from_config_key": setting,
        "codec_from_config_key": setting,
    }


def give_each_type_choice_what_it_may_choose(
    node: str,
    input_ports: Sequence[Mapping[str, Any]],
    output_ports: Sequence[Mapping[str, Any]],
    among_of_each_output: Mapping[str, Sequence[PinType]],
) -> None:
    """Write, onto each `PinTypeChoice` field of `node`'s `input_ports`, the types the one output
    of `output_ports` it types may choose, `among_of_each_output` by the output's port.

    Refused, as a `NodeDeclarationRefusal`: an output naming a setting that is no `PinTypeChoice`
    field, two outputs naming one, a default choosing none of the types, and a `PinTypeChoice`
    field no output names, which would choose nothing."""
    choice_by_name = {
        port["name"]: port for port in input_ports if _chooses_a_pin_type(port["pin_declaration"])
    }
    output_typed_by_each_choice: dict[str, str] = {}
    for output in output_ports:
        among = among_of_each_output.get(output["name"])
        if among is None:
            continue
        setting = output["pin_declaration"]["pin_type"]["input_name"]
        choice = choice_by_name.get(setting)
        if choice is None:
            raise NodeDeclarationRefusal(
                f"`{node}`'s output `{output['name']}` is typed by the setting `{setting}`, and "
                f"`{node}` has no `PinTypeChoice` field `{setting}`. To fix: declare "
                f"`{setting}: PinTypeChoice = PinTypeChoice(...)`, choosing one of its types.",
                finding_code="output_type_found_from_no_input",
                node=node,
                pin=output["name"],
            )
        if setting in output_typed_by_each_choice:
            raise NodeDeclarationRefusal(
                f"`{node}`'s outputs `{output_typed_by_each_choice[setting]}` and "
                f"`{output['name']}` are both typed by the setting `{setting}`, and a "
                f"`PinTypeChoice` types one output. To fix: give each output a setting of its own.",
                finding_code="pin_annotation_repeated",
                node=node,
                pin=output["name"],
            )
        output_typed_by_each_choice[setting] = output["name"]
        _refuse_a_default_choosing_none_of(node, choice, among)
        choice["pin_declaration"]["pin_type"]["pin_type"]["among"] = list(among)
    for name in choice_by_name:
        if name not in output_typed_by_each_choice:
            raise NodeDeclarationRefusal(
                f"`{node}`'s `PinTypeChoice` field `{name}` types no output, so it would choose "
                f"nothing. To fix: type an output by it, `OutputPin[Annotated[EncodedVideo | "
                f'EncodedAudio, PinTypeFromSetting("{name}")]]`, or drop it.',
                finding_code="pin_annotation_not_taken",
                node=node,
                pin=name,
            )


def _refuse_a_default_choosing_none_of(
    node: str, choice: Mapping[str, Any], among: Sequence[PinType]
) -> None:
    default = choice.get("pin_default")
    if default is None or default in among:
        return
    raise NodeDeclarationRefusal(
        f"`{node}`'s `PinTypeChoice` field `{choice['name']}` defaults to "
        f"{pin_type_in_words(default)}, which is none of the types the output it types may "
        f"choose, {', '.join(pin_type_in_words(each) for each in among)}. To fix: default to "
        f"one of them.",
        finding_code="pin_default_not_of_its_type",
        node=node,
        pin=choice["name"],
    )


def _chooses_a_pin_type(declaration: PinDeclarationOnTheWire) -> bool:
    declared_pin_type: Any = declaration["pin_type"]
    return declared_pin_type["kind"] == "is" and declared_pin_type["pin_type"]["type"] == "pin_type_choice"
