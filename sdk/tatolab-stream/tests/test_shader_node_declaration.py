# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`@node.shader` reads a signature over Images and floats into a node carrying its Slang source, and refuses by name what is not one."""

from __future__ import annotations

from dataclasses import dataclass

import pytest
from tatolab.stream import (
    ChannelsBlock,
    ImageFrame,
    ShaderNode,
    StreamBuilder,
    Time,
    node,
)
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_CONFIG_SCHEMA_STAMP,
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_KIND_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)
from tatolab.stream._shader_node_declaration import (
    NODE_DECLARATION_SHADER_SIGNATURE_STAMP,
    NODE_DECLARATION_SHADER_SOURCE_STAMP,
)

BLEND_SLANG = """
float4 blend(ImageInput a, ImageInput b, float amount)
{
    return lerp(a.pixel, b.pixel, amount);
}
"""


@node.shader(BLEND_SLANG)
def blend(a: ImageFrame, b: ImageFrame, amount: float = 0.5) -> ImageFrame: ...


def test_a_shader_node_carries_kind_shader_and_its_slang_source():
    assert isinstance(blend, ShaderNode)
    assert getattr(blend, NODE_DECLARATION_KIND_STAMP) == "shader"
    assert getattr(blend, NODE_DECLARATION_SHADER_SOURCE_STAMP) == BLEND_SLANG


def test_images_are_image_inputs_floats_are_channels_pins_and_the_return_is_result():
    input_ports = getattr(blend, NODE_DECLARATION_INPUT_PORTS_STAMP)
    assert [(port["name"], port["family"]["family"]) for port in input_ports] == [
        ("a", "image"),
        ("b", "image"),
        ("amount", "channels"),
    ]
    assert input_ports[2]["pin_default"] == 0.5
    assert [
        (port["name"], port["family"]["family"])
        for port in getattr(blend, NODE_DECLARATION_OUTPUT_PORTS_STAMP)
    ] == [("result", "image")]


def test_the_signature_stamp_names_the_function_each_input_in_order_and_the_output():
    assert getattr(blend, NODE_DECLARATION_SHADER_SIGNATURE_STAMP) == {
        "function_name": "blend",
        "inputs": [
            {"port": "a", "value_type": "image_frame"},
            {"port": "b", "value_type": "image_frame"},
            {"port": "amount", "value_type": "float"},
        ],
        "output_port": "result",
    }


def test_a_shader_node_declares_no_config():
    assert getattr(blend, NODE_DECLARATION_CONFIG_SCHEMA_STAMP)["additionalProperties"] is False


def test_a_shader_node_is_never_called_in_python():
    with pytest.raises(TypeError, match="never called in Python"):
        blend()


def test_a_shader_node_is_added_to_a_stream_like_any_node():
    stream_builder = StreamBuilder("shaders")
    stream_builder.add(blend)


def tint(image: ImageFrame) -> ImageFrame: ...


def flat(level: float) -> ImageFrame: ...


def levels(image: ImageFrame, level: ChannelsBlock) -> ImageFrame: ...


def brightness(image: ImageFrame) -> float: ...


def animated(image: ImageFrame, *, now: Time) -> ImageFrame: ...


@dataclass(frozen=True)
class TintConfig:
    amount: float = 1.0


def configured(image: ImageFrame, config: TintConfig) -> ImageFrame: ...


def test_a_source_that_defines_no_function_named_for_the_node_is_refused():
    with pytest.raises(ValueError, match="defines no `float4 tint"):
        node.shader("float4 something_else(ImageInput image) { return image.pixel; }")(tint)


def test_a_shader_with_no_image_input_is_refused():
    with pytest.raises(TypeError, match="takes no `ImageFrame`"):
        node.shader("float4 flat(float level) { return float4(level); }")(flat)


def test_an_input_that_is_neither_an_image_nor_a_float_is_refused_naming_it():
    with pytest.raises(TypeError, match="input `level` is a channels_block"):
        node.shader("float4 levels(ImageInput image) { return image.pixel; }")(levels)


def test_a_return_that_is_not_one_image_is_refused():
    with pytest.raises(TypeError, match="must return one `ImageFrame`"):
        node.shader("float4 brightness(ImageInput image) { return image.pixel; }")(brightness)


def test_time_and_state_are_refused():
    with pytest.raises(TypeError, match="`Time` or `State"):
        node.shader("float4 animated(ImageInput image) { return image.pixel; }")(animated)


def test_a_config_is_refused_pointing_at_float_inputs():
    with pytest.raises(TypeError, match="push constant"):
        node.shader("float4 configured(ImageInput image) { return image.pixel; }")(configured)


def test_a_function_defined_inside_another_is_refused():
    def nested(image: ImageFrame) -> ImageFrame: ...

    with pytest.raises(TypeError, match="module-level `def`"):
        node.shader("float4 nested(ImageInput image) { return image.pixel; }")(nested)


def test_the_decorator_takes_the_source_never_the_function():
    with pytest.raises(TypeError, match="Slang source as its one argument"):
        node.shader(blend)  # type: ignore[arg-type]
