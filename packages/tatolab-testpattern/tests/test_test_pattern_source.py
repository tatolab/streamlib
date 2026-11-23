# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`TestPatternSource`'s declaration, settings, placements and push-constant block, with no GPU.

The pixels are proven on a runtime by the integration suite's
`tests/stream-on-runtime/test_test_pattern_module.py`.
"""

import importlib.metadata
import struct
from pathlib import Path

import tatolab.testpattern
from tatolab.stream._class_node_construction import construct_class_node
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_EXECUTION_STAMP,
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)
from tatolab.stream._node_type_source_reader import read_retired_declarations_from_module_source
from tatolab.stream import StreamBuilder, run_graph_generator, graph_generator
from tatolab.testpattern import (
    LARGEST_TEST_PATTERN_FRAME_SIDE_PIXELS,
    POTATO_SPRITE,
    SAMPLE_STREAM_CAPTION_PANEL,
    OverlayTopLeftInTheFrame,
    TestPatternSource,
    caption_panel_top_left,
    pack_push_constants_for_frame,
    potato_sprite_top_left_at_frame,
    compute_kernel_slang_rendering_a_test_pattern_frame,
)


def the_port(name: str) -> dict:
    ports = [
        *getattr(TestPatternSource, NODE_DECLARATION_INPUT_PORTS_STAMP),
        *getattr(TestPatternSource, NODE_DECLARATION_OUTPUT_PORTS_STAMP),
    ]
    return next(port for port in ports if port["name"] == name)


def test_the_node_is_a_continuous_source_with_one_rgba8_texture_output():
    output_ports = getattr(TestPatternSource, NODE_DECLARATION_OUTPUT_PORTS_STAMP)

    assert [port["name"] for port in output_ports] == ["video"]
    assert the_port("video")["pin_declaration"]["pin_type"] == {
        "kind": "is",
        "pin_type": {"type": "texture", "pixel_formats": ["rgba8_unorm"]},
    }
    assert getattr(TestPatternSource, NODE_DECLARATION_EXECUTION_STAMP) == {
        "mode": "continuous",
        "interval_ms": 33,
    }


def test_the_module_declares_its_node_in_no_retired_form():
    module_path = Path(tatolab.testpattern.__file__)

    assert (
        read_retired_declarations_from_module_source(
            "tatolab.testpattern", str(module_path), module_path.read_text()
        )
        == []
    )


@graph_generator
def a_test_pattern(stream_builder: StreamBuilder) -> None:
    stream_builder.add(TestPatternSource, input_values={"width": 640})


def test_a_stream_names_the_source_by_the_type_the_module_publishes():
    graph = run_graph_generator(a_test_pattern)

    assert [(node["name"], node["type"], node["input_values"]) for node in graph["nodes"]] == [
        ("testpatternsource", "tatolab.testpattern:TestPatternSource", {"width": 640})
    ]


def test_the_frame_size_defaults_to_720p_and_is_an_integer_input_of_1_to_16k_pixels_read_each_frame():
    for name, default, description in [
        ("width", 1280, "Frame width in pixels."),
        ("height", 720, "Frame height in pixels."),
    ]:
        port = the_port(name)
        assert port["pin_declaration"]["pin_type"]["pin_type"]["type"] == "integer", name
        assert port["pin_default"] == default, name
        assert port["description"] == description, name
        assert port["pin_declaration"]["annotations"]["range"] == {
            "minimum": 1,
            "maximum": LARGEST_TEST_PATTERN_FRAME_SIDE_PIXELS,
        }, name
        assert port["input_mode"] == {"mode": "wire_or_value"}, name


def test_a_source_built_with_a_frame_size_renders_it():
    source = construct_class_node(
        TestPatternSource, field_values={"width": 640, "height": 360}, pins={"video": object()}
    )

    assert (source.width, source.height) == (640, 360)


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
    kernel_source = compute_kernel_slang_rendering_a_test_pattern_frame()

    for binding_declaration in (
        "[[vk::binding(0, 0)]] Sampler2D potato_sprite_texture;",
        "[[vk::binding(1, 0)]] Sampler2D caption_panel_texture;",
        '[[vk::binding(2, 0)]] [format("rgba8")] RWTexture2D<float4> test_pattern_frame;',
        "[numthreads(16, 16, 1)]",
    ):
        assert binding_declaration in kernel_source


def test_the_distribution_lists_the_source_in_the_node_catalog_entry_point_group():
    node_entry_points = importlib.metadata.entry_points(group="tatolab.nodes")
    test_pattern_entry_point = next(
        entry_point for entry_point in node_entry_points if entry_point.name == "TestPatternSource"
    )

    assert test_pattern_entry_point.load() is TestPatternSource
