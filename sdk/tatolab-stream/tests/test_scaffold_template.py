# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The scaffold templates `tatolab new` embeds and writes, checked as files.

The tree is the app `new` writes, so it is a real project: it lints,
type-checks (pyright runs over `scaffold_template/` as its own execution
environment) and compiles with no runtime. Rendering it — the project name, the
stripped licence header, the overwrite refusal — is the native CLI's, and tested
there.
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
EFFECT_MODULE_PATH = "nodes/halftone_lens_effect.py"
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
    "pyproject.toml": ['name = "tatolab-stream-project"'],
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
    """`dev` loads every `@stream` in `stream.py` as its own stream, and the
    scaffold starts a project with exactly one, bare."""
    entry_source = template_text("stream.py")
    stream_functions = [
        node.name
        for node in ast.parse(entry_source).body
        if isinstance(node, ast.FunctionDef)
        and [ast.unparse(decorator) for decorator in node.decorator_list] == ["stream"]
    ]

    assert stream_functions == ["main"]


def stream_builder_calls_in_the_entry(method_name: str) -> "list[str]":
    return [
        ast.unparse(call)
        for call in ast.walk(ast.parse(template_text("stream.py")))
        if isinstance(call, ast.Call) and ast.unparse(call.func) == f"stream_builder.{method_name}"
    ]


def test_the_stream_encodes_the_test_pattern_through_the_effect_and_exposes_it_publicly():
    """The sample runs on a machine with no capture device and no display: it
    needs no camera, the runtime opens no windows, and a browser plays it."""
    entry_source = template_text("stream.py")

    assert stream_builder_calls_in_the_entry("add") == [
        "stream_builder.add(TestPatternSource)",
        "stream_builder.add(HalftoneLensEffect)",
        "stream_builder.add(BrightnessMeter)",
        "stream_builder.add(H264Encoder)",
    ]
    assert stream_builder_calls_in_the_entry("connect") == [
        "stream_builder.connect(source.output('video'), effect.input('video_from_upstream'))",
        "stream_builder.connect(effect.output('video_to_downstream'), "
        "meter.input('video_from_upstream'))",
        "stream_builder.connect(effect.output('video_to_downstream'), encoder.input('video'))",
    ]
    assert stream_builder_calls_in_the_entry("expose") == [
        "stream_builder.expose(encoder.output('encoded_video'), Exposure.PUBLIC)"
    ]
    assert "DisplayWindow" not in entry_source


def test_the_camera_is_named_only_as_the_one_line_swap_in_a_comment():
    """No camera runs in the sample, by default or behind a flag; the comment
    tells the reader which line points it at theirs."""
    entry_source = template_text("stream.py")
    names_in_the_code = {
        node.id for node in ast.walk(ast.parse(entry_source)) if isinstance(node, ast.Name)
    }
    comment_lines = [
        line.strip() for line in entry_source.splitlines() if line.strip().startswith("#")
    ]

    assert "CameraSource" not in names_in_the_code
    assert any("stream_builder.add(CameraSource)" in line for line in comment_lines), (
        entry_source
    )


@pytest.mark.parametrize(
    ("module_path", "class_name"),
    [(EFFECT_MODULE_PATH, "HalftoneLensEffect"), (METER_MODULE_PATH, "BrightnessMeter")],
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
    explicit CPU view off a fan-out, so the slow door never sits on the path to
    the encoder."""
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
        "encoder.input('video')",
        "meter.input('video_from_upstream')",
    ], "the meter reads a fan-out of the effect's output, beside the encoder"


def test_the_scaffold_pins_the_python_version_the_plan_names():
    assert template_text("python-version").strip() == "3.12"


def test_the_scaffold_depends_on_tatolab_stream_and_numpy_only():
    assert 'dependencies = ["tatolab-stream>=0.54.2", "numpy>=2.1"]\n' in template_text(
        "pyproject.toml"
    ), (
        "the pixel effect needs no GPU package of the user's own, no runtime enters, and "
        "the floor keeps PyPI's 0.0.0 tatolab-stream placeholder out of the resolve"
    )


def test_the_scaffold_takes_tatolab_stream_from_pypi_and_names_no_other_index():
    """A uv index or source here would resolve `tatolab-stream` from somewhere other
    than PyPI, where the release publishes it."""
    assert "tool.uv" not in template_text("pyproject.toml")


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
    document = json.loads(finished.stdout)
    assert set(document) == {"project_directory", "streams"}, document
    assert document["project_directory"] == str(app_directory)
    (stream,) = document["streams"]
    assert stream["stream_function"] == "stream:main"
    assert stream["description"], "the scaffold's stream carries its docstring"
    assert stream["stream_graph"] == {
            "stream": "main",
            "nodes": [
                {
                    "name": "testpatternsource",
                    "type": "tatolab.stream:TestPatternSource",
                    "config": {},
                },
                {
                    "name": "halftonelenseffect",
                    "type": "nodes.halftone_lens_effect:HalftoneLensEffect",
                    "config": {},
                },
                {
                    "name": "brightnessmeter",
                    "type": "nodes.brightness_meter:BrightnessMeter",
                    "config": {},
                },
                {
                    "name": "h264encoder",
                    "type": "tatolab.stream:H264Encoder",
                    "config": {},
                },
            ],
            "links": [
                {
                    "source": {"node": "testpatternsource", "port": "video"},
                    "target": {"node": "halftonelenseffect", "port": "video_from_upstream"},
                },
                {
                    "source": {"node": "halftonelenseffect", "port": "video_to_downstream"},
                    "target": {"node": "brightnessmeter", "port": "video_from_upstream"},
                },
                {
                    "source": {"node": "halftonelenseffect", "port": "video_to_downstream"},
                    "target": {"node": "h264encoder", "port": "video"},
                },
            ],
            "exposed": [{"node": "h264encoder", "port": "encoded_video", "level": "public"}],
    }


def test_the_scaffold_binds_to_no_floor():
    report = check_app_directory_for_floor_bindings(SCAFFOLD_TEMPLATE_DIRECTORY)

    assert report.findings == [], render_cross_floor_warning_block(
        report, SCAFFOLD_TEMPLATE_DIRECTORY
    )
