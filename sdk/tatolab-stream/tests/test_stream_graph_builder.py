# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`@stream`, the `StreamBuilder` and `compile_stream_to_graph`, with no engine.

A stream compiles to its graph as plain data, so every case is a literal
expectation about that data or a refusal at the line the author wrote.
"""

import ast
import functools
import json
import math
import sys
from collections.abc import Mapping
from enum import Enum, IntEnum
from pathlib import Path
from types import MappingProxyType
from typing import Any

import numpy
import pytest

import tatolab.stream
from shared_port_address_cases import (
    PORT_ADDRESS_CASES_THAT_ARE_REFUSED,
    PORT_ADDRESS_CASES_THAT_PARSE,
    port_address_case_id,
)
from stream_graph_builder_nodes import BrightnessReader, FrameFilters, FrameInverter
from tatolab.stream import (
    NodeInputPortReference,
    NodeOutputPortReference,
    NodeOutputPortReferenceByAddress,
    NodeReference,
    StreamBuilder,
    compile_stream_to_graph,
    node,
    stream,
)
from tatolab.stream._exposed_name_cast import (
    EXPOSED_NAME_MAXIMUM_LENGTH,
    ExposedNameCastsToNothingError,
)
from tatolab.stream._port_address import InvalidPortAddressError
from tatolab.stream._stream_graph_builder import is_stream_function

STREAM_GRAPH_BUILDER_SOURCE = (
    Path(tatolab.stream.__file__).resolve().parent / "_stream_graph_builder.py"
)

TEST_PATTERN_SOURCE_TYPE = "tatolab.stream:TestPatternSource"
H264_ENCODER_TYPE = "tatolab.stream:H264Encoder"
VIRTUAL_CAMERA_SINK_TYPE = "tatolab.stream:VirtualCameraSink"
FRAME_INVERTER_TYPE = "stream_graph_builder_nodes:FrameInverter"
BRIGHTNESS_READER_TYPE = "stream_graph_builder_nodes:BrightnessReader"
FRAME_DARKENER_TYPE = "stream_graph_builder_nodes:FrameFilters.FrameDarkener"


class StandInMarker:
    """A class carrying a `str` `type`, the way every native marker does."""

    type = "stand_in_builtins::stand_in_marker::StandInMarker"


class UndecoratedFilter:
    """A class that never met `@node`."""


class StreamBuildingFailure(Exception):
    """The one exception `a_stream_that_raises` raises."""


class FrameColour(str, Enum):
    """A `str` enum, whose `str()` is `FrameColour.RED` rather than its value."""

    RED = "red"


class FrameRate(IntEnum):
    """An `int` enum."""

    SIXTY = 60


class KeyComparedByIdentity(str):
    """A `str` a mapping can hold beside an equal plain `str` key."""

    def __eq__(self, other: object) -> bool:
        return self is other

    def __hash__(self) -> int:
        return id(self)


THE_STREAM_BUILDING_FAILURE = StreamBuildingFailure("the camera rig is unplugged")

NESTED_CONFIG: "dict[str, Any]" = {
    "title": "Rig",
    "size": (640, 480),
    "overlay": MappingProxyType({"labels": ["left", "right"], "opacity": 0.5}),
    "enabled": True,
    "device": None,
}


@stream
def every_kind_of_node(stream_builder: StreamBuilder) -> None:
    """A `@node` class, a nested one and a marker-shaped class.

    The second line of the description.
    """
    stream_builder.add(FrameInverter)
    stream_builder.add(FrameFilters.FrameDarkener)
    stream_builder.add(StandInMarker)


@stream
def camera_rig(stream_builder: StreamBuilder) -> None:
    """A marker source fanned out to an inverter feeding two readers and to a second inverter."""
    source = stream_builder.add(
        tatolab.stream.TestPatternSource, config={"width": 1280, "height": 720}
    )
    inverter = stream_builder.add(FrameInverter)
    first_reader = stream_builder.add(BrightnessReader)
    backup_reader = stream_builder.add(BrightnessReader, name="Backup Meter")
    second_inverter = stream_builder.add(FrameInverter)
    encoder = stream_builder.add(tatolab.stream.H264Encoder, config={"bitrate_bps": 4_000_000})
    stream_builder.connect(source.output("video"), inverter.input("video_from_upstream"))
    stream_builder.connect(
        inverter.output("video_to_downstream"),
        first_reader.input("VIDEO_FROM_UPSTREAM"),
    )
    stream_builder.connect(
        inverter.output("video_to_downstream"),
        backup_reader.input("video_from_upstream"),
    )
    stream_builder.connect(
        source.output("Video"),
        second_inverter.input("video_from_upstream"),
    )
    stream_builder.connect(second_inverter.output("video_to_downstream"), encoder.input("Video"))
    stream_builder.expose(inverter.output("video_to_downstream"))
    stream_builder.expose(second_inverter.output("VIDEO_TO_DOWNSTREAM"))


@stream
def two_markers(stream_builder: StreamBuilder) -> None:
    stream_builder.add(tatolab.stream.TestPatternSource)
    stream_builder.add(tatolab.stream.H264Encoder)


@stream
def a_virtual_camera(stream_builder: StreamBuilder) -> None:
    stream_builder.add(tatolab.stream.VirtualCameraSink)


@stream
def adds_nothing(stream_builder: StreamBuilder) -> None:
    """A stream whose function adds no node."""


@stream
def a_stream_that_raises(stream_builder: StreamBuilder) -> None:
    stream_builder.add(FrameInverter)
    raise THE_STREAM_BUILDING_FAILURE


@stream
def a_configured_stream(stream_builder: StreamBuilder) -> None:
    stream_builder.add(StandInMarker, config=NESTED_CONFIG)


@stream
def a_stream_with_a_typed_duplicate(stream_builder: StreamBuilder) -> None:
    stream_builder.add(FrameInverter, name="Front Camera")
    stream_builder.add(BrightnessReader, name="front-CAMERA")


@stream
def hand_built_references(stream_builder: StreamBuilder) -> None:
    """Every port named through a reference built by hand rather than minted."""
    stream_builder.add(FrameInverter)
    stream_builder.add(BrightnessReader)
    stream_builder.connect(
        NodeOutputPortReference("FrameInverter", "VIDEO_TO_DOWNSTREAM"),
        NodeInputPortReference("BrightnessReader", "VIDEO_FROM_UPSTREAM"),
    )
    stream_builder.expose(NodeOutputPortReference("FrameInverter", "Video_To_Downstream"))


@stream
def reads_another_stream(stream_builder: StreamBuilder) -> None:
    """An inverter pulling stream `Camera Rig`'s exposed output, and its own output to a reader."""
    inverter = stream_builder.add(FrameInverter)
    reader = stream_builder.add(BrightnessReader)
    stream_builder.connect(
        stream_builder.remote_output("Camera Rig/FrameInverter/Video_To_Downstream"),
        inverter.input("video_from_upstream"),
    )
    stream_builder.connect(
        stream_builder.remote_output("reads_another_stream/frameinverter/video_to_downstream"),
        reader.input("video_from_upstream"),
    )


def not_decorated(stream_builder: StreamBuilder) -> None:
    stream_builder.add(FrameInverter)


def names_of_nodes_added(stream_builder: StreamBuilder, *node_classes: type) -> "list[str]":
    return [stream_builder.add(node_class).name for node_class in node_classes]


def config_compiled_for(config: "Mapping[str, Any]") -> "dict[str, Any]":
    """The config `compile_stream_to_graph` records for one node added with `config`."""
    namespace: "dict[str, Any]" = {
        "__name__": "rig_streams",
        "StandInMarker": StandInMarker,
        "config_under_test": config,
    }
    exec(
        "def main(stream_builder):\n    stream_builder.add(StandInMarker, config=config_under_test)\n",
        namespace,
    )
    return compile_stream_to_graph(stream(namespace["main"]))["nodes"][0]["config"]


def a_config_holding_itself() -> "dict[str, Any]":
    config: "dict[str, Any]" = {"title": "Rig"}
    config["overlay"] = {"parent": config}
    return config


def a_config_holding_a_list_holding_itself() -> "dict[str, Any]":
    labels: "list[Any]" = ["left"]
    labels.append(labels)
    return {"overlay": {"labels": labels}}


# serde_json's default recursion limit parses 127 nested containers, `tatolab`
# reads the graph one container inside its compile document, and the graph, its
# `nodes` list and the node enclose every config.
CONTAINERS_A_CONFIG_NESTS_AT_MOST_COUNTING_ITSELF = 127 - 1 - 3


def a_config_nesting_containers_deep(containers_counting_the_config: int) -> "dict[str, Any]":
    """`{"nested": [[...]]}`, `containers_counting_the_config` containers deep in all."""
    nested: "list[Any]" = []
    for _ in range(containers_counting_the_config - 2):
        nested = [nested]
    return {"nested": nested}


def test_a_defaulted_name_is_the_cast_short_name_and_a_duplicate_takes_the_next_suffix() -> (
    None
):
    stream_builder = StreamBuilder("rig")

    assert names_of_nodes_added(stream_builder, *[FrameInverter] * 4) == [
        "frameinverter",
        "frameinverter-2",
        "frameinverter-3",
        "frameinverter-4",
    ]


def test_distinct_classes_keep_their_own_defaulted_names() -> None:
    stream_builder = StreamBuilder("rig")

    assert names_of_nodes_added(stream_builder, FrameInverter, BrightnessReader) == [
        "frameinverter",
        "brightnessreader",
    ]


def test_a_typed_name_is_cast() -> None:
    assert StreamBuilder("rig").add(FrameInverter, name="Front Camera").name == "front-camera"


def test_a_typed_duplicate_is_refused_at_the_add_that_typed_it_naming_both() -> None:
    stream_builder = StreamBuilder("rig")
    stream_builder.add(FrameInverter, name="Front Camera")

    with pytest.raises(ValueError) as refusal:
        stream_builder.add(BrightnessReader, name="front-CAMERA")

    message = str(refusal.value)
    assert "'front-CAMERA'" in message
    assert "`front-camera`" in message
    assert "typed as 'Front Camera'" in message
    assert "give one of them another name" in message
    assert "leave the name out to take a `-2` suffix" in message


def test_a_typed_name_casting_like_a_defaulted_one_is_refused_naming_the_default() -> (
    None
):
    stream_builder = StreamBuilder("rig")
    stream_builder.add(FrameInverter)
    stream_builder.add(FrameInverter)

    with pytest.raises(ValueError) as refusal:
        stream_builder.add(BrightnessReader, name="FrameInverter-2")

    message = str(refusal.value)
    assert "'FrameInverter-2'" in message
    assert "`frameinverter-2`" in message
    assert "the default name of FrameInverter" in message


def test_a_refused_typed_name_records_nothing() -> None:
    stream_builder = StreamBuilder("rig")
    stream_builder.add(FrameInverter)

    with pytest.raises(ValueError):
        stream_builder.add(FrameInverter, name="FRAMEINVERTER")

    assert stream_builder.add(FrameInverter).name == "frameinverter-2"


def test_a_defaulted_name_skips_a_suffix_a_typed_name_holds() -> None:
    stream_builder = StreamBuilder("rig")
    stream_builder.add(FrameInverter, name="frameinverter-2")

    assert names_of_nodes_added(stream_builder, FrameInverter, FrameInverter) == [
        "frameinverter",
        "frameinverter-3",
    ]


def test_a_suffix_on_a_name_at_the_bound_cuts_the_name_to_fit() -> None:
    stream_builder = StreamBuilder("rig")
    long_named_marker = type(
        "m" * (EXPOSED_NAME_MAXIMUM_LENGTH + 7),
        (),
        {"type": "stand_in_builtins::long::LongNamed"},
    )

    names = names_of_nodes_added(stream_builder, *[long_named_marker] * 10)

    assert names[0] == "m" * EXPOSED_NAME_MAXIMUM_LENGTH
    assert names[1] == "m" * (EXPOSED_NAME_MAXIMUM_LENGTH - 2) + "-2"
    assert names[9] == "m" * (EXPOSED_NAME_MAXIMUM_LENGTH - 3) + "-10"
    assert all(len(name) <= EXPOSED_NAME_MAXIMUM_LENGTH for name in names)


def test_a_cut_ending_in_a_dash_drops_it_before_the_suffix() -> None:
    stream_builder = StreamBuilder("rig")
    marker_whose_cut_ends_in_a_dash = type(
        "a" * 60 + " bcd", (), {"type": "stand_in_builtins::dash::DashNamed"}
    )

    assert names_of_nodes_added(stream_builder, *[marker_whose_cut_ends_in_a_dash] * 2) == [
        "a" * 60 + "-bc",
        "a" * 60 + "-2",
    ]


def test_a_typed_name_casting_to_nothing_is_refused_naming_it() -> None:
    with pytest.raises(ExposedNameCastsToNothingError, match="'✨'"):
        StreamBuilder("rig").add(FrameInverter, name="✨")


def test_a_typed_name_that_is_not_a_string_is_refused() -> None:
    with pytest.raises(TypeError, match="node name"):
        StreamBuilder("rig").add(FrameInverter, name=7)  # pyright: ignore[reportCallIssue, reportArgumentType]


def test_a_name_of_a_type_outside_builtins_is_refused_naming_its_module_and_the_fix() -> (
    None
):
    with pytest.raises(TypeError) as refusal:
        StreamBuilder("rig").add(FrameInverter, name=numpy.int64(7))  # pyright: ignore[reportCallIssue, reportArgumentType]

    assert str(refusal.value) == (
        "a node name is a str; got np.int64(7), of type `numpy.int64` — pass the name "
        "as a str"
    )


def test_the_stream_name_is_cast() -> None:
    assert StreamBuilder("Camera Rig").name == "camera-rig"


def test_a_stream_name_casting_to_nothing_is_refused_naming_it() -> None:
    with pytest.raises(ExposedNameCastsToNothingError, match="'日本'"):
        StreamBuilder("日本")


def test_a_node_class_is_its_module_and_qualname_and_a_marker_its_type() -> None:
    assert compile_stream_to_graph(every_kind_of_node)["nodes"] == [
        {"name": "frameinverter", "type": FRAME_INVERTER_TYPE, "config": {}},
        {"name": "framedarkener", "type": FRAME_DARKENER_TYPE, "config": {}},
        {"name": "standinmarker", "type": StandInMarker.type, "config": {}},
    ]


def test_a_native_marker_is_the_type_its_class_carries() -> None:
    assert compile_stream_to_graph(two_markers)["nodes"] == [
        {"name": "testpatternsource", "type": TEST_PATTERN_SOURCE_TYPE, "config": {}},
        {"name": "h264encoder", "type": H264_ENCODER_TYPE, "config": {}},
    ]


def test_a_node_class_defined_inside_a_function_is_refused_naming_stream_add() -> None:
    @node
    class FunctionLocalFilter:
        @node.input(delivery_profile="newest")
        def video_from_upstream(self) -> None: ...

        @node.output()
        def video_to_downstream(self) -> None: ...

    with pytest.raises(ValueError) as refusal:
        StreamBuilder("rig").add(FunctionLocalFilter)

    message = str(refusal.value)
    assert message.startswith(
        "node `test_stream_graph_builder:test_a_node_class_defined_inside_a_function_"
        "is_refused_naming_stream_add.<locals>.FunctionLocalFilter` is defined inside "
        "a function"
    )
    assert "Move the class to module scope." in message
    assert "`stream_builder.add(..., config={...})`" in message


def test_a_node_class_in_the_entry_file_is_refused_naming_stream_py() -> None:
    entry_file_detector = type(
        "EntryFileDetector",
        (),
        {"__module__": "__main__", "__tatolab_node_declared__": True},
    )

    with pytest.raises(ValueError) as refusal:
        StreamBuilder("rig").add(entry_file_detector)

    message = str(refusal.value)
    assert message.startswith(
        "node `EntryFileDetector` is defined in the entry file, so it identifies as "
        "`__main__:EntryFileDetector`"
    )
    assert (
        "    # entry_file_detector.py\n"
        "    @node(...)\n"
        "    class EntryFileDetector: ...\n"
        "\n"
        "    # stream.py\n"
        "    from entry_file_detector import EntryFileDetector\n"
    ) in message
    assert message.endswith(
        "The entry file itself may still run as `__main__`; only node classes may "
        "not live in it."
    )


def test_a_class_both_function_local_and_in_the_entry_file_gets_the_function_local_fix() -> (
    None
):
    local_entry_file_class = type(
        "Local",
        (),
        {
            "__module__": "__main__",
            "__qualname__": "build.<locals>.Local",
            "__tatolab_node_declared__": True,
        },
    )

    with pytest.raises(ValueError, match="is defined inside a function"):
        StreamBuilder("rig").add(local_entry_file_class)


def test_an_undecorated_class_is_refused_naming_node() -> None:
    with pytest.raises(TypeError) as refusal:
        StreamBuilder("rig").add(UndecoratedFilter)

    message = str(refusal.value)
    assert "is not a node: decorate the class with @tatolab.stream.node" in message
    assert "pass the class itself rather than an instance of it" in message


def test_an_instance_is_refused_naming_the_class_itself() -> None:
    with pytest.raises(
        TypeError, match="pass the class itself rather than an instance"
    ):
        StreamBuilder("rig").add(FrameInverter())  # pyright: ignore[reportArgumentType]


@pytest.mark.parametrize("platform", ["linux", "darwin"])
def test_a_virtual_camera_sink_is_recorded_whatever_floor_the_stream_compiles_on(
    monkeypatch: pytest.MonkeyPatch, platform: str
) -> None:
    monkeypatch.setattr(sys, "platform", platform)

    assert compile_stream_to_graph(a_virtual_camera)["nodes"] == [
        {"name": "virtualcamerasink", "type": VIRTUAL_CAMERA_SINK_TYPE, "config": {}}
    ]


@pytest.mark.parametrize(
    "config",
    [
        {"labels": {"left"}},
        {1: "left"},
        {"gain": math.nan},
        {"frame_count": 2**64},
        a_config_holding_itself(),
        ["gain"],
    ],
    ids=[
        "a-set",
        "a-non-string-key",
        "nan",
        "an-integer-beyond-64-bits",
        "a-config-holding-itself",
        "not-a-mapping",
    ],
)
def test_an_add_refused_for_its_config_records_nothing(config: Any) -> None:
    stream_builder = StreamBuilder("rig")

    with pytest.raises((TypeError, ValueError)):
        stream_builder.add(FrameInverter, config=config)
    with pytest.raises((TypeError, ValueError)):
        stream_builder.add(FrameInverter, name="Front Camera", config=config)

    assert stream_builder.add(FrameInverter).name == "frameinverter"
    assert stream_builder.add(BrightnessReader, name="Front Camera").name == "front-camera"


def test_config_is_normalised_to_plain_json() -> None:
    config = compile_stream_to_graph(a_configured_stream)["nodes"][0]["config"]

    assert config == {
        "title": "Rig",
        "size": [640, 480],
        "overlay": {"labels": ["left", "right"], "opacity": 0.5},
        "enabled": True,
        "device": None,
    }
    assert type(config["overlay"]) is dict
    assert type(config["size"]) is list
    assert config["overlay"]["labels"] is not NESTED_CONFIG["overlay"]["labels"]


def test_a_node_with_no_config_records_an_empty_object() -> None:
    graph = compile_stream_to_graph(every_kind_of_node)

    assert [each_node["config"] for each_node in graph["nodes"]] == [{}, {}, {}]


def test_a_config_that_is_not_a_mapping_is_refused() -> None:
    with pytest.raises(TypeError, match="config must be a mapping"):
        StreamBuilder("rig").add(FrameInverter, config=[("gain", 2)])  # pyright: ignore[reportArgumentType]


def test_a_config_value_json_cannot_carry_is_refused_naming_its_key_path() -> None:
    with pytest.raises(TypeError) as refusal:
        StreamBuilder("rig").add(
            FrameInverter, config={"overlay": {"labels": ["left", {"right"}]}}
        )

    message = str(refusal.value)
    assert message.startswith("config must be JSON:")
    assert "`config['overlay']['labels'][1]` is of type `set`" in message
    assert "a collection with `list(...)`" in message


@pytest.mark.parametrize(
    ("value", "type_as_written"),
    [(numpy.bool_(True), "numpy.bool"), (numpy.int64(7), "numpy.int64")],
    ids=["numpy-bool", "numpy-int64"],
)
def test_a_config_value_of_a_type_outside_builtins_is_refused_naming_its_module(
    value: object, type_as_written: str
) -> None:
    with pytest.raises(TypeError) as refusal:
        StreamBuilder("rig").add(FrameInverter, config={"enabled": value})

    message = str(refusal.value)
    assert message.startswith(
        f"config must be JSON: `config['enabled']` is of type `{type_as_written}`, and a "
        f"graph carries only dict, list, tuple, str, int, float, bool and None"
    )
    assert (
        "convert it with `bool(...)`, `int(...)`, `float(...)` or `str(...)`" in message
    )


def test_a_config_key_that_is_not_a_string_is_refused_naming_its_path() -> None:
    with pytest.raises(TypeError) as refusal:
        StreamBuilder("rig").add(FrameInverter, config={"overlay": {1: "left"}})

    message = str(refusal.value)
    assert message.startswith("config must be JSON:")
    assert "`config['overlay']` has the key 1, of type `int`" in message
    assert "convert it with `str(...)`" in message


def test_a_config_float_json_cannot_carry_is_refused_naming_its_key_path() -> None:
    with pytest.raises(ValueError) as refusal:
        StreamBuilder("rig").add(FrameInverter, config={"gain": math.nan})

    assert str(refusal.value) == (
        "config must be JSON: `config['gain']` is nan, which JSON cannot carry — pass "
        "`None` where there is no value, or carry it as a `str`"
    )


def test_config_values_subclassing_str_int_or_float_are_recorded_as_the_base_type() -> (
    None
):
    config = config_compiled_for(
        {
            "colour": FrameColour.RED,
            "frame_rate": FrameRate.SIXTY,
            "gain": numpy.float64(0.5),
            "palette": [FrameColour.RED, FrameRate.SIXTY, numpy.float64(0.25)],
            "enabled": True,
        }
    )

    assert config == {
        "colour": "red",
        "frame_rate": 60,
        "gain": 0.5,
        "palette": ["red", 60, 0.25],
        "enabled": True,
    }
    assert [type(value) for value in config.values()] == [str, int, float, list, bool]
    assert [type(item) for item in config["palette"]] == [str, int, float]


def test_config_keys_subclassing_str_are_recorded_as_plain_strings() -> None:
    config = config_compiled_for({FrameColour.RED: {FrameColour.RED: 1}})

    assert config == {"red": {"red": 1}}
    assert type(next(iter(config))) is str
    assert type(next(iter(config["red"]))) is str


def test_config_keys_equal_only_as_plain_strings_are_refused_naming_the_key() -> None:
    with pytest.raises(ValueError) as refusal:
        StreamBuilder("rig").add(
            FrameInverter,
            config={"overlay": {"gain": 1, KeyComparedByIdentity("gain"): 2}},
        )

    message = str(refusal.value)
    assert message.startswith("config must be JSON:")
    assert "`config['overlay']`" in message
    assert "'gain'" in message
    assert message.endswith("keep one of them, or rename the other")


def test_config_integers_at_the_64_bit_bounds_are_kept() -> None:
    bounds = [-(2**63), 2**63 - 1, 2**64 - 1]

    assert config_compiled_for({"bounds": bounds}) == {"bounds": bounds}


@pytest.mark.parametrize(
    "integer",
    [2**64, -(2**63) - 1, 10**5000],
    ids=["past-u64", "below-i64", "five-thousand-digits"],
)
def test_a_config_integer_beyond_64_bits_is_refused_naming_its_key_path(
    integer: int,
) -> None:
    with pytest.raises(ValueError) as refusal:
        StreamBuilder("rig").add(FrameInverter, config={"limits": [0, integer]})

    assert str(refusal.value) == (
        "config integers must fit the graph's 64-bit range: `config['limits'][1]` is "
        "outside -2**63 to 2**64 - 1; carry a value that large as a `str`"
    )


def test_a_config_holding_itself_is_refused_naming_where_it_loops() -> None:
    with pytest.raises(ValueError) as refusal:
        StreamBuilder("rig").add(FrameInverter, config=a_config_holding_itself())

    message = str(refusal.value)
    assert message.startswith("config must be JSON:")
    assert "`config['overlay']['parent']` is `config`" in message
    assert message.endswith(
        "Break the cycle: put the data `config['overlay']['parent']` should carry "
        "there, not the container holding it"
    )


def test_a_list_holding_itself_is_refused_naming_where_it_loops() -> None:
    with pytest.raises(ValueError) as refusal:
        StreamBuilder("rig").add(
            FrameInverter, config=a_config_holding_a_list_holding_itself()
        )

    message = str(refusal.value)
    assert message.startswith("config must be JSON:")
    assert (
        "`config['overlay']['labels'][1]` is `config['overlay']['labels']`" in message
    )


def test_a_container_two_keys_share_is_recorded_under_each() -> None:
    shared_size = [640, 480]

    config = config_compiled_for({"size": shared_size, "preview_size": shared_size})

    assert config == {"size": [640, 480], "preview_size": [640, 480]}
    assert config["size"] is not config["preview_size"]


def test_a_config_nested_as_deep_as_a_graph_carries_compiles_unchanged() -> None:
    config = a_config_nesting_containers_deep(
        CONTAINERS_A_CONFIG_NESTS_AT_MOST_COUNTING_ITSELF
    )

    assert config_compiled_for(config) == config


def test_a_config_nested_one_past_what_a_graph_carries_is_refused_at_add_by_key_path() -> (
    None
):
    with pytest.raises(ValueError) as refusal:
        StreamBuilder("rig").add(
            FrameInverter,
            config=a_config_nesting_containers_deep(
                CONTAINERS_A_CONFIG_NESTS_AT_MOST_COUNTING_ITSELF + 1
            ),
        )

    deepest_key_path = "config['nested']" + "[0]" * (
        CONTAINERS_A_CONFIG_NESTS_AT_MOST_COUNTING_ITSELF - 1
    )
    assert str(refusal.value) == (
        f"config nests too deep for a graph: `{deepest_key_path}` is a container 124 "
        f"deep counting `config` itself, and a config nests at most 123 — "
        f"a graph nests at most 126 containers from its root, and the graph, its "
        f"`nodes` list and the node enclose every config. Nest the data at most 123 "
        f"containers deep, or carry the deeper part as a `str`"
    )


def test_a_config_nested_5000_deep_is_refused_by_name_rather_than_by_recursion() -> None:
    with pytest.raises(ValueError) as refusal:
        StreamBuilder("rig").add(FrameInverter, config=a_config_nesting_containers_deep(5000))

    assert type(refusal.value) is ValueError
    assert str(refusal.value).startswith("config nests too deep for a graph: ")
    assert "a config nests at most 123" in str(refusal.value)


def test_connect_refuses_an_input_as_its_source_naming_the_fix() -> None:
    stream_builder = StreamBuilder("rig")
    inverter = stream_builder.add(FrameInverter)
    reader = stream_builder.add(BrightnessReader)

    with pytest.raises(TypeError) as refusal:
        stream_builder.connect(
            inverter.input("video_from_upstream"),  # pyright: ignore[reportArgumentType]
            reader.input("video_from_upstream"),
        )

    message = str(refusal.value)
    assert "connect's source must name an output port" in message
    assert "`stream_builder.add(...).output(port_name)`" in message
    assert '`stream_builder.remote_output("<stream>/<node>/<port>")`' in message
    assert "NodeInputPortReference" in message


def test_connect_refuses_an_output_as_its_destination_naming_the_fix() -> None:
    stream_builder = StreamBuilder("rig")
    inverter = stream_builder.add(FrameInverter)

    with pytest.raises(TypeError) as refusal:
        stream_builder.connect(
            inverter.output("video_to_downstream"),
            inverter.output("video_to_downstream"),  # pyright: ignore[reportArgumentType]
        )

    message = str(refusal.value)
    assert "connect's destination must name an input port" in message
    assert "`stream_builder.add(...).input(port_name)`" in message


def test_connect_refuses_a_node_reference_where_a_port_belongs() -> None:
    stream_builder = StreamBuilder("rig")
    inverter = stream_builder.add(FrameInverter)
    reader = stream_builder.add(BrightnessReader)

    with pytest.raises(TypeError, match="connect's source must name an output port"):
        stream_builder.connect(inverter, reader.input("video_from_upstream"))  # pyright: ignore[reportArgumentType]


def test_connect_refuses_a_node_this_stream_does_not_hold_naming_the_ones_it_does() -> (
    None
):
    stream_builder = StreamBuilder("rig")
    reader = stream_builder.add(BrightnessReader)
    stream_builder.add(FrameInverter)
    elsewhere = StreamBuilder("elsewhere").add(FrameFilters.FrameDarkener)

    with pytest.raises(ValueError) as refusal:
        stream_builder.connect(
            elsewhere.output("video_to_downstream"),
            reader.input("video_from_upstream"),
        )

    message = str(refusal.value)
    assert "`framedarkener`" in message
    assert "`rig`" in message
    assert "brightnessreader, frameinverter" in message


def test_expose_refuses_what_is_not_a_local_output_naming_the_fix() -> None:
    stream_builder = StreamBuilder("rig")
    inverter = stream_builder.add(FrameInverter)

    for not_a_local_output in (
        inverter.input("video_from_upstream"),
        inverter,
    ):
        with pytest.raises(TypeError, match=r"`stream_builder\.add\(\.\.\.\)\.output\(port_name\)`"):
            stream_builder.expose(not_a_local_output)  # pyright: ignore[reportArgumentType]


def test_expose_refuses_a_node_this_stream_does_not_hold() -> None:
    stream_builder = StreamBuilder("rig")
    stream_builder.add(FrameInverter)

    with pytest.raises(ValueError, match="`framedarkener`"):
        stream_builder.expose(NodeOutputPortReference("framedarkener", "video_to_downstream"))


def test_exposing_one_output_twice_is_refused_at_the_second_naming_it() -> None:
    stream_builder = StreamBuilder("rig")
    inverter = stream_builder.add(FrameInverter)
    stream_builder.expose(inverter.output("video_to_downstream"))

    with pytest.raises(ValueError) as refusal:
        stream_builder.expose(inverter.output("VIDEO_TO_DOWNSTREAM"))

    message = str(refusal.value)
    assert "`video_to_downstream`" in message
    assert "`frameinverter`" in message
    assert "already exposes" in message


@pytest.mark.parametrize("case", PORT_ADDRESS_CASES_THAT_PARSE, ids=port_address_case_id)
def test_remote_output_names_the_port_a_fixture_address_parses_to(
    case: "dict[str, Any]",
) -> None:
    remote_output = StreamBuilder("rig").remote_output(case["address"])

    assert (
        remote_output.machine_name,
        remote_output.stream_name,
        remote_output.node_name,
        remote_output.port_name,
    ) == (case.get("machine"), case.get("stream"), case["node"], case["port"])


@pytest.mark.parametrize("case", PORT_ADDRESS_CASES_THAT_ARE_REFUSED, ids=port_address_case_id)
def test_remote_output_refuses_a_fixture_address_naming_no_port_where_it_is_written(
    case: "dict[str, Any]",
) -> None:
    with pytest.raises(InvalidPortAddressError) as refusal:
        StreamBuilder("rig").remote_output(case["address"])

    assert repr(case["address"]) in str(refusal.value)


def test_remote_output_refuses_a_chunk_casting_to_nothing_naming_its_part() -> None:
    with pytest.raises(InvalidPortAddressError) as refusal:
        StreamBuilder("rig").remote_output("main/../video")

    assert str(refusal.value).startswith(
        "the node chunk '..' of the port address 'main/../video' cannot name a node: "
    )


def test_remote_output_refuses_an_address_that_is_not_a_string() -> None:
    with pytest.raises(TypeError) as refusal:
        StreamBuilder("rig").remote_output(7)  # pyright: ignore[reportArgumentType]

    assert str(refusal.value) == (
        "a port address is a str, `<stream>/<node>/<port>`; got 7, of type `int` — pass "
        "the address as a str"
    )


def test_an_address_reference_holds_its_address_cast() -> None:
    remote_output = StreamBuilder("rig").remote_output("Main Stream/Camera Source 2/Video")

    assert remote_output.address == "main-stream/camera-source-2/video"
    assert remote_output == NodeOutputPortReferenceByAddress("main-stream/camera-source-2/video")
    assert repr(remote_output) == (
        "NodeOutputPortReferenceByAddress(address='main-stream/camera-source-2/video', "
        "machine_name=None, stream_name='main-stream', node_name='camera-source-2', "
        "port_name='video')"
    )
    with pytest.raises(AttributeError):
        remote_output.stream_name = "other"  # pyright: ignore[reportAttributeAccessIssue]


def test_a_link_from_another_streams_output_compiles_to_its_stream_node_and_port() -> None:
    stream_builder = StreamBuilder("rig")
    inverter = stream_builder.add(FrameInverter)

    stream_builder.connect(
        stream_builder.remote_output("Main/Camera/Video"), inverter.input("video_from_upstream")
    )

    assert stream_builder._compiled_graph()["links"] == [
        {
            "source": {"stream": "main", "node": "camera", "port": "video"},
            "target": {"node": "frameinverter", "port": "video_from_upstream"},
        }
    ]


@pytest.mark.parametrize(
    ("address", "machine_name"),
    [
        ("desk/main/camera/video", "desk"),
        ("desk.tail2950db.ts.net/main/camera/video", "desk.tail2950db.ts.net"),
        ("Rig/rig/frameinverter/video_to_downstream", "rig"),
    ],
    ids=["machine", "full-tailnet-name", "naming-this-stream"],
)
def test_an_address_naming_a_machine_compiles_with_its_machine_chunk(
    address: str, machine_name: str
) -> None:
    stream_builder = StreamBuilder("rig")
    reader = stream_builder.add(BrightnessReader)
    remote_output = stream_builder.remote_output(address)

    stream_builder.connect(remote_output, reader.input("video_from_upstream"))

    assert stream_builder._compiled_graph()["links"][0]["source"] == {
        "machine": machine_name,
        "stream": remote_output.stream_name,
        "node": remote_output.node_name,
        "port": remote_output.port_name,
    }


@pytest.mark.parametrize(
    "address",
    ["FrameInverter/Video_To_Downstream", "Rig/FrameInverter/Video_To_Downstream"],
    ids=["node-and-port", "this-stream"],
)
def test_an_address_in_this_stream_compiles_to_a_link_within_it(address: str) -> None:
    stream_builder = StreamBuilder("Rig")
    stream_builder.add(FrameInverter)
    reader = stream_builder.add(BrightnessReader)

    stream_builder.connect(
        stream_builder.remote_output(address), reader.input("video_from_upstream")
    )

    assert stream_builder._compiled_graph()["links"] == [
        {
            "source": {"node": "frameinverter", "port": "video_to_downstream"},
            "target": {"node": "brightnessreader", "port": "video_from_upstream"},
        }
    ]


def test_an_address_in_this_stream_naming_a_node_it_does_not_hold_is_refused_at_connect() -> (
    None
):
    stream_builder = StreamBuilder("rig")
    reader = stream_builder.add(BrightnessReader)

    with pytest.raises(ValueError) as refusal:
        stream_builder.connect(
            stream_builder.remote_output("rig/camera/video"), reader.input("video_from_upstream")
        )

    message = str(refusal.value)
    assert "node `camera` is not in stream `rig`" in message
    assert "brightnessreader" in message


def test_a_stream_reading_another_compiles_to_its_graph() -> None:
    graph = compile_stream_to_graph(reads_another_stream)

    assert graph["links"] == [
        {
            "source": {
                "stream": "camera-rig",
                "node": "frameinverter",
                "port": "video_to_downstream",
            },
            "target": {"node": "frameinverter", "port": "video_from_upstream"},
        },
        {
            "source": {"node": "frameinverter", "port": "video_to_downstream"},
            "target": {"node": "brightnessreader", "port": "video_from_upstream"},
        },
    ]
    assert json.loads(json.dumps(graph)) == graph


def test_a_stream_loaded_under_the_name_its_address_names_links_within_itself() -> None:
    graph = compile_stream_to_graph(reads_another_stream, name="Camera Rig")

    assert graph["links"][0]["source"] == {
        "node": "frameinverter",
        "port": "video_to_downstream",
    }


def test_connect_refuses_an_address_as_its_destination_because_a_link_is_only_pulled() -> (
    None
):
    stream_builder = StreamBuilder("rig")
    inverter = stream_builder.add(FrameInverter)

    with pytest.raises(TypeError) as refusal:
        stream_builder.connect(
            inverter.output("video_to_downstream"),
            stream_builder.remote_output("main/reader/video_from_upstream"),  # pyright: ignore[reportArgumentType]
        )

    message = str(refusal.value)
    assert message.startswith("connect's destination must name an input port")
    assert "A link is only pulled — the stream that owns the input creates it" in message
    assert "no reference names another stream's input" in message
    assert (
        'its source `stream_builder.remote_output("rig/frameinverter/video_to_downstream")`, '
        "once this stream exposes that output with `stream_builder.expose(...)`."
    ) in message


@pytest.mark.parametrize(
    ("source_address", "fix_the_refusal_ends_with"),
    [
        ("a/camera/video", 'its source `stream_builder.remote_output("a/camera/video")`.'),
        (
            "FrameInverter/Video_To_Downstream",
            'its source `stream_builder.remote_output("rig/frameinverter/video_to_downstream")`, '
            "once this stream exposes that output with `stream_builder.expose(...)`.",
        ),
    ],
    ids=["another-stream", "this-stream"],
)
def test_connect_refuses_addresses_at_both_ends_naming_the_source_as_the_reader_writes_it(
    source_address: str, fix_the_refusal_ends_with: str
) -> None:
    stream_builder = StreamBuilder("rig")
    stream_builder.add(FrameInverter)

    with pytest.raises(TypeError) as refusal:
        stream_builder.connect(
            stream_builder.remote_output(source_address),
            stream_builder.remote_output("b/reader/video_from_upstream"),  # pyright: ignore[reportArgumentType]
        )

    message = str(refusal.value)
    assert "no reference names another stream's input" in message
    assert message.endswith(fix_the_refusal_ends_with)


def test_a_refused_connect_records_no_link() -> None:
    stream_builder = StreamBuilder("rig")
    inverter = stream_builder.add(FrameInverter)

    with pytest.raises(TypeError):
        stream_builder.connect(
            inverter.output("video_to_downstream"),
            stream_builder.remote_output("main/reader/video_from_upstream"),  # pyright: ignore[reportArgumentType]
        )

    assert stream_builder._compiled_graph()["links"] == []


def test_expose_refuses_an_address_naming_the_stream_that_exposes_it() -> None:
    stream_builder = StreamBuilder("rig")
    stream_builder.add(FrameInverter)

    with pytest.raises(TypeError) as refusal:
        stream_builder.expose(
            stream_builder.remote_output("main/camera/video")  # pyright: ignore[reportArgumentType]
        )

    message = str(refusal.value)
    assert "`stream_builder.add(...).output(port_name)`" in message
    assert "a stream exposes only its own nodes' outputs" in message
    assert "`main/camera/video` is exposed in the function of the stream that adds its node" in (
        message
    )
    assert stream_builder._compiled_graph()["exposed"] == []


def test_a_node_reference_casts_the_port_names_it_names() -> None:
    inverter = NodeReference("frameinverter")

    assert inverter.output("VIDEO_TO_DOWNSTREAM") == NodeOutputPortReference(
        "frameinverter", "video_to_downstream"
    )
    assert inverter.input("Video From Upstream") == NodeInputPortReference(
        "frameinverter", "video-from-upstream"
    )


def test_a_port_name_casting_to_nothing_is_refused_naming_it() -> None:
    with pytest.raises(ExposedNameCastsToNothingError, match=r"'\.\.'"):
        NodeReference("frameinverter").output("..")


def test_a_hand_built_reference_is_cast_like_a_minted_one() -> None:
    stream_builder = StreamBuilder("rig")
    inverter = stream_builder.add(FrameInverter)

    assert NodeReference("Frame Inverter").name == "frame-inverter"
    assert NodeReference("FrameInverter") == inverter
    assert NodeOutputPortReference(
        "FrameInverter", "Video To Downstream"
    ) == inverter.output("video to downstream")
    assert NodeInputPortReference("FRAMEINVERTER", "VIDEO_FROM_UPSTREAM") == (
        NodeInputPortReference("frameinverter", "video_from_upstream")
    )


@pytest.mark.parametrize(
    "build_the_reference",
    [
        lambda: NodeReference("✨"),
        lambda: NodeOutputPortReference("frameinverter", ".."),
        lambda: NodeInputPortReference("---", "video_from_upstream"),
    ],
    ids=["node", "output-port", "input-node"],
)
def test_a_hand_built_reference_casting_to_nothing_is_refused_at_construction(
    build_the_reference: Any,
) -> None:
    with pytest.raises(ExposedNameCastsToNothingError):
        build_the_reference()


def test_a_hand_built_reference_given_a_name_that_is_not_a_string_is_refused() -> None:
    with pytest.raises(TypeError, match="port name"):
        NodeInputPortReference("frameinverter", 7)  # pyright: ignore[reportArgumentType]


def test_a_hand_built_reference_reaches_the_graph_cast() -> None:
    assert compile_stream_to_graph(hand_built_references) == {
        "stream": "hand_built_references",
        "nodes": [
            {"name": "frameinverter", "type": FRAME_INVERTER_TYPE, "config": {}},
            {"name": "brightnessreader", "type": BRIGHTNESS_READER_TYPE, "config": {}},
        ],
        "links": [
            {
                "source": {"node": "frameinverter", "port": "video_to_downstream"},
                "target": {"node": "brightnessreader", "port": "video_from_upstream"},
            },
        ],
        "exposed": [
            {"node": "frameinverter", "port": "video_to_downstream", "level": "private"}
        ],
    }


def test_exposing_a_hand_built_output_then_its_minted_twin_is_refused_at_the_second() -> (
    None
):
    stream_builder = StreamBuilder("rig")
    inverter = stream_builder.add(FrameInverter)
    stream_builder.expose(NodeOutputPortReference("FrameInverter", "Video To Downstream"))

    with pytest.raises(ValueError, match="already exposes"):
        stream_builder.expose(inverter.output("video to downstream"))


def test_references_are_immutable_values() -> None:
    reference = NodeReference("frameinverter")

    assert reference == NodeReference("frameinverter")
    assert repr(reference) == "NodeReference(name='frameinverter')"
    with pytest.raises(AttributeError):
        reference.name = "other"  # pyright: ignore[reportAttributeAccessIssue]


def test_a_stream_compiles_to_its_graph() -> None:
    graph = compile_stream_to_graph(camera_rig)

    assert graph == {
        "stream": "camera_rig",
        "nodes": [
            {
                "name": "testpatternsource",
                "type": TEST_PATTERN_SOURCE_TYPE,
                "config": {"width": 1280, "height": 720},
            },
            {"name": "frameinverter", "type": FRAME_INVERTER_TYPE, "config": {}},
            {"name": "brightnessreader", "type": BRIGHTNESS_READER_TYPE, "config": {}},
            {"name": "backup-meter", "type": BRIGHTNESS_READER_TYPE, "config": {}},
            {"name": "frameinverter-2", "type": FRAME_INVERTER_TYPE, "config": {}},
            {
                "name": "h264encoder",
                "type": H264_ENCODER_TYPE,
                "config": {"bitrate_bps": 4_000_000},
            },
        ],
        "links": [
            {
                "source": {"node": "testpatternsource", "port": "video"},
                "target": {"node": "frameinverter", "port": "video_from_upstream"},
            },
            {
                "source": {"node": "frameinverter", "port": "video_to_downstream"},
                "target": {"node": "brightnessreader", "port": "video_from_upstream"},
            },
            {
                "source": {"node": "frameinverter", "port": "video_to_downstream"},
                "target": {"node": "backup-meter", "port": "video_from_upstream"},
            },
            {
                "source": {"node": "testpatternsource", "port": "video"},
                "target": {"node": "frameinverter-2", "port": "video_from_upstream"},
            },
            {
                "source": {"node": "frameinverter-2", "port": "video_to_downstream"},
                "target": {"node": "h264encoder", "port": "video"},
            },
        ],
        "exposed": [
            {"node": "frameinverter", "port": "video_to_downstream", "level": "private"},
            {"node": "frameinverter-2", "port": "video_to_downstream", "level": "private"},
        ],
    }
    assert json.loads(json.dumps(graph)) == graph


def test_a_compiled_graph_has_exactly_the_four_keys() -> None:
    graph = compile_stream_to_graph(every_kind_of_node)

    assert list(graph) == ["stream", "nodes", "links", "exposed"]
    assert graph["stream"] == "every_kind_of_node"
    assert graph["links"] == []
    assert graph["exposed"] == []


def test_a_stream_that_adds_nothing_compiles_to_an_empty_graph() -> None:
    assert compile_stream_to_graph(adds_nothing) == {
        "stream": "adds_nothing",
        "nodes": [],
        "links": [],
        "exposed": [],
    }


def test_each_compile_returns_a_fresh_graph() -> None:
    first = compile_stream_to_graph(a_configured_stream)
    first["nodes"][0]["config"]["overlay"]["labels"].append("mutated")

    assert compile_stream_to_graph(a_configured_stream)["nodes"][0]["config"][
        "overlay"
    ]["labels"] == ["left", "right"]


def test_a_stream_function_that_raises_propagates_its_exception_unchanged() -> None:
    with pytest.raises(StreamBuildingFailure) as raised:
        compile_stream_to_graph(a_stream_that_raises)

    assert raised.value is THE_STREAM_BUILDING_FAILURE


def test_a_builder_refusal_inside_a_stream_function_propagates() -> None:
    with pytest.raises(ValueError, match="'front-CAMERA'"):
        compile_stream_to_graph(a_stream_with_a_typed_duplicate)


def test_a_name_given_at_compile_overrides_the_functions_and_is_cast() -> None:
    graph = compile_stream_to_graph(every_kind_of_node, name="Front Camera Rig")

    assert graph["stream"] == "front-camera-rig"


def test_a_name_given_at_compile_casting_to_nothing_is_refused_naming_it() -> None:
    with pytest.raises(ExposedNameCastsToNothingError, match="'---'"):
        compile_stream_to_graph(every_kind_of_node, name="---")


def test_a_function_that_is_not_a_stream_is_refused_naming_the_decorator() -> None:
    with pytest.raises(TypeError, match="decorate it with @stream"):
        compile_stream_to_graph(not_decorated)


def test_stream_stamps_identity_name_and_description() -> None:
    assert getattr(every_kind_of_node, "__streamlib_stream_identity__") == (
        "test_stream_graph_builder:every_kind_of_node"
    )
    assert getattr(every_kind_of_node, "__streamlib_stream_name__") == (
        "every_kind_of_node"
    )
    assert getattr(every_kind_of_node, "__streamlib_stream_description__") == (
        "A `@node` class, a nested one and a marker-shaped class.\n\n"
        "The second line of the description."
    )


def test_stream_returns_the_function_it_declares_unchanged() -> None:
    namespace: "dict[str, Any]" = {"__name__": "rig_streams"}
    exec("def main(stream_builder):\n    pass\n", namespace)
    undeclared_function = namespace["main"]

    assert stream(undeclared_function) is undeclared_function


def test_a_stream_with_no_docstring_has_an_empty_description() -> None:
    assert getattr(a_stream_that_raises, "__streamlib_stream_description__") == ""


def test_a_stream_in_the_entry_file_is_named_from_main() -> None:
    entry_file_namespace: "dict[str, Any]" = {"__name__": "__main__"}
    exec("def main(stream_builder):\n    pass\n", entry_file_namespace)

    entry_file_stream = stream(entry_file_namespace["main"])

    assert (
        getattr(entry_file_stream, "__streamlib_stream_identity__") == "__main__:main"
    )
    assert getattr(entry_file_stream, "__streamlib_stream_name__") == "main"


@pytest.mark.parametrize(
    "misuse",
    [
        lambda: stream(),  # pyright: ignore[reportCallIssue]
        lambda: stream(name="rig"),  # pyright: ignore[reportCallIssue]
        lambda: stream(not_decorated, name="rig"),  # pyright: ignore[reportCallIssue]
    ],
    ids=["no-arguments", "a-name", "a-function-and-a-name"],
)
def test_stream_called_with_arguments_is_refused_naming_the_bare_form(
    misuse: Any,
) -> None:
    with pytest.raises(TypeError) as refusal:
        misuse()

    assert str(refusal.value).startswith(
        "@stream takes no arguments: the name is the function's, overridden at load "
        "with `--name`"
    )
    assert "`def main(stream_builder: StreamBuilder) -> None:`" in str(refusal.value)


def test_a_nested_function_is_refused_as_not_module_level() -> None:
    def main(stream_builder: StreamBuilder) -> None:
        stream_builder.add(FrameInverter)

    with pytest.raises(TypeError) as refusal:
        stream(main)

    message = str(refusal.value)
    assert "a stream is a module-level function" in message
    assert "<locals>.main" in message


def test_a_method_is_refused_as_not_module_level() -> None:
    namespace: "dict[str, Any]" = {"__name__": "rig_streams"}
    exec("class Streams:\n    def main(stream_builder):\n        pass\n", namespace)

    with pytest.raises(TypeError) as refusal:
        stream(namespace["Streams"].main)

    message = str(refusal.value)
    assert "a stream is a module-level function" in message
    assert "`rig_streams:Streams.main`" in message


@pytest.mark.parametrize(
    "source",
    [
        "def main():\n    pass\n",
        "def main(stream, extra):\n    pass\n",
        "def main(stream, *, extra=1):\n    pass\n",
        "def main(*streams):\n    pass\n",
        "def main(*, stream):\n    pass\n",
        "def main(**streams):\n    pass\n",
    ],
    ids=[
        "none",
        "two",
        "a-keyword-too",
        "var-positional",
        "keyword-only",
        "var-keyword",
    ],
)
def test_a_function_not_taking_exactly_one_positional_parameter_is_refused(
    source: str,
) -> None:
    namespace: "dict[str, Any]" = {"__name__": "rig_streams"}
    exec(source, namespace)

    with pytest.raises(TypeError, match="exactly one positional parameter") as refusal:
        stream(namespace["main"])
    assert "(stream_builder: StreamBuilder) -> None:" in str(refusal.value)


@pytest.mark.parametrize(
    "callable_but_not_a_plain_function",
    [UndecoratedFilter, len, StreamBuilder("rig").add],
    ids=["a-class", "a-builtin", "a-bound-method"],
)
def test_a_callable_that_is_not_a_plain_function_is_refused_as_what_it_is(
    callable_but_not_a_plain_function: Any,
) -> None:
    with pytest.raises(TypeError) as refusal:
        stream(callable_but_not_a_plain_function)

    message = str(refusal.value)
    assert message.startswith("@stream decorates a plain module-level function")
    assert "@stream takes no arguments" not in message


@pytest.mark.parametrize(
    "not_callable",
    ["rig", UndecoratedFilter()],
    ids=["a-string", "an-instance"],
)
def test_what_is_not_callable_is_refused_naming_the_bare_form(
    not_callable: Any,
) -> None:
    with pytest.raises(TypeError) as refusal:
        stream(not_callable)

    message = str(refusal.value)
    assert message.startswith("@stream decorates a plain module-level function")
    assert "@stream takes no arguments: the name is the function's" in message


@pytest.mark.parametrize(
    ("class_member_decorator", "descriptor_type_as_written"),
    [
        ("classmethod", "classmethod"),
        ("staticmethod", "staticmethod"),
        ("property", "property"),
        ("functools.cached_property", "functools.cached_property"),
    ],
    ids=["classmethod", "staticmethod", "property", "cached-property"],
)
def test_stream_stacked_over_a_class_member_decorator_is_refused_naming_it(
    class_member_decorator: str, descriptor_type_as_written: str
) -> None:
    namespace: "dict[str, Any]" = {
        "__name__": "rig_streams",
        "functools": functools,
        "stream": stream,
    }

    with pytest.raises(TypeError) as refusal:
        exec(
            f"@stream\n@{class_member_decorator}\ndef main(stream_builder):\n    pass\n",
            namespace,
        )

    message = str(refusal.value)
    assert message.startswith("@stream decorates a plain module-level function")
    assert (
        f"is of type `{descriptor_type_as_written}`, which makes a class member of what it "
        f"wraps. Put `@stream` on a plain module-level `def`, with no "
        f"`@{descriptor_type_as_written}` under it."
    ) in message
    assert "@stream takes no arguments" not in message


def test_a_lambda_is_refused_naming_def() -> None:
    namespace: "dict[str, Any]" = {"__name__": "rig_streams"}
    exec("main = lambda stream: None\n", namespace)

    with pytest.raises(TypeError, match="`def`"):
        stream(namespace["main"])


@pytest.mark.parametrize(
    "source",
    [
        "async def main(stream_builder):\n    pass\n",
        "def main(stream_builder):\n    yield stream_builder\n",
        "async def main(stream_builder):\n    yield stream_builder\n",
    ],
    ids=["async", "generator", "async-generator"],
)
def test_a_function_whose_call_runs_none_of_its_body_is_refused(source: str) -> None:
    namespace: "dict[str, Any]" = {"__name__": "rig_streams"}
    exec(source, namespace)

    with pytest.raises(TypeError, match="runs none of its body"):
        stream(namespace["main"])


def test_is_stream_function_tells_a_stream_from_everything_else() -> None:
    assert is_stream_function(every_kind_of_node)
    assert not is_stream_function(names_of_nodes_added)
    assert not is_stream_function(UndecoratedFilter)
    assert not is_stream_function(None)
    assert not is_stream_function("every_kind_of_node")


def test_the_builder_is_on_the_public_surface_and_the_cli_helper_is_not() -> None:
    for public_name in (
        "NodeInputPortReference",
        "NodeOutputPortReference",
        "NodeOutputPortReferenceByAddress",
        "NodeReference",
        "StreamBuilder",
        "compile_stream_to_graph",
        "stream",
    ):
        assert public_name in tatolab.stream.__all__
    assert "is_stream_function" not in tatolab.stream.__all__


def test_the_builder_is_not_importable_as_stream() -> None:
    assert "Stream" not in tatolab.stream.__all__
    with pytest.raises(ImportError, match="'Stream'"):
        exec("from tatolab.stream import Stream", {})


def test_the_builder_module_imports_nothing_native() -> None:
    module = ast.parse(STREAM_GRAPH_BUILDER_SOURCE.read_text(encoding="utf-8"))
    imported: "list[tuple[int, str]]" = []
    for statement in ast.walk(module):
        if isinstance(statement, ast.Import):
            imported.extend((0, alias.name) for alias in statement.names)
        elif isinstance(statement, ast.ImportFrom):
            imported.append((statement.level, statement.module or ""))

    assert imported
    for level, module_name in imported:
        if level == 0:
            assert module_name.split(".")[0] in sys.stdlib_module_names, module_name
        else:
            assert (level, module_name) in {
                (1, "_exposed_name_cast"),
                (1, "_built_in_node"),
                (1, "_node_declaration"),
                (1, "_port_address"),
            }, module_name
