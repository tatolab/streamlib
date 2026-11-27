# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`AlphaOverCompositor`'s declaration, its dispatches and their push-constant blocks, with no GPU.

The pixels are proven on a runtime by the integration suite's
`test_alpha_over_compositor.py`.
"""

import importlib.metadata
import struct
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

import tatolab.compositor.alpha_over_compositor as alpha_over_compositor_module
from tatolab.compositor import AlphaOverCompositor, AlphaOverLayer
from tatolab.compositor.alpha_over_compositor import (
    MOST_TEXTURES_ONE_DISPATCH_LAYERS,
    alpha_over_compute_kernel_slang,
    layers_of_each_dispatch,
    pack_layer_placements,
)
from tatolab.stream._class_node_construction import construct_class_node
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_EXECUTION_STAMP,
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)
from tatolab.stream._node_type_source_reader import read_retired_declarations_from_module_source

PIN_GROUPS_STAMP = "__tatolab_node_pin_groups__"


def test_the_node_declares_its_canvas_size_a_group_of_layers_that_grows_as_it_is_wired_and_one_rgba8_texture_output():
    input_ports = getattr(AlphaOverCompositor, NODE_DECLARATION_INPUT_PORTS_STAMP)
    output_ports = getattr(AlphaOverCompositor, NODE_DECLARATION_OUTPUT_PORTS_STAMP)
    (layers,) = getattr(AlphaOverCompositor, PIN_GROUPS_STAMP)

    assert [(port["name"], port["pin_default"]) for port in input_ports] == [
        ("width", 1920),
        ("height", 1080),
    ]
    assert [port["name"] for port in output_ports] == ["video"]
    assert output_ports[0]["pin_declaration"]["pin_type"] == {
        "kind": "is",
        "pin_type": {"type": "texture", "pixel_formats": ["rgba8_unorm"]},
    }
    assert layers["name"] == "layers"
    assert [port["name"] for port in layers["inputs"]] == ["picture", "x", "y", "opacity"]
    picture = layers["inputs"][0]
    assert picture["pin_declaration"] == {
        "pin_type": {"kind": "is", "pin_type": {"type": "texture"}},
        "flow": "stream",
    }
    assert picture["delivery_profile"] == "newest"
    assert [port["pin_default"] for port in layers["inputs"][1:]] == [0, 0, 1.0]
    assert layers["inputs"][3]["pin_declaration"]["annotations"]["range"] == {
        "minimum": 0.0,
        "maximum": 1.0,
    }
    assert layers["outputs"] == []
    assert getattr(AlphaOverCompositor, NODE_DECLARATION_EXECUTION_STAMP)["mode"] == "reactive"


def test_the_module_declares_its_node_in_no_retired_form():
    module_path = Path(alpha_over_compositor_module.__file__)

    assert (
        read_retired_declarations_from_module_source(
            alpha_over_compositor_module.__name__, str(module_path), module_path.read_text()
        )
        == []
    )


@pytest.mark.parametrize(
    "layer_count, layers_of_each",
    [
        (0, []),
        (1, [range(0, 1)]),
        (8, [range(0, 8)]),
        (9, [range(0, 8), range(8, 9)]),
        (15, [range(0, 8), range(8, 15)]),
        (16, [range(0, 8), range(8, 15), range(15, 16)]),
    ],
)
def test_up_to_eight_layers_take_one_dispatch_and_each_one_after_lays_up_to_seven_more(
    layer_count, layers_of_each
):
    assert layers_of_each_dispatch(layer_count) == layers_of_each


def test_one_dispatchs_push_constant_block_packs_each_placement_at_its_std430_offset():
    block = pack_layer_placements([(0, 0, 1.0, True), (-3, 7, 0.25, True), (0, 0, 1.0, False)])

    assert len(block) == 48
    assert struct.unpack_from("<iifi", block, 0) == (0, 0, 1.0, 1)
    assert struct.unpack_from("<iifi", block, 16) == (-3, 7, 0.25, 1)
    assert struct.unpack_from("<iifi", block, 32) == (0, 0, 1.0, 0)


@pytest.mark.parametrize("texture_count", [1, 3, MOST_TEXTURES_ONE_DISPATCH_LAYERS])
def test_the_kernel_is_generated_for_the_textures_its_dispatch_lays(texture_count):
    kernel_slang = alpha_over_compute_kernel_slang(texture_count)

    for index in range(texture_count):
        assert f"Sampler2D layer_{index}_texture;" in kernel_slang
        assert f"placements.layers[{index}]" in kernel_slang
    assert f"Sampler2D layer_{texture_count}_texture;" not in kernel_slang
    assert f"LayerPlacement layers[{texture_count}];" in kernel_slang
    assert (
        f"[[vk::binding({texture_count}, 0)]] [format(\"rgba8\")] RWTexture2D<float4>"
        in kernel_slang
    )
    assert "above + below * (1.0 - above.a)" in kernel_slang


@pytest.mark.parametrize("texture_count", [0, MOST_TEXTURES_ONE_DISPATCH_LAYERS + 1])
def test_a_kernel_past_the_push_constant_block_is_refused(texture_count):
    with pytest.raises(ValueError, match="1 to 8 textures"):
        alpha_over_compute_kernel_slang(texture_count)


class _OneFramePin:
    """A layer's picture pin holding one frame, or none."""

    def __init__(self, frame: Any) -> None:
        self._frame = frame

    def read(self) -> Any:
        frame, self._frame = self._frame, None
        return frame


class _RecordingKernel:
    def __init__(self, texture_count: int, dispatches: "list[dict[str, Any]]") -> None:
        self.texture_count = texture_count
        self._dispatches = dispatches

    def dispatch(self, bindings, group_count, push_constants=None) -> None:
        self._dispatches.append(
            {"texture_count": self.texture_count, "bindings": bindings, "push_constants": push_constants}
        )


class _RecordingGpu:
    """The GPU as far as the compositor reaches it: each texture a new name, each kernel and dispatch recorded."""

    def __init__(self) -> None:
        self.kernels: "list[dict[str, Any]]" = []
        self.dispatches: "list[dict[str, Any]]" = []
        self._textures_handed_out = 0

    def create_compute_kernel(self, source, push_constant_size, bindings) -> _RecordingKernel:
        texture_count = len(bindings) - 1
        self.kernels.append({"texture_count": texture_count, "push_constant_size": push_constant_size})
        return _RecordingKernel(texture_count, self.dispatches)

    def acquire_texture_from_node_output_pool(self, pool_key, depth, width, height, texture_format, usage):
        self._textures_handed_out += 1
        return SimpleNamespace(
            surface_id=f"texture-{self._textures_handed_out}",
            width=width,
            height=height,
            format=texture_format,
        )

    def copy_surface_to_surface(self, source_surface_id, destination) -> None:
        pass


def _a_compositor_of(layer_count: int, *, frames_on: "set[int]") -> AlphaOverCompositor:
    layers = [
        AlphaOverLayer(
            picture=_OneFramePin(  # type: ignore[arg-type]
                SimpleNamespace(
                    surface_id=f"frame-{index}", width=8, height=8, colour="srgb", timestamp_ns=1000
                )
                if index in frames_on
                else None
            ),
            x=index,
            y=-index,
            opacity=0.5,
        )
        for index in range(layer_count)
    ]
    return construct_class_node(
        AlphaOverCompositor,
        field_values={"width": 32, "height": 16},
        pins={"video": object()},
        pin_groups={"layers": layers},
    )


def test_nine_layers_build_one_kernel_for_eight_textures_and_one_for_two():
    gpu = _RecordingGpu()
    compositor = _a_compositor_of(9, frames_on={0})

    compositor.setup(SimpleNamespace(gpu_limited_access=gpu))  # type: ignore[arg-type]

    assert gpu.kernels == [
        {"texture_count": 8, "push_constant_size": 128},
        {"texture_count": 2, "push_constant_size": 32},
    ]


def test_a_ninth_layer_is_laid_over_the_composite_of_the_first_eight_and_published_on_layer_0s_frame():
    gpu = _RecordingGpu()
    compositor = _a_compositor_of(9, frames_on={0, 8})
    written: "list[tuple[Any, Any]]" = []
    compositor.video = SimpleNamespace(  # type: ignore[assignment]
        write=lambda frame, timestamp_ns: written.append((frame, timestamp_ns))
    )
    compositor.setup(SimpleNamespace(gpu_limited_access=gpu))  # type: ignore[arg-type]

    compositor.process(SimpleNamespace(gpu_limited_access=gpu))  # type: ignore[arg-type]

    first, second = gpu.dispatches
    assert first["texture_count"] == 8
    assert [
        struct.unpack_from("<iifi", first["push_constants"], 16 * index) for index in range(8)
    ] == [(0, 0, 0.5, 1)] + [(index, -index, 0.5, 0) for index in range(1, 8)]
    assert second["texture_count"] == 2
    assert second["bindings"]["layer_0_texture"] == first["bindings"]["composite_output"]
    assert struct.unpack_from("<iifi", second["push_constants"], 0) == (0, 0, 1.0, 1)
    assert struct.unpack_from("<iifi", second["push_constants"], 16) == (8, -8, 0.5, 1)
    ((frame, timestamp_ns),) = written
    assert timestamp_ns == 1000
    assert frame.surface_id == second["bindings"]["composite_output"].surface_id
    assert (frame.width, frame.height) == (32, 16)


def test_a_compositor_with_no_layer_wired_writes_nothing():
    gpu = _RecordingGpu()
    compositor = _a_compositor_of(0, frames_on=set())
    compositor.setup(SimpleNamespace(gpu_limited_access=gpu))  # type: ignore[arg-type]

    compositor.process(SimpleNamespace(gpu_limited_access=gpu))  # type: ignore[arg-type]

    assert (gpu.kernels, gpu.dispatches) == ([], [])


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
