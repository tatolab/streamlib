# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The streams `test_pixel_exchange.py` starts on `tatolabd`.

A standalone probe acquires its own surface and reports from `setup`, so its
stream is the probe alone. The end-to-end scenarios put a native test pattern
in front of one or two Python processors.
"""

import tatolab.stream
from tatolab.stream import StreamBuilder, stream

import pixel_exchange_probes


@stream
def numpy_view_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(pixel_exchange_probes.NumpyViewProbe)


@stream
def unlocked_export_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(pixel_exchange_probes.UnlockedExportProbe)


@stream
def lock_mode_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(pixel_exchange_probes.LockModeProbe)


@stream
def shared_memory_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(pixel_exchange_probes.SharedMemoryProbe)


@stream
def tensor_outlives_the_surface_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(pixel_exchange_probes.TensorOutlivesTheSurfaceProbe)


@stream
def pool_cycle_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(pixel_exchange_probes.PoolCycleProbe)


@stream
def dlpack_consumer_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(pixel_exchange_probes.DlpackConsumerProbe)


@stream
def unsupported_format_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(pixel_exchange_probes.UnsupportedFormatProbe)


def _add_the_cross_process_edit(stream_builder: StreamBuilder, *, skip_edit: bool) -> None:
    pattern = stream_builder.add(
        tatolab.stream.TestPatternSource, config={"width": 320, "height": 180}
    )
    effect = stream_builder.add(
        pixel_exchange_probes.ReportingInvertingEffect, config={"skip_edit": skip_edit}
    )
    verifier = stream_builder.add(pixel_exchange_probes.FrameDigestVerifier)
    stream_builder.connect(pattern.output("video"), effect.input("video_from_upstream"))
    stream_builder.connect(
        effect.output("video_to_downstream"), verifier.input("video_from_upstream")
    )


@stream
def a_test_pattern_into_an_inverting_effect(stream_builder: StreamBuilder) -> None:
    """Native source → Python effect, the user-facing story: the frames a
    native processor produces are edited in place by a child interpreter."""
    pattern = stream_builder.add(
        tatolab.stream.TestPatternSource, config={"width": 320, "height": 180}
    )
    effect = stream_builder.add(pixel_exchange_probes.InvertingEffect)
    stream_builder.connect(pattern.output("video"), effect.input("video_from_upstream"))


@stream
def a_test_pattern_edited_then_digested_in_another_process(stream_builder: StreamBuilder) -> None:
    """Native source → Python effect → a second Python processor: the edit
    one child makes is read back by another, through the engine's memory."""
    _add_the_cross_process_edit(stream_builder, skip_edit=False)


@stream
def a_test_pattern_left_unedited_then_digested_in_another_process(
    stream_builder: StreamBuilder,
) -> None:
    """The cross-process edit's negative control: the effect leaves the pixels alone."""
    _add_the_cross_process_edit(stream_builder, skip_edit=True)
