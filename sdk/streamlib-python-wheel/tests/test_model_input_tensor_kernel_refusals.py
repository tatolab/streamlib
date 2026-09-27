# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`ModelInputTensorKernel`'s own refusals, geometry and dispatch, with the
capabilities stood in for.

Everything here is decided in the wheel before the GPU runs: the parameters,
the fit's geometry, the boxes mapped back, and the tensor and dispatch the
kernel asks for. The tensor's values against a torch reference need a GPU and
are proven in `test_model_input_tensor_kernel.py`.
"""

from __future__ import annotations

import struct
from typing import Any, cast

import numpy
import pytest

from streamlib import (
    GpuContextFullAccess,
    GpuContextLimitedAccess,
    GpuSurfaceHandle,
    ModelInputTensorGeometry,
    ModelInputTensorKernel,
)


class ComputeKernelStandIn:
    def __init__(self) -> None:
        self.dispatches: "list[dict[str, Any]]" = []

    def dispatch(self, **dispatch: Any) -> None:
        self.dispatches.append(dispatch)


class GpuFullAccessStandIn:
    """Records the kernel the preset asks for and answers a stand-in."""

    def __init__(self) -> None:
        self.kernel_requests: "list[dict[str, Any]]" = []
        self.kernel = ComputeKernelStandIn()

    def create_compute_kernel(self, **request: Any) -> ComputeKernelStandIn:
        self.kernel_requests.append(request)
        return self.kernel


class SurfaceHandleStandIn:
    def __init__(
        self,
        surface_id: str,
        width: int = 0,
        height: int = 0,
        format: str = "rgba32",
        shape: "list[int] | None" = None,
        dtype: "str | None" = None,
    ) -> None:
        self.surface_id = surface_id
        self.width = width
        self.height = height
        self.format = format
        self.shape = shape
        self.dtype = dtype


class GpuLimitedAccessStandIn:
    """Answers numbered surfaces and records every engine call in order."""

    def __init__(self) -> None:
        self.calls: "list[str]" = []
        self.tensor_requests: "list[tuple[int, list[int], str]]" = []

    def acquire_texture_from_processor_output_pool(
        self, pool_key: str, rotation_depth: int, width: int, height: int,
        texture_format: str, usage: "list[str]",
    ) -> SurfaceHandleStandIn:
        self.calls.append(f"acquire {texture_format} {width}x{height}")
        return SurfaceHandleStandIn(f"texture#{len(self.calls)}")

    def copy_surface_to_surface(
        self, source_surface_id: str, destination_surface: SurfaceHandleStandIn
    ) -> None:
        self.calls.append(f"copy {source_surface_id} -> {destination_surface.surface_id}")

    def acquire_storage_buffer_from_processor_output_pool(
        self, pool_key: str, rotation_depth: int, shape: "list[int]", dtype: str
    ) -> SurfaceHandleStandIn:
        self.calls.append(f"acquire tensor {shape} {dtype}")
        self.tensor_requests.append((rotation_depth, shape, dtype))
        return SurfaceHandleStandIn(f"tensor#{len(self.calls)}", shape=shape, dtype=dtype)


def compiled(**parameters: Any) -> "tuple[ModelInputTensorKernel, GpuFullAccessStandIn]":
    gpu = GpuFullAccessStandIn()
    kernel = ModelInputTensorKernel.compile(
        cast(GpuContextFullAccess, gpu), **{"width": 640, "height": 640, **parameters}
    )
    return kernel, gpu


def refusal_of_compile(**parameters: Any) -> str:
    gpu = GpuFullAccessStandIn()
    with pytest.raises(ValueError) as refusal:
        ModelInputTensorKernel.compile(
            cast(GpuContextFullAccess, gpu), **{"width": 640, "height": 640, **parameters}
        )
    assert gpu.kernel_requests == [], "a refusal must fire before the engine is asked"
    return str(refusal.value)


def frame(width: int = 1920, height: int = 1080, format: str = "rgba32") -> GpuSurfaceHandle:
    return cast(GpuSurfaceHandle, SurfaceHandleStandIn("camera#7", width, height, format))


def apply(
    kernel: ModelInputTensorKernel, source: "GpuSurfaceHandle | None" = None
) -> "tuple[Any, GpuLimitedAccessStandIn]":
    gpu = GpuLimitedAccessStandIn()
    model_input = kernel.apply_to_surface(
        cast(GpuContextLimitedAccess, gpu), source if source is not None else frame()
    )
    return model_input, gpu


def refusal_of_apply(kernel: ModelInputTensorKernel, source: GpuSurfaceHandle) -> str:
    gpu = GpuLimitedAccessStandIn()
    with pytest.raises(ValueError) as refusal:
        kernel.apply_to_surface(cast(GpuContextLimitedAccess, gpu), source)
    assert gpu.calls == [], "a source refusal must fire before any GPU work"
    return str(refusal.value)


@pytest.mark.parametrize(
    ("parameter", "value", "allowed"),
    [
        ("fit", "crop", "'stretch', 'letterbox', 'pad_bottom_right'"),
        ("layout", "chw", "'nchw', 'nhwc'"),
        ("dtype", "uint8", "'float32', 'float16'"),
        ("channel_order", "rgba", "'rgb', 'bgr'"),
    ],
)
def test_an_unknown_option_is_refused_naming_the_parameter_and_its_options(
    parameter: str, value: str, allowed: str
) -> None:
    message = refusal_of_compile(**{parameter: value})
    assert f"{parameter} {value!r}" in message
    assert allowed in message


@pytest.mark.parametrize("fit", ["stretch", "letterbox"])
def test_a_pad_to_multiple_of_is_refused_for_a_fit_that_pads_no_edge(fit: str) -> None:
    message = refusal_of_compile(fit=fit, pad_to_multiple_of=32)
    assert "pad_to_multiple_of" in message
    assert "pad_bottom_right" in message


@pytest.mark.parametrize(
    "parameters",
    [{"width": 0}, {"height": -1}, {"width": 640.0}, {"height": True},
     {"fit": "pad_bottom_right", "pad_to_multiple_of": 0}],
)
def test_a_size_that_is_not_a_positive_int_is_refused(parameters: "dict[str, Any]") -> None:
    assert "must be a positive int" in refusal_of_compile(**parameters)


@pytest.mark.parametrize(
    "parameters",
    [{"mean": (0.5, 0.5)}, {"std": "abc"}, {"mean": (0.0, float("nan"), 0.0)},
     {"std": (1.0, 1.0, float("inf"))}],
)
def test_a_mean_or_std_that_is_not_three_finite_numbers_is_refused(
    parameters: "dict[str, Any]",
) -> None:
    assert "three finite numbers" in refusal_of_compile(**parameters)


def test_a_zero_std_is_refused() -> None:
    assert "zero" in refusal_of_compile(std=(0.229, 0.0, 0.225))


def test_a_non_finite_scale_is_refused() -> None:
    assert "scale" in refusal_of_compile(scale=float("inf"))


def test_a_float16_tensor_with_an_odd_element_count_is_refused() -> None:
    message = refusal_of_compile(width=5, height=7, dtype="float16")
    assert "5x7" in message
    assert "float32" in message
    compiled(width=5, height=7, dtype="float32")
    compiled(width=6, height=7, dtype="float16")


def test_a_tensor_surface_is_refused_as_a_source_naming_it() -> None:
    kernel, _ = compiled()
    tensor = cast(
        GpuSurfaceHandle,
        SurfaceHandleStandIn("tensor#3", shape=[1, 3, 640, 640], dtype="float32"),
    )
    message = refusal_of_apply(kernel, tensor)
    assert "'tensor#3'" in message
    assert "not an RGBA frame" in message


@pytest.mark.parametrize("format", ["bgra32", "nv12_video_range", "rgba8_unorm_srgb", "rgba32_float"])
def test_a_source_that_is_not_8_bit_rgba_is_refused_naming_its_format(format: str) -> None:
    kernel, _ = compiled()
    message = refusal_of_apply(kernel, frame(format=format))
    assert "'camera#7'" in message
    assert repr(format) in message
    assert "8-bit RGBA" in message


@pytest.mark.parametrize("format", ["rgba32", "rgba8_unorm"])
def test_a_pixel_buffer_or_a_texture_in_rgba_is_landed_and_dispatched(format: str) -> None:
    kernel, _ = compiled()
    model_input, gpu = apply(kernel, frame(format=format))
    assert gpu.calls == [
        "acquire rgba8_unorm 1920x1080",
        "copy camera#7 -> texture#1",
        "acquire tensor [1, 3, 640, 640] float32",
    ]
    assert model_input.tensor_surface.surface_id == "tensor#3"


@pytest.mark.parametrize(
    ("layout", "dtype", "expected_shape"),
    [
        ("nchw", "float32", [1, 3, 640, 640]),
        ("nhwc", "float16", [1, 640, 640, 3]),
    ],
)
def test_the_tensor_comes_from_the_kernels_output_pool_in_its_layout(
    layout: str, dtype: str, expected_shape: "list[int]"
) -> None:
    kernel, _ = compiled(layout=layout, dtype=dtype)
    assert kernel.tensor_shape == expected_shape
    assert kernel.tensor_dtype == dtype
    _, gpu = apply(kernel)
    assert gpu.tensor_requests == [(2, expected_shape, dtype)]


def test_pad_to_multiple_of_rounds_the_tensor_extent_up() -> None:
    kernel, _ = compiled(width=1920, height=1080, fit="pad_bottom_right", pad_to_multiple_of=32)
    assert kernel.tensor_shape == [1, 3, 1088, 1920]


def test_the_kernel_binds_the_landed_source_and_the_tensor_by_their_reflected_names() -> None:
    kernel, gpu = compiled()
    assert gpu.kernel_requests[0]["bindings"] == {
        "streamlib_source": "sampled_texture",
        "streamlib_model_input_tensor": "storage_buffer",
    }
    assert gpu.kernel_requests[0]["push_constant_size"] == 16
    _, limited = apply(kernel)
    bindings = gpu.kernel.dispatches[0]["bindings"]
    assert bindings["streamlib_source"].surface_id == "texture#1"
    assert bindings["streamlib_model_input_tensor"].surface_id == "tensor#3"


def test_the_fit_reaches_the_kernel_as_resized_extent_then_pad_offset() -> None:
    kernel, gpu = compiled(fit="letterbox")
    apply(kernel)
    assert struct.unpack("<4i", gpu.kernel.dispatches[0]["push_constants"]) == (640, 360, 0, 140)


@pytest.mark.parametrize(
    ("width", "height", "dtype", "expected_group_count"),
    [
        # 3 * 8 * 8 = 192 words: three workgroups of 64.
        (8, 8, "float32", (3, 1, 1)),
        # 192 elements packed two to a word: 96 words, two workgroups.
        (8, 8, "float16", (2, 1, 1)),
        # 3 * 640 * 640 = 1,228,800 words: rows of 65,536, the last partial.
        (640, 640, "float32", (1024, 19, 1)),
    ],
)
def test_the_dispatch_covers_every_word_of_the_tensor(
    width: int, height: int, dtype: str, expected_group_count: "tuple[int, int, int]"
) -> None:
    kernel, gpu = compiled(width=width, height=height, dtype=dtype)
    apply(kernel)
    assert gpu.kernel.dispatches[0]["group_count"] == expected_group_count


def test_the_affine_and_channel_order_are_compiled_into_the_kernel() -> None:
    _, gpu = compiled(channel_order="bgr", scale=0.5, mean=(1.0, 2.0, 3.0), std=(4.0, 5.0, 6.0))
    source = gpu.kernel_requests[0]["source"]
    assert "const float SCALE = float(0.5);" in source
    assert "const vec3 MEAN = vec3(float(1.0), float(2.0), float(3.0));" in source
    assert "const vec3 STD = vec3(float(4.0), float(5.0), float(6.0));" in source
    assert "const ivec3 SOURCE_CHANNEL_BY_OUTPUT_CHANNEL = ivec3(2, 1, 0);" in source


@pytest.mark.parametrize(
    ("fit", "source_extent", "expected"),
    [
        ("stretch", (1920, 1080), (640, 640, 0, 0)),
        ("letterbox", (1920, 1080), (640, 360, 0, 140)),
        ("letterbox", (480, 640), (480, 640, 80, 0)),
        ("pad_bottom_right", (1920, 1080), (640, 360, 0, 0)),
        ("pad_bottom_right", (320, 320), (640, 640, 0, 0)),
    ],
)
def test_each_fit_places_the_source_in_the_tensor(
    fit: str, source_extent: "tuple[int, int]", expected: "tuple[int, int, int, int]"
) -> None:
    kernel, _ = compiled(fit=fit)
    model_input, _ = apply(kernel, frame(*source_extent))
    geometry = model_input.geometry
    assert (geometry.source_width, geometry.source_height) == source_extent
    assert (
        geometry.resized_width,
        geometry.resized_height,
        geometry.pad_left,
        geometry.pad_top,
    ) == expected


def test_a_letterboxed_box_maps_exactly_back_to_the_source() -> None:
    kernel, _ = compiled(fit="letterbox")
    model_input, _ = apply(kernel, frame(1920, 1080))
    assert model_input.geometry.boxes_to_source([[100, 240, 200, 340]]) == [
        [300.0, 300.0, 600.0, 600.0]
    ]


def test_a_stretched_box_maps_back_per_axis() -> None:
    kernel, _ = compiled(fit="stretch")
    model_input, _ = apply(kernel, frame(1280, 320))
    assert model_input.geometry.boxes_to_source([(64, 64, 320, 128)]) == [
        [128.0, 32.0, 640.0, 64.0]
    ]


def test_a_box_padded_to_a_multiple_is_already_in_frame_coordinates() -> None:
    kernel, _ = compiled(width=1920, height=1080, fit="pad_bottom_right", pad_to_multiple_of=32)
    model_input, _ = apply(kernel, frame(1920, 1080))
    boxes = numpy.array([[10, 20, 1900, 1070], [0, 0, 1, 1]], dtype=numpy.int64)
    numpy.testing.assert_array_equal(model_input.geometry.boxes_to_source(boxes), boxes)


def test_an_array_of_boxes_maps_to_a_new_float_array_of_the_same_shape() -> None:
    geometry = ModelInputTensorGeometry(1920, 1080, 640, 360, 0, 140)
    boxes = numpy.array([[[100, 240, 200, 340]]], dtype=numpy.int32)
    mapped = geometry.boxes_to_source(boxes)
    assert mapped.shape == (1, 1, 4)
    assert mapped.dtype == numpy.float64
    numpy.testing.assert_array_equal(mapped, [[[300.0, 300.0, 600.0, 600.0]]])
    numpy.testing.assert_array_equal(boxes, [[[100, 240, 200, 340]]])


def test_a_torch_tensor_of_boxes_maps_on_its_own_device() -> None:
    torch = pytest.importorskip("torch")
    geometry = ModelInputTensorGeometry(1920, 1080, 640, 360, 0, 140)
    mapped = geometry.boxes_to_source(torch.tensor([[100.0, 240.0, 200.0, 340.0]]))
    assert isinstance(mapped, torch.Tensor)
    assert mapped.tolist() == [[300.0, 300.0, 600.0, 600.0]]


@pytest.mark.parametrize("boxes", [numpy.zeros((2, 5)), numpy.float64(3.0), [[1, 2, 3]]])
def test_boxes_that_are_not_xyxy_are_refused(boxes: Any) -> None:
    geometry = ModelInputTensorGeometry(1920, 1080, 640, 360, 0, 140)
    with pytest.raises(ValueError, match="xyxy"):
        geometry.boxes_to_source(boxes)
