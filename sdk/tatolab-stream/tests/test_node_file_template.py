# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The node file template `create_node_file` writes, held to the current `tatolab.stream` idiom.

The runtime renders it by dropping the licence header and renaming `NewNode` in its function
statement after the file; the render's own refusals are the runtime's, and tested there.
"""

import importlib.util
import sys
from pathlib import Path
from typing import Any

import pytest

from tatolab.stream._project_node_declaration_check_entry import (
    check_the_node_declarations_of_a_project,
)
from tatolab.stream._project_node_type_listing_entry import (
    list_the_node_types_and_graph_generators_of_a_project,
)
from tatolab.stream.description_lint import lint_the_descriptions_of_a_node_type

NODE_FILE_TEMPLATE_PATH = Path(__file__).resolve().parents[1] / "node_file_template" / "new_node.py"
TEMPLATE_LICENCE_HEADER = "# Copyright (c) 2025 Tatolab LLC\n# SPDX-License-Identifier: GPL-3.0-only\n\n"
TEMPLATE_FUNCTION_STATEMENT = "def NewNode[Frame: Texture]("


def a_project_holding_the_template_rendered_at_nodes_edge_glow(project_directory: Path) -> Path:
    template = NODE_FILE_TEMPLATE_PATH.read_text(encoding="utf-8")
    assert template.startswith(TEMPLATE_LICENCE_HEADER)
    rendered = template.removeprefix(TEMPLATE_LICENCE_HEADER).replace(
        TEMPLATE_FUNCTION_STATEMENT, "def EdgeGlow[Frame: Texture]("
    )
    node_file = project_directory / "nodes" / "edge_glow.py"
    node_file.parent.mkdir(parents=True)
    node_file.write_text(rendered, encoding="utf-8")
    return node_file


def test_the_rendered_template_is_listed_as_a_node_type_of_the_project(tmp_path: Path) -> None:
    a_project_holding_the_template_rendered_at_nodes_edge_glow(tmp_path)

    listing = list_the_node_types_and_graph_generators_of_a_project(
        tmp_path, [], installed_distribution_search_path=[]
    )

    (project_source,) = [source for source in listing["sources"] if source["source"] == "project"]
    assert project_source["unreadable_modules"] == []
    (node_type,) = project_source["node_types"]
    assert node_type["type"] == "nodes.edge_glow:EdgeGlow"
    assert node_type["kind"] == "process"
    assert node_type["description"]
    assert [port["name"] for port in node_type["inputs"]] == ["picture_in", "every_nth"]
    assert [port["name"] for port in node_type["outputs"]] == ["picture_out"]


def test_the_rendered_template_imports_as_a_declared_node_with_every_description(tmp_path: Path) -> None:
    node_file = a_project_holding_the_template_rendered_at_nodes_edge_glow(tmp_path)
    module_spec = importlib.util.spec_from_file_location("nodes.edge_glow", node_file)
    assert module_spec is not None and module_spec.loader is not None
    module = importlib.util.module_from_spec(module_spec)
    module_spec.loader.exec_module(module)

    assert lint_the_descriptions_of_a_node_type(module.EdgeGlow) == []


def test_the_rendered_template_passes_the_declaration_check_with_no_finding(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The check imports the module with the project first on `sys.path`, as the runtime starts it."""
    a_project_holding_the_template_rendered_at_nodes_edge_glow(tmp_path)
    monkeypatch.syspath_prepend(str(tmp_path))
    for module in ("nodes", "nodes.edge_glow"):
        monkeypatch.delitem(sys.modules, module, raising=False)
    lines: list[dict[str, Any]] = []

    check_the_node_declarations_of_a_project(tmp_path, ["nodes/edge_glow.py"], lines.append)

    (modules_line, module_line) = lines
    assert modules_line["modules"] == [{"module": "nodes.edge_glow", "path": "nodes/edge_glow.py"}]
    assert module_line["findings"] == []
