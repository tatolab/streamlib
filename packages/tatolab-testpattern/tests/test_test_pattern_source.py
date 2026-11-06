# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`TestPatternSource`'s declaration, config, placements and push-constant block, with no GPU.

The pixels are proven on a runtime by the integration suite's
`tests/stream-on-runtime/test_test_pattern_module.py`.
"""

import importlib.metadata
import struct

import pytest

from tatolab.stream._node_config_schema import derive_config_class_json_schema
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_EXECUTION_STAMP,
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)
from tatolab.stream import StreamBuilder, compile_stream_to_graph, stream
from tatolab.testpattern import (
    POTATO_SPRITE,
    SAMPLE_STREAM_CAPTION_PANEL,
    OverlayTopLeftInTheFrame,
    TestPatternSource,
    TestPatternSourceConfig,
    caption_panel_top_left,
    pack_push_constants_for_frame,
    potato_sprite_top_left_at_frame,
    compute_kernel_glsl_rendering_a_test_pattern_frame,
)


def test_the_node_is_a_continuous_source_with_one_rgba8_image_output():
    output_ports = getattr(TestPatternSource, NODE_DECLARATION_OUTPUT_PORTS_STAMP)

    assert getattr(TestPatternSource, NODE_DECLARATION_INPUT_PORTS_STAMP) == []
    assert [port["name"] for port in output_ports] == ["video"]
    assert output_ports[0]["family"] == {"family": "image", "formats": ["rgba8_unorm"]}
    assert getattr(TestPatternSource, NODE_DECLARATION_EXECUTION_STAMP) == {
        "mode": "continuous",
        "interval_ms": 33,
    }


@stream
def a_test_pattern(stream_builder: StreamBuilder) -> None:
    stream_builder.add(TestPatternSource, config={"width": 640})


def test_a_stream_names_the_source_by_the_type_the_module_publishes():
    graph = compile_stream_to_graph(a_test_pattern)

    assert [(node["name"], node["type"], node["config"]) for node in graph["nodes"]] == [
        ("testpatternsource", "tatolab.testpattern:TestPatternSource", {"width": 640})
    ]


def test_the_config_defaults_to_720p_and_publishes_each_fields_type_description_and_default():
    assert (TestPatternSourceConfig().width, TestPatternSourceConfig().height) == (1280, 720)
    config_schema = derive_config_class_json_schema(TestPatternSourceConfig)

    for field_name, default, description in [
        ("width", 1280, "Frame width in pixels."),
        ("height", 720, "Frame height in pixels."),
    ]:
        rendered = config_schema["properties"][field_name]
        assert rendered["type"] == "integer", field_name
        assert rendered["default"] == default, field_name
        assert rendered["description"] == description, field_name


@pytest.mark.parametrize(
    "config_keys, refused_naming",
    [
        ({"width": 0}, "width"),
        ({"height": 16_385}, "height"),
        ({"width": True}, "width"),
        ({"height": 720.0}, "height"),
    ],
)
def test_a_frame_side_the_source_cannot_render_is_refused_naming_it(config_keys, refused_naming):
    with pytest.raises(ValueError, match=refused_naming):
        TestPatternSourceConfig(**config_keys)


def test_a_setting_the_source_does_not_take_is_refused():
    with pytest.raises(TypeError, match="fps"):
        TestPatternSourceConfig(fps=60)  # type: ignore[call-arg]


def test_the_baked_overlays_are_the_extents_their_names_say():
    for overlay in (POTATO_SPRITE, SAMPLE_STREAM_CAPTION_PANEL):
        assert len(overlay.rgba_rows()) == overlay.width * overlay.height * 4
    assert (POTATO_SPRITE.width, POTATO_SPRITE.height) == (160, 160)
    assert (SAMPLE_STREAM_CAPTION_PANEL.width, SAMPLE_STREAM_CAPTION_PANEL.height) == (548, 62)


def test_the_potato_leaves_the_top_left_corner_at_frame_zero():
    assert potato_sprite_top_left_at_frame(0, 1280, 720) == OverlayTopLeftInTheFrame(0, 0)


def test_the_potato_bounces_off_each_edge_it_reaches():
    # 1280 - 160 = 1120 px of horizontal travel at 4 px a frame is 280 frames;
    # 720 - 160 = 560 px of vertical travel is 140.
    def at(frame_index):
        return potato_sprite_top_left_at_frame(frame_index, 1280, 720)

    assert at(140) == OverlayTopLeftInTheFrame(560, 560)
    assert at(141) == OverlayTopLeftInTheFrame(564, 556)
    assert at(280) == OverlayTopLeftInTheFrame(1120, 0)
    assert at(281) == OverlayTopLeftInTheFrame(1116, 4)
    assert at(560) == OverlayTopLeftInTheFrame(0, 0)


def test_the_potato_never_leaves_the_frame():
    for frame_index in range(2000):
        at = potato_sprite_top_left_at_frame(frame_index, 1280, 720)
        assert 0 <= at.left <= 1120 and 0 <= at.top <= 560, (frame_index, at)


def test_a_frame_no_larger_than_the_potato_holds_it_still_at_the_origin():
    for frame_index in (0, 1, 99, 12_345):
        assert potato_sprite_top_left_at_frame(frame_index, 64, 64) == OverlayTopLeftInTheFrame(
            0, 0
        )


def test_a_frame_index_past_any_run_length_still_places_the_potato():
    at = potato_sprite_top_left_at_frame(2**64 - 1, 1280, 720)
    assert 0 <= at.left <= 1120 and 0 <= at.top <= 560


def test_the_caption_panel_sits_inset_from_the_bottom_right_corner():
    assert caption_panel_top_left(1280, 720) == OverlayTopLeftInTheFrame(
        1280 - 548 - 24, 720 - 62 - 24
    )


def test_a_caption_panel_past_a_tiny_frame_sits_at_a_negative_offset_rather_than_wrapping():
    assert caption_panel_top_left(64, 32) == OverlayTopLeftInTheFrame(
        64 - 548 - 24, 32 - 62 - 24
    )


def test_the_push_constant_block_is_the_shaders_ten_words_in_its_order():
    block = pack_push_constants_for_frame(141, 1280, 720)

    assert len(block) == 40
    assert struct.unpack("<IIiiIIiiII", block) == (
        1280,
        720,
        564,
        556,
        160,
        160,
        1280 - 548 - 24,
        720 - 62 - 24,
        548,
        62,
    )


def test_the_kernel_declares_the_bindings_the_node_dispatches_by_name():
    kernel_source = compute_kernel_glsl_rendering_a_test_pattern_frame()

    for binding_declaration in (
        "uniform sampler2D potato_sprite_texture;",
        "uniform sampler2D caption_panel_texture;",
        "rgba8) uniform writeonly image2D test_pattern_frame;",
        "local_size_x = 16, local_size_y = 16",
    ):
        assert binding_declaration in kernel_source


def test_the_distribution_lists_the_source_in_the_node_catalog_entry_point_group():
    node_entry_points = importlib.metadata.entry_points(group="tatolab.nodes")
    test_pattern_entry_point = next(
        entry_point for entry_point in node_entry_points if entry_point.name == "TestPatternSource"
    )

    assert test_pattern_entry_point.load() is TestPatternSource
