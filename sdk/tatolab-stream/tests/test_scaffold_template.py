# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The scaffold templates `tatolab new` embeds and writes, checked as files.

The tree is the camera variant of the app, so it is a real project: it lints,
type-checks (pyright runs over `scaffold_template/` as its own execution
environment) and compiles with no runtime. Rendering it — the test-pattern
variant, the project name, the stripped licence header, the overwrite refusal —
is the native CLI's, and tested there.
"""

import ast
import json
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from tatolab.stream._cross_floor_check import (
    check_app_directory_for_floor_bindings,
    render_cross_floor_warning_block,
)

SCAFFOLD_TEMPLATE_DIRECTORY = Path(__file__).resolve().parents[1] / "scaffold_template"
SCAFFOLD_TEMPLATE_LICENSE_HEADER = (
    "# Copyright (c) 2025 Jonathan Fontanez\n# SPDX-License-Identifier: BUSL-1.1\n\n"
)
EFFECT_MODULE_PATH = "nodes/inverting_effect.py"
METER_MODULE_PATH = "nodes/brightness_meter.py"
# Stored without their dot so no packaging walk skips them as hidden or reads the
# template's `.gitignore` as its own ignore rules; `new` writes them dotted.
SCAFFOLD_TEMPLATE_FILE_NAMES = {
    "stream.py",
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
    "stream.py": [
        "A StreamLib stream: camera →",
        "from tatolab.stream import CameraSource, DisplayWindow, StreamBuilder, stream",
        '\"\"\"Camera, inverted,',
        "stream_builder.add(CameraSource)",
    ],
    "pyproject.toml": ['name = "streamlib-app"'],
}
COMPILE_ENTRY_TIMEOUT_SECONDS = 60.0


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
    first meets the rule that keeps node classes out of the entry file.
    """
    assert ast.get_docstring(ast.parse(template_text(template_file))), (
        f"{template_file} carries no module docstring — every file `new` writes "
        f"explains what it is for"
    )


def test_the_stream_declares_one_stream_named_main():
    """`dev` with no argument compiles the sole `@stream` in `stream.py`, so the
    scaffold declares exactly one, bare."""
    entry_source = template_text("stream.py")
    stream_functions = [
        node.name
        for node in ast.parse(entry_source).body
        if isinstance(node, ast.FunctionDef)
        and [ast.unparse(decorator) for decorator in node.decorator_list] == ["stream"]
    ]

    assert stream_functions == ["main"]
    assert "CameraSource" in entry_source
    assert "DisplayWindow" in entry_source


@pytest.mark.parametrize(
    ("module_path", "class_name"),
    [(EFFECT_MODULE_PATH, "InvertingEffect"), (METER_MODULE_PATH, "BrightnessMeter")],
)
def test_each_node_lives_outside_the_entry_file(module_path: str, class_name: str):
    """A node class in the entry file identifies as `__main__:<Type>`, which is a
    wiring error — the entry runs as `__main__`, and the child interpreter that
    runs the node imports its class by name.
    """
    entry_source = template_text("stream.py")
    node_source = template_text(module_path)
    module_name = module_path.removesuffix(".py").replace("/", ".")

    assert "@node" not in entry_source
    assert f"class {class_name}" not in entry_source
    assert "@node" in node_source, "the class belongs in the importable module"
    assert f"class {class_name}" in node_source
    assert f"from {module_name} import {class_name}" in entry_source, (
        "the entry file imports the class it wires"
    )


def test_the_scaffold_models_pixels_on_the_gpu_and_logic_on_the_cpu():
    """The effect in the video path runs on the GPU, and the numpy node reads an
    explicit CPU view off a fan-out, so the slow door never sits between the
    camera and the window."""
    entry_source = template_text("stream.py")
    effect_source = template_text(EFFECT_MODULE_PATH)
    meter_source = template_text(METER_MODULE_PATH)

    assert "GlslPixelEffect.compile(" in effect_source
    assert "numpy" not in effect_source, "the effect in the video path touches no host pixels"
    assert "frame.cpu()" in meter_source, "the meter's host view of the pixels is explicit"
    assert "ctx.time" in meter_source, "the meter paces itself on the monotonic clock"
    readers_of_the_effect_output = sorted(
        ast.unparse(call.args[1])
        for call in ast.walk(ast.parse(entry_source))
        if isinstance(call, ast.Call)
        and ast.unparse(call.func) == "stream_builder.connect"
        and ast.unparse(call.args[0]) == "effect.output('video_to_downstream')"
    )
    assert readers_of_the_effect_output == [
        "meter.input('video_from_upstream')",
        "window.input('video')",
    ], "the meter reads a fan-out of the effect's output, off the window's path"


def test_the_scaffold_pins_the_python_version_the_plan_names():
    assert template_text("python-version").strip() == "3.12"


def test_the_scaffold_depends_on_tatolab_stream_and_numpy_only():
    assert 'dependencies = ["tatolab-stream", "numpy>=2.1"]\n' in template_text(
        "pyproject.toml"
    ), "the pixel effect needs no GPU package of the user's own, and no runtime enters"


def test_the_scaffold_sources_tatolab_stream_from_its_own_index():
    manifest = template_text("pyproject.toml")

    assert (
        '[[tool.uv.index]]\nname = "tatolab"\n'
        'url = "https://tatolab.github.io/streamlib/simple/"\nexplicit = true\n'
    ) in manifest
    assert manifest.endswith('[tool.uv.sources]\ntatolab-stream = { index = "tatolab" }\n'), (
        "uv reads an explicit index only for a dependency sourced from it"
    )


def test_the_scaffold_compiles_to_its_graph_with_no_runtime(tmp_path: Path):
    """The compile `tatolab run` makes, over the template tree as `new` lays it out."""
    app_directory = tmp_path.resolve() / "demo"
    shutil.copytree(SCAFFOLD_TEMPLATE_DIRECTORY, app_directory)

    finished = subprocess.run(
        [sys.executable, "-m", "tatolab.stream._project_stream_compile_entry", "--verb", "dev"],
        cwd=app_directory,
        capture_output=True,
        text=True,
        timeout=COMPILE_ENTRY_TIMEOUT_SECONDS,
        check=False,
    )

    assert finished.returncode == 0, finished.stdout + finished.stderr
    if sys.version_info >= (3, 11):
        assert finished.stderr == "", "the scaffold binds to no floor, so the compile says nothing"
    else:
        assert "the cross-floor check found nothing" in finished.stderr, finished.stderr
    assert json.loads(finished.stdout) == {
        "stream_graph": {
            "stream": "main",
            "nodes": [
                {"name": "camerasource", "type": "tatolab.stream:CameraSource", "config": {}},
                {
                    "name": "invertingeffect",
                    "type": "nodes.inverting_effect:InvertingEffect",
                    "config": {},
                },
                {
                    "name": "brightnessmeter",
                    "type": "nodes.brightness_meter:BrightnessMeter",
                    "config": {},
                },
                {
                    "name": "displaywindow",
                    "type": "tatolab.stream:DisplayWindow",
                    "config": {"title": "StreamLib", "scaling": "fit"},
                },
            ],
            "links": [
                {
                    "source": {"node": "camerasource", "port": "video"},
                    "target": {"node": "invertingeffect", "port": "video_from_upstream"},
                },
                {
                    "source": {"node": "invertingeffect", "port": "video_to_downstream"},
                    "target": {"node": "displaywindow", "port": "video"},
                },
                {
                    "source": {"node": "invertingeffect", "port": "video_to_downstream"},
                    "target": {"node": "brightnessmeter", "port": "video_from_upstream"},
                },
            ],
            "exposed": [{"node": "invertingeffect", "port": "video_to_downstream"}],
        },
        "project_directory": str(app_directory),
    }


def test_the_scaffold_binds_to_no_floor():
    report = check_app_directory_for_floor_bindings(SCAFFOLD_TEMPLATE_DIRECTORY)

    assert report.findings == [], render_cross_floor_warning_block(
        report, SCAFFOLD_TEMPLATE_DIRECTORY
    )
