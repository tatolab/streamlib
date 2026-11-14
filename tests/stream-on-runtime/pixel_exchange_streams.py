# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The streams `test_pixel_exchange.py` starts on `tatolabd`.

A standalone probe acquires its own surface and reports from `setup`, so its
stream is the probe alone. The end-to-end scenarios put the test pattern module
in front of one or two Python processors.
"""

from tatolab.stream import StreamBuilder, graph_generator
from tatolab.testpattern import TestPatternSource

import pixel_exchange_probes


@graph_generator
def numpy_view_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(pixel_exchange_probes.NumpyViewProbe)


@graph_generator
def unlocked_export_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(pixel_exchange_probes.UnlockedExportProbe)


@graph_generator
def lock_mode_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(pixel_exchange_probes.LockModeProbe)


@graph_generator
def shared_memory_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(pixel_exchange_probes.SharedMemoryProbe)


@graph_generator
def tensor_outlives_the_surface_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(pixel_exchange_probes.TensorOutlivesTheSurfaceProbe)


@graph_generator
def pool_cycle_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(pixel_exchange_probes.PoolCycleProbe)


@graph_generator
def dlpack_consumer_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(pixel_exchange_probes.DlpackConsumerProbe)


@graph_generator
def unsupported_format_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(pixel_exchange_probes.UnsupportedFormatProbe)


def _add_the_cross_process_edit(stream_builder: StreamBuilder, *, skip_edit: bool) -> None:
    pattern = stream_builder.add(
        TestPatternSource, config={"width": 320, "height": 180}
    )
    effect = stream_builder.add(
        pixel_exchange_probes.ReportingInvertingEffect, config={"skip_edit": skip_edit}
    )
    verifier = stream_builder.add(pixel_exchange_probes.FrameDigestVerifier)
    stream_builder.connect(pattern.output("video"), effect.input("video_from_upstream"))
    stream_builder.connect(
        effect.output("video_to_downstream"), verifier.input("video_from_upstream")
    )


@graph_generator
def a_test_pattern_into_an_inverting_effect(stream_builder: StreamBuilder) -> None:
    """Test pattern → Python effect, the user-facing story: the frames one
    processor produces are edited in place by another one's interpreter."""
    pattern = stream_builder.add(
        TestPatternSource, config={"width": 320, "height": 180}
    )
    effect = stream_builder.add(pixel_exchange_probes.InvertingEffect)
    stream_builder.connect(pattern.output("video"), effect.input("video_from_upstream"))


@graph_generator
def a_test_pattern_edited_then_digested_in_another_process(stream_builder: StreamBuilder) -> None:
    """Native source → Python effect → a second Python processor: the edit
    one child makes is read back by another, through the engine's memory."""
    _add_the_cross_process_edit(stream_builder, skip_edit=False)


@graph_generator
def a_test_pattern_left_unedited_then_digested_in_another_process(
    stream_builder: StreamBuilder,
) -> None:
    """The cross-process edit's negative control: the effect leaves the pixels alone."""
    _add_the_cross_process_edit(stream_builder, skip_edit=True)
