# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A frozen dataclass on a function node under bare `@node` is one bundle pin on `tatolabd`: a
node takes a `Tone` and a `list` of `Dot`s set as input values, the `Tone` leaving out a member
with a default, and gives a `Tone` back, which a second node reads and hands out as one number.

The live graph answers each bundle port's members, with name, type and meaning, in declared
order.
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

#: The project's first load makes its venv.
PROJECT_LOAD_TIMEOUT_SECONDS = 900.0

TONE_READ_TIMEOUT_SECONDS = 120.0

TONE_READ_MARKER = "BUNDLE_TONE_READ"

PYPROJECT = """\
[project]
name = "a-bundle"
version = "0.1.0"
requires-python = ">=3.12"
dependencies = ["tatolab-stream", "numpy"]
"""

NODES_OF_THE_PROJECT = '''\
import json
from dataclasses import dataclass
from typing import Annotated

import numpy
from tatolab.stream import Channels, Input, PinMeaning, log, node


@dataclass(frozen=True)
class Tone:
    frequency: Annotated[float, PinMeaning("frequency")]
    gain: float = 1.0


@dataclass(frozen=True)
class Dot:
    x: float
    y: float = 0.0


@node.meta(description="A tone an octave up, its gain raised by one, nudged by each dot's x.")
@node
def Louder(tone: Tone = Tone(440.0), dots: list[Dot] = []) -> Tone:
    return Tone(tone.frequency * 2 + sum(dot.x for dot in dots), tone.gain + 1)


@node.meta(description="A tone as one number: its frequency plus a thousand times its gain.")
@node
def ToneAsOneNumber(tone: Tone = Tone(0.0)) -> float:
    return tone.frequency + 1000 * tone.gain


@node.meta(description="Reports the number it reads, once.")
@node.process(execution="continuous", interval_ms=20)
class ReportsTheToneItReads:
    tone = Input(Channels, delivery="newest")

    def __init__(self) -> None:
        self.reported = False

    def process(self, ctx) -> None:
        block = ctx.inputs.read(self.tone)
        if block is not None and not self.reported:
            self.reported = True
            log.info("MARKER:BUNDLE_TONE_READ " + json.dumps({"tone": float(numpy.from_dlpack(block)[0][0])}))
'''


TONE_BUNDLE = {
    "type": "bundle",
    "members": [
        {"name": "frequency", "member_type": {"type": "float"}, "meaning": "frequency"},
        {"name": "gain", "member_type": {"type": "float"}, "default_value": 1.0},
    ],
}

DOT_BUNDLE = {
    "type": "bundle",
    "members": [
        {"name": "x", "member_type": {"type": "float"}},
        {"name": "y", "member_type": {"type": "float"}, "default_value": 0.0},
    ],
}


def a_node(name: str, node_type: str, input_values: "dict[str, Any] | None" = None) -> "dict[str, Any]":
    placed: "dict[str, Any]" = {"id": str(uuid.uuid4()), "name": name, "type": node_type, "config": {}}
    if input_values is not None:
        placed["input_values"] = input_values
    return placed


def a_stream_of_a_tone_through_two_bundle_nodes() -> "dict[str, Any]":
    nodes = [
        a_node(
            "louder",
            "nodes.tones:Louder",
            {"tone": {"frequency": 220.0}, "dots": [{"x": 3.0}, {"x": 0.5, "y": 9.0}]},
        ),
        a_node("as_one_number", "nodes.tones:ToneAsOneNumber"),
        a_node("report", "nodes.tones:ReportsTheToneItReads"),
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
            link("louder", "result", "as_one_number", "tone"),
            link("as_one_number", "result", "report", "tone"),
        ],
    }


def test_a_function_node_takes_and_gives_a_dataclass_as_one_bundle_pin_on_a_running_stream(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
    tmp_path: Path,
):
    project_directory = tmp_path / "a-bundle"
    (project_directory / "streams").mkdir(parents=True)
    (project_directory / "nodes").mkdir()
    write_the_project_file_unless_there(project_directory)
    (project_directory / "pyproject.toml").write_text(PYPROJECT, encoding="utf-8")
    (project_directory / ".python-version").write_text("3.12\n", encoding="utf-8")
    (project_directory / "nodes" / "__init__.py").write_text("", encoding="utf-8")
    (project_directory / "nodes" / "tones.py").write_text(NODES_OF_THE_PROJECT, encoding="utf-8")
    (project_directory / "streams" / "main.graph.json").write_text(
        json.dumps(a_stream_of_a_tone_through_two_bundle_nodes(), indent=2) + "\n",
        encoding="utf-8",
    )

    tatolabd = start_tatolabd_running_stream(TatolabRunOfAProject(working_directory=project_directory))
    ended_load = tatolabd.stream_runs[-1].action_once_ended(timeout=PROJECT_LOAD_TIMEOUT_SECONDS)
    assert ended_load["state"] == "done", ended_load
    local_api = tatolabd.local_api_client()
    [listed] = local_api.list_streams()
    assert listed["failed_because"] is None, listed

    read = tatolabd.await_marker(TONE_READ_MARKER, timeout=TONE_READ_TIMEOUT_SECONDS)
    [louder] = [node for node in local_api.graph(listed["stream"])["nodes"] if node["name"] == "louder"]
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert read["tone"] == pytest.approx((220.0 * 2 + 3.0 + 0.5) + 1000 * (1.0 + 1)), (
        "Louder is handed the Tone set on it, its gain left out and read as its default, and "
        f"the Dots, and the Tone it gives reaches ToneAsOneNumber whole; the class read {read}"
    )
    pin_type_of_each_port = {
        port["name"]: port.get("pin_type")
        for port in louder["ports"]["inputs"] + louder["ports"]["outputs"]
    }
    assert pin_type_of_each_port["tone"] == pin_type_of_each_port["result"] == TONE_BUNDLE, louder
    assert pin_type_of_each_port["dots"] == {"type": "list_of", "element_type": DOT_BUNDLE}, louder
