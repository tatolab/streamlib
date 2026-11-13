# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The Core Nodes project on `tatolabd`: its streams start, and each picture node writes frames.

The project is loaded from a copy, since the runtime writes a project's
`pyproject.toml` and `.venv`. Beside the streams it brings, the copy is given
one more: canvases into every image input of one of each shader node, so every
shader compiles, binds and dispatches with no stream kept in the project for it.
"""

import importlib.util
import json
import shutil
import time
import uuid
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from conftest import TatolabdUnderTest
from stream_runs_on_tatolabd import TatolabRunOfAProject

pytestmark = pytest.mark.requires_gpu

CORE_NODES_PROJECT_DIRECTORY = (
    Path(__file__).resolve().parent.parent.parent / "packages" / "tatolab-core-nodes"
)

#: What a copy of the project leaves behind: what a run of it writes, and its own tests.
NOT_COPIED_WITH_THE_PROJECT = shutil.ignore_patterns(".venv", ".tatolab", "__pycache__", "tests")

EVERY_SHADER_NODE_STREAM_NAME = "every_shader_node"

PROGRAM_VIEW_TYPE = "Core Nodes:program"

#: The project's first load makes its venv, which installs Numba.
PROJECT_LOAD_TIMEOUT_SECONDS = 900.0

FRAMES_WRITTEN_TIMEOUT_SECONDS = 120.0

GRAPH_POLL_INTERVAL_SECONDS = 0.5

#: A channel carries at most 32 destinations; each canvas feeds fewer.
LINKS_OUT_OF_ONE_CANVAS = 16


def shader_nodes_the_project_declares() -> "dict[str, list[dict[str, Any]]]":
    """Each shader node type of the project with the inputs of its signature, in order."""
    signature_inputs_by_node_type: "dict[str, list[dict[str, Any]]]" = {}
    for module_path in sorted((CORE_NODES_PROJECT_DIRECTORY / "nodes").glob("*.py")):
        if "@node.shader" not in module_path.read_text(encoding="utf-8"):
            continue
        spec = importlib.util.spec_from_file_location(
            f"core_nodes_project_{module_path.stem}", module_path
        )
        assert spec is not None and spec.loader is not None, module_path
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        for name, member in vars(module).items():
            signature = getattr(member, "__tatolab_node_shader_signature__", None)
            if signature is not None and member.__module__ == module.__name__:
                signature_inputs_by_node_type[f"nodes.{module_path.stem}:{name}"] = signature[
                    "inputs"
                ]
    return signature_inputs_by_node_type


def a_stream_of_canvases_into_one_of_each_shader_node() -> "dict[str, Any]":
    nodes: "list[dict[str, Any]]" = []
    links: "list[dict[str, Any]]" = []
    canvas_id = ""
    links_out_of_the_canvas = LINKS_OUT_OF_ONE_CANVAS
    for node_type, signature_inputs in sorted(shader_nodes_the_project_declares().items()):
        image_ports = [
            signature_input["port"]
            for signature_input in signature_inputs
            if signature_input["value_type"] == "image_frame"
        ]
        if links_out_of_the_canvas + len(image_ports) > LINKS_OUT_OF_ONE_CANVAS:
            canvas_id = str(uuid.uuid4())
            links_out_of_the_canvas = 0
            nodes.append(
                {
                    "id": canvas_id,
                    "name": f"canvas_before_{node_type.rsplit(':', 1)[1]}",
                    "type": "nodes.solid_color:SolidColor",
                    "config": {"width": 64, "height": 36},
                }
            )
        node_id = str(uuid.uuid4())
        nodes.append(
            {"id": node_id, "name": node_type.rsplit(":", 1)[1], "type": node_type, "config": {}}
        )
        links.extend(
            {
                "source": {"node": canvas_id, "port": "image"},
                "target": {"node": node_id, "port": image_port},
            }
            for image_port in image_ports
        )
        links_out_of_the_canvas += len(image_ports)
    return {
        "asset": str(uuid.uuid4()),
        "display_name": "Every shader node",
        "nodes": nodes,
        "links": links,
    }


def a_copy_of_the_core_nodes_project(directory: Path) -> Path:
    """The project copied under `directory`, with the stream of every shader node added."""
    project_directory = directory / "tatolab-core-nodes"
    shutil.copytree(
        CORE_NODES_PROJECT_DIRECTORY, project_directory, ignore=NOT_COPIED_WITH_THE_PROJECT
    )
    (project_directory / "streams" / f"{EVERY_SHADER_NODE_STREAM_NAME}.graph.json").write_text(
        json.dumps(a_stream_of_canvases_into_one_of_each_shader_node(), indent=2, sort_keys=True)
        + "\n",
        encoding="utf-8",
    )
    return project_directory


def names_of_the_nodes_feeding_the_program_view(stream_document: "dict[str, Any]") -> "set[str]":
    node_by_id = {node["id"]: node for node in stream_document["nodes"]}
    return {
        node_by_id[link["source"]["node"]]["name"]
        for link in stream_document["links"]
        if node_by_id[link["target"]["node"]]["type"] == PROGRAM_VIEW_TYPE
    }


def await_frames_written_by(
    tatolabd: TatolabdUnderTest, stream_id: str, node_names: "set[str]"
) -> None:
    """Poll the live graph until each of `node_names` has written on one of its outputs."""
    local_api = tatolabd.local_api_client()
    deadline = time.monotonic() + FRAMES_WRITTEN_TIMEOUT_SECONDS
    while True:
        written_by_node_name = {
            node["name"]: node.get("components", {})
            .get("metrics", {})
            .get("written_bags_by_output_port", {})
            for node in local_api.graph(stream=stream_id)["nodes"]
        }
        yet_to_write = sorted(
            node_name
            for node_name in node_names
            if not any(written > 0 for written in written_by_node_name.get(node_name, {}).values())
        )
        if not yet_to_write:
            return
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"{yet_to_write} wrote nothing within {FRAMES_WRITTEN_TIMEOUT_SECONDS}s:\n"
                f"{tatolabd.recent_stderr()}"
            )
        time.sleep(GRAPH_POLL_INTERVAL_SECONDS)


def test_each_stream_of_the_core_nodes_project_starts_and_each_shader_node_writes_frames(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
    tmp_path: Path,
):
    project_directory = a_copy_of_the_core_nodes_project(tmp_path)
    stream_documents = {
        stream_document_path.name.removesuffix(".graph.json"): json.loads(
            stream_document_path.read_text(encoding="utf-8")
        )
        for stream_document_path in (project_directory / "streams").glob("*.graph.json")
    }
    tatolabd = start_tatolabd_running_stream(
        TatolabRunOfAProject(working_directory=project_directory)
    )
    ended_load = tatolabd.stream_runs[-1].action_once_ended(timeout=PROJECT_LOAD_TIMEOUT_SECONDS)
    assert ended_load["state"] == "done", ended_load

    listed_by_display_name = {
        listed["display_name"]: listed for listed in tatolabd.local_api_client().list_streams()
    }
    assert listed_by_display_name.keys() == {
        stream_document["display_name"] for stream_document in stream_documents.values()
    }
    why_each_failed_stream_failed = {
        display_name: listed["failed_because"]
        for display_name, listed in listed_by_display_name.items()
        if listed["failed_because"] is not None
    }
    assert why_each_failed_stream_failed == {}

    for stream_name, stream_document in stream_documents.items():
        node_names_awaited = (
            {node["name"] for node in stream_document["nodes"]}
            if stream_name == EVERY_SHADER_NODE_STREAM_NAME
            else names_of_the_nodes_feeding_the_program_view(stream_document)
        )
        assert node_names_awaited, stream_name
        await_frames_written_by(
            tatolabd,
            listed_by_display_name[stream_document["display_name"]]["stream"],
            node_names_awaited,
        )

    stderr_text = tatolabd.stderr_text
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    assert "shader node wrote no frame" not in stderr_text, stderr_text
