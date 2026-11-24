# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A picture sent with the region of it that matters, as one bundle on `tatolabd`: a class node
fills a texture from its own output pool and writes it in a `Framed` beside a `Rectangle`, and a
second class node, in an interpreter of its own, reads both off its one `InputPin[Framed]`.

The reader's Texture is the pool slot the writer filled, its size, format and colour kept, and
the live graph answers each port's one bundle pin type, the Texture among its members.
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

FRAMED_READ_TIMEOUT_SECONDS = 120.0

WRITTEN_MARKER = "FRAMED_PICTURE_WRITTEN"

READ_MARKER = "FRAMED_PICTURE_READ"

PYPROJECT = """\
[project]
name = "a-framed-picture"
version = "0.1.0"
requires-python = ">=3.12"
dependencies = ["tatolab-stream"]
"""

NODES_OF_THE_PROJECT = '''\
import json
from dataclasses import dataclass

from tatolab.stream import (
    ColorInfo,
    ImageFrameToWrite,
    InputPin,
    NodeOutputTextureRing,
    OutputPin,
    Rectangle,
    Texture,
    log,
    node,
)

PICTURE_WIDTH = 64
PICTURE_HEIGHT = 32


@dataclass(frozen=True)
class Framed:
    picture: Texture
    region: Rectangle = Rectangle(0.0, 0.0, 1.0, 1.0)


@node.meta(description="Sends one texture of its own pool, with the region of it that matters.")
@node(execution="continuous", interval_ms=10)
class FramesAPicture:
    framed: OutputPin[Framed]

    def setup(self, ctx) -> None:
        self.texture = NodeOutputTextureRing(
            "rgba8_unorm", ["render_attachment", "texture_binding", "copy_src", "copy_dst"], depth=1
        ).next_texture_for_this_frame(ctx.gpu_full_access, PICTURE_WIDTH, PICTURE_HEIGHT)
        self.reported = False

    def process(self, ctx) -> None:
        picture = ImageFrameToWrite.of_texture(
            self.texture, ColorInfo(primaries="bt709", transfer="srgb", matrix="identity", range="full")
        )
        self.framed.write(Framed(picture, Rectangle(0.25, 0.5, 0.5, 0.25)))
        if not self.reported:
            self.reported = True
            log.info("MARKER:FRAMED_PICTURE_WRITTEN " + json.dumps({"surface_id": picture.surface_id}))


@node.meta(description="Reports the first framed picture it reads.")
@node(execution="continuous", interval_ms=10)
class ReportsTheFramedPicture:
    framed: InputPin[Framed]

    def setup(self, ctx) -> None:
        self.reported = False

    def process(self, ctx) -> None:
        framed = self.framed.read()
        if framed is None or self.reported:
            return
        self.reported = True
        picture = framed.picture
        log.info(
            "MARKER:FRAMED_PICTURE_READ "
            + json.dumps(
                {
                    "is_framed": type(framed) is Framed,
                    "surface_id": picture.surface_id,
                    "width": picture.width,
                    "height": picture.height,
                    "format": picture.format,
                    "colour": [
                        picture.colour.primaries,
                        picture.colour.transfer,
                        picture.colour.matrix,
                        picture.colour.range,
                    ],
                    "region": list(framed.region.parts),
                }
            )
        )
'''

FRAMED_BUNDLE = {
    "type": "bundle",
    "name": "Framed",
    "members": [
        {"name": "picture", "member_type": {"type": "texture"}},
        {
            "name": "region",
            "member_type": {"type": "rectangle"},
            "default_value": {"x": 0.0, "y": 0.0, "width": 1.0, "height": 1.0},
        },
    ],
}


def a_node(name: str, node_type: str) -> "dict[str, Any]":
    return {"id": str(uuid.uuid4()), "name": name, "type": node_type, "config": {}}


def a_stream_of_a_framed_picture() -> "dict[str, Any]":
    writer = a_node("frames", "nodes.framed:FramesAPicture")
    reader = a_node("report", "nodes.framed:ReportsTheFramedPicture")
    return {
        "asset": str(uuid.uuid4()),
        "display_name": "Main",
        "nodes": [writer, reader],
        "links": [
            {
                "source": {"node": writer["id"], "port": "framed"},
                "target": {"node": reader["id"], "port": "framed"},
            }
        ],
    }


def test_a_class_node_sends_a_texture_with_its_region_as_one_bundle_on_a_running_stream(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
    tmp_path: Path,
):
    project_directory = tmp_path / "a-framed-picture"
    (project_directory / "streams").mkdir(parents=True)
    (project_directory / "nodes").mkdir()
    write_the_project_file_unless_there(project_directory)
    (project_directory / "pyproject.toml").write_text(PYPROJECT, encoding="utf-8")
    (project_directory / ".python-version").write_text("3.12\n", encoding="utf-8")
    (project_directory / "nodes" / "__init__.py").write_text("", encoding="utf-8")
    (project_directory / "nodes" / "framed.py").write_text(NODES_OF_THE_PROJECT, encoding="utf-8")
    (project_directory / "streams" / "main.graph.json").write_text(
        json.dumps(a_stream_of_a_framed_picture(), indent=2) + "\n",
        encoding="utf-8",
    )

    tatolabd = start_tatolabd_running_stream(TatolabRunOfAProject(working_directory=project_directory))
    ended_load = tatolabd.stream_runs[-1].action_once_ended(timeout=PROJECT_LOAD_TIMEOUT_SECONDS)
    assert ended_load["state"] == "done", ended_load
    local_api = tatolabd.local_api_client()
    [listed] = local_api.list_streams()
    assert listed["failed_because"] is None, listed

    written = tatolabd.await_marker(WRITTEN_MARKER, timeout=FRAMED_READ_TIMEOUT_SECONDS)
    read = tatolabd.await_marker(READ_MARKER, timeout=FRAMED_READ_TIMEOUT_SECONDS)
    graph_nodes = local_api.graph(listed["stream"])["nodes"]
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert read["is_framed"], read
    assert read["surface_id"] == written["surface_id"], (
        "the reader's Texture is the pool slot the writer filled, carried by reference, with no "
        f"copy; the writer sent {written}, the reader read {read}"
    )
    assert (read["width"], read["height"], read["format"]) == (64, 32, "rgba8_unorm"), read
    assert read["colour"] == ["bt709", "srgb", "identity", "full"], read
    assert read["region"] == [0.25, 0.5, 0.5, 0.25], (
        f"the small member rides the same message as the picture; the reader read {read}"
    )
    pin_type_of_each_port = {
        (graph_node["name"], port["name"]): port.get("pin_type")
        for graph_node in graph_nodes
        for port in graph_node["ports"]["inputs"] + graph_node["ports"]["outputs"]
    }
    assert pin_type_of_each_port[("frames", "framed")] == FRAMED_BUNDLE, graph_nodes
    assert pin_type_of_each_port[("report", "framed")] == FRAMED_BUNDLE, graph_nodes
