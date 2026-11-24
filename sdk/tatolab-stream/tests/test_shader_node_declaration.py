# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`@node.shader` reads a signature over Images and values into a node carrying its Slang source, and refuses by name what is not one."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Annotated

import pytest
from tatolab.stream import (
    ChannelsBlock,
    Color,
    Description,
    ImageFrame,
    ShaderNode,
    StreamBuilder,
    Texture,
    Time,
    Vector2,
    Vector3,
    Vector4,
    node,
)
from tatolab.stream._node_declaration import (
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
            {"port": "amount", "value_type": "float", "pin_type": {"type": "float"}},
        ],
        "output_port": "result",
    }


@node.shader(BLEND_SLANG.replace("blend", "blend_of_textures"))
def blend_of_textures(
    a: Texture, b: Annotated[Texture, Description("The picture faded in.")], amount: float = 0.5
) -> Texture: ...


def test_a_texture_reads_as_the_image_an_image_frame_does():
    texture_input_ports = getattr(blend_of_textures, NODE_DECLARATION_INPUT_PORTS_STAMP)
    assert texture_input_ports == [
        {**port, "description": "The picture faded in." if port["name"] == "b" else ""}
        for port in getattr(blend, NODE_DECLARATION_INPUT_PORTS_STAMP)
    ]
    assert getattr(blend_of_textures, NODE_DECLARATION_OUTPUT_PORTS_STAMP) == getattr(
        blend, NODE_DECLARATION_OUTPUT_PORTS_STAMP
    )
    assert (
        getattr(blend_of_textures, NODE_DECLARATION_SHADER_SIGNATURE_STAMP)["inputs"]
        == getattr(blend, NODE_DECLARATION_SHADER_SIGNATURE_STAMP)["inputs"]
    )


def test_a_shader_node_stamps_no_config():
    """A shader takes each value as an input, so it stamps no config schema.

    Mental-revert: stamping the empty config again sets the attribute on every shader node.
    """
    assert not hasattr(blend, "__tatolab_node_config_schema__")


def test_a_shader_node_is_never_called_in_python():
    with pytest.raises(TypeError, match="never called in Python"):
        blend()


def test_a_shader_node_is_added_to_a_stream_like_any_node():
    stream_builder = StreamBuilder("shaders")
    stream_builder.add(blend)


def tint(image: ImageFrame) -> ImageFrame: ...


def flat(level: float = 1.0) -> ImageFrame: ...


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
    with pytest.raises(TypeError, match="takes no `Texture`"):
        node.shader("float4 flat(float level) { return float4(level); }")(flat)


def test_an_input_that_is_neither_an_image_nor_a_float_is_refused_naming_it():
    with pytest.raises(TypeError, match="input `level` is typed `ChannelsBlock`"):
        node.shader("float4 levels(ImageInput image) { return image.pixel; }")(levels)


def test_a_return_that_is_not_one_image_is_refused():
    with pytest.raises(TypeError, match="must return one `Texture`"):
        node.shader("float4 brightness(ImageInput image) { return image.pixel; }")(brightness)


def test_time_and_state_are_refused():
    with pytest.raises(TypeError, match="`Time` or `State"):
        node.shader("float4 animated(ImageInput image) { return image.pixel; }")(animated)


def test_a_config_is_refused_pointing_at_value_inputs():
    with pytest.raises(TypeError, match="takes `config`, and a node takes each setting as an input"):
        node.shader("float4 configured(ImageInput image) { return image.pixel; }")(configured)


def test_a_function_defined_inside_another_is_refused():
    def nested(image: ImageFrame) -> ImageFrame: ...

    with pytest.raises(TypeError, match="module-level `def`"):
        node.shader("float4 nested(ImageInput image) { return image.pixel; }")(nested)


def test_the_decorator_takes_the_source_never_the_function():
    with pytest.raises(TypeError, match="Slang source as its one argument"):
        node.shader(blend)  # type: ignore[arg-type]


TINT_AND_SHIFT_SLANG = """
float4 tint_and_shift(ImageInput image, float4 tint_color, float2 shift, float3 gain, float4 mix)
{
    return image.sample(image.uv + shift) * tint_color * float4(gain, 1.0) * mix;
}
"""


@node.shader(TINT_AND_SHIFT_SLANG)
def tint_and_shift(
    image: ImageFrame,
    tint_color: Color = Color(1.0, 0.5, 0.25),
    shift: Vector2 = Vector2(0.0, 0.0),
    gain: Vector3 = Vector3(1.0, 1.0, 1.0),
    mix: Vector4 = Vector4(1.0, 1.0, 1.0, 1.0),
) -> ImageFrame: ...


def test_a_color_and_vectors_are_typed_pins_whose_signature_states_their_pin_types():
    signature = getattr(tint_and_shift, NODE_DECLARATION_SHADER_SIGNATURE_STAMP)
    assert signature["inputs"] == [
        {"port": "image", "value_type": "image_frame"},
        {"port": "tint_color", "value_type": "color", "pin_type": {"type": "color"}},
        {
            "port": "shift",
            "value_type": "vector2",
            "pin_type": {"type": "vector", "component_count": "two"},
        },
        {
            "port": "gain",
            "value_type": "vector3",
            "pin_type": {"type": "vector", "component_count": "three"},
        },
        {
            "port": "mix",
            "value_type": "vector4",
            "pin_type": {"type": "vector", "component_count": "four"},
        },
    ]
    input_ports = {port["name"]: port for port in getattr(tint_and_shift, NODE_DECLARATION_INPUT_PORTS_STAMP)}
    assert "pin_declaration" not in input_ports["image"]
    assert input_ports["tint_color"]["family"] == {"family": "channels"}
    assert input_ports["tint_color"]["pin_declaration"]["pin_type"] == {
        "kind": "is",
        "pin_type": {"type": "color"},
    }
    assert input_ports["tint_color"]["pin_default"] == {
        "red": 1.0,
        "green": 0.5,
        "blue": 0.25,
        "alpha": 1.0,
    }
    assert input_ports["shift"]["pin_default"] == {"x": 0.0, "y": 0.0}


def integer_steps(image: ImageFrame, steps: int = 4) -> ImageFrame: ...


def test_a_value_a_shader_holds_no_push_constant_for_is_refused_naming_its_types():
    with pytest.raises(TypeError, match="input `steps` is a int.*`Color`, `Vector2`"):
        node.shader("float4 integer_steps(ImageInput image) { return image.pixel; }")(
            integer_steps
        )
