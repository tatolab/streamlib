# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""Importing the module of a node type, from the import folder that leads it when one does.

A node type naming a class in an import's own node code is led by that import's folder,
relative to the project: `imports/<folder>/<module>:<qualname>`. Its module is imported with
that folder first on `sys.path`. One interpreter holds one module per top-level name, so a
top-level name already imported from another folder, or from the project and its venv, is
refused by name rather than read from the wrong place.
"""

from __future__ import annotations

import importlib
import os
import sys
from types import ModuleType
from typing import Optional

#: What ends the import folder leading a node type; neither a Python nor a Rust import path
#: holds it. The engine's `IMPORT_FOLDER_OF_A_NODE_TYPE_SEPARATOR` is the same character.
IMPORT_FOLDER_OF_A_NODE_TYPE_SEPARATOR = "/"

#: Each import folder this interpreter put on `sys.path` to import a node type's module.
_IMPORT_FOLDERS_PUT_ON_SYS_PATH: "set[str]" = set()


class NodeTypeModuleImportedFromTwoPlaces(ImportError):
    """A node type's top-level module is already imported in this interpreter from elsewhere."""


def import_folder_and_class_import_path(node_type: str) -> "tuple[Optional[str], str]":
    """The import folder leading `node_type`, `None` when none does, and its `<module>:<qualname>`."""
    import_folder, separator, class_import_path = node_type.rpartition(
        IMPORT_FOLDER_OF_A_NODE_TYPE_SEPARATOR
    )
    if not separator or not import_folder:
        return None, node_type
    return import_folder, class_import_path


def import_the_module_of_a_node_type(module_name: str, import_folder: Optional[str]) -> ModuleType:
    """Import `module_name`, from `import_folder` relative to the working directory when given."""
    top_level_module = module_name.partition(".")[0]
    folder_on_disk = os.path.realpath(import_folder) if import_folder is not None else None
    if top_level_module in sys.modules:
        _refuse_unless_imported_from(top_level_module, module_name, folder_on_disk)
    if folder_on_disk is not None and folder_on_disk not in _IMPORT_FOLDERS_PUT_ON_SYS_PATH:
        sys.path.insert(0, folder_on_disk)
        _IMPORT_FOLDERS_PUT_ON_SYS_PATH.add(folder_on_disk)
    module = importlib.import_module(module_name)
    _refuse_unless_imported_from(top_level_module, module_name, folder_on_disk)
    return module


def _refuse_unless_imported_from(
    top_level_module: str, module_name: str, folder_on_disk: Optional[str]
) -> None:
    imported_from = _the_import_folder_a_loaded_module_came_from(top_level_module)
    if imported_from != folder_on_disk:
        raise NodeTypeModuleImportedFromTwoPlaces(
            f"the top-level module `{top_level_module}` is imported in this interpreter from "
            f"{_named(imported_from)}, so `{module_name}` is not imported from "
            f"{_named(folder_on_disk)}; one interpreter holds one `{top_level_module}`"
        )


def _the_import_folder_a_loaded_module_came_from(top_level_module: str) -> Optional[str]:
    # Read from the module's own namespace: `getattr` would reach a PEP 562 module
    # `__getattr__`, which may raise for any name it does not hold.
    module_namespace = vars(sys.modules[top_level_module])
    locations = list(module_namespace.get("__path__") or [])
    module_file = module_namespace.get("__file__")
    if module_file:
        locations.append(os.path.dirname(module_file))
    locations_on_disk = [os.path.realpath(location) for location in locations]
    folders_holding_it = [
        folder
        for folder in _IMPORT_FOLDERS_PUT_ON_SYS_PATH
        if any(
            location == folder or location.startswith(folder + os.sep)
            for location in locations_on_disk
        )
    ]
    return max(folders_holding_it, key=len, default=None)


def _named(import_folder: Optional[str]) -> str:
    return "the project and its venv" if import_folder is None else f"`{import_folder}`"
