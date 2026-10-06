# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A model's input tensor, prepared from an RGBA frame in one compute pass.

The frame is stretched or letterboxed to the model's input size, or padded
on the bottom and right at its own extent — its alpha dropped and its channels put in the
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
from typing import Any, Literal, get_args

from tatolab.runtime._engine import (
    ComputeKernel,
    GpuContextFullAccess,
    GpuSurfaceHandle,
)

from ._sampled_source_landing import (
    SAMPLED_SOURCE_BINDING_NAME,
    GpuContextWithSurfaceCopy,
    SampledSourceLandingTextureRing,
)
from .node_output_texture_ring import STANDARD_RING_DEPTH

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

_FITS: "tuple[ModelInputTensorFit, ...]" = get_args(ModelInputTensorFit)
_SOURCE_CHANNEL_BY_OUTPUT_CHANNEL: "dict[ModelInputTensorChannelOrder, tuple[int, int, int]]" = {
    "rgb": (0, 1, 2),
    "bgr": (2, 1, 0),
}
_LAYOUTS: "tuple[ModelInputTensorLayout, ...]" = get_args(ModelInputTensorLayout)
_DTYPES: "tuple[ModelInputTensorDtype, ...]" = get_args(ModelInputTensorDtype)

_CHANNEL_COUNT = 3

# A pixel buffer's and a texture's spelling of 8-bit RGBA — the two sources the
# engine copy lands in the kernel's `rgba8_unorm` texture as they are.
_RGBA_SOURCE_FORMATS = ("rgba32", "rgba8_unorm")

_MODEL_INPUT_TENSOR_BINDING_NAME = "streamlib_model_input_tensor"

# `ivec2 resized_extent; ivec2 pad_offset; ivec2 tensor_extent; uint word_count;`
# — the per-apply fit, since a `pad_bottom_right` tensor takes the frame's extent.
_FIT_PUSH_CONSTANT_FORMAT = "<6iI"
_FIT_PUSH_CONSTANT_BLOCK_BYTE_SIZE = struct.calcsize(_FIT_PUSH_CONSTANT_FORMAT)

# One invocation writes one 32-bit word of the tensor: one float32 element,
# or two float16 elements packed, since no 16-bit storage access is assumed.
# Words run in rows of 1024 workgroups, so a large tensor stays inside the
# 65535 workgroups per dimension every device guarantees.
_INVOCATIONS_PER_WORKGROUP = 64
_WORKGROUPS_PER_ROW = 1024
_WORDS_PER_ROW = _INVOCATIONS_PER_WORKGROUP * _WORKGROUPS_PER_ROW
_MAXIMUM_ROWS = 65535
# The shader indexes elements in 32-bit unsigned arithmetic.
_MAXIMUM_ELEMENT_COUNT = 2**32 - 1


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
class _ModelInputTensorExtent:
    """One apply's tensor: its extent, where the frame lands in it, and its layout."""

    tensor_width: int
    tensor_height: int
    geometry: ModelInputTensorGeometry
    layout: ModelInputTensorLayout
    dtype: ModelInputTensorDtype

    @property
    def dimensions(self) -> "list[int]":
        if self.layout == "nchw":
            return [1, _CHANNEL_COUNT, self.tensor_height, self.tensor_width]
        return [1, self.tensor_height, self.tensor_width, _CHANNEL_COUNT]

    @property
    def word_count(self) -> int:
        return _tensor_word_count(self.tensor_width, self.tensor_height, self.dtype)

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

    @property
    def push_constants(self) -> bytes:
        return struct.pack(
            _FIT_PUSH_CONSTANT_FORMAT,
            self.geometry.resized_width,
            self.geometry.resized_height,
            self.geometry.pad_left,
            self.geometry.pad_top,
            self.tensor_width,
            self.tensor_height,
            self.word_count,
        )


def _elements_per_word(dtype: ModelInputTensorDtype) -> int:
    return 1 if dtype == "float32" else 2


def _tensor_word_count(tensor_width: int, tensor_height: int, dtype: ModelInputTensorDtype) -> int:
    return _CHANNEL_COUNT * tensor_width * tensor_height // _elements_per_word(dtype)


def _refuse_a_tensor_extent_the_kernel_cannot_write(
    refusal_subject: str, tensor_width: int, tensor_height: int, dtype: ModelInputTensorDtype
) -> None:
    element_count = _CHANNEL_COUNT * tensor_width * tensor_height
    if (
        element_count > _MAXIMUM_ELEMENT_COUNT
        or _tensor_word_count(tensor_width, tensor_height, dtype) > _MAXIMUM_ROWS * _WORDS_PER_ROW
    ):
        raise ValueError(
            f"{refusal_subject}: a {tensor_width}x{tensor_height} tensor is past "
            f"the largest one dispatch covers"
        )
    if element_count % _elements_per_word(dtype):
        raise ValueError(
            f"{refusal_subject}: a float16 tensor of {tensor_width}x{tensor_height} "
            f"has an odd element count, and the kernel writes float16 elements in "
            f"pairs — make the width or the height even, or use float32"
        )


@dataclass(frozen=True)
class _ModelInputTensorPlan:
    """What `compile` fixed: the fit, the model's extent when the fit has one, and the layout."""

    fit: ModelInputTensorFit
    model_width: "int | None"
    model_height: "int | None"
    pad_to_multiple_of: int
    layout: ModelInputTensorLayout
    dtype: ModelInputTensorDtype

    def extent_for_source(
        self, source_width: int, source_height: int, refusal_subject: str
    ) -> _ModelInputTensorExtent:
        if self.model_width is None or self.model_height is None:
            tensor_width = _rounded_up_to_multiple(source_width, self.pad_to_multiple_of)
            tensor_height = _rounded_up_to_multiple(source_height, self.pad_to_multiple_of)
            _refuse_a_tensor_extent_the_kernel_cannot_write(
                refusal_subject, tensor_width, tensor_height, self.dtype
            )
            return _ModelInputTensorExtent(
                tensor_width,
                tensor_height,
                ModelInputTensorGeometry(
                    source_width, source_height, source_width, source_height, 0, 0
                ),
                self.layout,
                self.dtype,
            )
        if self.fit == "stretch":
            geometry = ModelInputTensorGeometry(
                source_width, source_height, self.model_width, self.model_height, 0, 0
            )
        else:
            scale = min(self.model_width / source_width, self.model_height / source_height)
            resized_width = min(self.model_width, max(1, round(source_width * scale)))
            resized_height = min(self.model_height, max(1, round(source_height * scale)))
            geometry = ModelInputTensorGeometry(
                source_width,
                source_height,
                resized_width,
                resized_height,
                (self.model_width - resized_width) // 2,
                (self.model_height - resized_height) // 2,
            )
        return _ModelInputTensorExtent(
            self.model_width, self.model_height, geometry, self.layout, self.dtype
        )


def _compute_kernel_glsl(
    layout: ModelInputTensorLayout,
    dtype: ModelInputTensorDtype,
    channel_order: ModelInputTensorChannelOrder,
    scale: float,
    mean: "tuple[float, float, float]",
    std: "tuple[float, float, float]",
) -> str:
    if layout == "nchw":
        element_position = (
            "    uint channel = element_index / plane;\n"
            "    uint pixel_index = element_index % plane;\n"
        )
    else:
        element_position = (
            f"    uint channel = element_index % {_CHANNEL_COUNT}u;\n"
            f"    uint pixel_index = element_index / {_CHANNEL_COUNT}u;\n"
        )
    if dtype == "float32":
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
        "    ivec2 tensor_extent;\n"
        "    uint word_count;\n"
        "} fit;\n"
        f"const float SCALE = {_glsl_float(scale)};\n"
        f"const vec3 MEAN = vec3({', '.join(_glsl_float(value) for value in mean)});\n"
        f"const vec3 STD = vec3({', '.join(_glsl_float(value) for value in std)});\n"
        f"const ivec3 SOURCE_CHANNEL_BY_OUTPUT_CHANNEL = ivec3({source_channels});\n"
        "float model_input_element(uint element_index) {\n"
        "    uint tensor_width = uint(fit.tensor_extent.x);\n"
        "    uint plane = tensor_width * uint(fit.tensor_extent.y);\n"
        f"{element_position}"
        "    ivec2 at = ivec2(int(pixel_index % tensor_width), int(pixel_index / tensor_width));\n"
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
        "    if (word_index >= fit.word_count) {\n"
        "        return;\n"
        "    }\n"
        f"{write_word}"
        "}\n"
    )


class ModelInputTensorKernel:
    """Prepares a model's input tensor from an RGBA frame on the GPU, frame by frame."""

    def __init__(self, compute_kernel: ComputeKernel, tensor_plan: _ModelInputTensorPlan) -> None:
        self._compute_kernel = compute_kernel
        self._tensor_plan = tensor_plan
        self._source_landing_ring = SampledSourceLandingTextureRing()
        self._tensor_output_pool_key = f"model-input-tensor-kernel-{uuid.uuid4().hex}"

    @classmethod
    def compile(
        cls,
        gpu_full_access: GpuContextFullAccess,
        width: "int | None" = None,
        height: "int | None" = None,
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

        `stretch` fills the model's `width` x `height` and `letterbox` keeps
        the aspect and pads both sides evenly. `pad_bottom_right` takes no
        `width` or `height`: it never resizes, and each tensor is the frame's
        own extent rounded up to `pad_to_multiple_of`, so boxes come back in
        frame coordinates. A pixel's 0-255 channel value `x` is written as
        `(x * scale - mean) / std`, per output channel; padding is a black
        pixel through the same affine. Raises naming the parameter for an
        unknown fit, layout, dtype or channel order.
        """
        _refuse_unless_one_of("fit", fit, _FITS)
        _refuse_unless_one_of("channel_order", channel_order, tuple(_SOURCE_CHANNEL_BY_OUTPUT_CHANNEL))
        _refuse_unless_one_of("layout", layout, _LAYOUTS)
        _refuse_unless_one_of("dtype", dtype, _DTYPES)
        if fit == "pad_bottom_right":
            if width is not None or height is not None:
                raise ValueError(
                    f"ModelInputTensorKernel.compile: fit='pad_bottom_right' never "
                    f"resizes, so it takes no width or height (got {width!r} x "
                    f"{height!r}) — each tensor is the frame's own extent, rounded "
                    f"up to pad_to_multiple_of"
                )
            model_width = model_height = None
            multiple = (
                1
                if pad_to_multiple_of is None
                else _positive_int("pad_to_multiple_of", pad_to_multiple_of)
            )
        else:
            if pad_to_multiple_of is not None:
                raise ValueError(
                    f"ModelInputTensorKernel.compile: pad_to_multiple_of pads after "
                    f"the frame, so it needs fit='pad_bottom_right', not {fit!r}"
                )
            model_width = _positive_int("width", width)
            model_height = _positive_int("height", height)
            multiple = 1
            _refuse_a_tensor_extent_the_kernel_cannot_write(
                "ModelInputTensorKernel.compile", model_width, model_height, dtype
            )
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
        compute_kernel = gpu_full_access.create_compute_kernel(
            source=_compute_kernel_glsl(
                layout, dtype, channel_order, float(scale), mean_by_channel, std_by_channel
            ),
            push_constant_size=_FIT_PUSH_CONSTANT_BLOCK_BYTE_SIZE,
            bindings={
                SAMPLED_SOURCE_BINDING_NAME: "sampled_texture",
                _MODEL_INPUT_TENSOR_BINDING_NAME: "storage_buffer",
            },
        )
        return cls(
            compute_kernel,
            _ModelInputTensorPlan(fit, model_width, model_height, multiple, layout, dtype),
        )

    def apply_to_surface(
        self, gpu_limited_access: GpuContextWithSurfaceCopy, surface: GpuSurfaceHandle
    ) -> ModelInputTensor:
        """Write the model input for `surface` into the next pooled tensor, in `process()`.

        `surface` is one 8-bit RGBA frame, a pixel buffer's `rgba32` or a
        texture's `rgba8_unorm`. The returned tensor surface is written when
        this returns; `torch.from_dlpack` reads it with no copy, and its id
        publishes downstream. A frame of a new extent under `pad_bottom_right`
        re-sizes the pool's tensors. Raises naming the surface for a tensor,
        a non-RGBA source, or an extent the kernel cannot write, and when
        every pool slot is still held.
        """
        source_surface_id = surface.surface_id
        refusal_subject = f"ModelInputTensorKernel.apply_to_surface: surface {source_surface_id!r}"
        if surface.shape is not None:
            raise ValueError(
                f"{refusal_subject} is a tensor of {surface.shape} {surface.dtype}, "
                f"not an RGBA frame"
            )
        if surface.format not in _RGBA_SOURCE_FORMATS:
            raise ValueError(
                f"{refusal_subject} is {surface.format!r}; the source is one 8-bit "
                f"RGBA frame, {' or '.join(repr(name) for name in _RGBA_SOURCE_FORMATS)} — "
                f"the kernel converts no colour"
            )
        source_width, source_height = surface.width, surface.height
        tensor_extent = self._tensor_plan.extent_for_source(
            source_width, source_height, refusal_subject
        )

        source_landing_texture = self._source_landing_ring.land_source_for_this_frame(
            gpu_limited_access, source_surface_id, source_width, source_height, refusal_subject
        )
        tensor_surface = gpu_limited_access.acquire_storage_buffer_from_processor_output_pool(
            self._tensor_output_pool_key,
            STANDARD_RING_DEPTH,
            tensor_extent.dimensions,
            tensor_extent.dtype,
        )
        self._compute_kernel.dispatch(
            bindings={
                SAMPLED_SOURCE_BINDING_NAME: source_landing_texture,
                _MODEL_INPUT_TENSOR_BINDING_NAME: tensor_surface,
            },
            group_count=tensor_extent.dispatch_group_count,
            push_constants=tensor_extent.push_constants,
        )
        return ModelInputTensor(tensor_surface, tensor_extent.geometry)
