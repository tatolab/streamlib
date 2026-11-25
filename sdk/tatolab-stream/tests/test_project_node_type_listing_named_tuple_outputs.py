# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A node returning a named tuple, listed from source with one output per field.

Each module read raises at import, so a listing that ran one would fail.
"""

from pathlib import Path

from tatolab.stream._node_type_source_reader import (
    read_node_types_and_graph_generators_from_module_sources,
)

RAISES_AT_IMPORT = 'raise RuntimeError("this module ran")\n'


SHAPES_MODULE_SOURCE = "\n".join(
    [
        "import typing",
        "from dataclasses import dataclass",
        "from typing import Annotated, NamedTuple",
        "from tatolab.stream import Description, named_pin_type",
        "",
        "class Bounds(NamedTuple):",
        "    low: Annotated[float, Description('The least value.')]",
        "    high: float",
        "",
        "class Clipped(Bounds):",
        "    def width(self) -> float:",
        "        return self.high - self.low",
        "",
        "Spread = typing.NamedTuple('Spread', [('width', float), ('count', int)])",
        "",
        "@named_pin_type(description='A span.')",
        "class Span(NamedTuple):",
        "    start: float",
        "",
        "    @classmethod",
        "    def from_bytes(cls, payload: bytes) -> 'Span':",
        "        return cls(float(bytes(payload).decode()))",
        "",
        "    def to_bytes(self) -> bytes:",
        "        return str(self.start).encode()",
        "",
        "@dataclass(frozen=True)",
        "class Counted:",
        "    count: int = 0",
        RAISES_AT_IMPORT,
    ]
)

BOUNDS_OUTPUTS = [
    {
        "name": "low",
        "description": "The least value.",
        "annotation": "Annotated[float, Description('The least value.')]",
    },
    {"name": "high", "description": "", "annotation": "float"},
]


def the_outputs_listed_of(*modules: "tuple[str, str, str]") -> "dict[str, list[str]]":
    """The names of the outputs listed for each node type read from `modules`."""
    read = read_node_types_and_graph_generators_from_module_sources(list(modules))
    return {
        node_type["type"]: [port["name"] for port in node_type["outputs"]]
        for node_type in read["node_types"]
    }


def test_a_named_tuple_imported_from_another_module_is_listed_as_one_output_per_field() -> None:
    limits_source = "\n".join(
        [
            "import nodes.shapes as shapes_by_alias",
            "from nodes import shapes",
            "from nodes.shapes import Bounds, Clipped as Kept, Spread",
            "from .shapes import Bounds as Near",
            "from tatolab.stream import node",
            "",
            "@node",
            "def Limits(value: float = 0.0) -> Bounds:",
            "    ...",
            "",
            "@node",
            "def NearLimits(value: float = 0.0) -> 'Near':",
            "    ...",
            "",
            "@node",
            "def ByModule(value: float = 0.0) -> shapes.Bounds:",
            "    ...",
            "",
            "@node",
            "def ByAlias(value: float = 0.0) -> shapes_by_alias.Bounds:",
            "    ...",
            "",
            "@node",
            "def KeptLimits(value: float = 0.0) -> Kept:",
            "    ...",
            "",
            "@node",
            "def Widen(value: float = 0.0) -> Spread:",
            "    ...",
            RAISES_AT_IMPORT,
        ]
    )

    read = read_node_types_and_graph_generators_from_module_sources(
        [
            ("nodes.limits", "nodes/limits.py", limits_source),
            ("nodes.shapes", "nodes/shapes.py", SHAPES_MODULE_SOURCE),
        ]
    )

    outputs_by_node_type = {
        node_type["type"]: node_type["outputs"] for node_type in read["node_types"]
    }
    # The description of a field is read in the module that types it.
    assert outputs_by_node_type["nodes.limits:Limits"] == BOUNDS_OUTPUTS
    for node_type in ("NearLimits", "ByModule", "ByAlias"):
        assert outputs_by_node_type[f"nodes.limits:{node_type}"] == BOUNDS_OUTPUTS, node_type
    # A class subclassing a NamedTuple holds that NamedTuple's fields, as its `_fields` does.
    assert outputs_by_node_type["nodes.limits:KeptLimits"] == BOUNDS_OUTPUTS
    assert outputs_by_node_type["nodes.limits:Widen"] == [
        {"name": "width", "description": "", "annotation": "float"},
        {"name": "count", "description": "", "annotation": "int"},
    ]


def test_a_named_tuple_reexported_by_a_package_and_one_called_into_being_are_read() -> None:
    package_source = "from .shapes import Bounds\n"
    limits_source = "\n".join(
        [
            "from typing import NamedTuple",
            "from tatolab.stream import node",
            "from nodes import Bounds",
            "",
            "Pair = NamedTuple('Pair', left=float, right=float)",
            "",
            "class Wider(Bounds):",
            "    pass",
            "",
            "@node",
            "def Limits(value: float = 0.0) -> Wider:",
            "    ...",
            "",
            "@node",
            "def Split(value: float = 0.0) -> Pair:",
            "    ...",
            RAISES_AT_IMPORT,
        ]
    )

    outputs = the_outputs_listed_of(
        ("nodes", "nodes/__init__.py", package_source),
        ("nodes.limits", "nodes/limits.py", limits_source),
        ("nodes.shapes", "nodes/shapes.py", SHAPES_MODULE_SOURCE),
    )

    assert outputs["nodes.limits:Limits"] == ["low", "high"]
    assert outputs["nodes.limits:Split"] == ["left", "right"]


def test_an_imported_record_lists_as_the_loaded_node_reads_it() -> None:
    limits_source = "\n".join(
        [
            "from nodes.shapes import Counted, Span",
            "from tatolab.stream import node",
            "",
            "@node",
            "def Measure(value: float = 0.0) -> Span:",
            "    ...",
            "",
            "@node",
            "def Tally(value: float = 0.0) -> Counted:",
            "    ...",
            RAISES_AT_IMPORT,
        ]
    )

    outputs = the_outputs_listed_of(
        ("nodes.limits", "nodes/limits.py", limits_source),
        ("nodes.shapes", "nodes/shapes.py", SHAPES_MODULE_SOURCE),
    )

    assert outputs["nodes.limits:Measure"] == ["result"]
    assert outputs["nodes.limits:Tally"] == ["result"]


def test_a_record_the_source_does_not_hold_or_an_import_cycle_is_listed_as_one_output() -> None:
    cycle_source = "\n".join(
        [
            "from nodes.other import Bounds",
            "import somewhere_installed",
            "from somewhere_installed import Reading",
            "from typing import NamedTuple",
            "from tatolab.stream import node",
            "",
            "class Local(NamedTuple):",
            "    q: float",
            "",
            "@node",
            "def ReadElsewhere(value: float = 0.0) -> somewhere_installed.Local:",
            "    ...",
            "",
            "@node",
            "def Limits(value: float = 0.0) -> Bounds:",
            "    ...",
            "",
            "@node",
            "def Read(value: float = 0.0) -> Reading:",
            "    ...",
            RAISES_AT_IMPORT,
        ]
    )
    other_source = "from nodes.limits import Bounds\n"

    outputs = the_outputs_listed_of(
        ("nodes.limits", "nodes/limits.py", cycle_source),
        ("nodes.other", "nodes/other.py", other_source),
    )

    assert outputs["nodes.limits:Limits"] == ["result"]
    assert outputs["nodes.limits:Read"] == ["result"]
    # A record of a module the source does not hold is never one of the same name defined here.
    assert outputs["nodes.limits:ReadElsewhere"] == ["result"]


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
            (port["name"], port["annotation"]) for port in node_type["outputs"]
        ]
        for node_type in read["node_types"]
    }
    assert outputs_by_node_type["core_nodes.combine_and_separate:SeparateColor"] == [
        (component, "float") for component in ("red", "green", "blue", "alpha")
    ]
    assert outputs_by_node_type["core_nodes.combine_and_separate:SeparateVector"] == [
        (component, "float") for component in ("x", "y", "z", "w")
    ]
