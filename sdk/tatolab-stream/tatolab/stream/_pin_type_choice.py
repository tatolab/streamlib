# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`PinTypeChoice`, the value of a setting that chooses another pin's type, and
`PinTypeFromSetting`, which types a pin by the setting that chooses it."""

from __future__ import annotations

from typing import Annotated, Any, Final, final

from ._contract_pin_types import PinType

__all__ = ["PinTypeChoice", "PinTypeFromSetting"]

_WHAT_A_CHOICE_IS_READ_AS: Final = "PinTypeChoice"

_TYPES_NO_FAMILY_CARRIES: Final[frozenset[str]] = frozenset(
    {"list_of", "node_reference", "pin_type_choice"}
)
"""The types whose values no family's message carries, as the runtime's
`PortFamily::a_chosen_pin_type_travels_on` reads them: a List and the types that take no wire. A bundle travels as one message of its own,
so `MakeBundle` and `BreakBundle` choose one, and a named type as the bytes its class packs, so a
`GraphInput` or a module's output its setting types chooses one."""


def pin_type_in_words(pin_type: "PinType") -> str:
    """`pin_type` named as the runtime's errors name it."""
    from ._pin_value_stored_form import PIN_TYPE_DESCRIPTION_BY_NAME

    held: dict[str, Any] = dict(pin_type)
    label = PIN_TYPE_DESCRIPTION_BY_NAME[held["type"]].label
    if held["type"] == "list_of":
        return f"{label} of {pin_type_in_words(held['element_type'])}"
    if held["type"] == "bundle":
        members = ", ".join(
            f"{member['name']}: {pin_type_in_words(member['member_type'])}" for member in held["members"]
        )
        if held.get("name"):
            return f"{label} `{held['name']}` ({members})"
        return f"{label} ({members})"
    if held["type"] == "named":
        return f"{label} `{held['named_pin_type']}`"
    if held["type"] in ("integer", "float") and held.get("exact_size"):
        size = str(held["exact_size"]).replace("uint", "UInt").replace("int", "Int")
        return f"{label} ({size.replace('float', 'Float')})"
    if held["type"] == "vector":
        components = {"two": 2, "three": 3, "four": 4}[held["component_count"]]
        return f"{label} of {components}"
    return label


class PinTypeChoice:
    """A chosen pin type: `PinTypeChoice(T, *annotations)` takes one type a pin is written
    with, such as `float` or `EncodedVideo`, then the annotations that narrow it, the ones
    `Annotated[T, ...]` would carry.

    Its `chosen` is the type as the runtime stores it, such as `{"type": "float"}`, and a
    builder records that as the setting's value.
    """

    __slots__ = ("chosen",)

    chosen: PinType
    """The chosen type, as the runtime stores it."""

    def __init__(self, written_type: Any, *annotations: object) -> None:
        from ._pin_declaration import read_pin_declaration

        annotation: object = (
            Annotated[(written_type, *annotations)]  # pyright: ignore[reportInvalidTypeForm]
            if annotations
            else written_type
        )
        declared = read_pin_declaration(
            _WHAT_A_CHOICE_IS_READ_AS, "chosen", annotation, direction="input", form="pin"
        ).declaration["pin_type"]
        if declared["kind"] != "is":
            raise TypeError(
                f"`PinTypeChoice` chooses one type, and {annotation!r} declares a type found "
                f"another way. To fix: name one type, such as `PinTypeChoice(float)`"
            )
        chosen = declared["pin_type"]
        if chosen["type"] in _TYPES_NO_FAMILY_CARRIES:
            raise TypeError(
                f"`PinTypeChoice` chooses {pin_type_in_words(chosen)}, which no stream's port carries "
                f"yet. To fix: choose a type a single message carries, such as a Float, a String "
                f"or a Texture"
            )
        self.chosen = chosen

    @classmethod
    def of_the_chosen_type(cls, chosen: PinType) -> PinTypeChoice:
        """The choice of `chosen`, the type as the runtime stores it, as a setting's stored value holds it."""
        choice = cls.__new__(cls)
        choice.chosen = chosen
        return choice

    def __eq__(self, other: object) -> bool:
        return isinstance(other, PinTypeChoice) and other.chosen == self.chosen

    def __hash__(self) -> int:
        return hash(repr(self.chosen))

    def __repr__(self) -> str:
        return f"PinTypeChoice(chosen={self.chosen!r})"


@final
class PinTypeFromSetting:
    """Types a pin by the `PinTypeChoice` setting `setting` names: a module's output of several
    types, `OutputPin[Annotated[EncodedVideo | EncodedAudio, PinTypeFromSetting("content_type")]]`,
    of the one the setting chooses, or a pin of the runtime's own node written `object`."""

    __slots__ = ("_setting",)

    def __init__(self, setting: str) -> None:
        if not isinstance(setting, str) or not setting:
            raise TypeError(f"PinTypeFromSetting(...) names a setting, got {setting!r}.")
        self._setting = setting

    @property
    def setting(self) -> str:
        """The setting that chooses the pin's type, as written."""
        return self._setting

    def __eq__(self, other: object) -> bool:
        return isinstance(other, PinTypeFromSetting) and other._setting == self._setting

    def __hash__(self) -> int:
        return hash((PinTypeFromSetting, self._setting))

    def __repr__(self) -> str:
        return f"PinTypeFromSetting({self._setting!r})"
