# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The built-in node classes `tatolab.stream` publishes, generated from the
runtime's own descriptors: each named by the type the runtime registers, each
compiled into a graph by pure Python. That the generated types are exactly the
ones the runtime registers is tested against the runtime."""

import inspect

import pytest

import tatolab.stream
from tatolab.stream import _built_in_nodes
from tatolab.stream._built_in_node import BuiltInNode
from tatolab.stream._stream_graph_builder import graph_generator, run_graph_generator
from tatolab.stream._typed_node_pins import TypedNodePins

BUILT_IN_NODE_TYPE_PREFIX = "tatolab.stream:"

GENERATED_BUILT_IN_NODE_CLASSES = [
    exported
    for exported in vars(_built_in_nodes).values()
    if inspect.isclass(exported)
    and issubclass(exported, BuiltInNode)
    and exported is not BuiltInNode
]


def test_the_generated_module_holds_every_built_in():
    assert len(GENERATED_BUILT_IN_NODE_CLASSES) == 13


@pytest.mark.parametrize(
    "built_in_node_class", GENERATED_BUILT_IN_NODE_CLASSES, ids=lambda cls: cls.__name__
)
def test_a_built_in_is_named_by_its_own_class_in_the_stream_package(built_in_node_class):
    class_name = built_in_node_class.__name__
    assert built_in_node_class.type == f"{BUILT_IN_NODE_TYPE_PREFIX}{class_name}"
    assert getattr(tatolab.stream, class_name) is built_in_node_class
    assert class_name in tatolab.stream.__all__


# The built-ins whose every port states one pin type and which take no config: each declares
# its fields and pins and publishes no config class.
TYPED_BUILT_IN_NODE_CLASS_NAMES = {
    "AudioToTensor",
    "BooleanToFloat",
    "BooleanToInteger",
    "FloatToBoolean",
    "FloatToInteger",
    "IntegerToBoolean",
    "IntegerToFloat",
    "TensorToAudio",
    "TensorToTexture",
    "TextureFormatConvert",
    "TextureToTensor",
}


@pytest.mark.parametrize(
    "built_in_node_class", GENERATED_BUILT_IN_NODE_CLASSES, ids=lambda cls: cls.__name__
)
def test_a_typed_built_in_declares_fields_and_pins_and_one_with_a_config_its_config(
    built_in_node_class,
):
    class_name = built_in_node_class.__name__
    typed = class_name in TYPED_BUILT_IN_NODE_CLASS_NAMES
    assert (TypedNodePins.of(built_in_node_class, class_name) is not None) == typed
    assert (f"{class_name}Config" in tatolab.stream.__all__) != typed
    assert hasattr(_built_in_nodes, f"{class_name}Config") != typed


def test_a_typed_built_ins_settings_are_fields_read_at_setup_with_their_defaults():
    pins = TypedNodePins.of(tatolab.stream.TensorToAudio, "TensorToAudio")
    assert pins is not None
    assert pins.stored_input_values(
        {"attribute": "gains", "channel_names": ["left", "right"], "sample_rate": 44_100}
    ) == {"attribute": "gains", "channel_names": ["left", "right"], "sample_rate": 44_100}
    assert tatolab.stream.TensorToAudio.sample_rate == 48_000
    assert tatolab.stream.TensorToTexture.format == "rgba8_unorm"
    inputs = {
        port["name"]: port
        for port in getattr(tatolab.stream.TensorToTexture, "__tatolab_node_input_ports__")
    }
    assert inputs["format"]["input_mode"] == {"mode": "setting_only", "read": "at_setup"}
    assert inputs["format"]["pin_declaration"]["pin_type"]["pin_type"]["type"] == "menu"


@pytest.mark.parametrize(
    "built_in_node_class", GENERATED_BUILT_IN_NODE_CLASSES, ids=lambda cls: cls.__name__
)
def test_the_built_in_class_cannot_be_instantiated(built_in_node_class):
    with pytest.raises(TypeError):
        built_in_node_class()


def test_a_built_in_is_never_constructed_and_says_how_to_add_it():
    with pytest.raises(TypeError, match=r"stream_builder\.add\(ChannelsToRecords, input_values="):
        tatolab.stream.ChannelsToRecords()  # pyright: ignore[reportCallIssue]


@graph_generator
def a_channels_to_records_conversion(stream_builder: tatolab.stream.StreamBuilder) -> None:
    stream_builder.add(
        _built_in_nodes.ChannelsToRecords,
        config={"channel_names": ["level"]},
    )


def test_a_stream_of_built_ins_compiles_with_no_runtime_installed():
    graph = run_graph_generator(a_channels_to_records_conversion)

    assert [(node["name"], node["type"]) for node in graph["nodes"]] == [
        ("channelstorecords", "tatolab.stream:ChannelsToRecords"),
    ]
    assert graph["nodes"][0]["input_values"] == {"channel_names": ["level"]}


@graph_generator
def a_control_value_round_trip(stream_builder: tatolab.stream.StreamBuilder) -> None:
    to_channels = stream_builder.add(
        tatolab.stream.RecordsToChannels,
        config={"channel_names": ["gain"], "sample_rate": 30},
    )
    to_array = stream_builder.add(tatolab.stream.AudioToTensor)
    back_to_channels = stream_builder.add(
        tatolab.stream.TensorToAudio,
        input_values={"channel_names": ["gain"], "sample_rate": 30},
    )
    to_records = stream_builder.add(
        tatolab.stream.ChannelsToRecords, config={"channel_names": ["gain"]}
    )
    stream_builder.connect(to_channels.output("channels"), to_array.input("channels"))
    stream_builder.connect(to_array.output("array"), back_to_channels.input("array"))
    stream_builder.connect(back_to_channels.output("channels"), to_records.input("channels"))


def test_the_conversion_built_ins_compile_with_no_runtime_installed():
    graph = run_graph_generator(a_control_value_round_trip)

    assert [node["type"] for node in graph["nodes"]] == [
        "tatolab.stream:RecordsToChannels",
        "tatolab.stream:AudioToTensor",
        "tatolab.stream:TensorToAudio",
        "tatolab.stream:ChannelsToRecords",
    ]
    assert graph["nodes"][2]["input_values"] == {"channel_names": ["gain"], "sample_rate": 30}


@pytest.mark.parametrize(
    ("input_values", "refused"),
    [
        ({"sample_rate": "fast"}, r"input `sample_rate` is set 'fast'"),
        ({"format": "nv12"}, r"input `format` is set 'nv12'"),
        ({"gain": 2.0}, r"declares no input `gain`"),
    ],
)
def test_a_typed_built_in_refuses_a_value_its_input_does_not_take(input_values, refused):
    node_class = (
        tatolab.stream.TensorToTexture if "format" in input_values else tatolab.stream.TensorToAudio
    )

    with pytest.raises((TypeError, ValueError), match=refused):
        tatolab.stream.StreamBuilder("rig").add(node_class, input_values=input_values)


def test_a_typed_built_in_refuses_a_config():
    with pytest.raises(TypeError, match=r"`TensorToTexture` is a typed node"):
        tatolab.stream.StreamBuilder("rig").add(
            tatolab.stream.TensorToTexture,
            config={"format": "bgra8_unorm"},  # pyright: ignore[reportArgumentType]
        )
