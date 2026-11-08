# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The scaffold templates `tatolab new` embeds and writes, checked as files.

The tree is the app `new` writes, so it is a real project: its nodes lint and
type-check (pyright runs over `scaffold_template/` as its own execution
environment), and its one stream is the graph document `streams/main.graph.json`.
Rendering it — the project file, the UUIDs the runtime mints into the document, the
project name, the stripped licence header, the overwrite refusal — is the runtime's,
and tested there.
"""

import ast
import json
import subprocess
import sys
from pathlib import Path
from typing import Any

import pytest

from testpattern_module_stand_in import TESTPATTERN_MODULE_NODE_TYPE
from tatolab.stream._cross_floor_check import (
    check_app_directory_for_floor_bindings,
    render_cross_floor_warning_block,
)

SCAFFOLD_TEMPLATE_DIRECTORY = Path(__file__).resolve().parents[1] / "scaffold_template"
SCAFFOLD_TEMPLATE_LICENSE_HEADER = (
    "# Copyright (c) 2025 Jonathan Fontanez\n# SPDX-License-Identifier: BUSL-1.1\n\n"
)
MAIN_GRAPH_DOCUMENT_PATH = "streams/main.graph.json"
EFFECT_MODULE_PATH = "nodes/halftone_lens_effect.py"
METER_MODULE_PATH = "nodes/brightness_meter.py"
# Stored without their dot so no packaging walk skips them as hidden or reads the
# template's `.gitignore` as its own ignore rules; `new` writes them dotted.
SCAFFOLD_TEMPLATE_FILE_NAMES = {
    MAIN_GRAPH_DOCUMENT_PATH,
    "nodes/__init__.py",
    EFFECT_MODULE_PATH,
    METER_MODULE_PATH,
    "pyproject.toml",
    "python-version",
    "gitignore",
}
SCAFFOLD_TEMPLATE_PYTHON_FILE_NAMES = sorted(
    file_name for file_name in SCAFFOLD_TEMPLATE_FILE_NAMES if file_name.endswith(".py")
)
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


@pytest.mark.parametrize("template_file", SCAFFOLD_TEMPLATE_PYTHON_FILE_NAMES)
def test_every_python_template_opens_with_the_licence_header_new_strips(template_file: str):
    assert template_text(template_file).startswith(SCAFFOLD_TEMPLATE_LICENSE_HEADER)


@pytest.mark.parametrize("ruff_arguments", [("check",), ("format", "--check")])
@pytest.mark.parametrize("template_file", SCAFFOLD_TEMPLATE_PYTHON_FILE_NAMES)
def test_every_python_template_passes_ruff(template_file: str, ruff_arguments: "tuple[str, ...]"):
    """Checked in place, so ruff reads this package's `[tool.ruff]`, rooted at the template."""
    finished = subprocess.run(
        [
            sys.executable,
            "-m",
            "ruff",
            *ruff_arguments,
            "--no-cache",
            str(SCAFFOLD_TEMPLATE_DIRECTORY / template_file),
        ],
        capture_output=True,
        text=True,
        check=False,
    )

    assert finished.returncode == 0, (
        f"ruff {' '.join(ruff_arguments)} rejects {template_file}:\n"
        f"{finished.stdout}{finished.stderr}"
    )


@pytest.mark.parametrize("template_file", SCAFFOLD_TEMPLATE_PYTHON_FILE_NAMES)
def test_no_python_template_suppresses_a_lint(template_file: str):
    """The app `new` writes is the shape an author copies, so it lints clean as
    written — no import shadows a builtin under a `noqa`."""
    assert "noqa" not in template_text(template_file)


@pytest.mark.parametrize("template_file", SCAFFOLD_TEMPLATE_PYTHON_FILE_NAMES)
def test_every_python_template_is_valid_python_that_explains_itself(template_file: str):
    """The scaffold is the first code the user reads, `__init__.py` included.

    An empty package init parses but teaches nothing, and this is where a reader
    first meets the rule of one importable module per node.
    """
    assert ast.get_docstring(ast.parse(template_text(template_file))), (
        f"{template_file} carries no module docstring — every file `new` writes "
        f"explains what it is for"
    )


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


def test_the_stream_runs_the_test_pattern_through_the_effect_with_a_meter_on_a_fan_out():
    """The sample runs on a machine with no capture device and no display: it
    needs no camera, the runtime opens no windows, and nothing leaves the stream."""
    assert node_types_in_the_main_graph_document() == [
        TESTPATTERN_MODULE_NODE_TYPE,
        "nodes.halftone_lens_effect:HalftoneLensEffect",
        "nodes.brightness_meter:BrightnessMeter",
    ]
    assert links_in_the_main_graph_document() == [
        ("testpatternsource", "video", "halftonelenseffect", "video_from_upstream"),
        ("halftonelenseffect", "video_to_downstream", "brightnessmeter", "video_from_upstream"),
    ]
    assert not any(
        "DisplayWindow" in node_type or "Camera" in node_type
        for node_type in node_types_in_the_main_graph_document()
    )


@pytest.mark.parametrize(
    ("module_path", "class_name"),
    [(EFFECT_MODULE_PATH, "HalftoneLensEffect"), (METER_MODULE_PATH, "BrightnessMeter")],
)
def test_each_project_node_the_document_names_is_a_node_class_of_its_module(
    module_path: str, class_name: str
):
    """The child interpreter that runs a node imports its class by the `module:Class` the
    document names."""
    module_name = module_path.removesuffix(".py").replace("/", ".")
    node_source = template_text(module_path)

    assert f"{module_name}:{class_name}" in node_types_in_the_main_graph_document()
    assert "@node" in node_source
    assert f"class {class_name}" in node_source


@pytest.mark.parametrize("module_path", [EFFECT_MODULE_PATH, METER_MODULE_PATH])
def test_each_node_declares_its_ports_as_family_class_attributes(module_path: str):
    """The scaffold is the shape an author copies, so its ports are `Input` and
    `Output` class attributes of the Image family and no decorator port."""
    node_source = template_text(module_path)
    port_declarations = [
        ast.unparse(statement.value)
        for class_definition in ast.walk(ast.parse(node_source))
        if isinstance(class_definition, ast.ClassDef)
        for statement in class_definition.body
        if isinstance(statement, ast.Assign)
        and isinstance(statement.value, ast.Call)
        and ast.unparse(statement.value.func) in ("Input", "Output")
    ]

    assert "@node.input" not in node_source and "@node.output" not in node_source
    assert port_declarations, f"{module_path} declares no family port"
    assert all(declaration.split("(")[1].startswith("Image") for declaration in port_declarations)


def test_the_scaffold_models_pixels_on_the_gpu_and_logic_on_the_cpu():
    """The effect in the video path runs on the GPU, and the numpy node reads an
    explicit CPU view off the effect's output, so the slow door never sits in
    the video path."""
    effect_source = template_text(EFFECT_MODULE_PATH)
    meter_source = template_text(METER_MODULE_PATH)

    assert "create_compute_kernel(" in effect_source
    assert "numpy" not in effect_source, "the effect in the video path touches no host pixels"
    assert "frame.cpu()" in meter_source, "the meter's host view of the pixels is explicit"
    assert "ctx.time" in meter_source, "the meter paces itself on the monotonic clock"
    readers_of_the_effect_output = [
        (target_node, target_port)
        for source_node, source_port, target_node, target_port in (
            links_in_the_main_graph_document()
        )
        if (source_node, source_port) == ("halftonelenseffect", "video_to_downstream")
    ]
    assert readers_of_the_effect_output == [("brightnessmeter", "video_from_upstream")], (
        "the meter reads the effect's output"
    )


def test_the_scaffold_pins_the_python_version_the_plan_names():
    assert template_text("python-version").strip() == "3.12"


def test_the_scaffold_depends_on_tatolab_stream_the_testpattern_module_and_numpy_only():
    """`new` raises the `tatolab-stream` and `tatolab-testpattern` floors to
    the version of the runtime that writes it; the template's own floors keep PyPI's 0.0.0 placeholder out of
    a resolve of the tree."""
    assert (
        'dependencies = ["tatolab-stream>=0.54.2", '
        '"tatolab-testpattern>=0.68.18", "numpy>=2.1"]\n'
        in template_text("pyproject.toml")
    ), "the pixel effect needs no GPU package of the user's own, and no runtime enters"


def test_the_scaffold_takes_tatolab_stream_from_pypi_and_names_no_other_index():
    """A uv index or source here would resolve `tatolab-stream` from somewhere other
    than PyPI, where the release publishes it."""
    assert "tool.uv" not in template_text("pyproject.toml")


def test_the_scaffold_binds_to_no_floor():
    report = check_app_directory_for_floor_bindings(SCAFFOLD_TEMPLATE_DIRECTORY)

    assert report.findings == [], render_cross_floor_warning_block(
        report, SCAFFOLD_TEMPLATE_DIRECTORY
    )
