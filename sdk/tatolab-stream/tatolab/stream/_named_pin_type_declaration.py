# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A module's own pin type: a class under `@named_pin_type(description="…")`, carried as the bytes it packs.

The type's name is the class's import path, `module:QualifiedClassName`, and a pin of it links
only to a pin of the same name, so a module's data never reaches a node that would misread it.
The class packs a value with `to_bytes()` and builds one back with its `from_bytes` classmethod,
the one `read(port, into=…)` takes, so a value travels between nodes as bytes, never as a live
object. A named type holds no stored value: an input of one reads only what a link brings.
"""

from __future__ import annotations

import inspect
from collections.abc import Buffer, Callable
from dataclasses import dataclass
from typing import Any, Final, Optional, Protocol, Self, TypeVar, runtime_checkable

__all__ = [
    "NAMED_PIN_TYPE_STAMP",
    "NamedPinTypeDeclared",
    "NamedPinTypeValue",
    "named_pin_type",
    "named_pin_type_declared_by",
]

NAMED_PIN_TYPE_STAMP: Final = "__tatolab_named_pin_type__"
"""The attribute `@named_pin_type` stamps on the class it declares, holding a `NamedPinTypeDeclared`."""


@runtime_checkable
class NamedPinTypeValue(Protocol):
    """A value of a named pin type: it packs itself into bytes, and its class builds it back from them."""

    @classmethod
    def from_bytes(cls, payload: Buffer, /) -> Self:
        """The value `to_bytes` packed into `payload`."""
        ...

    def to_bytes(self) -> bytes:
        """This value packed as the bytes a link carries."""
        ...


@dataclass(frozen=True, slots=True)
class NamedPinTypeDeclared:
    """What `@named_pin_type` read of the class it declares."""

    import_path: str
    """The type's name: the class's import path, `module:QualifiedClassName`."""

    description: str
    """What a value of the type is, as `list_pin_types` and a pin's hint show it."""


_NamedPinTypeClass = TypeVar("_NamedPinTypeClass", bound=type)


def named_pin_type(*, description: str) -> Callable[[_NamedPinTypeClass], _NamedPinTypeClass]:
    """Declare the class it decorates as a pin type of its module, named by its import path.

    The class has a `from_bytes` classmethod and a `to_bytes` method, and is defined at the top
    level of its module or inside another class, so its import path finds it. Anything else is
    refused with a `TypeError` naming the class and the fix.
    """
    if not isinstance(description, str) or not description.strip():  # pyright: ignore[reportUnnecessaryIsInstance]
        raise TypeError(
            f"@named_pin_type(description=...) takes what a value of the type is, in words, and "
            f"was given {description!r}. To fix: describe it, "
            f'`@named_pin_type(description="A face found in a picture, as a box and a score.")`.'
        )

    def declare(declared_class: _NamedPinTypeClass) -> _NamedPinTypeClass:
        if not inspect.isclass(declared_class):
            raise TypeError(
                f"@named_pin_type goes on a class, and was put on {declared_class!r}. To fix: "
                f"decorate the class whose values the pins carry."
            )
        _refuse_a_class_that_does_not_pack_its_values(declared_class)
        import_path = _the_import_path_of(declared_class)
        setattr(
            declared_class,
            NAMED_PIN_TYPE_STAMP,
            NamedPinTypeDeclared(import_path=import_path, description=description.strip()),
        )
        return declared_class

    return declare


def named_pin_type_declared_by(annotation: object) -> Optional[NamedPinTypeDeclared]:
    """What `@named_pin_type` declared of `annotation`; `None` for anything it did not decorate.

    A subclass of a declared class is not itself declared: it has an import path of its own and
    would carry another type's name.
    """
    if not inspect.isclass(annotation):
        return None
    declared: Any = annotation.__dict__.get(NAMED_PIN_TYPE_STAMP)
    return declared if isinstance(declared, NamedPinTypeDeclared) else None


def _the_import_path_of(declared_class: type) -> str:
    qualified_name = declared_class.__qualname__
    if "<locals>" in qualified_name.split("."):
        raise TypeError(
            f"@named_pin_type is put on `{qualified_name}`, which is defined inside a function, "
            f"so no import path finds it and no other node can name its type. To fix: define "
            f"the class at the top level of its module."
        )
    return f"{declared_class.__module__}:{qualified_name}"


def _refuse_a_class_that_does_not_pack_its_values(declared_class: type) -> None:
    named = f"`{declared_class.__qualname__}`"
    from_bytes = inspect.getattr_static(declared_class, "from_bytes", None)
    if not isinstance(from_bytes, classmethod):
        raise TypeError(
            f"@named_pin_type is put on {named}, which has no `from_bytes` classmethod, "
            f"so a node reading the type cannot build a value from the bytes a link carries. "
            f"To fix: add `@classmethod def from_bytes(cls, payload: Buffer) -> Self`."
        )
    to_bytes = inspect.getattr_static(declared_class, "to_bytes", None)
    if not inspect.isfunction(to_bytes):
        raise TypeError(
            f"@named_pin_type is put on {named}, which has no `to_bytes` method, so a "
            f"node writing the type cannot pack a value into the bytes a link carries. To fix: "
            f"add `def to_bytes(self) -> bytes`."
        )
