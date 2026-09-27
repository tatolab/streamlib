# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probes for `ModelInputTensorKernel`, from a helper process.

Each applies kernels to one test-pattern frame and compares every tensor with
the same model input made by a torch reference on the CPU: bilinear resize,
alpha dropped, channels reordered, the affine, then the pad.
"""

import json
import os
import sys
import traceback
from typing import Any, Callable, Tuple, TypedDict

from streamlib import (
    GpuSurfaceHandle,
    ModelInputTensorChannelOrder,
    ModelInputTensorDtype,
    ModelInputTensorFit,
    ModelInputTensorKernel,
    ModelInputTensorLayout,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,
    log,
    processor,
)

RESULT_MARKER = "MARKER:PROBE_RESULT "

# Neither a multiple of the tensor extents below, and a different aspect, so
# the letterbox pads and every resize is a real one.
FRAME_WIDTH = 60
FRAME_HEIGHT = 36

SCALE = 1.0 / 255.0
IMAGENET_MEAN = (0.485, 0.456, 0.406)
IMAGENET_STD = (0.229, 0.224, 0.225)
# The negative control's mean: every channel off by a tenth.
WRONG_MEAN = tuple(value + 0.1 for value in IMAGENET_MEAN)

# Bilinear weights are quantized on the GPU (8 sub-texel bits on common
# drivers), so a sharp colour-bar edge may land a level off torch's.
TOLERATED_ERROR_IN_PIXEL_LEVELS = 1.5

NATURAL_TORCH_DEVICE_TYPE = "mps" if sys.platform == "darwin" else "cuda"


class FitCase(TypedDict):
    fit: ModelInputTensorFit
    width: int
    height: int
    pad_to_multiple_of: "int | None"
    # The geometry worked out by hand for a 60x36 frame:
    # resized width, resized height, pad left, pad top.
    expected_geometry: "list[int]"
    expected_tensor_extent: "list[int]"


FIT_CASES: "dict[str, FitCase]" = {
    "stretch": {
        "fit": "stretch", "width": 32, "height": 24, "pad_to_multiple_of": None,
        "expected_geometry": [32, 24, 0, 0], "expected_tensor_extent": [32, 24],
    },
    "letterbox": {
        # min(32/60, 24/36) = 8/15; 36 * 8/15 = 19.2 rounds to 19, padded 2 above.
        "fit": "letterbox", "width": 32, "height": 24, "pad_to_multiple_of": None,
        "expected_geometry": [32, 19, 0, 2], "expected_tensor_extent": [32, 24],
    },
    "pad_bottom_right": {
        "fit": "pad_bottom_right", "width": 32, "height": 24, "pad_to_multiple_of": None,
        "expected_geometry": [32, 19, 0, 0], "expected_tensor_extent": [32, 24],
    },
    "pad_bottom_right_to_multiple_of_16": {
        # The frame at its own extent, so boxes stay in frame coordinates.
        "fit": "pad_bottom_right", "width": 60, "height": 36, "pad_to_multiple_of": 16,
        "expected_geometry": [60, 36, 0, 0], "expected_tensor_extent": [64, 48],
    },
}

LAYOUTS: "tuple[ModelInputTensorLayout, ...]" = ("nchw", "nhwc")
DTYPES: "tuple[ModelInputTensorDtype, ...]" = ("float32", "float16")

MatrixCase = Tuple[
    str, ModelInputTensorLayout, ModelInputTensorDtype, ModelInputTensorChannelOrder
]


def matrix_case_name(fit_case_name: str, layout: str, dtype: str, channel_order: str) -> str:
    return f"{fit_case_name}/{layout}/{dtype}/{channel_order}"


def matrix_cases_for_fit(fit_case_name: str) -> "list[MatrixCase]":
    """Every layout x dtype for one fit — and `bgr` beside the letterbox.

    One fit per run: each kernel keeps one landing texture of the frame's
    extent, and the engine's texture pool holds 16 of one extent.
    """
    cases: "list[MatrixCase]" = [
        (fit_case_name, layout, dtype, "rgb") for layout in LAYOUTS for dtype in DTYPES
    ]
    if fit_case_name == "letterbox":
        cases.append((fit_case_name, "nchw", "float32", "bgr"))
    return cases


def _report(probe_body: Callable[[], "dict[str, Any]"]) -> None:
    try:
        observation = probe_body()
    except BaseException:  # noqa: BLE001 — re-raised by the asserting test
        observation = {"failure": traceback.format_exc()}
    log.info(RESULT_MARKER + json.dumps({"pid": os.getpid(), **observation}))


def _torch_device_unavailable_reason() -> "str | None":
    import torch

    if NATURAL_TORCH_DEVICE_TYPE == "mps" and not torch.backends.mps.is_available():
        return "torch sees no MPS device"
    if NATURAL_TORCH_DEVICE_TYPE == "cuda" and not torch.cuda.is_available():
        return "torch sees no CUDA device"
    return None


def torch_reference_model_input(
    torch,
    source_rgba,
    fit_case: FitCase,
    layout: str,
    channel_order: str,
    mean: "tuple[float, ...]",
    std: "tuple[float, ...]",
):
    """The model input, in float64 on the CPU, made the way a torch user would."""
    resized_width, resized_height, pad_left, pad_top = fit_case["expected_geometry"]
    tensor_width, tensor_height = fit_case["expected_tensor_extent"]
    source_channels = [0, 1, 2] if channel_order == "rgb" else [2, 1, 0]

    source = torch.from_numpy(source_rgba).to(torch.float64)
    source = source[..., source_channels].permute(2, 0, 1).unsqueeze(0)
    resized = torch.nn.functional.interpolate(
        source, size=(resized_height, resized_width), mode="bilinear", align_corners=False
    )
    mean_by_channel = torch.tensor(mean, dtype=torch.float64).view(1, 3, 1, 1)
    std_by_channel = torch.tensor(std, dtype=torch.float64).view(1, 3, 1, 1)
    padded = torch.zeros((1, 3, tensor_height, tensor_width), dtype=torch.float64)
    padded[
        :, :, pad_top : pad_top + resized_height, pad_left : pad_left + resized_width
    ] = resized
    model_input = (padded * SCALE - mean_by_channel) / std_by_channel
    if layout == "nhwc":
        model_input = model_input.permute(0, 2, 3, 1).contiguous()
    return model_input


def _source_pixels(frame_surface: GpuSurfaceHandle):
    frame_surface.lock(read_only=True)
    try:
        return frame_surface.as_numpy()[..., :4].copy()
    finally:
        frame_surface.unlock()


def compare_with_the_torch_reference(
    gpu,
    frame_surface: GpuSurfaceHandle,
    source_rgba,
    kernel: ModelInputTensorKernel,
    fit_case: FitCase,
    layout: str,
    channel_order: str,
    mean: "tuple[float, ...]",
    std: "tuple[float, ...]",
) -> "dict[str, Any]":
    import torch

    model_input = kernel.apply_to_surface(gpu, frame_surface)
    with model_input.tensor_surface as tensor_surface:
        read = torch.from_dlpack(tensor_surface)
        observed = read.cpu().to(torch.float64)
        observation: "dict[str, Any]" = {
            "stated_shape": tensor_surface.shape,
            "stated_dtype": tensor_surface.dtype,
            "tensor_device": str(read.device),
            "tensor_shape": list(read.shape),
            "tensor_dtype": str(read.dtype),
        }
    geometry = model_input.geometry
    expected = torch_reference_model_input(
        torch, source_rgba, fit_case, layout, channel_order, mean, std
    )
    std_by_channel = torch.tensor(std, dtype=torch.float64)
    std_along_channels = (
        std_by_channel.view(1, 3, 1, 1) if layout == "nchw" else std_by_channel.view(1, 1, 1, 3)
    )
    error_in_pixel_levels = (observed - expected).abs() * std_along_channels / SCALE
    return {
        **observation,
        "geometry": [
            geometry.resized_width,
            geometry.resized_height,
            geometry.pad_left,
            geometry.pad_top,
        ],
        "max_error_in_pixel_levels": float(error_in_pixel_levels.max()),
    }


class ModelInputTensorMatrixProbeConfig(TypedDict, total=False):
    fit_case_name: str
    # The negative control: every kernel compiled with `WRONG_MEAN`, compared
    # with a reference made with the right one.
    compile_with_the_wrong_mean: bool


@processor
class ModelInputTensorMatrixProbe:
    """Applies a kernel per layout x dtype of one fit to the first frame and
    reports each tensor's largest error against torch."""

    @input(delivery_profile="ordered")
    def video_from_upstream(self) -> VideoFrame: ...

    def __init__(self, config: ModelInputTensorMatrixProbeConfig) -> None:
        self.compiled_mean = (
            WRONG_MEAN if config.get("compile_with_the_wrong_mean") else IMAGENET_MEAN
        )
        self.fit_case_name = config.get("fit_case_name", "letterbox")
        self.reported = False

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.kernels = {
            case: ModelInputTensorKernel.compile(
                ctx.gpu_full_access,
                width=FIT_CASES[case[0]]["width"],
                height=FIT_CASES[case[0]]["height"],
                fit=FIT_CASES[case[0]]["fit"],
                pad_to_multiple_of=FIT_CASES[case[0]]["pad_to_multiple_of"],
                channel_order=case[3],
                layout=case[1],
                dtype=case[2],
                scale=SCALE,
                mean=self.compiled_mean,
                std=IMAGENET_STD,
            )
            for case in matrix_cases_for_fit(self.fit_case_name)
        }

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
        if frame is None or self.reported:
            return
        self.reported = True
        gpu = ctx.gpu_limited_access

        def apply_each() -> "dict[str, Any]":
            unavailable = _torch_device_unavailable_reason()
            if unavailable is not None:
                return {"torch_device_unavailable": unavailable}
            with gpu.resolve_surface(frame.surface_id) as frame_surface:
                source_rgba = _source_pixels(frame_surface)
                cases = {
                    matrix_case_name(*case): compare_with_the_torch_reference(
                        gpu,
                        frame_surface,
                        source_rgba,
                        kernel,
                        FIT_CASES[case[0]],
                        case[1],
                        case[3],
                        IMAGENET_MEAN,
                        IMAGENET_STD,
                    )
                    for case, kernel in self.kernels.items()
                }
            return {
                "source_is_not_uniform": bool((source_rgba != source_rgba[0, 0]).any()),
                "cases": cases,
            }

        _report(apply_each)


@processor
class NonRgbaSourceRefusalProbe:
    """Hands the kernel a `bgra32` pixel buffer and a tensor surface, and
    reports each refusal."""

    @input(delivery_profile="ordered")
    def video_from_upstream(self) -> VideoFrame: ...

    def __init__(self) -> None:
        self.reported = False

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.kernel = ModelInputTensorKernel.compile(ctx.gpu_full_access, width=32, height=24)

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
        if frame is None or self.reported:
            return
        self.reported = True
        gpu = ctx.gpu_limited_access

        def refusal_of(surface: GpuSurfaceHandle) -> str:
            try:
                self.kernel.apply_to_surface(gpu, surface)
            except ValueError as refusal:
                return str(refusal)
            return "not refused"

        def refuse_each() -> "dict[str, Any]":
            with (
                gpu.acquire_pixel_buffer(FRAME_WIDTH, FRAME_HEIGHT, "bgra") as bgra_surface,
                gpu.acquire_storage_buffer([1, 3, 24, 32], "float32") as tensor_surface,
            ):
                return {
                    "bgra_surface_id": bgra_surface.surface_id,
                    "bgra_refusal": refusal_of(bgra_surface),
                    "tensor_surface_id": tensor_surface.surface_id,
                    "tensor_refusal": refusal_of(tensor_surface),
                }

        _report(refuse_each)
