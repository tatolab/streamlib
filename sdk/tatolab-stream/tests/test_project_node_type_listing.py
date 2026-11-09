# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A project's node types and graph generators, listed from source without running its code.

The corpus is a plain project, one with a linked dependency, one with an installed
distribution that declares `tatolab.nodes`, and one with a syntax error. Every module in it
raises at import, so a listing that ran any of them would fail.
"""

import json
import subprocess
import sys
from pathlib import Path
from typing import Any

from tatolab.stream._project_node_type_listing_entry import (
    list_the_node_types_and_graph_generators_of_a_project,
)

LISTING_ENTRY_MODULE = "tatolab.stream._project_node_type_listing_entry"
# Bounded: the entry parses a handful of small files.
LISTING_ENTRY_TIMEOUT_SECONDS = 60.0

RAISES_AT_IMPORT = 'raise RuntimeError("this module ran")\n'

PROJECT_NODE_SOURCE = (
    '''
from dataclasses import dataclass

from tatolab.stream import Image, Input, Output, State, Stateful, Time, node


@node.process(execution="continuous", category="Robot")
class InverseKinematics:
    """Solves joint angles for a target pose."""

    target = Input(Image, delivery="newest", description="The target pose.")
    Joint_Angles = Output(family_named("joints"))

    def __init__(self) -> None:
        pass


@dataclass(frozen=True)
class Split:
    low: float
    high: float | None


@dataclass(frozen=True)
class Phase:
    phase: float = 0.0


@node.function(description="Splits a value at a threshold.")
def split(x: float, threshold: float = 0.5, config: "SplitConfig" = None, now: Time = 0) -> Split:
    return Split(x, None)


@node.function
def ramp(rate: float, previous: State[Phase]) -> Stateful[float, Phase]:
    """A ramp at `rate`."""
    return previous.phase, previous


@node.shader("float4 tint(ImageInput image, float amount) { return image.pixel * amount; }")
def tint(image: ImageFrame, amount: float = 1.0) -> ImageFrame: ...


def not_a_node(x: float) -> float:
    return x
'''
    + RAISES_AT_IMPORT
)

PROJECT_GENERATOR_SOURCE = (
    '''
import tatolab.stream as ts


@ts.graph_generator
def arm_rig(stream_builder, joints: int = 6, *, label: str = "arm", gain=compute_gain()) -> None:
    """Six joints and a solver."""
'''
    + RAISES_AT_IMPORT
)

DEPENDENCY_NODE_SOURCE = (
    '''
from tatolab import stream


@stream.node.process
class Gripper:
    grip = stream.Input(stream.Channels, delivery="ordered")
'''
    + RAISES_AT_IMPORT
)

INSTALLED_PACKAGE_INIT_SOURCE = (
    '''
from tatolab.stream import node

from .image_file import ImageFile

Lerp = node.rust_function(_native.Lerp)


@node.function
def Add(a: float = 0.0, b: float = 0.0) -> float:
    return a + b
'''
    + RAISES_AT_IMPORT
)

INSTALLED_PACKAGE_IMAGE_FILE_SOURCE = (
    '''
from tatolab.stream import Image, Output, node


@node.process(execution="continuous")
class ImageFile:
    video = Output(Image)
'''
    + RAISES_AT_IMPORT
)

INSTALLED_PACKAGE_ENTRY_POINTS = """[tatolab.nodes]
Add = tatolab_fixture_ops:Add
Lerp = tatolab_fixture_ops:Lerp
ImageFile = tatolab_fixture_ops:ImageFile
Missing = tatolab_fixture_ops:Missing
"""


def write_files(root: Path, files: dict[str, str]) -> None:
    for relative_path, text in files.items():
        file_path = root / relative_path
        file_path.parent.mkdir(parents=True, exist_ok=True)
        file_path.write_text(text)


def a_plain_project(project_directory: Path) -> Path:
    write_files(
        project_directory,
        {
            "nodes/__init__.py": "",
            "nodes/inverse_kinematics.py": PROJECT_NODE_SOURCE,
            "generators/arm.py": PROJECT_GENERATOR_SOURCE,
            "tools/unrelated_script.py": "print('no sdk here'\n",
            ".venv/lib/site.py": "from tatolab.stream import node\n@node.function\ndef hidden(x: float) -> float: ...\n",
            "nodes/__pycache__/stale.py": "from tatolab.stream import node\n(",
        },
    )
    return project_directory


def an_installed_distribution(site_directory: Path) -> list[str]:
    write_files(
        site_directory,
        {
            "tatolab_fixture_ops/__init__.py": INSTALLED_PACKAGE_INIT_SOURCE,
            "tatolab_fixture_ops/image_file.py": INSTALLED_PACKAGE_IMAGE_FILE_SOURCE,
            "tatolab_fixture_ops-1.2.3.dist-info/METADATA": (
                "Metadata-Version: 2.1\nName: tatolab-fixture-ops\nVersion: 1.2.3\n"
            ),
            "tatolab_fixture_ops-1.2.3.dist-info/entry_points.txt": INSTALLED_PACKAGE_ENTRY_POINTS,
            "tatolab_fixture_ops-1.2.3.dist-info/RECORD": (
                "tatolab_fixture_ops/__init__.py,,\n"
                "tatolab_fixture_ops/image_file.py,,\n"
                "tatolab_fixture_ops-1.2.3.dist-info/METADATA,,\n"
                "../../../bin/fixture-tool,,\n"
            ),
            "unrelated-0.1.dist-info/METADATA": "Metadata-Version: 2.1\nName: unrelated\nVersion: 0.1\n",
            "unrelated-0.1.dist-info/RECORD": "unrelated.py,,\n",
            "unrelated.py": "from tatolab.stream import node\n@node.function\ndef unlisted(x: float) -> float: ...\n",
        },
    )
    return [str(site_directory)]


def the_source(listing: dict[str, Any], source: str) -> dict[str, Any]:
    (found,) = [each for each in listing["sources"] if each["source"] == source]
    return found


def node_types_by_type(source: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {node_type["type"]: node_type for node_type in source["node_types"]}


def test_a_plain_projects_node_types_are_read_with_their_ports_and_nothing_runs(
    tmp_path: Path,
) -> None:
    listing = list_the_node_types_and_graph_generators_of_a_project(
        a_plain_project(tmp_path), [], installed_distribution_search_path=[]
    )

    project = the_source(listing, "project")
    node_types = node_types_by_type(project)
    assert sorted(node_types) == [
        "nodes.inverse_kinematics:InverseKinematics",
        "nodes.inverse_kinematics:ramp",
        "nodes.inverse_kinematics:split",
        "nodes.inverse_kinematics:tint",
    ]
    assert node_types["nodes.inverse_kinematics:InverseKinematics"] == {
        "type": "nodes.inverse_kinematics:InverseKinematics",
        "kind": "process",
        "description": "Solves joint angles for a target pose.",
        "category": "Robot",
        "inputs": [{"name": "target", "family": "image", "description": "The target pose.", "annotation": None}],
        "outputs": [{"name": "joint_angles", "family": None, "description": "", "annotation": None}],
        "path": "nodes/inverse_kinematics.py",
        "line": 8,
    }
    split = node_types["nodes.inverse_kinematics:split"]
    assert split["kind"] == "function"
    assert split["description"] == "Splits a value at a threshold."
    assert split["category"] == "nodes.inverse_kinematics"
    assert split["inputs"] == [
        {"name": "x", "family": "channels", "description": "", "annotation": "float"},
        {"name": "threshold", "family": "channels", "description": "", "annotation": "float", "pin_default": 0.5},
    ]
    assert split["outputs"] == [
        {"name": "low", "family": "channels", "description": "", "annotation": "float"},
        {"name": "high", "family": "channels", "description": "", "annotation": "float | None"},
    ]
    ramp = node_types["nodes.inverse_kinematics:ramp"]
    assert ramp["description"] == "A ramp at `rate`."
    assert ramp["inputs"] == [{"name": "rate", "family": "channels", "description": "", "annotation": "float"}]
    assert ramp["outputs"] == [{"name": "result", "family": "channels", "description": "", "annotation": "float"}]
    tint = node_types["nodes.inverse_kinematics:tint"]
    assert tint["kind"] == "shader"
    assert [port["family"] for port in tint["inputs"]] == ["image", "channels"]
    assert tint["outputs"] == [{"name": "result", "family": "image", "description": "", "annotation": "ImageFrame"}]

    assert project["graph_generators"] == [
        {
            "generator": "generators.arm:arm_rig",
            "description": "Six joints and a solver.",
            "promoted_config": [
                {"name": "joints", "annotation": "int", "default": 6},
                {"name": "label", "annotation": "str", "default": "arm"},
                {"name": "gain", "annotation": None, "default_as_written": "compute_gain()"},
            ],
            "path": "generators/arm.py",
            "line": 6,
        }
    ]
    assert project["unreadable_modules"] == []


def test_a_dependencys_node_code_is_read_from_the_files_at_its_pin(tmp_path: Path) -> None:
    dependency = {
        "project": "6f1c2a8e-0000-4000-8000-000000000001",
        "revision": "v1.0",
        "files": {
            "nodes/gripper.py": DEPENDENCY_NODE_SOURCE,
            "streams/cell.graph.json": "{}",
        },
    }

    listing = list_the_node_types_and_graph_generators_of_a_project(
        tmp_path, [dependency], installed_distribution_search_path=[]
    )

    dependency_source = the_source(listing, "dependency")
    assert dependency_source["project"] == dependency["project"]
    assert dependency_source["revision"] == "v1.0"
    assert [
        (node_type["type"], node_type["inputs"]) for node_type in dependency_source["node_types"]
    ] == [
        ("nodes.gripper:Gripper", [{"name": "grip", "family": "channels", "description": "", "annotation": None}])
    ]


def test_an_installed_distribution_lists_each_node_type_its_entry_points_name(
    tmp_path: Path,
) -> None:
    search_path = an_installed_distribution(tmp_path / "site-packages")

    listing = list_the_node_types_and_graph_generators_of_a_project(
        tmp_path / "project", [], installed_distribution_search_path=search_path
    )

    (installed,) = [each for each in listing["sources"] if each["source"] == "installed_distribution"]
    assert installed["distribution"] == "tatolab-fixture-ops"
    assert installed["distribution_version"] == "1.2.3"
    node_types = node_types_by_type(installed)
    assert sorted(node_types) == [
        "tatolab_fixture_ops:Add",
        "tatolab_fixture_ops:ImageFile",
        "tatolab_fixture_ops:Lerp",
        "tatolab_fixture_ops:Missing",
    ]
    assert node_types["tatolab_fixture_ops:Add"]["inputs"] == [
        {"name": "a", "family": "channels", "description": "", "annotation": "float", "pin_default": 0.0},
        {"name": "b", "family": "channels", "description": "", "annotation": "float", "pin_default": 0.0},
    ]
    assert node_types["tatolab_fixture_ops:ImageFile"]["path"] == "tatolab_fixture_ops/image_file.py"
    assert node_types["tatolab_fixture_ops:ImageFile"]["outputs"] == [
        {"name": "video", "family": "image", "description": "", "annotation": None}
    ]
    assert node_types["tatolab_fixture_ops:Lerp"]["kind"] == "function"
    assert node_types["tatolab_fixture_ops:Lerp"]["inputs"] is None
    assert node_types["tatolab_fixture_ops:Missing"]["kind"] is None


def test_a_syntax_error_is_reported_for_its_module_only(tmp_path: Path) -> None:
    project_directory = a_plain_project(tmp_path)
    write_files(
        project_directory,
        {"nodes/broken.py": "from tatolab.stream import node\n\n@node.function\ndef broken(x: float -> float:\n"},
    )

    project = the_source(
        list_the_node_types_and_graph_generators_of_a_project(
            project_directory, [], installed_distribution_search_path=[]
        ),
        "project",
    )

    (unreadable,) = project["unreadable_modules"]
    assert unreadable["module"] == "nodes.broken"
    assert unreadable["path"] == "nodes/broken.py"
    assert unreadable["line"] == 4
    assert unreadable["why"].startswith("SyntaxError: ")
    assert len(project["node_types"]) == 4
    assert len(project["graph_generators"]) == 1


def test_the_entry_prints_one_document_for_the_project_in_its_working_directory(
    tmp_path: Path,
) -> None:
    project_directory = a_plain_project(tmp_path)
    request = {
        "dependencies": [
            {
                "project": "6f1c2a8e-0000-4000-8000-000000000001",
                "revision": "main",
                "files": {"nodes/gripper.py": DEPENDENCY_NODE_SOURCE},
            }
        ]
    }

    finished = subprocess.run(
        [sys.executable, "-I", "-m", LISTING_ENTRY_MODULE],
        cwd=project_directory,
        input=json.dumps(request),
        capture_output=True,
        text=True,
        timeout=LISTING_ENTRY_TIMEOUT_SECONDS,
        check=False,
    )

    assert finished.returncode == 0, finished.stderr
    listing = json.loads(finished.stdout)
    assert [source["source"] for source in listing["sources"][:2]] == ["project", "dependency"]
    assert len(listing["sources"][0]["node_types"]) == 4


def test_the_entry_answers_why_when_its_request_is_not_json(tmp_path: Path) -> None:
    finished = subprocess.run(
        [sys.executable, "-I", "-m", LISTING_ENTRY_MODULE],
        cwd=tmp_path,
        input="not json",
        capture_output=True,
        text=True,
        timeout=LISTING_ENTRY_TIMEOUT_SECONDS,
        check=False,
    )

    assert finished.returncode == 0, finished.stderr
    assert "JSONDecodeError" in json.loads(finished.stdout)["failed_because"]
