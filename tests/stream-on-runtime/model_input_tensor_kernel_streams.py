# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One stream per `ModelInputTensorKernel` scenario: a test-pattern frame fed to
one probe, which runs in a processor interpreter of its own.

The matrix probe runs one fit per stream — each kernel keeps one landing
texture of the frame's extent, and the engine's texture pool holds 16 of one
extent — so every fit case has a stream of its own.
"""

from typing import Any

import tatolab.stream
from tatolab.stream import StreamBuilder, stream

import model_input_tensor_kernel_probes


def _wire_a_test_pattern_into_probe(
    stream_builder: StreamBuilder, probe_class: type, probe_config: "dict[str, Any] | None"
) -> None:
    pattern = stream_builder.add(
        tatolab.stream.TestPatternSource,
        config={
            "width": model_input_tensor_kernel_probes.FRAME_WIDTH,
            "height": model_input_tensor_kernel_probes.FRAME_HEIGHT,
        },
    )
    probe = stream_builder.add(probe_class, config=probe_config)
    stream_builder.connect(pattern.output("video"), probe.input("video_from_upstream"))


def _wire_a_test_pattern_into_the_matrix_probe(
    stream_builder: StreamBuilder, fit_case_name: str, compile_with_the_wrong_mean: bool = False
) -> None:
    matrix_probe_config: "dict[str, Any]" = {"fit_case_name": fit_case_name}
    if compile_with_the_wrong_mean:
        matrix_probe_config["compile_with_the_wrong_mean"] = True
    _wire_a_test_pattern_into_probe(
        stream_builder,
        model_input_tensor_kernel_probes.ModelInputTensorMatrixProbe,
        matrix_probe_config,
    )


@stream
def a_test_pattern_into_the_stretch_matrix_probe(stream_builder: StreamBuilder) -> None:
    _wire_a_test_pattern_into_the_matrix_probe(stream_builder, "stretch")


@stream
def a_test_pattern_into_the_letterbox_matrix_probe(stream_builder: StreamBuilder) -> None:
    _wire_a_test_pattern_into_the_matrix_probe(stream_builder, "letterbox")


@stream
def a_test_pattern_into_the_pad_bottom_right_matrix_probe(stream_builder: StreamBuilder) -> None:
    _wire_a_test_pattern_into_the_matrix_probe(stream_builder, "pad_bottom_right")


@stream
def a_test_pattern_into_the_pad_bottom_right_to_multiple_of_16_matrix_probe(
    stream_builder: StreamBuilder,
) -> None:
    _wire_a_test_pattern_into_the_matrix_probe(
        stream_builder, "pad_bottom_right_to_multiple_of_16"
    )


@stream
def a_test_pattern_into_the_letterbox_matrix_probe_with_the_wrong_mean(
    stream_builder: StreamBuilder,
) -> None:
    """The comparison's negative control: the letterbox kernels compiled with a mean a tenth off."""
    _wire_a_test_pattern_into_the_matrix_probe(
        stream_builder, "letterbox", compile_with_the_wrong_mean=True
    )


@stream
def a_test_pattern_into_the_non_rgba_source_refusal_probe(stream_builder: StreamBuilder) -> None:
    _wire_a_test_pattern_into_probe(
        stream_builder, model_input_tensor_kernel_probes.NonRgbaSourceRefusalProbe, None
    )


@stream
def a_test_pattern_into_the_pad_bottom_right_extent_change_probe(
    stream_builder: StreamBuilder,
) -> None:
    _wire_a_test_pattern_into_probe(
        stream_builder, model_input_tensor_kernel_probes.PadBottomRightExtentChangeProbe, None
    )


MATRIX_STREAM_BY_FIT_CASE_NAME = {
    "stretch": a_test_pattern_into_the_stretch_matrix_probe,
    "letterbox": a_test_pattern_into_the_letterbox_matrix_probe,
    "pad_bottom_right": a_test_pattern_into_the_pad_bottom_right_matrix_probe,
    "pad_bottom_right_to_multiple_of_16": (
        a_test_pattern_into_the_pad_bottom_right_to_multiple_of_16_matrix_probe
    ),
}
