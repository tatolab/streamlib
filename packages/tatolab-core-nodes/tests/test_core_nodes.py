# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Every core node declares a group and a description, its Slang takes what its signature declares, and each stream names only nodes and ports that exist."""

from __future__ import annotations

import importlib
import json
import re
from pathlib import Path
from typing import Any

import pytest

PROJECT_DIRECTORY = Path(__file__).resolve().parent.parent

NODE_MODULES = (
    "animation",
    "color",
    "composite",
    "filters",
    "generators",
    "image_file",
    "logic",
    "math_nodes",
    "oscillator",
    "solid_color",
    "transform",
)

GROUPS_THE_ADD_NODE_SEARCH_LISTS = {
    "Sources",
    "Generators",
    "Color",
    "Filters",
    "Transform",
    "Composite",
    "Math",
    "Animation",
    "Logic",
    "Audio",
}

PROGRAM_VIEW_TYPE = "Core Nodes:program"


def every_declared_node() -> dict[str, Any]:
    declared: dict[str, Any] = {}
    for module_name in NODE_MODULES:
        module = importlib.import_module(f"core_nodes.{module_name}")
        for name, member in vars(module).items():
            if (
                getattr(member, "__tatolab_node_declared__", False)
                and member.__module__ == module.__name__
            ):
                declared[f"core_nodes.{module_name}:{name}"] = member
    return declared


DECLARED_NODES = every_declared_node()


def test_the_project_declares_every_node_it_is_meant_to():
    assert len(DECLARED_NODES) == 75


@pytest.mark.parametrize("node_type", sorted(DECLARED_NODES))
def test_each_node_declares_a_listed_group_and_a_description(node_type: str):
    declared = DECLARED_NODES[node_type]
    assert declared.__tatolab_node_category__ in GROUPS_THE_ADD_NODE_SEARCH_LISTS
    assert declared.__tatolab_node_description__.strip().endswith(".")


SHADER_NODES = {
    node_type: declared
    for node_type, declared in DECLARED_NODES.items()
    if hasattr(declared, "__tatolab_node_shader_source__")
}


@pytest.mark.parametrize("node_type", sorted(SHADER_NODES))
def test_each_shader_takes_in_slang_what_its_signature_declares_in_the_same_order(node_type: str):
    declared = SHADER_NODES[node_type]
    signature = declared.__tatolab_node_shader_signature__
    slang_parameters = re.search(
        rf"\bfloat4\s+{signature['function_name']}\s*\(([^)]*)\)",
        declared.__tatolab_node_shader_source__,
    )
    assert slang_parameters is not None
    in_slang = [
        tuple(parameter.split()) for parameter in slang_parameters.group(1).split(",")
    ]
    slang_type_of = {"image_frame": "ImageInput", "float": "float"}
    assert in_slang == [
        (slang_type_of[each["value_type"]], each["port"]) for each in signature["inputs"]
    ]


@pytest.mark.parametrize("node_type", sorted(SHADER_NODES))
def test_each_shader_helper_is_named_for_its_node(node_type: str):
    declared = SHADER_NODES[node_type]
    function_name = declared.__tatolab_node_shader_signature__["function_name"]
    defined = re.findall(
        r"^(?:float[234]?|void)\s+(\w+)\s*\(", declared.__tatolab_node_shader_source__, re.MULTILINE
    )
    assert function_name in defined
    assert all(
        name == function_name or name.startswith(f"{function_name}_") for name in defined
    ), "two nodes fused into one kernel must never define the same helper"


def ports_of(declared: Any, side: str) -> set[str]:
    return {port["name"] for port in getattr(declared, f"__tatolab_node_{side}_ports__")}


def program_view_ports(side: str) -> set[str]:
    manifest = json.loads((PROJECT_DIRECTORY / "views/program/tatolab_view.json").read_text())
    return {port["name"] for port in manifest.get(side, [])}


@pytest.mark.parametrize(
    "stream_document",
    sorted((PROJECT_DIRECTORY / "streams").glob("*.graph.json")),
    ids=lambda path: path.name,
)
def test_each_stream_names_only_nodes_and_ports_the_project_brings(stream_document: Path):
    graph = json.loads(stream_document.read_text())
    assert stream_document.read_text() == json.dumps(graph, indent=2, sort_keys=True) + "\n"
    type_by_node_id = {node["id"]: node["type"] for node in graph["nodes"]}
    assert len(type_by_node_id) == len(graph["nodes"])

    def ports(node_type: str, side: str) -> set[str]:
        if node_type == PROGRAM_VIEW_TYPE:
            return program_view_ports(f"{side}s")
        return ports_of(DECLARED_NODES[node_type], side)

    for node in graph["nodes"]:
        assert node["type"] == PROGRAM_VIEW_TYPE or node["type"] in DECLARED_NODES
        assert set(node.get("pins", {})) <= ports(node["type"], "input")
    inputs_linked: set[tuple[str, str]] = set()
    for link in graph["links"]:
        source, target = link["source"], link["target"]
        assert source["port"] in ports(type_by_node_id[source["node"]], "output")
        assert target["port"] in ports(type_by_node_id[target["node"]], "input")
        assert (target["node"], target["port"]) not in inputs_linked
        inputs_linked.add((target["node"], target["port"]))
