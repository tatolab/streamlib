# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The names a renamed pin had, written beside it: `Annotated[float, FormerlyNamed("frequency_hz")]`.

A project saved before the rename names the pin by one of these, and the runtime finds the pin
under each of them, so the saved value and links land on it under its name. It is no key of the
contract's `PinAnnotations`: it fills the declaration's own `former_names`.
"""

from __future__ import annotations

from typing import final

from ._exposed_name_cast import ExposedNameCastsToNothingError, cast_exposed_name_to_url_safe

__all__ = ["FormerlyNamed"]


@final
class FormerlyNamed:
    """The names a pin had before it was renamed, as written then: `FormerlyNamed("frequency_hz")`.

    A pin renamed more than once lists each name it had, the one it had last first.
    """

    __slots__ = ("_names", "_port_names")

    def __init__(self, name: str, /, *earlier_names: str) -> None:
        names = (name, *earlier_names)
        port_names: list[str] = []
        for each in names:
            if not isinstance(each, str):
                raise TypeError(f"FormerlyNamed(...) takes each name as a str, got {each!r}.")
            try:
                port_name = cast_exposed_name_to_url_safe(each)
            except ExposedNameCastsToNothingError as names_nothing:
                raise ValueError(
                    f"FormerlyNamed(...) takes the names a pin had, and {each!r} names no pin: "
                    f"{names_nothing}"
                ) from names_nothing
            if port_name in port_names:
                raise ValueError(
                    f"FormerlyNamed(...) lists {each!r}, which names the same pin as a name "
                    f"before it. To fix: list each name once."
                )
            port_names.append(port_name)
        self._names = names
        self._port_names = tuple(port_names)

    @property
    def names(self) -> "tuple[str, ...]":
        """The names as written."""
        return self._names

    @property
    def port_names(self) -> "tuple[str, ...]":
        """The names as the runtime holds a port's name, each cast as a pin's own name is."""
        return self._port_names

    def __eq__(self, other: object) -> bool:
        return isinstance(other, FormerlyNamed) and other._names == self._names

    def __hash__(self) -> int:
        return hash((FormerlyNamed, self._names))

    def __repr__(self) -> str:
        return f"FormerlyNamed({', '.join(repr(name) for name in self._names)})"
