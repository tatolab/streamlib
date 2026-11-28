# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The entry the runtime runs one graph generator through, in the project's interpreter.

Each test runs it as the runtime does: `python -I -m` in a child, the project directory as
the working directory, reading the one JSON document on stdout. Nothing here needs a
runtime.
"""

import json
import subprocess
import sys
from pathlib import Path
from typing import Any

import pytest

RUN_ENTRY_MODULE = "tatolab.stream._graph_generator_run_entry"
# Bounded: a run imports a few modules and runs one function.
RUN_ENTRY_TIMEOUT_SECONDS = 60.0

LOOK_GENERATOR_SOURCE = '''
from typing import Annotated

from tatolab.stream import Description, GraphInput, GraphOutput, StreamBuilder, graph_generator


@graph_generator
def look(stream_builder: StreamBuilder, port: str = "video") -> None:
    """A pass-through: one Input wired to one Output."""
    source = stream_builder.add(GraphInput, input_values={"port": port})
    sink = stream_builder.add(GraphOutput, input_values={"port": port})
    stream_builder.connect(source.output("value"), sink.input("value"))


@graph_generator
def tuned(
    stream_builder: StreamBuilder,
    passes: Annotated[int, Description("How many times the picture is blurred.")] = 3,
    label="glow",
) -> None:
    stream_builder.add(GraphInput, input_values={"port": label})


@graph_generator
def clashing(stream_builder: StreamBuilder, video: int = 1) -> None:
    stream_builder.add(GraphInput, input_values={"port": "video"})


@graph_generator
def places_itself(stream_builder: StreamBuilder) -> None:
    stream_builder.add(places_itself)


@graph_generator
def raises(stream_builder: StreamBuilder) -> None:
    raise RuntimeError("the rig list is empty")


def not_a_generator(stream_builder: StreamBuilder) -> None:
    pass
'''


def run_entry(project_directory: Path, *arguments: str) -> dict[str, Any]:
    finished = subprocess.run(
        [sys.executable, "-I", "-m", RUN_ENTRY_MODULE, *arguments],
        cwd=project_directory,
        capture_output=True,
        text=True,
        timeout=RUN_ENTRY_TIMEOUT_SECONDS,
        check=False,
    )
    assert finished.returncode == 0, finished.stderr
    return json.loads(finished.stdout)


@pytest.fixture
def project_directory(tmp_path: Path) -> Path:
    generators_directory = tmp_path / "generators"
    generators_directory.mkdir()
    (generators_directory / "__init__.py").write_text("")
    (generators_directory / "looks.py").write_text(LOOK_GENERATOR_SOURCE)
    return tmp_path


def test_a_generator_run_answers_the_fragment_node_it_is_stored_as(
    project_directory: Path,
) -> None:
    document = run_entry(
        project_directory,
        "generators.looks:look",
        "--name",
        "Front Look",
        "--settings",
        json.dumps({"port": "camera"}),
    )

    assert document == {
        "generator": "generators.looks:look",
        "placed": {
            "name": "front-look",
            "type": "generators.looks:look",
            "input_values": {"port": "camera"},
            "fragment": {
                "nodes": [
                    {"name": "graphinput", "type": "tatolab.stream:GraphInput",
                     "input_values": {"port": "camera"}},
                    {"name": "graphoutput", "type": "tatolab.stream:GraphOutput",
                     "input_values": {"port": "camera"}},
                ],
                "links": [
                    {"source": {"node": "graphinput", "port": "value"},
                     "target": {"node": "graphoutput", "port": "value"}},
                ],
                "settings": [
                    {"name": "port", "description": "",
                     "pin_declaration": {"pin_type": {"kind": "is", "pin_type": {"type": "string"}},
                                         "flow": "value"},
                     "input_mode": {"mode": "setting_only"},
                     "pin_default": "video"},
                ],
            },
        },
    }


def test_each_typed_setting_a_generator_promotes_is_declared_with_its_description(
    project_directory: Path,
) -> None:
    document = run_entry(project_directory, "generators.looks:tuned")

    assert document["placed"]["input_values"] == {}
    assert document["placed"]["fragment"]["settings"] == [
        {"name": "passes", "description": "How many times the picture is blurred.",
         "pin_declaration": {"pin_type": {"kind": "is", "pin_type": {"type": "integer"}},
                             "flow": "value"},
         "input_mode": {"mode": "setting_only"},
         "pin_default": 3},
    ], "an untyped setting, `label`, is declared as no input"


def test_a_fragment_given_no_name_takes_its_generators(project_directory: Path) -> None:
    assert run_entry(project_directory, "generators.looks:look")["placed"]["name"] == "look"


@pytest.mark.parametrize(
    ("arguments", "failure_names"),
    [
        (("generators.looks:places_itself",), "places itself: places_itself → places_itself"),
        (("generators.looks:raises",), "RuntimeError: the rig list is empty"),
        (
            ("generators.looks:not_a_generator",),
            "`generators.looks:not_a_generator` is not a @graph_generator function. Module "
            "`generators.looks` defines: `generators.looks:clashing`, `generators.looks:look`, "
            "`generators.looks:places_itself`, `generators.looks:raises`",
        ),
        (
            ("generators.looks:clashing",),
            "promotes a setting `video` and its fragment declares an Input `video`",
        ),
        (("generators.absent:look",), "ModuleNotFoundError: No module named 'generators.absent'"),
        (("generators.looks:look", "--settings", '{"radius": 3}'), "it promotes no `radius`"),
        (("generators.looks:look", "--settings", "[]"), "promoted settings are one JSON object"),
        (("look",), "named by its import path, `<module>:<function>`"),
    ],
)
def test_a_generator_that_places_no_fragment_answers_why(
    project_directory: Path, arguments: tuple[str, ...], failure_names: str
) -> None:
    document = run_entry(project_directory, *arguments)

    assert set(document) == {"generator", "failed_because"}
    assert failure_names in document["failed_because"]


def test_a_generator_failure_starts_at_the_projects_own_frame(project_directory: Path) -> None:
    failed_because = run_entry(project_directory, "generators.looks:raises")["failed_because"]

    frame_lines = [
        line for line in failed_because.splitlines() if line.lstrip().startswith('File "')
    ]
    assert frame_lines
    assert all("tatolab/stream/" not in frame_line for frame_line in frame_lines), failed_because


def test_what_the_generator_prints_never_reaches_the_document(project_directory: Path) -> None:
    (project_directory / "generators" / "loud.py").write_text(
        "from tatolab.stream import StreamBuilder, graph_generator\n"
        "print('imported')\n"
        "@graph_generator\n"
        "def loud(stream_builder: StreamBuilder) -> None:\n"
        "    print('ran')\n"
    )

    assert run_entry(project_directory, "generators.loud:loud")["placed"]["fragment"] == {
        "nodes": [],
        "links": [],
    }
