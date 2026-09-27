# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A model's input tensor, prepared from an RGBA frame in one compute pass.

The frame is fitted to the model's input size — stretched, letterboxed, or
padded on the bottom and right — its alpha dropped and its channels put in the
model's order, then written as `(x * scale - mean) / std` into a tensor
surface laid out `nchw` or `nhwc`. The tensor comes from a processor output
pool, so a tensor a consumer still holds is never rewritten, and
`torch.from_dlpack` reads it with no copy. The fit's geometry maps the model's
boxes back to the frame's own coordinates.

Wheel grammar over `create_compute_kernel`, the landing copy
`GlslPixelEffect` shares, `acquire_storage_buffer_from_processor_output_pool`
and `dispatch`; the engine sees nothing but a copy, a kernel and a tensor.
No colour conversion: a YUV frame is converted before it is published.
"""

from __future__ import annotations

import math
import struct
import uuid
from collections.abc import Sequence
from dataclasses import dataclass
from typing import Any, Literal

from ._engine import ComputeKernel, GpuContextFullAccess, GpuSurfaceHandle
from ._sampled_source_landing import (
    SAMPLED_SOURCE_BINDING_NAME,
    GpuContextWithSurfaceCopy,
    SampledSourceLandingTextureRing,
)
from .processor_output_texture_ring import STANDARD_RING_DEPTH

__all__ = [
    "ModelInputTensor",
    "ModelInputTensorChannelOrder",
    "ModelInputTensorDtype",
    "ModelInputTensorFit",
    "ModelInputTensorGeometry",
    "ModelInputTensorKernel",
    "ModelInputTensorLayout",
]

ModelInputTensorFit = Literal["stretch", "letterbox", "pad_bottom_right"]
ModelInputTensorChannelOrder = Literal["rgb", "bgr"]
ModelInputTensorLayout = Literal["nchw", "nhwc"]
ModelInputTensorDtype = Literal["float32", "float16"]

_FITS: "tuple[ModelInputTensorFit, ...]" = ("stretch", "letterbox", "pad_bottom_right")
_SOURCE_CHANNEL_BY_OUTPUT_CHANNEL: "dict[ModelInputTensorChannelOrder, tuple[int, int, int]]" = {
    "rgb": (0, 1, 2),
    "bgr": (2, 1, 0),
}
_LAYOUTS: "tuple[ModelInputTensorLayout, ...]" = ("nchw", "nhwc")
_DTYPES: "tuple[ModelInputTensorDtype, ...]" = ("float32", "float16")

_CHANNEL_COUNT = 3

# A pixel buffer's and a texture's spelling of 8-bit RGBA — the two sources the
# engine copy lands in the kernel's `rgba8_unorm` texture as they are.
_RGBA_SOURCE_FORMATS = ("rgba32", "rgba8_unorm")

_MODEL_INPUT_TENSOR_BINDING_NAME = "streamlib_model_input_tensor"

# `ivec2 resized_extent; ivec2 pad_offset;` — the per-apply fit.
_FIT_PUSH_CONSTANT_FORMAT = "<4i"
_FIT_PUSH_CONSTANT_BLOCK_BYTE_SIZE = struct.calcsize(_FIT_PUSH_CONSTANT_FORMAT)

# One invocation writes one 32-bit word of the tensor: one float32 element,
# or two float16 elements packed, since no 16-bit storage access is assumed.
# Words run in rows of 1024 workgroups, so a large tensor stays inside the
# 65535 workgroups per dimension every device guarantees.
_INVOCATIONS_PER_WORKGROUP = 64
_WORKGROUPS_PER_ROW = 1024
_WORDS_PER_ROW = _INVOCATIONS_PER_WORKGROUP * _WORKGROUPS_PER_ROW
_MAXIMUM_ROWS = 65535


def _is_plain_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _is_finite_number(value: Any) -> bool:
    return (
        isinstance(value, (int, float))
        and not isinstance(value, bool)
        and math.isfinite(value)
    )


def _refuse_unless_one_of(parameter: str, value: Any, allowed: "Sequence[str]") -> None:
    if value not in allowed:
        raise ValueError(
            f"ModelInputTensorKernel.compile: {parameter} {value!r} is not one of "
            f"{', '.join(repr(option) for option in allowed)}"
        )


def _positive_int(parameter: str, value: Any) -> int:
    if not _is_plain_int(value) or value < 1:
        raise ValueError(
            f"ModelInputTensorKernel.compile: {parameter} must be a positive int, "
            f"got {value!r}"
        )
    return value


def _three_finite_numbers(parameter: str, value: Any) -> "tuple[float, float, float]":
    if (
        isinstance(value, (str, bytes))
        or not isinstance(value, Sequence)
        or len(value) != _CHANNEL_COUNT
        or not all(_is_finite_number(component) for component in value)
    ):
        raise ValueError(
            f"ModelInputTensorKernel.compile: {parameter} must be three finite "
            f"numbers, one per output channel in channel_order, got {value!r}"
        )
    return (float(value[0]), float(value[1]), float(value[2]))


def _divided_rounding_up(value: int, divisor: int) -> int:
    return (value + divisor - 1) // divisor


def _rounded_up_to_multiple(value: int, multiple: int) -> int:
    return _divided_rounding_up(value, multiple) * multiple


def _glsl_float(value: float) -> str:
    return f"float({value!r})"


@dataclass(frozen=True)
class ModelInputTensorGeometry:
    """Where the source frame landed in the tensor: its resized extent and its top-left pad."""

    source_width: int
    source_height: int
    resized_width: int
    resized_height: int
    pad_left: int
    pad_top: int

    @property
    def scale_x(self) -> float:
        """Tensor pixels per source pixel, horizontally."""
        return self.resized_width / self.source_width

    @property
    def scale_y(self) -> float:
        """Tensor pixels per source pixel, vertically."""
        return self.resized_height / self.source_height

    def boxes_to_source(self, boxes_xyxy: Any) -> Any:
        """Map `x0, y0, x1, y1` boxes in tensor pixels to the source frame's pixels.

        A torch tensor or numpy array whose last dimension is 4 maps to a
        new floating array of its own kind, on its own device; a sequence
        of four-number boxes maps to a list of lists.
        """
        shape = getattr(boxes_xyxy, "shape", None)
        if shape is not None:
            if len(shape) == 0 or shape[-1] != 4:
                raise ValueError(
                    f"ModelInputTensorGeometry.boxes_to_source: boxes of shape "
                    f"{tuple(shape)} are not xyxy — the last dimension holds "
                    f"x0, y0, x1, y1"
                )
            mapped: Any = boxes_xyxy * 1.0
            mapped[..., 0::2] -= self.pad_left
            mapped[..., 0::2] /= self.scale_x
            mapped[..., 1::2] -= self.pad_top
            mapped[..., 1::2] /= self.scale_y
            return mapped
        mapped_boxes: "list[list[float]]" = []
        for box in boxes_xyxy:
            if len(box) != 4:
                raise ValueError(
                    f"ModelInputTensorGeometry.boxes_to_source: box {box!r} is not "
                    f"xyxy — each box is x0, y0, x1, y1"
                )
            x0, y0, x1, y1 = box
            mapped_boxes.append(
                [
                    (x0 - self.pad_left) / self.scale_x,
                    (y0 - self.pad_top) / self.scale_y,
                    (x1 - self.pad_left) / self.scale_x,
                    (y1 - self.pad_top) / self.scale_y,
                ]
            )
        return mapped_boxes


@dataclass(frozen=True)
class ModelInputTensor:
    """One apply's tensor surface and the geometry that maps its detections back."""

    tensor_surface: GpuSurfaceHandle
    geometry: ModelInputTensorGeometry


@dataclass(frozen=True)
class _ModelInputTensorLayoutPlan:
    """The tensor's extent, the region the fit may fill, and how it is laid out."""

    tensor_width: int
    tensor_height: int
    fit_width: int
    fit_height: int
    fit: ModelInputTensorFit
    layout: ModelInputTensorLayout
    dtype: ModelInputTensorDtype

    @property
    def dimensions(self) -> "list[int]":
        if self.layout == "nchw":
            return [1, _CHANNEL_COUNT, self.tensor_height, self.tensor_width]
        return [1, self.tensor_height, self.tensor_width, _CHANNEL_COUNT]

    @property
    def element_count(self) -> int:
        return _CHANNEL_COUNT * self.tensor_width * self.tensor_height

    @property
    def elements_per_word(self) -> int:
        return 1 if self.dtype == "float32" else 2

    @property
    def word_count(self) -> int:
        return self.element_count // self.elements_per_word

    @property
    def dispatch_group_count(self) -> "tuple[int, int, int]":
        return (
            min(
                _WORKGROUPS_PER_ROW,
                _divided_rounding_up(self.word_count, _INVOCATIONS_PER_WORKGROUP),
            ),
            _divided_rounding_up(self.word_count, _WORDS_PER_ROW),
            1,
        )

    def geometry_for_source(self, source_width: int, source_height: int) -> ModelInputTensorGeometry:
        if self.fit == "stretch":
            return ModelInputTensorGeometry(
                source_width, source_height, self.fit_width, self.fit_height, 0, 0
            )
        scale = min(self.fit_width / source_width, self.fit_height / source_height)
        resized_width = min(self.fit_width, max(1, round(source_width * scale)))
        resized_height = min(self.fit_height, max(1, round(source_height * scale)))
        if self.fit == "pad_bottom_right":
            return ModelInputTensorGeometry(
                source_width, source_height, resized_width, resized_height, 0, 0
            )
        return ModelInputTensorGeometry(
            source_width,
            source_height,
            resized_width,
            resized_height,
            (self.fit_width - resized_width) // 2,
            (self.fit_height - resized_height) // 2,
        )


def _compute_kernel_glsl(
    tensor_layout_plan: _ModelInputTensorLayoutPlan,
    channel_order: ModelInputTensorChannelOrder,
    scale: float,
    mean: "tuple[float, float, float]",
    std: "tuple[float, float, float]",
) -> str:
    width = tensor_layout_plan.tensor_width
    plane = tensor_layout_plan.tensor_width * tensor_layout_plan.tensor_height
    if tensor_layout_plan.layout == "nchw":
        element_position = (
            f"    uint channel = element_index / {plane}u;\n"
            f"    uint pixel_index = element_index % {plane}u;\n"
        )
    else:
        element_position = (
            f"    uint channel = element_index % {_CHANNEL_COUNT}u;\n"
            f"    uint pixel_index = element_index / {_CHANNEL_COUNT}u;\n"
        )
    if tensor_layout_plan.dtype == "float32":
        tensor_word_type = "float"
        write_word = (
            "    streamlib_model_input_tensor.words[word_index] = "
            "model_input_element(word_index);\n"
        )
    else:
        # packHalf2x16 puts its first argument in the low half, which a
        # little-endian tensor reads as the earlier element.
        tensor_word_type = "uint"
        write_word = (
            "    streamlib_model_input_tensor.words[word_index] = packHalf2x16(vec2(\n"
            "        model_input_element(2u * word_index),\n"
            "        model_input_element(2u * word_index + 1u)));\n"
        )
    source_channels = ", ".join(
        str(channel) for channel in _SOURCE_CHANNEL_BY_OUTPUT_CHANNEL[channel_order]
    )
    return (
        "#version 450\n"
        f"layout(local_size_x = {_INVOCATIONS_PER_WORKGROUP}) in;\n"
        f"layout(set = 0, binding = 0) uniform sampler2D {SAMPLED_SOURCE_BINDING_NAME};\n"
        "layout(set = 0, binding = 1, std430) writeonly buffer ModelInputTensorWords {\n"
        f"    {tensor_word_type} words[];\n"
        f"}} {_MODEL_INPUT_TENSOR_BINDING_NAME};\n"
        "layout(push_constant) uniform ModelInputTensorFit {\n"
        "    ivec2 resized_extent;\n"
        "    ivec2 pad_offset;\n"
        "} fit;\n"
        f"const float SCALE = {_glsl_float(scale)};\n"
        f"const vec3 MEAN = vec3({', '.join(_glsl_float(value) for value in mean)});\n"
        f"const vec3 STD = vec3({', '.join(_glsl_float(value) for value in std)});\n"
        f"const ivec3 SOURCE_CHANNEL_BY_OUTPUT_CHANNEL = ivec3({source_channels});\n"
        "float model_input_element(uint element_index) {\n"
        f"{element_position}"
        f"    ivec2 at = ivec2(int(pixel_index % {width}u), int(pixel_index / {width}u));\n"
        "    ivec2 in_resized = at - fit.pad_offset;\n"
        "    float pixel_value = 0.0;\n"
        "    if (all(greaterThanEqual(in_resized, ivec2(0)))\n"
        "            && all(lessThan(in_resized, fit.resized_extent))) {\n"
        "        vec2 uv = (vec2(in_resized) + 0.5) / vec2(fit.resized_extent);\n"
        f"        pixel_value = textureLod({SAMPLED_SOURCE_BINDING_NAME}, uv, 0.0)"
        "[SOURCE_CHANNEL_BY_OUTPUT_CHANNEL[channel]] * 255.0;\n"
        "    }\n"
        "    return (pixel_value * SCALE - MEAN[channel]) / STD[channel];\n"
        "}\n"
        "void main() {\n"
        f"    uint word_index = gl_GlobalInvocationID.y * {_WORDS_PER_ROW}u"
        " + gl_GlobalInvocationID.x;\n"
        f"    if (word_index >= {tensor_layout_plan.word_count}u) {{\n"
        "        return;\n"
        "    }\n"
        f"{write_word}"
        "}\n"
    )


class ModelInputTensorKernel:
    """Prepares a model's input tensor from an RGBA frame on the GPU, frame by frame."""

    def __init__(
        self, compute_kernel: ComputeKernel, tensor_layout_plan: _ModelInputTensorLayoutPlan
    ) -> None:
        self._compute_kernel = compute_kernel
        self._tensor_layout_plan = tensor_layout_plan
        self._dispatch_group_count = tensor_layout_plan.dispatch_group_count
        self._source_landing_ring = SampledSourceLandingTextureRing()
        self._tensor_output_pool_key = f"model-input-tensor-kernel-{uuid.uuid4().hex}"

    @property
    def tensor_shape(self) -> "list[int]":
        """The shape of every tensor this kernel writes, batch of one first."""
        return self._tensor_layout_plan.dimensions

    @property
    def tensor_dtype(self) -> ModelInputTensorDtype:
        """The element type of every tensor this kernel writes."""
        return self._tensor_layout_plan.dtype

    @classmethod
    def compile(
        cls,
        gpu_full_access: GpuContextFullAccess,
        width: int,
        height: int,
        fit: ModelInputTensorFit = "stretch",
        pad_to_multiple_of: "int | None" = None,
        channel_order: ModelInputTensorChannelOrder = "rgb",
        layout: ModelInputTensorLayout = "nchw",
        dtype: ModelInputTensorDtype = "float32",
        scale: float = 1.0 / 255.0,
        mean: "Sequence[float]" = (0.0, 0.0, 0.0),
        std: "Sequence[float]" = (1.0, 1.0, 1.0),
    ) -> "ModelInputTensorKernel":
        """Build the kernel for one model's input, in `setup()`.

        The frame fits `width` x `height`: `stretch` fills it, `letterbox`
        keeps the aspect and pads both sides evenly, and `pad_bottom_right`
        keeps the aspect and pads after the frame — `pad_to_multiple_of`
        then rounds the tensor's extent up. A pixel's 0-255 channel value
        `x` is written as `(x * scale - mean) / std`, per output channel;
        padding is a black pixel through the same affine. Raises naming the
        parameter for an unknown fit, layout, dtype or channel order.
        """
        _refuse_unless_one_of("fit", fit, _FITS)
        _refuse_unless_one_of("channel_order", channel_order, tuple(_SOURCE_CHANNEL_BY_OUTPUT_CHANNEL))
        _refuse_unless_one_of("layout", layout, _LAYOUTS)
        _refuse_unless_one_of("dtype", dtype, _DTYPES)
        fit_width = _positive_int("width", width)
        fit_height = _positive_int("height", height)
        tensor_width, tensor_height = fit_width, fit_height
        if pad_to_multiple_of is not None:
            if fit != "pad_bottom_right":
                raise ValueError(
                    f"ModelInputTensorKernel.compile: pad_to_multiple_of pads after "
                    f"the frame, so it needs fit='pad_bottom_right', not {fit!r}"
                )
            multiple = _positive_int("pad_to_multiple_of", pad_to_multiple_of)
            tensor_width = _rounded_up_to_multiple(fit_width, multiple)
            tensor_height = _rounded_up_to_multiple(fit_height, multiple)
        if not _is_finite_number(scale):
            raise ValueError(
                f"ModelInputTensorKernel.compile: scale must be a finite number, "
                f"got {scale!r}"
            )
        mean_by_channel = _three_finite_numbers("mean", mean)
        std_by_channel = _three_finite_numbers("std", std)
        if 0.0 in std_by_channel:
            raise ValueError(
                f"ModelInputTensorKernel.compile: std {std!r} holds a zero, which "
                f"every element of that channel would be divided by"
            )
        tensor_layout_plan = _ModelInputTensorLayoutPlan(
            tensor_width, tensor_height, fit_width, fit_height, fit, layout, dtype
        )
        if tensor_layout_plan.word_count > _MAXIMUM_ROWS * _WORDS_PER_ROW:
            raise ValueError(
                f"ModelInputTensorKernel.compile: a {tensor_width}x{tensor_height} "
                f"tensor is past the largest one dispatch covers — reduce width "
                f"and height"
            )
        if tensor_layout_plan.element_count % tensor_layout_plan.elements_per_word:
            raise ValueError(
                f"ModelInputTensorKernel.compile: a float16 tensor of "
                f"{tensor_width}x{tensor_height} has an odd element count, and "
                f"the kernel writes float16 elements in pairs — make the width "
                f"or the height even, or use float32"
            )
        compute_kernel = gpu_full_access.create_compute_kernel(
            source=_compute_kernel_glsl(
                tensor_layout_plan, channel_order, float(scale), mean_by_channel, std_by_channel
            ),
            push_constant_size=_FIT_PUSH_CONSTANT_BLOCK_BYTE_SIZE,
            bindings={
                SAMPLED_SOURCE_BINDING_NAME: "sampled_texture",
                _MODEL_INPUT_TENSOR_BINDING_NAME: "storage_buffer",
            },
        )
        return cls(compute_kernel, tensor_layout_plan)

    def apply_to_surface(
        self, gpu_limited_access: GpuContextWithSurfaceCopy, surface: GpuSurfaceHandle
    ) -> ModelInputTensor:
        """Write the model input for `surface` into the next pooled tensor, in `process()`.

        `surface` is one 8-bit RGBA frame, a pixel buffer's `rgba32` or a
        texture's `rgba8_unorm`. The returned tensor surface is written when
        this returns; `torch.from_dlpack` reads it with no copy, and its id
        publishes downstream. Raises naming the surface for a tensor or a
        non-RGBA source, and when every pool slot is still held.
        """
        source_surface_id = surface.surface_id
        if surface.shape is not None:
            raise ValueError(
                f"ModelInputTensorKernel.apply_to_surface: surface {source_surface_id!r} "
                f"is a tensor of {surface.shape} {surface.dtype}, not an RGBA frame"
            )
        if surface.format not in _RGBA_SOURCE_FORMATS:
            raise ValueError(
                f"ModelInputTensorKernel.apply_to_surface: surface {source_surface_id!r} "
                f"is {surface.format!r}; the source is one 8-bit RGBA frame, "
                f"{' or '.join(repr(name) for name in _RGBA_SOURCE_FORMATS)} — "
                f"the kernel converts no colour"
            )
        source_width, source_height = surface.width, surface.height
        geometry = self._tensor_layout_plan.geometry_for_source(source_width, source_height)

        source_landing_texture = self._source_landing_ring.land_source_for_this_frame(
            gpu_limited_access,
            source_surface_id,
            source_width,
            source_height,
            f"ModelInputTensorKernel.apply_to_surface: surface {source_surface_id!r}",
        )

        tensor_surface = gpu_limited_access.acquire_storage_buffer_from_processor_output_pool(
            self._tensor_output_pool_key,
            STANDARD_RING_DEPTH,
            self._tensor_layout_plan.dimensions,
            self._tensor_layout_plan.dtype,
        )
        self._compute_kernel.dispatch(
            bindings={
                SAMPLED_SOURCE_BINDING_NAME: source_landing_texture,
                _MODEL_INPUT_TENSOR_BINDING_NAME: tensor_surface,
            },
            group_count=self._dispatch_group_count,
            push_constants=struct.pack(
                _FIT_PUSH_CONSTANT_FORMAT,
                geometry.resized_width,
                geometry.resized_height,
                geometry.pad_left,
                geometry.pad_top,
            ),
        )
        return ModelInputTensor(tensor_surface, geometry)
