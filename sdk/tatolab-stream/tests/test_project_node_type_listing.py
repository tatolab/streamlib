# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

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

import pytest

from tatolab.stream._node_type_source_reader import (
    read_node_types_and_graph_generators_from_module_sources,
)
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
from typing import Annotated

from tatolab.stream import Color, Description, Image, Input, Output, State, Stateful, Time, Vector2, node


INVERSE_KINEMATICS_DESCRIPTION = "Solves joint angles for a target pose."


@node.meta(category="Robot/Arms", description=INVERSE_KINEMATICS_DESCRIPTION)
@node.process(execution="continuous")
class InverseKinematics:
    """A docstring, which the listing never reads."""

    target = Input(Image, delivery="newest", description="The target pose.")
    Joint_Angles = Output(family_named("joints"))

    def __init__(self) -> None:
        pass


@dataclass(frozen=True)
class Split:
    low: Annotated[float, Description("What falls under `threshold`.")]
    high: float | None


@dataclass(frozen=True)
class Phase:
    phase: float = 0.0


@node.function
@node.meta(description="Splits a value at a threshold.")
def split(
    x: "Annotated[float, Description(INVERSE_KINEMATICS_DESCRIPTION)] | None",
    threshold: float = 0.5,
    config: "SplitConfig" = None,
    now: Time = 0,
) -> Split:
    return Split(x, None)


@node.function
def ramp(rate: float, previous: State[Phase]) -> Stateful[Annotated[float, Description("The ramp.")], Phase]:
    """A ramp at `rate`."""
    return previous.phase, previous


@node.meta(
    category="Color",
    description="Scales every channel by `amount`.",
)
@node.meta(category="Effects")
@node.shader("float4 tint(ImageInput image, float amount, float4 hue, float2 shift) { return image.pixel * amount; }")
def tint(
    image: ImageFrame, amount: float = 1.0, hue: Color = Color(1, 1, 1), shift: Vector2 = Vector2(0, 0)
) -> ImageFrame: ...


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
        "category": "Robot/Arms",
        "inputs": [{"name": "target", "family": "image", "description": "The target pose.", "annotation": None}],
        "outputs": [{"name": "joint_angles", "family": None, "description": "", "annotation": None}],
        "path": "nodes/inverse_kinematics.py",
        "line": 13,
    }
    split = node_types["nodes.inverse_kinematics:split"]
    assert split["kind"] == "function"
    assert split["description"] == "Splits a value at a threshold."
    assert split["category"] is None
    assert split["inputs"] == [
        {
            "name": "x",
            "family": "channels",
            "description": "Solves joint angles for a target pose.",
            "annotation": "'Annotated[float, Description(INVERSE_KINEMATICS_DESCRIPTION)] | None'",
        },
        {"name": "threshold", "family": "channels", "description": "", "annotation": "float", "pin_default": 0.5},
    ]
    assert split["outputs"] == [
        {
            "name": "low",
            "family": "channels",
            "description": "What falls under `threshold`.",
            "annotation": "Annotated[float, Description('What falls under `threshold`.')]",
        },
        {"name": "high", "family": "channels", "description": "", "annotation": "float | None"},
    ]
    ramp = node_types["nodes.inverse_kinematics:ramp"]
    assert ramp["description"] == ""
    assert ramp["inputs"] == [{"name": "rate", "family": "channels", "description": "", "annotation": "float"}]
    assert ramp["outputs"] == [
        {
            "name": "result",
            "family": "channels",
            "description": "The ramp.",
            "annotation": "Annotated[float, Description('The ramp.')]",
        }
    ]
    tint = node_types["nodes.inverse_kinematics:tint"]
    assert tint["kind"] == "shader"
    assert (tint["category"], tint["description"]) == ("Color", "Scales every channel by `amount`.")
    assert [port["family"] for port in tint["inputs"]] == ["image", "channels", "channels", "channels"]
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


NODE_META_AS_WRITTEN = '@node.meta(category="Effects/Stylize", description="Tints a frame.")'
KIND_DECORATOR_AND_DECLARATION_BY_KIND = {
    "process": ('@node.process(execution="reactive")', "class Tint:\n    pass"),
    "function": ("@node.function", "def Tint(amount: float = 1.0) -> float:\n    return amount"),
    "shader": (
        '@node.shader("float4 Tint(ImageInput image) { return image.pixel; }")',
        "def Tint(image: ImageFrame) -> ImageFrame: ...",
    ),
}


@pytest.mark.parametrize("kind", sorted(KIND_DECORATOR_AND_DECLARATION_BY_KIND))
@pytest.mark.parametrize("meta_is_written", ["above_the_kind_decorator", "below_the_kind_decorator"])
def test_a_node_meta_is_read_from_source_above_or_below_the_decorator_of_every_kind(
    kind: str, meta_is_written: str
) -> None:
    kind_decorator, declaration = KIND_DECORATOR_AND_DECLARATION_BY_KIND[kind]
    decorators = [NODE_META_AS_WRITTEN, kind_decorator]
    if meta_is_written == "below_the_kind_decorator":
        decorators.reverse()
    module_source = "\n".join(
        ["from tatolab.stream import ImageFrame, node", "", "", *decorators, declaration, RAISES_AT_IMPORT]
    )

    read = read_node_types_and_graph_generators_from_module_sources(
        [("nodes.tint", "nodes/tint.py", module_source)]
    )

    assert read["unreadable_modules"] == []
    (node_type,) = read["node_types"]
    assert node_type["type"] == "nodes.tint:Tint"
    assert node_type["kind"] == kind
    assert node_type["category"] == "Effects/Stylize"
    assert node_type["description"] == "Tints a frame."


def test_a_function_under_bare_node_is_listed_as_a_function_node_returning_one_output() -> None:
    module_source = "\n".join(
        [
            "from dataclasses import dataclass",
            "from typing import Annotated",
            "from tatolab.stream import NumberRange, NumberUnit, node",
            "",
            "@dataclass(frozen=True)",
            "class Reading:",
            "    level: float = 0.0",
            "",
            '@node.meta(description="A tone.")',
            "@node",
            "def Tone(frequency: Annotated[float, NumberRange(minimum=20.0), NumberUnit('Hz')] = 440.0) -> Reading:",
            "    return Reading(frequency)",
            RAISES_AT_IMPORT,
        ]
    )

    read = read_node_types_and_graph_generators_from_module_sources(
        [("nodes.tone", "nodes/tone.py", module_source)]
    )

    (tone,) = read["node_types"]
    assert (tone["type"], tone["kind"], tone["description"]) == ("nodes.tone:Tone", "function", "A tone.")
    assert [port["name"] for port in tone["inputs"]] == ["frequency"]
    assert tone["inputs"][0]["pin_default"] == 440.0
    assert [port["name"] for port in tone["outputs"]] == ["result"]


def test_a_named_tuple_a_bare_node_function_returns_is_listed_as_one_output_per_field() -> None:
    module_source = "\n".join(
        [
            "import typing",
            "from dataclasses import dataclass",
            "from typing import Annotated, NamedTuple",
            "from tatolab.stream import Description, State, Stateful, Texture, named_pin_type, node",
            "",
            "@named_pin_type(description='A span.')",
            "class Span(NamedTuple):",
            "    start: float",
            "",
            "@node",
            "def Measure(value: float = 0.0) -> Span:",
            "    ...",
            "",
            "class Bounds(NamedTuple):",
            "    low: Annotated[float, Description('The least value.')]",
            "    high: float",
            "    picture: Texture",
            "",
            "class Spread(typing.NamedTuple):",
            "    width: float",
            "",
            "@dataclass(frozen=True)",
            "class Counted:",
            "    count: int = 0",
            "",
            "@node",
            "def Limits(value: float = 0.0) -> Bounds:",
            "    ...",
            "",
            "@node",
            "def Widen(value: float = 0.0) -> 'Spread':",
            "    ...",
            "",
            "@node",
            "def Count(value: float, previous: State[Counted]) -> Stateful[Bounds, Counted]:",
            "    ...",
            "",
            "@node",
            "def Tally(value: float = 0.0) -> Counted:",
            "    ...",
            "",
            "@node.function",
            "def OldTally(value: float = 0.0) -> Counted:",
            "    ...",
            RAISES_AT_IMPORT,
        ]
    )

    read = read_node_types_and_graph_generators_from_module_sources(
        [("nodes.limits", "nodes/limits.py", module_source)]
    )

    outputs_by_node_type = {
        node_type["type"]: node_type["outputs"] for node_type in read["node_types"]
    }
    bounds_outputs = [
        {
            "name": "low",
            "family": "channels",
            "description": "The least value.",
            "annotation": "Annotated[float, Description('The least value.')]",
        },
        {"name": "high", "family": "channels", "description": "", "annotation": "float"},
        {"name": "picture", "family": "image", "description": "", "annotation": "Texture"},
    ]
    assert outputs_by_node_type["nodes.limits:Limits"] == bounds_outputs
    assert outputs_by_node_type["nodes.limits:Count"] == bounds_outputs
    assert outputs_by_node_type["nodes.limits:Widen"] == [
        {"name": "width", "family": "channels", "description": "", "annotation": "float"}
    ]
    # A named pin type is one pin. Under bare `@node` a dataclass is one bundle pin; under
    # `@node.function`, one output a field.
    assert [port["name"] for port in outputs_by_node_type["nodes.limits:Measure"]] == ["result"]
    assert [port["name"] for port in outputs_by_node_type["nodes.limits:Tally"]] == ["result"]
    assert [port["name"] for port in outputs_by_node_type["nodes.limits:OldTally"]] == ["count"]


def test_a_named_tuple_imported_from_another_module_is_listed_as_one_output() -> None:
    # A module is read alone, so the fields of a named tuple it imports are not in its source.
    module_source = "\n".join(
        [
            "from tatolab.stream import node",
            "from nodes.shapes import Bounds",
            "",
            "@node",
            "def Limits(value: float = 0.0) -> Bounds:",
            "    ...",
            RAISES_AT_IMPORT,
        ]
    )

    read = read_node_types_and_graph_generators_from_module_sources(
        [("nodes.limits", "nodes/limits.py", module_source)]
    )

    (limits,) = read["node_types"]
    assert limits["outputs"] == [
        {"name": "result", "family": None, "description": "", "annotation": "Bounds"}
    ]


def test_the_core_separate_nodes_are_listed_with_a_float_output_per_component() -> None:
    core_nodes_source_path = (
        Path(__file__).resolve().parents[3]
        / "packages"
        / "tatolab-core-nodes"
        / "core_nodes"
        / "combine_and_separate.py"
    )

    read = read_node_types_and_graph_generators_from_module_sources(
        [
            (
                "core_nodes.combine_and_separate",
                "core_nodes/combine_and_separate.py",
                core_nodes_source_path.read_text(encoding="utf-8"),
            )
        ]
    )

    outputs_by_node_type = {
        node_type["type"]: [
            (port["name"], port["family"], port["annotation"]) for port in node_type["outputs"]
        ]
        for node_type in read["node_types"]
    }
    assert outputs_by_node_type["core_nodes.combine_and_separate:SeparateColor"] == [
        (component, "channels", "float") for component in ("red", "green", "blue", "alpha")
    ]
    assert outputs_by_node_type["core_nodes.combine_and_separate:SeparateVector"] == [
        (component, "channels", "float") for component in ("x", "y", "z", "w")
    ]


def test_a_class_under_bare_node_is_listed_with_its_fields_and_pins_and_no_state() -> None:
    module_source = "\n".join(
        [
            "from typing import Annotated, ClassVar",
            "from tatolab.stream import Description, InputPin, OutputPin, SettingOnly, Texture, node",
            "",
            '@node.meta(description="Blurs each picture.")',
            "@node(execution='reactive')",
            "class Blur:",
            "    video: Annotated[InputPin[Texture], Description('The pictures.')]",
            "    radius: Annotated[float, SettingOnly()] = 4.0",
            "    result: OutputPin[Texture]",
            "    frames: ClassVar[int] = 0",
            "    _kernel: list[float]",
            RAISES_AT_IMPORT,
        ]
    )

    read = read_node_types_and_graph_generators_from_module_sources(
        [("nodes.blur", "nodes/blur.py", module_source)]
    )

    (blur,) = read["node_types"]
    assert (blur["type"], blur["kind"], blur["description"]) == (
        "nodes.blur:Blur",
        "process",
        "Blurs each picture.",
    )
    assert [(port["name"], port["family"]) for port in blur["inputs"]] == [
        ("video", "image"),
        ("radius", "channels"),
    ]
    assert blur["inputs"][0]["description"] == "The pictures."
    assert blur["inputs"][1]["pin_default"] == 4.0
    assert [(port["name"], port["family"]) for port in blur["outputs"]] == [("result", "image")]


def test_a_pin_typed_by_a_bound_type_parameter_is_listed_with_the_family_of_its_bound() -> None:
    module_source = "\n".join(
        [
            "from typing import Annotated",
            "from tatolab.stream import Description, InputPin, OutputPin, Texture, node",
            "",
            "@node",
            "class HandsOn[Frame: Texture, Unbound]:",
            "    picture_in: InputPin[Annotated[Frame, Description('The pictures.')]]",
            "    other: InputPin[Unbound]",
            "    picture_out: OutputPin[Frame]",
            RAISES_AT_IMPORT,
        ]
    )

    read = read_node_types_and_graph_generators_from_module_sources(
        [("nodes.hands_on", "nodes/hands_on.py", module_source)]
    )

    (hands_on,) = read["node_types"]
    assert [(port["name"], port["family"]) for port in hands_on["inputs"]] == [
        ("picture_in", "image"),
        ("other", None),
    ]
    assert hands_on["inputs"][0]["description"] == "The pictures."
    assert [(port["name"], port["family"]) for port in hands_on["outputs"]] == [("picture_out", "image")]


def test_a_named_pin_type_is_listed_by_its_import_path_and_description_from_source() -> None:
    module_source = "\n".join(
        [
            "import tatolab.stream as stream",
            "from tatolab.stream import named_pin_type",
            "",
            'BOX_WORDS = "A face found in a picture, as a box and a score."',
            "",
            "@named_pin_type(description=BOX_WORDS)",
            "class FaceBox:",
            "    @classmethod",
            "    def from_bytes(cls, payload): ...",
            "    def to_bytes(self): ...",
            "",
            "class Finder:",
            '    @stream.named_pin_type(description="What the finder saw.")',
            "    class Sighting:",
            "        @classmethod",
            "        def from_bytes(cls, payload): ...",
            "        def to_bytes(self): ...",
            "",
            "class Undecorated:",
            "    pass",
            RAISES_AT_IMPORT,
        ]
    )

    read = read_node_types_and_graph_generators_from_module_sources(
        [("faces.boxes", "faces/boxes.py", module_source)]
    )

    assert read["named_pin_types"] == [
        {
            "named_pin_type": "faces.boxes:FaceBox",
            "description": "A face found in a picture, as a box and a score.",
            "path": "faces/boxes.py",
            "line": 7,
        },
        {
            "named_pin_type": "faces.boxes:Finder.Sighting",
            "description": "What the finder saw.",
            "path": "faces/boxes.py",
            "line": 14,
        },
    ]
    assert read["node_types"] == []


def test_an_imports_node_code_is_read_from_the_files_its_folder_holds(tmp_path: Path) -> None:
    dependency = {
        "project": "6f1c2a8e-0000-4000-8000-000000000001",
        "folder": "gripper",
        "version": "ab" * 32,
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
    assert dependency_source["folder"] == "gripper"
    assert dependency_source["version"] == "ab" * 32
    assert [
        (node_type["type"], node_type["inputs"]) for node_type in dependency_source["node_types"]
    ] == [
        (
            "imports/gripper/nodes.gripper:Gripper",
            [{"name": "grip", "family": "channels", "description": "", "annotation": None}],
        )
    ]
    assert [node_type["category"] for node_type in dependency_source["node_types"]] == [None]


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
    assert [node_type["category"] for node_type in node_types.values()] == [None, None, None, None]


def test_the_project_source_leaves_out_the_node_code_of_its_imports_folder(
    tmp_path: Path,
) -> None:
    project_directory = a_plain_project(tmp_path)
    write_files(
        project_directory,
        {
            "imports/gripper/nodes/gripper.py": DEPENDENCY_NODE_SOURCE,
            "nodes/imports/gripper.py": DEPENDENCY_NODE_SOURCE,
        },
    )

    project = the_source(
        list_the_node_types_and_graph_generators_of_a_project(
            project_directory, [], installed_distribution_search_path=[]
        ),
        "project",
    )

    assert "imports.gripper.nodes.gripper:Gripper" not in node_types_by_type(project)
    assert "nodes.imports.gripper:Gripper" in node_types_by_type(project)
    assert len(project["node_types"]) == 5


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
                "folder": "gripper",
                "version": "ab" * 32,
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
