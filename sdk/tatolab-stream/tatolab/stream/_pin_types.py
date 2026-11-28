# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The names an author types a pin with, and the one table from each name to its built-in pin type.

Every written form of every built-in pin type the contract lists is a row of
`PIN_TYPE_SPELLINGS`, and the conformance test holds the two equal, so a spelling that drifts
from its contract row fails a test. A row the typed grammar does not read yet says why, and
a declaration that uses it is refused by name with that reason.

The exact sizes are `Annotated` aliases of `int` and `float`, so a value of one is a plain
number in Python. A Color, a Vector, a Rectangle and a Transform are frozen value classes whose
fields are the keys of the type's stored JSON form, in the order the type packs its parts on a
link. An Asset is a frozen value class of the asset's UUID and, for an asset of an import, that
import's project UUID. A Texture, an Audio, a Tensor, an Encoded video and an Encoded audio hold
no stored value: each is the class of what a read of its pin returns, which a class node's
`InputPin[...]` or `OutputPin[...]` carries. A Trigger holds none either, and is a class that only
names its type.
"""

from __future__ import annotations

import dataclasses
import math
import uuid
from collections.abc import Iterable, Sequence
from dataclasses import dataclass
from typing import Annotated, Any, ClassVar, Final, Optional, Self, final

from ._contract_pin_types import BuiltInPinTypeName, FloatSize, IntegerSize, PinType
from ._node_context_protocols import Audio, EncodedAudio, EncodedVideo
from .array_frame import Tensor
from .image_frame import Texture

__all__ = [
    "PIN_TYPE_SPELLINGS",
    "PIN_VALUE_CLASSES",
    "STREAM_PIN_TYPE_CLASSES",
    "Asset",
    "Audio",
    "Color",
    "EncodedAudio",
    "EncodedVideo",
    "PinValueOfParts",
    "Rectangle",
    "Tensor",
    "Texture",
    "Transform",
    "Trigger",
    "Vector2",
    "Vector3",
    "Vector4",
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


class PinValueOfParts:
    """A value of a pin type that travels as its parts, one number on each channel of a control message.

    Each field is a number, a key of the type's stored JSON form; an `int` is held as a `float`,
    and `bool`, `NaN` and infinity are refused. Nothing is clamped, so a Color number outside 0
    to 1 is kept as given.
    """

    __slots__ = ()

    pin_type: ClassVar[PinType]
    """The built-in pin type a pin typed with this class declares."""

    value_type: ClassVar[str]
    """The name a function node's signature gives a value of this class, which the runtime
    reads it by."""

    def __post_init__(self) -> None:
        for field in dataclasses.fields(self):  # pyright: ignore[reportArgumentType]
            object.__setattr__(
                self, field.name, _a_finite_number(getattr(self, field.name), type(self), field.name)
            )
        self._refuse_what_its_type_does_not_take()

    def _refuse_what_its_type_does_not_take(self) -> None:
        """Refuse a value its type does not take beyond its numbers; every one is taken here."""

    @classmethod
    def part_names(cls) -> tuple[str, ...]:
        """The name of each of its numbers, in the order it packs them: the channel names it travels under."""
        return tuple(field.name for field in dataclasses.fields(cls))  # pyright: ignore[reportArgumentType]

    @property
    def parts(self) -> tuple[float, ...]:
        """Its numbers in the order it packs them on a link, which is its fields' order."""
        return tuple(getattr(self, field.name) for field in dataclasses.fields(self))  # pyright: ignore[reportArgumentType]

    @classmethod
    def from_parts(cls, parts: Iterable[float]) -> Self:
        """The value whose numbers, in the order it packs them, are `parts`."""
        parts = tuple(parts)
        field_names = [field.name for field in dataclasses.fields(cls)]  # pyright: ignore[reportArgumentType]
        if len(parts) != len(field_names):
            raise ValueError(
                f"a {cls.__name__} is {len(field_names)} numbers, "
                f"{', '.join(field_names)}, and {len(parts)} were given"
            )
        return cls(*parts)

    def stored_value(self) -> Any:
        """Its stored JSON form, as a document saves it and a client sets it: an object keyed by field."""
        return {field.name: getattr(self, field.name) for field in dataclasses.fields(self)}  # pyright: ignore[reportArgumentType]

    @classmethod
    def from_stored_value(cls, stored_value: Any) -> Self:
        """The value whose stored JSON form is `stored_value`, refused when it is not exactly one."""
        field_names = [field.name for field in dataclasses.fields(cls)]  # pyright: ignore[reportArgumentType]
        if not isinstance(stored_value, dict) or sorted(stored_value) != sorted(field_names):  # pyright: ignore[reportUnknownArgumentType]
            raise ValueError(
                f"a stored {cls.__name__} is an object of exactly {', '.join(field_names)}, "
                f"and was {stored_value!r}"
            )
        return cls(**stored_value)


def _a_finite_number(value: object, value_class: type, field: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise TypeError(
            f"{value_class.__name__}.{field} is a number, and was given {value!r}"
        )
    if not math.isfinite(value):
        raise ValueError(f"{value_class.__name__}.{field} is finite, and was given {value!r}")
    return float(value)


@final
@dataclass(frozen=True, slots=True)
class Color(PinValueOfParts):
    """A colour: sRGB red, green and blue and alpha, each 0 for none and 1 for full.

    Red, green and blue are what a hex colour holds divided by 255, not linear light, and are
    not multiplied by alpha. Alpha 1 is opaque.
    """

    pin_type: ClassVar[PinType] = {"type": "color"}
    value_type: ClassVar[str] = "color"

    red: float
    green: float
    blue: float
    alpha: float = 1.0


@final
@dataclass(frozen=True, slots=True)
class Vector2(PinValueOfParts):
    """A Vector of two components."""

    pin_type: ClassVar[PinType] = {"type": "vector", "component_count": "two"}
    value_type: ClassVar[str] = "vector2"

    x: float
    y: float


@final
@dataclass(frozen=True, slots=True)
class Vector3(PinValueOfParts):
    """A Vector of three components."""

    pin_type: ClassVar[PinType] = {"type": "vector", "component_count": "three"}
    value_type: ClassVar[str] = "vector3"

    x: float
    y: float
    z: float


@final
@dataclass(frozen=True, slots=True)
class Vector4(PinValueOfParts):
    """A Vector of four components."""

    pin_type: ClassVar[PinType] = {"type": "vector", "component_count": "four"}
    value_type: ClassVar[str] = "vector4"

    x: float
    y: float
    z: float
    w: float


@final
@dataclass(frozen=True, slots=True)
class Rectangle(PinValueOfParts):
    """A rectangle: its left edge `x`, its top edge `y`, and a `width` and `height` of 0 or more.

    The node says whether the numbers are pixels or a share of the frame.
    """

    pin_type: ClassVar[PinType] = {"type": "rectangle"}
    value_type: ClassVar[str] = "rectangle"

    x: float
    y: float
    width: float
    height: float

    def _refuse_what_its_type_does_not_take(self) -> None:
        for field in ("width", "height"):
            if getattr(self, field) < 0:
                raise ValueError(
                    f"a Rectangle's {field} is 0 or more, and was given {getattr(self, field)!r}"
                )


# The number of numbers a Transform holds: a 4 by 4 matrix.
_TRANSFORM_NUMBER_COUNT: Final = 16


@final
@dataclass(frozen=True, slots=True)
class Transform(PinValueOfParts):
    """A 4 by 4 matrix as its 16 numbers, row by row.

    It multiplies a column, so the fourth number of each of the first three rows is the
    translation. Its stored JSON form is the list of the 16 numbers.
    """

    pin_type: ClassVar[PinType] = {"type": "transform"}
    value_type: ClassVar[str] = "transform"

    numbers: tuple[float, ...]

    def __post_init__(self) -> None:
        numbers = tuple(self.numbers)
        if len(numbers) != _TRANSFORM_NUMBER_COUNT:
            raise ValueError(
                f"a Transform is {_TRANSFORM_NUMBER_COUNT} numbers, row by row, and was given "
                f"{len(numbers)}"
            )
        object.__setattr__(
            self,
            "numbers",
            tuple(
                _a_finite_number(number, Transform, f"numbers[{index}]")
                for index, number in enumerate(numbers)
            ),
        )

    @classmethod
    def part_names(cls) -> tuple[str, ...]:
        return tuple(f"row{row}_column{column}" for row in range(1, 5) for column in range(1, 5))

    @classmethod
    def identity(cls) -> Transform:
        """The Transform that moves nothing."""
        return cls(tuple(float(row == column) for row in range(4) for column in range(4)))

    @property
    def parts(self) -> tuple[float, ...]:
        return self.numbers

    @classmethod
    def from_parts(cls, parts: Iterable[float]) -> Transform:
        return cls(tuple(parts))

    def stored_value(self) -> Any:
        return list(self.numbers)

    @classmethod
    def from_stored_value(cls, stored_value: Any) -> Transform:
        if not isinstance(stored_value, Sequence) or isinstance(stored_value, str):
            raise ValueError(f"a stored Transform is a list of 16 numbers, and was {stored_value!r}")
        return cls(tuple(stored_value))  # pyright: ignore[reportUnknownArgumentType]


@final
@dataclass(frozen=True, slots=True)
class Asset:
    """One of the project's assets, by the UUID `list_assets` answers for it.

    `from_project` is the UUID of the imported project that holds it, and `None` for an asset
    of the project's own. Its stored JSON form is the object `{"asset": "<uuid>"}`, with
    `"from": "<project uuid>"` for an asset of an import, as a saved document holds it.
    """

    asset: str
    """The asset's UUID, in its lowercase hyphenated form."""

    from_project: Optional[str] = None
    """The UUID of the imported project that holds the asset; `None` for the project's own."""

    pin_type: ClassVar[PinType] = {"type": "asset"}  # pyright: ignore[reportAssignmentType]
    """The built-in pin type a pin typed `Asset` declares."""

    def __post_init__(self) -> None:
        object.__setattr__(self, "asset", _a_uuid(self.asset, "asset"))
        if self.from_project is not None:
            object.__setattr__(self, "from_project", _a_uuid(self.from_project, "from_project"))

    def stored_value(self) -> dict[str, str]:
        """Its stored JSON form: `{"asset": "<uuid>"}`, with `"from"` for an asset of an import."""
        stored = {"asset": self.asset}
        if self.from_project is not None:
            stored["from"] = self.from_project
        return stored

    @classmethod
    def from_stored_value(cls, stored_value: Any) -> Asset:
        """The asset whose stored JSON form is `stored_value`, refused when it is not exactly one."""
        if (
            not isinstance(stored_value, dict)
            or "asset" not in stored_value
            or not set(stored_value) <= {"asset", "from"}  # pyright: ignore[reportUnknownArgumentType]
        ):
            raise ValueError(
                f'a stored Asset is the object {{"asset": "<uuid>"}}, with "from" for an asset '
                f"of an import, and was {stored_value!r}"
            )
        return cls(stored_value["asset"], stored_value.get("from"))  # pyright: ignore[reportUnknownArgumentType, reportUnknownMemberType]


def _a_uuid(value: object, field: str) -> str:
    if not isinstance(value, str):
        raise TypeError(f"Asset.{field} is a UUID string, and was given {value!r}")
    try:
        return str(uuid.UUID(value))
    except ValueError:
        raise ValueError(f"Asset.{field} is a UUID, and was given {value!r}") from None


class _StreamPinType:
    """A pin type that holds no stored value, named in an annotation and never constructed."""

    __slots__ = ()

    def __init__(self) -> None:
        raise TypeError(
            f"`{type(self).__name__}` names a pin type and holds no value, so it is never "
            f"constructed. Write it as a pin, `InputPin[{type(self).__name__}]`."
        )


@final
class Trigger(_StreamPinType):
    """An event with no payload; its message's timestamp is the event."""

    __slots__ = ()


STREAM_PIN_TYPE_CLASSES: Final[tuple[type, ...]] = (
    Texture,
    Audio,
    Tensor,
    EncodedVideo,
    EncodedAudio,
    Trigger,
)
"""Each class that names a pin type holding no stored value, which only a pin carries."""


PIN_VALUE_CLASSES: Final[tuple[type[PinValueOfParts], ...]] = (
    Color,
    Vector2,
    Vector3,
    Vector4,
    Rectangle,
    Transform,
)
"""Each value class of a pin type that travels as its parts."""


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

    only_a_pin_carries: bool = False
    """Whether it holds no stored value, so a class node's `InputPin[...]` or `OutputPin[...]`
    carries it and no field or function parameter does."""


_SETTING_TYPE_NOT_TAKEN = "a function node takes no setting yet"
_A_CHOICE_ONLY_A_NODE_FUNCTION_INPUT_DECLARES = "only an input of a node written as one function chooses a pin's type"

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
    PinTypeSpelling("str", "string"),
    PinTypeSpelling("Literal", "menu"),
    PinTypeSpelling("Color", "color"),
    PinTypeSpelling("Vector2", "vector"),
    PinTypeSpelling("Vector3", "vector"),
    PinTypeSpelling("Vector4", "vector"),
    PinTypeSpelling("Rectangle", "rectangle"),
    PinTypeSpelling("Transform", "transform"),
    PinTypeSpelling("Texture", "texture", only_a_pin_carries=True),
    PinTypeSpelling("Audio", "audio", only_a_pin_carries=True),
    PinTypeSpelling("EncodedVideo", "encoded_video", only_a_pin_carries=True),
    PinTypeSpelling("EncodedAudio", "encoded_audio", only_a_pin_carries=True),
    PinTypeSpelling("Tensor", "tensor", only_a_pin_carries=True),
    PinTypeSpelling("Asset", "asset"),
    PinTypeSpelling("Trigger", "trigger", only_a_pin_carries=True),
    PinTypeSpelling("list", "list_of"),
    PinTypeSpelling("dataclass", "bundle"),
    PinTypeSpelling("named_pin_type", "named"),
    PinTypeSpelling("connection_type", "connection"),
    PinTypeSpelling("NodeReference", "node_reference", _SETTING_TYPE_NOT_TAKEN),
    PinTypeSpelling("PinTypeChoice", "pin_type_choice", _A_CHOICE_ONLY_A_NODE_FUNCTION_INPUT_DECLARES),
)

_SPELLING_BY_WRITTEN_FORM: Final[dict[str, PinTypeSpelling]] = {
    spelling.written_form: spelling for spelling in PIN_TYPE_SPELLINGS
}


def pin_type_spelling_written_as(written_form: str) -> PinTypeSpelling:
    """The row of `written_form`, one of the forms `PIN_TYPE_SPELLINGS` lists."""
    return _SPELLING_BY_WRITTEN_FORM[written_form]
