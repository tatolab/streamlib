# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""A node type led by an import's folder imports its module from that folder, and one interpreter refuses a top-level module from two places."""

import os
import sys
from pathlib import Path
from typing import Iterator

import pytest

from tatolab.runtime import _node_type_import_folder
from tatolab.runtime._node_type_import_folder import (
    NodeTypeModuleImportedFromTwoPlaces,
    import_folder_and_class_import_path,
    import_the_module_of_a_node_type,
)

TOP_LEVEL_MODULE = "node_type_import_folder_probe_nodes"


@pytest.fixture
def project_holding_an_import(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Iterator[Path]:
    """A project and its import `imports/robot-arm`, each with its own `<probe>/gripper.py`."""
    for root, made_by in [(tmp_path, "project"), (tmp_path / "imports/robot-arm", "robot-arm")]:
        package = root / TOP_LEVEL_MODULE
        package.mkdir(parents=True)
        (package / "__init__.py").write_text("")
        (package / "gripper.py").write_text(f"MADE_BY = {made_by!r}\n")
    nested = tmp_path / "imports/robot-arm/imports/claws" / TOP_LEVEL_MODULE
    nested.mkdir(parents=True)
    (nested / "__init__.py").write_text("")
    (nested / "gripper.py").write_text("MADE_BY = 'claws'\n")
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(sys, "path", [str(tmp_path), *sys.path])
    monkeypatch.setattr(_node_type_import_folder, "_IMPORT_FOLDERS_PUT_ON_SYS_PATH", set())
    yield tmp_path
    for module_name in [name for name in sys.modules if name.partition(".")[0] == TOP_LEVEL_MODULE]:
        del sys.modules[module_name]


def test_a_node_type_led_by_an_import_folder_reads_back_its_folder_and_class_import_path():
    assert import_folder_and_class_import_path(
        "imports/robot-arm/imports/claws/nodes.gripper:Gripper"
    ) == ("imports/robot-arm/imports/claws", "nodes.gripper:Gripper")
    for not_in_an_import in ["nodes.gripper:Gripper", "/nodes.gripper:Gripper"]:
        assert import_folder_and_class_import_path(not_in_an_import)[0] is None


def test_a_module_in_an_import_is_imported_from_its_folder_ahead_of_the_projects_own(
    project_holding_an_import: Path,
):
    module = import_the_module_of_a_node_type(f"{TOP_LEVEL_MODULE}.gripper", "imports/robot-arm")

    assert module.MADE_BY == "robot-arm"


def test_a_module_in_a_nested_import_is_imported_from_the_nested_folder(
    project_holding_an_import: Path,
):
    module = import_the_module_of_a_node_type(
        f"{TOP_LEVEL_MODULE}.gripper", "imports/robot-arm/imports/claws"
    )

    assert module.MADE_BY == "claws"


def test_the_same_top_level_module_from_the_project_and_an_import_is_refused_naming_both(
    project_holding_an_import: Path,
):
    assert import_the_module_of_a_node_type(f"{TOP_LEVEL_MODULE}.gripper", None).MADE_BY == (
        "project"
    )

    with pytest.raises(NodeTypeModuleImportedFromTwoPlaces) as refusal:
        import_the_module_of_a_node_type(f"{TOP_LEVEL_MODULE}.gripper", "imports/robot-arm")

    assert "the project and its venv" in str(refusal.value)
    assert os.path.realpath(project_holding_an_import / "imports/robot-arm") in str(refusal.value)


def test_a_projects_own_module_after_an_imports_of_the_same_name_is_refused(
    project_holding_an_import: Path,
):
    import_the_module_of_a_node_type(f"{TOP_LEVEL_MODULE}.gripper", "imports/robot-arm")

    with pytest.raises(NodeTypeModuleImportedFromTwoPlaces):
        import_the_module_of_a_node_type(f"{TOP_LEVEL_MODULE}.gripper", None)


def test_a_second_type_from_the_same_import_folder_imports(project_holding_an_import: Path):
    (project_holding_an_import / "imports/robot-arm" / TOP_LEVEL_MODULE / "wrist.py").write_text(
        "MADE_BY = 'robot-arm wrist'\n"
    )
    import_the_module_of_a_node_type(f"{TOP_LEVEL_MODULE}.gripper", "imports/robot-arm")

    assert (
        import_the_module_of_a_node_type(f"{TOP_LEVEL_MODULE}.wrist", "imports/robot-arm").MADE_BY
        == "robot-arm wrist"
    )
