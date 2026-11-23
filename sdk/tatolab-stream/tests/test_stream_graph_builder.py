# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The `StreamBuilder`, with no engine: the names it gives nodes, the classes it
adds, the config it records, and the ports it connects.

A stream compiles to its graph as plain data, so every case is a literal
expectation about that data or a refusal at the line the author wrote.
"""

import math
from collections.abc import Mapping
from enum import Enum, IntEnum
from typing import Any

import numpy
import pytest

import tatolab.stream
from stream_graph_builder_nodes import (
    BrightnessReader,
    FrameFilters,
    FrameInverter,
    TintedLevel,
    remap_onto_a_range,
)
from tatolab.stream import (
    Input,
    NodeInputPortReference,
    NodeOutputPortReference,
    NodeReference,
    Output,
    Packets,
    StreamBuilder,
    run_graph_generator,
    node,
    graph_generator,
)
from tatolab.stream._exposed_name_cast import (
    EXPOSED_NAME_MAXIMUM_LENGTH,
    ExposedNameCastsToNothingError,
)

from stream_graph_builder_streams import (
    BRIGHTNESS_READER_TYPE,
    FRAME_INVERTER_TYPE,
    NESTED_CONFIG,
    RIG_CHANNEL_NAMES,
    TENSOR_TO_AUDIO_TYPE,
    StandInMarker,
    UndecoratedFilter,
    a_configured_stream,
    every_kind_of_node,
    names_of_nodes_added,
)


FRAME_DARKENER_TYPE = "stream_graph_builder_nodes:FrameFilters.FrameDarkener"


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


@graph_generator
def a_native_marker(stream_builder: StreamBuilder) -> None:
    stream_builder.add(tatolab.stream.TensorToAudio, input_values=RIG_CHANNEL_NAMES)


@graph_generator
def hand_built_references(stream_builder: StreamBuilder) -> None:
    """Every port named through a reference built by hand rather than minted."""
    stream_builder.add(FrameInverter)
    stream_builder.add(BrightnessReader)
    stream_builder.connect(
        NodeOutputPortReference("FrameInverter", "VIDEO_TO_DOWNSTREAM"),
        NodeInputPortReference("BrightnessReader", "VIDEO_FROM_UPSTREAM"),
    )


def config_compiled_for(config: "Mapping[str, Any]") -> "dict[str, Any]":
    """The config `run_graph_generator` records for one node added with `config`."""
    namespace: "dict[str, Any]" = {
        "__name__": "rig_streams",
        "StandInMarker": StandInMarker,
        "config_under_test": config,
    }
    exec(
        "def main(stream_builder):\n    stream_builder.add(StandInMarker, config=config_under_test)\n",
        namespace,
    )
    return run_graph_generator(graph_generator(namespace["main"]))["nodes"][0]["input_values"]


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
    assert run_graph_generator(every_kind_of_node)["nodes"] == [
        {"name": "frameinverter", "type": FRAME_INVERTER_TYPE, "input_values": {}},
        {"name": "framedarkener", "type": FRAME_DARKENER_TYPE, "input_values": {}},
        {"name": "standinmarker", "type": StandInMarker.type, "input_values": {}},
    ]


def test_a_native_marker_is_the_type_its_class_carries() -> None:
    assert run_graph_generator(a_native_marker)["nodes"] == [
        {
            "name": "tensortoaudio",
            "type": TENSOR_TO_AUDIO_TYPE,
            "input_values": RIG_CHANNEL_NAMES,
        },
    ]


def test_a_node_class_defined_inside_a_function_is_refused_naming_stream_add() -> None:
    @node.process
    class FunctionLocalFilter:
        video_from_upstream = Input(Packets, delivery="newest")

        video_to_downstream = Output(Packets)

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
        "    @node.process(...)\n"
        "    class EntryFileDetector: ...\n"
        "\n"
        "    # stream.py\n"
        "    from entry_file_detector import EntryFileDetector\n"
    ) in message
    assert message.endswith(
        "The entry file itself may still run as `__main__`; only node classes may "
        "not live in it."
    )


def test_a_function_node_in_the_entry_file_is_refused_naming_its_reader() -> None:
    def entry_file_ease(x: float) -> float:
        return x

    entry_file_ease.__module__ = "__main__"
    entry_file_ease.__qualname__ = "entry_file_ease"
    entry_file_function_node = node.function(entry_file_ease)

    with pytest.raises(ValueError) as refusal:
        StreamBuilder("rig").add(entry_file_function_node)

    message = str(refusal.value)
    assert message.startswith(
        "function node `entry_file_ease` is defined in the entry file, so it identifies as "
        "`__main__:entry_file_ease`"
    )
    assert "runs inside the read of the node it feeds" in message
    assert "    @node.function\n    def entry_file_ease(...): ...\n" in message


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


def test_a_function_node_is_added_by_its_import_path_under_its_cast_name() -> None:
    stream_builder = StreamBuilder("rig")
    remap = stream_builder.add(remap_onto_a_range)
    reader = stream_builder.add(TintedLevel)
    stream_builder.connect(remap.output("result"), reader.input("level"))

    graph = stream_builder._compiled_graph()
    assert [(node["name"], node["type"]) for node in graph["nodes"]][0] == (
        "remap_onto_a_range",
        "stream_graph_builder_nodes:remap_onto_a_range",
    )
    assert graph["links"][0]["source"] == {"node": "remap_onto_a_range", "port": "result"}


def test_an_undecorated_function_is_refused_naming_node_function() -> None:
    with pytest.raises(TypeError, match="`@node.function`"):
        StreamBuilder("rig").add(a_config_holding_itself)  # pyright: ignore[reportArgumentType, reportCallIssue]


def test_an_undecorated_class_is_refused_naming_node() -> None:
    with pytest.raises(TypeError) as refusal:
        StreamBuilder("rig").add(UndecoratedFilter)

    message = str(refusal.value)
    assert "is not a node: decorate the class with `@node.process`" in message
    assert "pass it itself rather than an instance of it" in message


def test_an_instance_is_refused_naming_the_class_itself() -> None:
    with pytest.raises(
        TypeError, match="pass it itself rather than an instance"
    ):
        StreamBuilder("rig").add(FrameInverter())  # pyright: ignore[reportCallIssue, reportArgumentType]


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
        stream_builder.add(StandInMarker, config=config)
    with pytest.raises((TypeError, ValueError)):
        stream_builder.add(StandInMarker, name="Front Camera", config=config)

    assert stream_builder.add(FrameInverter).name == "frameinverter"
    assert stream_builder.add(BrightnessReader, name="Front Camera").name == "front-camera"


def test_config_is_normalised_to_plain_json() -> None:
    config = run_graph_generator(a_configured_stream)["nodes"][0]["input_values"]

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
    graph = run_graph_generator(every_kind_of_node)

    assert [each_node["input_values"] for each_node in graph["nodes"]] == [{}, {}, {}]


def test_a_config_that_is_not_a_mapping_is_refused() -> None:
    with pytest.raises(TypeError, match="config must be a mapping"):
        StreamBuilder("rig").add(StandInMarker, config=[("gain", 2)])  # pyright: ignore[reportCallIssue, reportArgumentType]


def test_a_config_value_json_cannot_carry_is_refused_naming_its_key_path() -> None:
    with pytest.raises(TypeError) as refusal:
        StreamBuilder("rig").add(StandInMarker, config={"overlay": {"labels": ["left", {"right"}]}}
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
        StreamBuilder("rig").add(StandInMarker, config={"enabled": value})

    message = str(refusal.value)
    assert message.startswith(
        f"config must be JSON: `config['enabled']` is of type `{type_as_written}`, and a "
        f"graph carries only dict, list, tuple, str, int, float, bool, None, a "
    )
    assert (
        "convert it with `bool(...)`, `int(...)`, `float(...)` or `str(...)`" in message
    )


def test_a_config_key_that_is_not_a_string_is_refused_naming_its_path() -> None:
    with pytest.raises(TypeError) as refusal:
        StreamBuilder("rig").add(StandInMarker, config={"overlay": {1: "left"}})

    message = str(refusal.value)
    assert message.startswith("config must be JSON:")
    assert "`config['overlay']` has the key 1, of type `int`" in message
    assert "convert it with `str(...)`" in message


def test_a_config_float_json_cannot_carry_is_refused_naming_its_key_path() -> None:
    with pytest.raises(ValueError) as refusal:
        StreamBuilder("rig").add(StandInMarker, config={"gain": math.nan})

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
        StreamBuilder("rig").add(StandInMarker,
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
        StreamBuilder("rig").add(StandInMarker, config={"limits": [0, integer]})

    assert str(refusal.value) == (
        "config integers must fit the graph's 64-bit range: `config['limits'][1]` is "
        "outside -2**63 to 2**64 - 1; carry a value that large as a `str`"
    )


def test_a_config_holding_itself_is_refused_naming_where_it_loops() -> None:
    with pytest.raises(ValueError) as refusal:
        StreamBuilder("rig").add(StandInMarker, config=a_config_holding_itself())

    message = str(refusal.value)
    assert message.startswith("config must be JSON:")
    assert "`config['overlay']['parent']` is `config`" in message
    assert message.endswith(
        "Break the cycle: put the data `config['overlay']['parent']` should carry "
        "there, not the container holding it"
    )


def test_a_list_holding_itself_is_refused_naming_where_it_loops() -> None:
    with pytest.raises(ValueError) as refusal:
        StreamBuilder("rig").add(StandInMarker, config=a_config_holding_a_list_holding_itself()
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
        StreamBuilder("rig").add(StandInMarker,
            config=a_config_nesting_containers_deep(
                CONTAINERS_A_CONFIG_NESTS_AT_MOST_COUNTING_ITSELF + 1
            ),
        )

    deepest_key_path = "config['nested']" + "[0]" * (
        CONTAINERS_A_CONFIG_NESTS_AT_MOST_COUNTING_ITSELF - 1
    )
    assert str(refusal.value) == (
        f"config nests too deep for a graph: `{deepest_key_path}` is a container 124 "
        f"deep counting `config` itself, and `config` nests at most 123 — "
        f"a graph nests at most 126 containers from its root, and the graph, its "
        f"`nodes` list and the node enclose every node's values. Nest the data at most 123 "
        f"containers deep, or carry the deeper part as a `str`"
    )


def test_a_config_nested_5000_deep_is_refused_by_name_rather_than_by_recursion() -> None:
    with pytest.raises(ValueError) as refusal:
        StreamBuilder("rig").add(StandInMarker, config=a_config_nesting_containers_deep(5000))

    assert type(refusal.value) is ValueError
    assert str(refusal.value).startswith("config nests too deep for a graph: ")
    assert "`config` nests at most 123" in str(refusal.value)


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
    assert "NodeInputPortReference" in message


def test_connect_refuses_a_port_address_as_its_source_naming_the_track_receive() -> None:
    stream_builder = StreamBuilder("rig")
    reader = stream_builder.add(BrightnessReader)

    with pytest.raises(TypeError) as refusal:
        stream_builder.connect(
            "main/camera/video",  # pyright: ignore[reportArgumentType]
            reader.input("video_from_upstream"),
        )

    message = str(refusal.value)
    assert message.startswith("connect's source must name an output port of a node this stream")
    assert "`stream_builder.add(...).output(port_name)`" in message
    assert (
        "A link's ends are both in this stream; read another stream's track at a relay, on "
        "this machine or another, through a `MoqTrackReceive` node this stream adds."
    ) in message
    assert stream_builder._compiled_graph()["links"] == []


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
    assert NodeOutputPortReference("FrameInverter", "Video To Downstream") == (
        NodeOutputPortReference("frameinverter", "video to downstream")
    )
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
    assert run_graph_generator(hand_built_references) == {
        "stream": "hand_built_references",
        "nodes": [
            {"name": "frameinverter", "type": FRAME_INVERTER_TYPE, "input_values": {}},
            {"name": "brightnessreader", "type": BRIGHTNESS_READER_TYPE, "input_values": {}},
        ],
        "links": [
            {
                "source": {"node": "frameinverter", "port": "video_to_downstream"},
                "target": {"node": "brightnessreader", "port": "video_from_upstream"},
            },
        ],
    }


def test_references_are_immutable_values() -> None:
    reference = NodeReference("frameinverter")

    assert reference == NodeReference("frameinverter")
    assert repr(reference) == "NodeReference(name='frameinverter')"
    with pytest.raises(AttributeError):
        reference.name = "other"  # pyright: ignore[reportAttributeAccessIssue]
