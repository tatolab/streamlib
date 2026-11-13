# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The Core Nodes project on `tatolabd`: its streams start, and each picture node keeps running;
and a project that holds no node of its own places Core Nodes' nodes and its Program view from
the copy the runtime unit carries.

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
from stream_runs_on_tatolabd import (
    TatolabRunOfAProject,
    write_the_project_file_unless_there,
)

pytestmark = pytest.mark.requires_gpu

CORE_NODES_PROJECT_DIRECTORY = (
    Path(__file__).resolve().parent.parent.parent / "packages" / "tatolab-core-nodes"
)

#: What a copy of the project leaves behind: what a run of it writes, and its own tests.
NOT_COPIED_WITH_THE_PROJECT = shutil.ignore_patterns(
    ".venv", ".tatolab", "__pycache__", "tests"
)

EVERY_SHADER_NODE_STREAM_NAME = "every_shader_node"

PROGRAM_VIEW_TYPE = "Core Nodes:program"

#: The project's first load makes its venv, which installs Numba.
PROJECT_LOAD_TIMEOUT_SECONDS = 900.0

FRAMES_WRITTEN_TIMEOUT_SECONDS = 120.0

GRAPH_POLL_INTERVAL_SECONDS = 0.5

#: More than a start alone makes: a picture node that keeps running, not one still frame.
EVALUATIONS_SHOWING_A_STREAM_KEEPS_RUNNING = 30

#: A channel carries at most 32 destinations; each canvas feeds fewer.
LINKS_OUT_OF_ONE_CANVAS = 16


def shader_nodes_the_project_declares() -> "dict[str, list[dict[str, Any]]]":
    """Each shader node type of the project with the inputs of its signature, in order."""
    signature_inputs_by_node_type: "dict[str, list[dict[str, Any]]]" = {}
    for module_path in sorted(
        (CORE_NODES_PROJECT_DIRECTORY / "core_nodes").glob("*.py")
    ):
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
                signature_inputs_by_node_type[
                    f"core_nodes.{module_path.stem}:{name}"
                ] = signature["inputs"]
    return signature_inputs_by_node_type


def a_stream_of_canvases_into_one_of_each_shader_node() -> "dict[str, Any]":
    nodes: "list[dict[str, Any]]" = []
    links: "list[dict[str, Any]]" = []
    canvas_id = ""
    links_out_of_the_canvas = LINKS_OUT_OF_ONE_CANVAS
    for node_type, signature_inputs in sorted(
        shader_nodes_the_project_declares().items()
    ):
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
                    "name": f"canvas_before_{node_type.rsplit(':', 1)[1].lower()}",
                    "type": "core_nodes.solid_color:SolidColor",
                    "config": {"width": 64, "height": 36},
                }
            )
        node_id = str(uuid.uuid4())
        nodes.append(
            {
                "id": node_id,
                "name": node_type.rsplit(":", 1)[1].lower(),
                "type": node_type,
                "config": {},
            }
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
        CORE_NODES_PROJECT_DIRECTORY,
        project_directory,
        ignore=NOT_COPIED_WITH_THE_PROJECT,
    )
    (
        project_directory / "streams" / f"{EVERY_SHADER_NODE_STREAM_NAME}.graph.json"
    ).write_text(
        json.dumps(
            a_stream_of_canvases_into_one_of_each_shader_node(),
            indent=2,
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )
    return project_directory


def names_of_the_nodes_feeding_the_program_view(
    stream_document: "dict[str, Any]",
) -> "set[str]":
    node_by_id = {node["id"]: node for node in stream_document["nodes"]}
    return {
        node_by_id[link["source"]["node"]]["name"]
        for link in stream_document["links"]
        if node_by_id[link["target"]["node"]]["type"] == PROGRAM_VIEW_TYPE
    }


def evaluations_of_each_node(graph: "dict[str, Any]") -> "dict[str, int]":
    """By node name: how many times a shader node's island was submitted, and for any other
    node the most it wrote on one output. A shader node nothing reads writes no counted
    message, so its submissions stand for it."""
    evaluations: "dict[str, int]" = {}
    for node in graph["nodes"]:
        components = node.get("components", {})
        fused_group = components.get("fused_group")
        if isinstance(fused_group, dict):
            evaluations[node["name"]] = fused_group.get("submissions", 0)
        else:
            written = components.get("metrics", {}).get(
                "written_bags_by_output_port", {}
            )
            evaluations[node["name"]] = max(written.values(), default=0)
    return evaluations


def await_evaluations_of(
    tatolabd: TatolabdUnderTest, stream_id: str, node_names: "set[str]", at_least: int
) -> None:
    """Poll the live graph until each of `node_names` has evaluated `at_least` times."""
    local_api = tatolabd.local_api_client()
    deadline = time.monotonic() + FRAMES_WRITTEN_TIMEOUT_SECONDS
    while True:
        evaluations = evaluations_of_each_node(local_api.graph(stream=stream_id))
        short_of_it = {
            node_name: evaluations.get(node_name)
            for node_name in sorted(node_names)
            if (evaluations.get(node_name) or 0) < at_least
        }
        if not short_of_it:
            return
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"these evaluated fewer than {at_least} times within "
                f"{FRAMES_WRITTEN_TIMEOUT_SECONDS}s: {short_of_it}\n{tatolabd.recent_stderr()}"
            )
        time.sleep(GRAPH_POLL_INTERVAL_SECONDS)


def test_each_stream_of_the_core_nodes_project_and_each_shader_node_keeps_running(
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
    ended_load = tatolabd.stream_runs[-1].action_once_ended(
        timeout=PROJECT_LOAD_TIMEOUT_SECONDS
    )
    assert ended_load["state"] == "done", ended_load

    listed_by_display_name = {
        listed["display_name"]: listed
        for listed in tatolabd.local_api_client().list_streams()
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
        await_evaluations_of(
            tatolabd,
            listed_by_display_name[stream_document["display_name"]]["stream"],
            node_names_awaited,
            at_least=EVALUATIONS_SHOWING_A_STREAM_KEEPS_RUNNING,
        )

    stderr_text = tatolabd.stderr_text
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    assert "shader node wrote no frame" not in stderr_text, stderr_text


#: The folder under `imports/` the runtime links the Core Nodes project it carries at.
CORE_NODES_IMPORT_FOLDER = "imports/core-nodes"

PYPROJECT_OF_A_PROJECT_WITH_NO_NODE_OF_ITS_OWN = """\
[project]
name = "a-show"
version = "0.1.0"
requires-python = ">=3.12"
dependencies = ["tatolab-stream"]
"""


def a_stream_of_core_nodes_placed_from_the_runtimes_copy() -> "dict[str, Any]":
    """A canvas tinted by an amount an `Lfo` moves, into the Program view: a process node, a
    shader node, a function node and a view, each named as an import's."""
    canvas, tint, lfo, program = (str(uuid.uuid4()) for _ in range(4))
    return {
        "asset": str(uuid.uuid4()),
        "display_name": "Main",
        "nodes": [
            {
                "id": canvas,
                "name": "canvas",
                "type": f"{CORE_NODES_IMPORT_FOLDER}/core_nodes.solid_color:SolidColor",
                "config": {"width": 64, "height": 36},
            },
            {
                "id": lfo,
                "name": "lfo",
                "type": f"{CORE_NODES_IMPORT_FOLDER}/core_nodes.animation:Lfo",
                "config": {},
            },
            {
                "id": tint,
                "name": "tint",
                "type": f"{CORE_NODES_IMPORT_FOLDER}/core_nodes.color:Tint",
                "config": {},
            },
            {"id": program, "name": "program", "type": PROGRAM_VIEW_TYPE, "config": {}},
        ],
        "links": [
            {
                "source": {"node": canvas, "port": "image"},
                "target": {"node": tint, "port": "image"},
            },
            {
                "source": {"node": lfo, "port": "result"},
                "target": {"node": tint, "port": "amount"},
            },
            {
                "source": {"node": tint, "port": "result"},
                "target": {"node": program, "port": "video"},
            },
        ],
    }


def test_a_project_with_no_node_of_its_own_places_core_nodes_from_the_runtimes_copy(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
    tmp_path: Path,
):
    project_directory = tmp_path / "a-show"
    (project_directory / "streams").mkdir(parents=True)
    project_file_as_written = write_the_project_file_unless_there(project_directory)
    (project_directory / "pyproject.toml").write_text(
        PYPROJECT_OF_A_PROJECT_WITH_NO_NODE_OF_ITS_OWN, encoding="utf-8"
    )
    (project_directory / ".python-version").write_text("3.12\n", encoding="utf-8")
    (project_directory / "streams" / "main.graph.json").write_text(
        json.dumps(a_stream_of_core_nodes_placed_from_the_runtimes_copy(), indent=2)
        + "\n",
        encoding="utf-8",
    )

    tatolabd = start_tatolabd_running_stream(
        TatolabRunOfAProject(working_directory=project_directory)
    )
    ended_load = tatolabd.stream_runs[-1].action_once_ended(
        timeout=PROJECT_LOAD_TIMEOUT_SECONDS
    )
    assert ended_load["state"] == "done", ended_load

    core_nodes_link = project_directory / CORE_NODES_IMPORT_FOLDER
    assert core_nodes_link.is_symlink(), (
        "the runtime links its copy, and copies nothing in"
    )
    assert (core_nodes_link / "tatolab-project.json").is_file()
    project_file_now = json.loads(
        (project_directory / "tatolab-project.json").read_text(encoding="utf-8")
    )
    assert (
        project_file_now["dependencies"]
        == project_file_as_written["dependencies"]
        == []
    )

    local_api = tatolabd.local_api_client()
    [listed] = local_api.list_streams()
    assert listed["failed_because"] is None, listed
    node_types = local_api.call_tool(
        "list_project_node_types", {"project_directory": str(project_directory)}
    )
    assert f"{CORE_NODES_IMPORT_FOLDER}/core_nodes.color:Tint" in json.dumps(node_types)
    views = local_api.call_tool(
        "list_project_views", {"project_directory": str(project_directory)}
    )
    assert PROGRAM_VIEW_TYPE in json.dumps(views)

    await_evaluations_of(
        tatolabd,
        listed["stream"],
        {"tint"},
        at_least=EVALUATIONS_SHOWING_A_STREAM_KEEPS_RUNNING,
    )

    stderr_text = tatolabd.stderr_text
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    assert "shader node wrote no frame" not in stderr_text, stderr_text
