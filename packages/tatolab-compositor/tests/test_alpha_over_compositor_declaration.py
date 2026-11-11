# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`AlphaOverCompositor`'s declaration, config and push-constant block, with no GPU.

The pixels are proven on a runtime by the integration suite's
`test_alpha_over_compositor.py`.
"""

import importlib.metadata
import struct

import pytest

from tatolab.compositor import (
    ALPHA_OVER_LAYER_COUNT,
    AlphaOverCompositor,
    AlphaOverCompositorConfig,
    AlphaOverLayerPlacement,
)
from tatolab.compositor.alpha_over_compositor import (
    alpha_over_compute_kernel_slang,
    pack_layer_placements,
)
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_CONFIG_SCHEMA_STAMP,
    NODE_DECLARATION_EXECUTION_STAMP,
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)


def test_the_node_declares_every_layer_input_back_to_front_and_one_video_output_all_image():
    input_ports = getattr(AlphaOverCompositor, NODE_DECLARATION_INPUT_PORTS_STAMP)
    output_ports = getattr(AlphaOverCompositor, NODE_DECLARATION_OUTPUT_PORTS_STAMP)

    assert [port["name"] for port in input_ports] == [
        f"layer_{index}" for index in range(ALPHA_OVER_LAYER_COUNT)
    ]
    assert {port["delivery_profile"] for port in input_ports} == {"newest"}
    assert [port["name"] for port in output_ports] == ["video"]
    assert all(port["family"] == {"family": "image"} for port in input_ports)
    assert output_ports[0]["family"] == {"family": "image", "formats": ["rgba8_unorm"]}
    assert getattr(AlphaOverCompositor, NODE_DECLARATION_EXECUTION_STAMP)["mode"] == "reactive"


def test_placements_arrive_as_maps_and_are_cast_to_placements():
    config = AlphaOverCompositorConfig(
        width=640, height=360, layers=[{"x": 10, "y": -4, "opacity": 0.5}]  # type: ignore[list-item]
    )

    assert config.layers == [AlphaOverLayerPlacement(x=10, y=-4, opacity=0.5)]
    assert config.placement_of_layer(0) == AlphaOverLayerPlacement(10, -4, 0.5)
    assert config.placement_of_layer(3) == AlphaOverLayerPlacement()


@pytest.mark.parametrize(
    "config_keys, refusal_names",
    [
        ({"width": 0}, "width"),
        ({"height": True}, "height"),
        ({"layers": [{}] * (ALPHA_OVER_LAYER_COUNT + 1)}, "layer_7"),
        ({"layers": [{"opacity": 1.5}]}, "opacity"),
        ({"layers": [{"x": 1.5}]}, "whole canvas pixels"),
        ({"layers": ["over there"]}, "layers[0]"),
    ],
)
def test_a_malformed_config_is_refused_naming_what_to_fix(config_keys, refusal_names):
    with pytest.raises(ValueError, match=refusal_names.replace("[", r"\[").replace("]", r"\]")):
        AlphaOverCompositorConfig(**config_keys)


def test_the_published_config_schema_bounds_each_layers_opacity_and_the_canvas_size():
    config_schema = getattr(AlphaOverCompositor, NODE_DECLARATION_CONFIG_SCHEMA_STAMP)
    properties = config_schema["properties"]
    opacity = properties["layers"]["items"]["properties"]["opacity"]

    assert (opacity["minimum"], opacity["maximum"]) == (0.0, 1.0)
    assert opacity["description"] == "0.0 hides the layer, 1.0 shows it as it arrives."
    assert properties["width"]["minimum"] == 1
    assert properties["height"]["minimum"] == 1


def test_the_push_constant_block_packs_each_placement_at_its_std430_offset():
    config = AlphaOverCompositorConfig(
        layers=[AlphaOverLayerPlacement(), AlphaOverLayerPlacement(x=-3, y=7, opacity=0.25)]
    )
    has_a_frame = [True, True] + [False] * (ALPHA_OVER_LAYER_COUNT - 2)

    block = pack_layer_placements(config, has_a_frame)

    assert len(block) == 128
    assert struct.unpack_from("<iifi", block, 0) == (0, 0, 1.0, 1)
    assert struct.unpack_from("<iifi", block, 16) == (-3, 7, 0.25, 1)
    assert struct.unpack_from("<iifi", block, 32) == (0, 0, 1.0, 0)


def test_the_kernel_samples_every_layer_and_writes_one_composite():
    kernel_slang = alpha_over_compute_kernel_slang()

    for index in range(ALPHA_OVER_LAYER_COUNT):
        assert f"Sampler2D layer_{index}_texture;" in kernel_slang
        assert f"placements.layers[{index}]" in kernel_slang
    assert (
        f"[[vk::binding({ALPHA_OVER_LAYER_COUNT}, 0)]] [format(\"rgba8\")] RWTexture2D<float4>"
        in kernel_slang
    )
    assert "above + below * (1.0 - above.a)" in kernel_slang


def test_the_installed_entry_point_names_the_class_by_its_own_import_path():
    """A node is identified by `module:qualname`, so a catalog entry naming it any
    other way cannot be described."""
    (entry_point,) = [
        entry_point
        for entry_point in importlib.metadata.entry_points(group="tatolab.nodes")
        if entry_point.name == "AlphaOverCompositor"
    ]

    assert entry_point.value == (
        f"{AlphaOverCompositor.__module__}:{AlphaOverCompositor.__qualname__}"
    )
