# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Core Nodes' combine and separate nodes on `tatolabd`: three number wires through
`CombineColor` drive a Color input, whose components `SeparateColor` hands out as Floats, and
two through `CombineVector` drive a Vector input of two components.

The project places the nodes from the copy of Core Nodes the runtime unit carries, beside two
nodes of its own: a function node taking a Vector of two, and a class that reports what it reads.
"""

import json
import uuid
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from conftest import TatolabdUnderTest
from stream_runs_on_tatolabd import (
    TatolabRunOfAProject,
    write_the_project_file_unless_there,
)

pytestmark = pytest.mark.requires_gpu

CORE_NODES_IMPORT_FOLDER = "imports/core-nodes"

#: The project's first load makes its venv.
PROJECT_LOAD_TIMEOUT_SECONDS = 900.0

LEVELS_READ_TIMEOUT_SECONDS = 120.0

LEVELS_READ_MARKER = "COMBINED_LEVELS_READ"

PYPROJECT = """\
[project]
name = "a-mix"
version = "0.1.0"
requires-python = ">=3.12"
dependencies = ["tatolab-stream", "numpy"]
"""

NODES_OF_THE_PROJECT = '''\
import json

import numpy
from tatolab.stream import Channels, Input, Vector2, log, node


@node.meta(description="A Vector of two as one number: x plus ten times y.")
@node
def SpotAsOneNumber(spot: Vector2 = Vector2(0.0, 0.0)) -> float:
    return spot.x + 10 * spot.y


@node.meta(description="Reports the green and the spot it reads, once both have arrived.")
@node.process
class ReportsTheLevelsItReads:
    green = Input(Channels, delivery="newest")
    spot = Input(Channels, delivery="newest")

    def __init__(self) -> None:
        self.latest = {}
        self.reported = False

    def process(self, ctx) -> None:
        for port in ("green", "spot"):
            block = ctx.inputs.read(port)
            if block is not None:
                self.latest[port] = float(numpy.from_dlpack(block)[0][0])
        if not self.reported and len(self.latest) == 2:
            self.reported = True
            log.info("MARKER:COMBINED_LEVELS_READ " + json.dumps(self.latest))
'''


def a_node(name: str, node_type: str, input_values: "dict[str, Any] | None" = None) -> "dict[str, Any]":
    placed: "dict[str, Any]" = {"id": str(uuid.uuid4()), "name": name, "type": node_type, "config": {}}
    if input_values is not None:
        placed["input_values"] = input_values
    return placed


def a_core_node(name: str, module_and_name: str, **input_values: float) -> "dict[str, Any]":
    return a_node(name, f"{CORE_NODES_IMPORT_FOLDER}/core_nodes.{module_and_name}", input_values or None)


def a_stream_of_numbers_through_the_combine_nodes() -> "dict[str, Any]":
    nodes = [
        a_core_node("red", "math_nodes:Constant", value=0.2),
        a_core_node("green", "math_nodes:Constant", value=0.4),
        a_core_node("blue", "math_nodes:Constant", value=0.6),
        a_core_node("combine_color", "combine_and_separate:CombineColor"),
        a_core_node("separate_color", "combine_and_separate:SeparateColor"),
        a_core_node("x", "math_nodes:Constant", value=0.25),
        a_core_node("y", "math_nodes:Constant", value=0.5),
        a_core_node("combine_vector", "combine_and_separate:CombineVector", z=9.0, w=9.0),
        a_node("spot", "nodes.levels:SpotAsOneNumber"),
        a_node("report", "nodes.levels:ReportsTheLevelsItReads"),
    ]
    id_of = {placed["name"]: placed["id"] for placed in nodes}

    def link(source: str, source_port: str, target: str, target_port: str) -> "dict[str, Any]":
        return {
            "source": {"node": id_of[source], "port": source_port},
            "target": {"node": id_of[target], "port": target_port},
        }

    return {
        "asset": str(uuid.uuid4()),
        "display_name": "Main",
        "nodes": nodes,
        "links": [
            link("red", "result", "combine_color", "red"),
            link("green", "result", "combine_color", "green"),
            link("blue", "result", "combine_color", "blue"),
            link("combine_color", "result", "separate_color", "color"),
            link("separate_color", "green", "report", "green"),
            link("x", "result", "combine_vector", "x"),
            link("y", "result", "combine_vector", "y"),
            link("combine_vector", "result", "spot", "spot"),
            link("spot", "result", "report", "spot"),
        ],
    }


def test_numbers_wired_through_the_combine_nodes_drive_a_color_and_a_vector_of_two(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
    tmp_path: Path,
):
    project_directory = tmp_path / "a-mix"
    (project_directory / "streams").mkdir(parents=True)
    (project_directory / "nodes").mkdir()
    write_the_project_file_unless_there(project_directory)
    (project_directory / "pyproject.toml").write_text(PYPROJECT, encoding="utf-8")
    (project_directory / ".python-version").write_text("3.12\n", encoding="utf-8")
    (project_directory / "nodes" / "__init__.py").write_text("", encoding="utf-8")
    (project_directory / "nodes" / "levels.py").write_text(NODES_OF_THE_PROJECT, encoding="utf-8")
    (project_directory / "streams" / "main.graph.json").write_text(
        json.dumps(a_stream_of_numbers_through_the_combine_nodes(), indent=2) + "\n",
        encoding="utf-8",
    )

    tatolabd = start_tatolabd_running_stream(TatolabRunOfAProject(working_directory=project_directory))
    ended_load = tatolabd.stream_runs[-1].action_once_ended(timeout=PROJECT_LOAD_TIMEOUT_SECONDS)
    assert ended_load["state"] == "done", ended_load
    [listed] = tatolabd.local_api_client().list_streams()
    assert listed["failed_because"] is None, listed

    levels = tatolabd.await_marker(LEVELS_READ_MARKER, timeout=LEVELS_READ_TIMEOUT_SECONDS)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert levels["green"] == pytest.approx(0.4), (
        f"the green wired into CombineColor comes out of SeparateColor, and the class read {levels}"
    )
    assert levels["spot"] == pytest.approx(0.25 + 10 * 0.5), (
        f"CombineVector's x and y reach a Vector of two, its z and w dropped, and the class read {levels}"
    )
