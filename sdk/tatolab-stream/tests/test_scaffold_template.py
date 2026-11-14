# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""The scaffold templates `tatolab new` embeds and writes, checked as files.

The tree is the project `new` writes: one stream asset, the graph document
`streams/main.graph.json`, holding the one source a new project starts with, and
no node code of its own. Rendering it — the project file, the UUIDs the runtime
mints into the document, the project name, the overwrite refusal — is the
runtime's, and tested there.
"""

import json
from pathlib import Path
from typing import Any

import pytest

from testpattern_module_stand_in import TESTPATTERN_MODULE_NODE_TYPE
from tatolab.stream._cross_floor_check import (
    check_app_directory_for_floor_bindings,
    render_cross_floor_warning_block,
)

SCAFFOLD_TEMPLATE_DIRECTORY = Path(__file__).resolve().parents[1] / "scaffold_template"
MAIN_GRAPH_DOCUMENT_PATH = "streams/main.graph.json"
# Stored without their dot so no packaging walk skips them as hidden or reads the
# template's `.gitignore` as its own ignore rules; `new` writes them dotted.
SCAFFOLD_TEMPLATE_FILE_NAMES = {
    MAIN_GRAPH_DOCUMENT_PATH,
    "pyproject.toml",
    "python-version",
    "gitignore",
}
# The text `new` replaces in each template; a template that loses one renders wrong.
RENDER_PLACEHOLDERS_BY_TEMPLATE_FILE = {
    "pyproject.toml": ['name = "tatolab-stream-project"'],
}


def template_text(file_name: str) -> str:
    return (SCAFFOLD_TEMPLATE_DIRECTORY / file_name).read_text(encoding="utf-8")


def test_every_scaffold_template_file_is_one_new_writes():
    """A template file outside the list `new` writes would never reach an app."""
    template_files = {
        path.relative_to(SCAFFOLD_TEMPLATE_DIRECTORY).as_posix()
        for path in SCAFFOLD_TEMPLATE_DIRECTORY.rglob("*")
        if path.is_file() and "__pycache__" not in path.parts
    }

    assert template_files == SCAFFOLD_TEMPLATE_FILE_NAMES


@pytest.mark.parametrize("template_file", sorted(RENDER_PLACEHOLDERS_BY_TEMPLATE_FILE))
def test_every_render_placeholder_is_in_its_template(template_file: str):
    contents = template_text(template_file)

    for placeholder in RENDER_PLACEHOLDERS_BY_TEMPLATE_FILE[template_file]:
        assert placeholder in contents, f"{template_file} lost its placeholder {placeholder!r}"


def the_main_graph_document() -> "dict[str, Any]":
    return json.loads(template_text(MAIN_GRAPH_DOCUMENT_PATH))


def node_types_in_the_main_graph_document() -> "list[str]":
    return [node["type"] for node in the_main_graph_document()["nodes"]]


def links_in_the_main_graph_document() -> "list[tuple[str, str, str, str]]":
    return [
        (
            link["source"]["node"],
            link["source"]["port"],
            link["target"]["node"],
            link["target"]["port"],
        )
        for link in the_main_graph_document()["links"]
    ]


def test_the_graph_document_holds_only_nodes_and_links_for_the_runtime_to_mint_uuids_into():
    """`new` gives the stream asset and each node a fresh UUID, so the template holds none."""
    document = the_main_graph_document()

    assert set(document) == {"nodes", "links"}, document
    assert all(set(node) == {"name", "type", "config"} for node in document["nodes"]), document


def test_the_stream_is_one_test_pattern_source_and_nothing_else():
    """A new project starts nearly empty: one source, seen in Tato Studio's viewer,
    needing no camera, no display and no node code of the project's own."""
    assert node_types_in_the_main_graph_document() == [TESTPATTERN_MODULE_NODE_TYPE]
    assert links_in_the_main_graph_document() == []


def test_the_scaffold_pins_the_python_version_the_plan_names():
    assert template_text("python-version").strip() == "3.12"


def test_the_scaffold_depends_on_tatolab_stream_alone():
    """`new` raises the `tatolab-stream` floor to the version of the runtime
    that writes it; the template's own floor keeps PyPI's 0.0.0 placeholder out
    of a resolve of the tree. The core first-party modules, `tatolab-testpattern`
    among them, ride the runtime, which declares them when it makes the
    project's `.venv`."""
    assert 'dependencies = ["tatolab-stream>=0.54.2"]\n' in template_text("pyproject.toml")


def test_the_scaffold_binds_to_no_floor():
    report = check_app_directory_for_floor_bindings(SCAFFOLD_TEMPLATE_DIRECTORY)

    assert report.findings == [], render_cross_floor_warning_block(
        report, SCAFFOLD_TEMPLATE_DIRECTORY
    )
