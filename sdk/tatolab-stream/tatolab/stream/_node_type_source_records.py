# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""What the source reader follows to a record a function node returns: the modules of one source,
each parsed at most once, the names each binds by its imports, and the `NamedTuple`s it calls
into being, read from source alone.
"""

from __future__ import annotations

import ast
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import PurePosixPath
from typing import TYPE_CHECKING, Optional

if TYPE_CHECKING:
    from ._node_type_source_reader import _ModuleDeclarationReading as ModuleNames

__all__ = [
    "NAMED_TUPLE_BASE",
    "FieldOfARecord",
    "ModulesOfOneSource",
    "RecordOfOutputs",
    "imported_modules_by_local_name",
    "imported_names_by_local_name",
    "named_tuples_called_by_name",
]

NAMED_TUPLE_BASE = "NamedTuple"


@dataclass(frozen=True)
class FieldOfARecord:
    """One field of a record a function node returns, and the module whose source types it."""

    name: str
    annotation: Optional[ast.expr]
    typed_in: "ModuleNames"


@dataclass(frozen=True)
class RecordOfOutputs:
    """A class a function node may return: its fields, whether it is a `NamedTuple` and whether
    `@named_pin_type` declares it, which makes it one pin."""

    fields: list[FieldOfARecord]
    is_a_named_tuple: bool
    is_a_named_pin_type: bool


class ModulesOfOneSource:
    """The modules one source hands the reader, by module name, each parsed at most once and read
    for the names it binds alone, so a module finds a record another of them defines."""

    def __init__(
        self,
        names_read_by: "Callable[[str, str, ast.Module, ModulesOfOneSource], ModuleNames]",
        sources_by_module_name: dict[str, tuple[str, str]],
    ) -> None:
        self._names_read_by = names_read_by
        self._sources_by_module_name = sources_by_module_name
        self._names_of_each_module: dict[str, Optional[ModuleNames]] = {}

    def is_a_package(self, module_name: str) -> bool:
        """Whether `module_name` is a package's `__init__`, which its relative imports start from."""
        source = self._sources_by_module_name.get(module_name)
        return source is not None and PurePosixPath(source[0]).stem == "__init__"

    def names_bound_by(self, module_name: str) -> Optional["ModuleNames"]:
        """The names `module_name` binds, or `None` for a module the source does not hold or that
        does not parse."""
        if module_name not in self._names_of_each_module:
            self._names_of_each_module[module_name] = self._the_names_read_from(module_name)
        return self._names_of_each_module[module_name]

    def _the_names_read_from(self, module_name: str) -> Optional["ModuleNames"]:
        source = self._sources_by_module_name.get(module_name)
        if source is None:
            return None
        listed_path, source_text = source
        try:
            module_tree = ast.parse(source_text, filename=listed_path)
        except (SyntaxError, ValueError):
            return None
        return self._names_read_by(module_name, listed_path, module_tree, self)


def imported_names_by_local_name(module_tree: ast.Module) -> dict[str, tuple[int, str, str]]:
    """Each name the module binds at its top level by `from ... import`, as `(relative level,
    module, name imported)`."""
    imported: dict[str, tuple[int, str, str]] = {}
    for statement in module_tree.body:
        if isinstance(statement, ast.ImportFrom):
            for alias in statement.names:
                if alias.name != "*":
                    imported[alias.asname or alias.name] = (
                        statement.level,
                        statement.module or "",
                        alias.name,
                    )
    return imported


def imported_modules_by_local_name(module_tree: ast.Module) -> dict[str, str]:
    """Each name the module binds at its top level by `import`, to the module it names: `import
    a.b` binds `a` to `a`, and `import a.b as c` binds `c` to `a.b`."""
    imported: dict[str, str] = {}
    for statement in module_tree.body:
        if isinstance(statement, ast.Import):
            for alias in statement.names:
                if alias.asname is not None:
                    imported[alias.asname] = alias.name
                else:
                    first_name = alias.name.partition(".")[0]
                    imported[first_name] = first_name
    return imported


def named_tuples_called_by_name(
    module_tree: ast.Module,
) -> dict[str, list[tuple[str, Optional[ast.expr]]]]:
    """Each `X = NamedTuple("X", [("field", T), ...])` at the module's top level, by `X`, with its
    fields; a call whose fields its source does not spell is left out."""
    named_tuples: dict[str, list[tuple[str, Optional[ast.expr]]]] = {}
    for statement in module_tree.body:
        if not (
            isinstance(statement, ast.Assign)
            and len(statement.targets) == 1
            and isinstance(statement.targets[0], ast.Name)
            and isinstance(statement.value, ast.Call)
            and _the_last_name_of(statement.value.func) == NAMED_TUPLE_BASE
        ):
            continue
        call = statement.value
        fields: list[tuple[str, Optional[ast.expr]]] = []
        if len(call.args) >= 2 and isinstance(call.args[1], (ast.List, ast.Tuple)):
            for field in call.args[1].elts:
                if not (
                    isinstance(field, ast.Tuple)
                    and len(field.elts) == 2
                    and isinstance(field.elts[0], ast.Constant)
                    and isinstance(field.elts[0].value, str)
                ):
                    break
                fields.append((field.elts[0].value, field.elts[1]))
            else:
                named_tuples[statement.targets[0].id] = fields
        elif len(call.args) == 1 and call.keywords:
            named_tuples[statement.targets[0].id] = [
                (keyword.arg, keyword.value) for keyword in call.keywords if keyword.arg is not None
            ]
    return named_tuples


def _the_last_name_of(expression: ast.expr) -> Optional[str]:
    """The last name of the dotted name `expression` spells: `NamedTuple` for `typing.NamedTuple`."""
    if isinstance(expression, ast.Name):
        return expression.id
    if isinstance(expression, ast.Attribute):
        return expression.attr
    return None
