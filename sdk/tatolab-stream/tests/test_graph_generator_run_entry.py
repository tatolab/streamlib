# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

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
from tatolab.stream import GraphInput, GraphOutput, Packets, StreamBuilder, graph_generator


@graph_generator
def look(stream_builder: StreamBuilder, port: str = "video") -> None:
    """A pass-through: one Input wired to one Output."""
    source = stream_builder.add(GraphInput, config={"port": port, "family": Packets})
    sink = stream_builder.add(GraphOutput, config={"port": port, "family": Packets})
    stream_builder.connect(source.output("value"), sink.input("value"))


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
            "config": {"port": "camera"},
            "fragment": {
                "nodes": [
                    {"name": "graphinput", "type": "tatolab.stream:GraphInput",
                     "config": {"port": "camera", "family": "packets"}},
                    {"name": "graphoutput", "type": "tatolab.stream:GraphOutput",
                     "config": {"port": "camera", "family": "packets"}},
                ],
                "links": [
                    {"source": {"node": "graphinput", "port": "value"},
                     "target": {"node": "graphoutput", "port": "value"}},
                ],
            },
        },
    }


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
            "`generators.looks` defines: `generators.looks:look`, "
            "`generators.looks:places_itself`, `generators.looks:raises`",
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
