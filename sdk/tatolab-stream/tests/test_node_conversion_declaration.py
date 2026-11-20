# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`@node.meta(conversion=True)` states that a node converts the pin type of its one input that takes a wire into that of its one output, and every other shape is refused at decoration."""

from __future__ import annotations

from typing import Annotated, Any

import pytest

from tatolab.stream import (
    EncodedVideo,
    Image,
    ImageFrame,
    Input,
    InputPin,
    NodeDeclarationRefusal,
    Output,
    OutputPin,
    SettingOnly,
    Texture,
    node,
)
from tatolab.stream._node_declaration import NODE_DECLARATION_CONVERSION_STAMP


def _slang_passing_the_image_on(function_name: str) -> str:
    return f"float4 {function_name}(ImageInput image) {{ return image.pixel; }}"


@node.meta(description="Encodes pictures as H.264.", conversion=True)
@node(execution="manual")
class H264EncoderStatingItsMetaAbove:
    bitrate_bps: Annotated[int, SettingOnly()] = 4_000_000
    video: InputPin[Texture]
    encoded_video: OutputPin[EncodedVideo]


@node(execution="manual")
@node.meta(conversion=True)
class H264EncoderStatingItsMetaBelow:
    bitrate_bps: Annotated[int, SettingOnly()] = 4_000_000
    video: InputPin[Texture]
    encoded_video: OutputPin[EncodedVideo]


@node.meta(conversion=True)
@node
def level_as_text_stating_its_meta_above(level: float = 0.0) -> str:
    return f"{level:.2f}"


@node
@node.meta(conversion=True)
def level_as_text_stating_its_meta_below(level: float = 0.0) -> str:
    return f"{level:.2f}"


@node.meta(conversion=True)
@node.shader(_slang_passing_the_image_on("picture_passed_on_stating_its_meta_above"))
def picture_passed_on_stating_its_meta_above(image: ImageFrame) -> ImageFrame: ...


@node.shader(_slang_passing_the_image_on("picture_passed_on_stating_its_meta_below"))
@node.meta(conversion=True)
def picture_passed_on_stating_its_meta_below(image: ImageFrame) -> ImageFrame: ...


@node
def level_doubled(level: float = 0.0) -> float:
    return level * 2.0


# Functions a test declares, each at module scope as a function node must be.


def LevelAsText(level: float = 0.0, places: int = 2) -> str:
    return f"{level:.{places}f}"


def Brighten(image: ImageFrame, amount: float = 0.0) -> ImageFrame: ...


def level_as_count(level: float) -> int:
    return round(level)


@pytest.mark.parametrize(
    "declared",
    [
        H264EncoderStatingItsMetaAbove,
        H264EncoderStatingItsMetaBelow,
        level_as_text_stating_its_meta_above,
        level_as_text_stating_its_meta_below,
        picture_passed_on_stating_its_meta_above,
        picture_passed_on_stating_its_meta_below,
    ],
)
def test_a_class_function_or_shader_stating_a_conversion_is_stamped_as_one(declared: Any):
    assert getattr(declared, NODE_DECLARATION_CONVERSION_STAMP) is True


def test_a_node_that_states_no_conversion_is_stamped_as_none():
    assert getattr(level_doubled, NODE_DECLARATION_CONVERSION_STAMP) is False


def test_a_conversion_keeps_its_category_and_description_beside_it():
    assert H264EncoderStatingItsMetaAbove.__tatolab_node_description__ == (  # pyright: ignore[reportAttributeAccessIssue]
        "Encodes pictures as H.264."
    )


@pytest.mark.parametrize("not_true", [False, 1, "yes"])
def test_conversion_takes_only_true(not_true: object):
    with pytest.raises(TypeError, match="takes `conversion` only as `True`"):
        node.meta(conversion=not_true)  # type: ignore[arg-type]


def test_a_class_with_a_second_input_taking_a_wire_is_refused_naming_it_and_the_fix():
    with pytest.raises(NodeDeclarationRefusal) as refused:

        @node.meta(conversion=True)
        @node(execution="manual")
        class H264Encoder:
            video: InputPin[Texture]
            bitrate_bps: int = 4_000_000
            encoded_video: OutputPin[EncodedVideo]

    assert str(refused.value) == (
        "`H264Encoder` states `conversion=True` and has two inputs that take a wire, `video` and "
        "`bitrate_bps`. To fix: mark `bitrate_bps` as `SettingOnly()`, or leave "
        "`conversion=True` out."
    )
    assert refused.value.finding_code == "conversion_not_one_input_to_one_output"
    assert refused.value.node == "H264Encoder"
    assert refused.value.pin == "bitrate_bps"


def test_a_class_with_two_streams_in_is_refused_with_no_setting_to_mark():
    with pytest.raises(NodeDeclarationRefusal, match="keep one input that takes a wire"):

        @node.meta(conversion=True)
        @node(execution="manual")
        class Mixer:
            video: InputPin[Texture]
            overlay: InputPin[Texture]
            result: OutputPin[Texture]


def test_a_function_with_two_inputs_is_refused_saying_each_takes_a_wire():
    with pytest.raises(NodeDeclarationRefusal) as refused:
        node.meta(conversion=True)(node(LevelAsText))

    assert str(refused.value) == (
        "`LevelAsText` states `conversion=True` and has two inputs that take a wire, `level` and "
        "`places`. To fix: drop `places`, as every input of a function node takes a wire and a "
        "setting is a class node's field marked `SettingOnly()`, or leave `conversion=True` out."
    )


def test_a_shader_with_a_value_beside_its_picture_is_refused():
    shader = node.shader(
        "float4 Brighten(ImageInput image, float amount) { return image.pixel + amount; }"
    )
    with pytest.raises(NodeDeclarationRefusal, match="every input of a shader node takes a wire"):
        shader(node.meta(conversion=True)(Brighten))


def test_a_node_with_no_input_taking_a_wire_is_refused():
    with pytest.raises(NodeDeclarationRefusal, match="has no input that takes a wire"):

        @node.meta(conversion=True)
        @node(execution="continuous", interval_ms=33)
        class Bars:
            width: Annotated[int, SettingOnly()] = 320
            bars: OutputPin[Texture]


def test_a_node_with_two_outputs_is_refused_naming_them():
    with pytest.raises(NodeDeclarationRefusal) as refused:

        @node.meta(conversion=True)
        @node
        class Split:
            video: InputPin[Texture]
            left: OutputPin[Texture]
            right: OutputPin[Texture]

    assert str(refused.value) == (
        "`Split` states `conversion=True` and has two outputs, `left` and `right`. To fix: keep "
        "only the output it converts to, or leave `conversion=True` out."
    )
    assert refused.value.pin == "right"


def test_a_conversion_under_node_process_is_refused_with_its_replacement():
    with pytest.raises(NodeDeclarationRefusal) as refused:

        @node.meta(conversion=True)
        @node.process
        class Decoder:
            encoded = Input(Image, delivery="newest")
            video = Output(Image)

    assert str(refused.value) == (
        "`Decoder` states `conversion=True` under `@node.process`, whose ports declare "
        "families, and a conversion is judged by the pin types its pins state. To fix: declare "
        "it with bare `@node`, its streams as `InputPin[...]` and `OutputPin[...]` and its "
        "settings as fields marked `SettingOnly()`, or leave `conversion=True` out."
    )
    assert refused.value.finding_code == "conversion_under_a_family_grammar"


@pytest.mark.parametrize("meta_above", [True, False])
def test_a_conversion_under_node_function_is_refused_with_its_replacement(meta_above: bool):
    with pytest.raises(NodeDeclarationRefusal, match="under `@node.function`") as refused:
        if meta_above:
            node.meta(conversion=True)(node.function(level_as_count))
        else:
            node.function(node.meta(conversion=True)(level_as_count))

    assert refused.value.finding_code == "conversion_under_a_family_grammar"
