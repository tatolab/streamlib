# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The names an author types a pin with, and the one table from each name to its built-in pin type.

Every written form of every built-in pin type the contract lists is a row of
`PIN_TYPE_SPELLINGS`, and the conformance test holds the two equal, so a spelling that drifts
from its contract row fails a test. A row the typed grammar does not read yet says why, and
a declaration that uses it is refused by name with that reason.

The exact sizes are `Annotated` aliases of `int` and `float`, so a value of one is a plain
number in Python.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Annotated, Final, Optional, final

from ._contract_pin_types import BuiltInPinTypeName, FloatSize, IntegerSize

__all__ = [
    "PIN_TYPE_SPELLINGS",
    "Float32",
    "Float64",
    "Int8",
    "Int16",
    "Int32",
    "Int64",
    "UInt8",
    "UInt16",
    "UInt32",
    "UInt64",
    "ExactNumberSize",
    "PinTypeSpelling",
    "pin_type_spelling_written_as",
]


@final
class ExactNumberSize:
    """The size an exact-size alias holds its number in, as the contract spells it."""

    __slots__ = ("_size",)

    def __init__(self, size: IntegerSize | FloatSize) -> None:
        self._size: IntegerSize | FloatSize = size

    @property
    def size(self) -> IntegerSize | FloatSize:
        """The contract's name of the size, such as `int8` or `float32`."""
        return self._size

    @property
    def written_form(self) -> str:
        """The alias that states this size, such as `Int8` or `Float32`."""
        return _WRITTEN_FORM_OF_EACH_EXACT_SIZE[self._size]

    def __eq__(self, other: object) -> bool:
        return isinstance(other, ExactNumberSize) and other._size == self._size

    def __hash__(self) -> int:
        return hash((ExactNumberSize, self._size))

    def __repr__(self) -> str:
        return f"ExactNumberSize({self._size!r})"


Float32 = Annotated[float, ExactNumberSize("float32")]
"""A Float held in 32 bits."""
Float64 = Annotated[float, ExactNumberSize("float64")]
"""A Float held in 64 bits, stated."""
Int8 = Annotated[int, ExactNumberSize("int8")]
"""An Integer that fits 8 signed bits."""
Int16 = Annotated[int, ExactNumberSize("int16")]
"""An Integer that fits 16 signed bits."""
Int32 = Annotated[int, ExactNumberSize("int32")]
"""An Integer that fits 32 signed bits."""
Int64 = Annotated[int, ExactNumberSize("int64")]
"""An Integer that fits 64 signed bits: a List or Tensor element only."""
UInt8 = Annotated[int, ExactNumberSize("uint8")]
"""An Integer that fits 8 unsigned bits."""
UInt16 = Annotated[int, ExactNumberSize("uint16")]
"""An Integer that fits 16 unsigned bits."""
UInt32 = Annotated[int, ExactNumberSize("uint32")]
"""An Integer that fits 32 unsigned bits."""
UInt64 = Annotated[int, ExactNumberSize("uint64")]
"""An Integer that fits 64 unsigned bits: a List or Tensor element only."""

_WRITTEN_FORM_OF_EACH_EXACT_SIZE: Final[dict[IntegerSize | FloatSize, str]] = {
    "float32": "Float32",
    "float64": "Float64",
    "int8": "Int8",
    "int16": "Int16",
    "int32": "Int32",
    "int64": "Int64",
    "uint8": "UInt8",
    "uint16": "UInt16",
    "uint32": "UInt32",
    "uint64": "UInt64",
}

# The lowest and highest whole number each integer size holds.
INTEGER_BOUNDS_OF_EACH_SIZE: Final[dict[IntegerSize, tuple[int, int]]] = {
    "int8": (-(2**7), 2**7 - 1),
    "int16": (-(2**15), 2**15 - 1),
    "int32": (-(2**31), 2**31 - 1),
    "int64": (-(2**63), 2**63 - 1),
    "uint8": (0, 2**8 - 1),
    "uint16": (0, 2**16 - 1),
    "uint32": (0, 2**32 - 1),
    "uint64": (0, 2**64 - 1),
}

# The sizes too wide for a single pin: a value travels as a 64-bit float, exact to 2^53.
INTEGER_SIZES_ONLY_AN_ELEMENT_HOLDS: Final[frozenset[IntegerSize]] = frozenset({"int64", "uint64"})


@dataclass(frozen=True, slots=True)
class PinTypeSpelling:
    """One way an author writes a built-in pin type."""

    written_form: str
    """How a declaration writes it, as the contract row's `python_forms` lists it."""

    pin_type_name: BuiltInPinTypeName
    """The built-in pin type it declares."""

    not_read_yet_because: Optional[str] = None
    """Why the typed grammar does not read it yet, as the end of a sentence that begins
    "it is not read yet because"; `None` when it is read."""


_VALUES_NOT_CARRIED = "the runtime does not carry its values on a link yet"
_STREAM_TYPE_NOT_SPELLED = (
    "a function under bare `@node` does not take a stream type yet; `@node.function` takes "
    "it by its read type"
)
_BUNDLE_NOT_CARRIED = "a bundle pin is not carried yet"
_NAMED_TYPE_NOT_CARRIED = "a named pin type is not carried yet"
_SETTING_TYPE_NOT_TAKEN = "a function node takes no setting yet"
_TRIGGER_NOT_CARRIED = "a function under bare `@node` does not take a Trigger yet"

PIN_TYPE_SPELLINGS: Final[tuple[PinTypeSpelling, ...]] = (
    PinTypeSpelling("bool", "boolean"),
    PinTypeSpelling("int", "integer"),
    PinTypeSpelling("Int8", "integer"),
    PinTypeSpelling("Int16", "integer"),
    PinTypeSpelling("Int32", "integer"),
    PinTypeSpelling("Int64", "integer"),
    PinTypeSpelling("UInt8", "integer"),
    PinTypeSpelling("UInt16", "integer"),
    PinTypeSpelling("UInt32", "integer"),
    PinTypeSpelling("UInt64", "integer"),
    PinTypeSpelling("float", "float"),
    PinTypeSpelling("Float32", "float"),
    PinTypeSpelling("Float64", "float"),
    PinTypeSpelling("str", "string", _VALUES_NOT_CARRIED),
    PinTypeSpelling("Literal", "menu", _VALUES_NOT_CARRIED),
    PinTypeSpelling("Color", "color", _VALUES_NOT_CARRIED),
    PinTypeSpelling("Vector2", "vector", _VALUES_NOT_CARRIED),
    PinTypeSpelling("Vector3", "vector", _VALUES_NOT_CARRIED),
    PinTypeSpelling("Vector4", "vector", _VALUES_NOT_CARRIED),
    PinTypeSpelling("Rectangle", "rectangle", _VALUES_NOT_CARRIED),
    PinTypeSpelling("Transform", "transform", _VALUES_NOT_CARRIED),
    PinTypeSpelling("Texture", "texture", _STREAM_TYPE_NOT_SPELLED),
    PinTypeSpelling("Audio", "audio", _STREAM_TYPE_NOT_SPELLED),
    PinTypeSpelling("EncodedVideo", "encoded_video", _STREAM_TYPE_NOT_SPELLED),
    PinTypeSpelling("EncodedAudio", "encoded_audio", _STREAM_TYPE_NOT_SPELLED),
    PinTypeSpelling("Tensor", "tensor", _STREAM_TYPE_NOT_SPELLED),
    PinTypeSpelling("Asset", "asset", _VALUES_NOT_CARRIED),
    PinTypeSpelling("Trigger", "trigger", _TRIGGER_NOT_CARRIED),
    PinTypeSpelling("list", "list_of", _VALUES_NOT_CARRIED),
    PinTypeSpelling("dataclass", "bundle", _BUNDLE_NOT_CARRIED),
    PinTypeSpelling("named_pin_type", "named", _NAMED_TYPE_NOT_CARRIED),
    PinTypeSpelling("NodeReference", "node_reference", _SETTING_TYPE_NOT_TAKEN),
    PinTypeSpelling("NodeReferenceToNodeType", "node_reference", _SETTING_TYPE_NOT_TAKEN),
    PinTypeSpelling(
        "SoleReferrerNodeReferenceToNodeType", "node_reference", _SETTING_TYPE_NOT_TAKEN
    ),
    PinTypeSpelling("PinTypeChoice", "pin_type_choice", _SETTING_TYPE_NOT_TAKEN),
)

_SPELLING_BY_WRITTEN_FORM: Final[dict[str, PinTypeSpelling]] = {
    spelling.written_form: spelling for spelling in PIN_TYPE_SPELLINGS
}


def pin_type_spelling_written_as(written_form: str) -> PinTypeSpelling:
    """The row of `written_form`, one of the forms `PIN_TYPE_SPELLINGS` lists."""
    return _SPELLING_BY_WRITTEN_FORM[written_form]
