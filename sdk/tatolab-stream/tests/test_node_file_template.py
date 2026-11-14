# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The node file template `create_node_file` writes, held to the current `tatolab.stream` idiom.

The runtime renders it by dropping the licence header and renaming `class NewNode:` after the
file; the render's own refusals are the runtime's, and tested there.
"""

import importlib.util
from pathlib import Path

from tatolab.stream._project_node_type_listing_entry import (
    list_the_node_types_and_graph_generators_of_a_project,
)
from tatolab.stream.description_lint import lint_the_descriptions_of_a_node_type

NODE_FILE_TEMPLATE_PATH = Path(__file__).resolve().parents[1] / "node_file_template" / "new_node.py"
TEMPLATE_LICENCE_HEADER = "# Copyright (c) 2025 Tatolab LLC\n# SPDX-License-Identifier: GPL-3.0-only\n\n"
TEMPLATE_CLASS_STATEMENT = "class NewNode:"


def a_project_holding_the_template_rendered_at_nodes_edge_glow(project_directory: Path) -> Path:
    template = NODE_FILE_TEMPLATE_PATH.read_text(encoding="utf-8")
    assert template.startswith(TEMPLATE_LICENCE_HEADER)
    rendered = template.removeprefix(TEMPLATE_LICENCE_HEADER).replace(
        TEMPLATE_CLASS_STATEMENT, "class EdgeGlow:"
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
    assert [(port["name"], port["family"]) for port in node_type["inputs"]] == [("video_in", "image")]
    assert [(port["name"], port["family"]) for port in node_type["outputs"]] == [("video_out", "image")]


def test_the_rendered_template_imports_as_a_declared_node_with_every_description(tmp_path: Path) -> None:
    node_file = a_project_holding_the_template_rendered_at_nodes_edge_glow(tmp_path)
    module_spec = importlib.util.spec_from_file_location("nodes.edge_glow", node_file)
    assert module_spec is not None and module_spec.loader is not None
    module = importlib.util.module_from_spec(module_spec)
    module_spec.loader.exec_module(module)

    assert lint_the_descriptions_of_a_node_type(module.EdgeGlow) == []
