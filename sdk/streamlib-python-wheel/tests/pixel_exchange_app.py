# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that run one pixel-exchange probe in its real placement.

Run as its own `python <script>.py` process: the probe executes in a helper
process, and its observation reaches this app — and the test driving it — over
the same log forwarding every child's records ride.
"""

import sys

import tatolab.runtime
import tatolab.stream
from tatolab.stream import Stream, compile_stream_to_graph, stream

import pixel_exchange_probes


def _probe_class_named_on_the_command_line() -> type:
    return getattr(pixel_exchange_probes, sys.argv[1])


def _add_the_cross_process_edit(stream: Stream, *, skip_edit: bool) -> None:
    pattern = stream.add(
        tatolab.stream.TestPatternSource, config={"width": 320, "height": 180}
    )
    effect = stream.add(
        pixel_exchange_probes.ReportingInvertingEffect, config={"skip_edit": skip_edit}
    )
    verifier = stream.add(pixel_exchange_probes.FrameDigestVerifier)
    stream.connect(pattern.output("video"), effect.input("video_from_upstream"))
    stream.connect(effect.output("video_to_downstream"), verifier.input("video_from_upstream"))


@stream
def one_pixel_exchange_probe(stream: Stream) -> None:
    """One probe, one graph. The probe reports from `setup`, so the graph has
    nothing to do but exist until the test has read the result and interrupts."""
    stream.add(_probe_class_named_on_the_command_line())


@stream
def a_test_pattern_into_an_inverting_effect(stream: Stream) -> None:
    """Native source → Python effect, the user-facing story: the frames a
    native processor produces are edited in place by a child interpreter."""
    pattern = stream.add(
        tatolab.stream.TestPatternSource, config={"width": 320, "height": 180}
    )
    effect = stream.add(pixel_exchange_probes.InvertingEffect)
    stream.connect(
        pattern.output("video"), effect.input("video_from_upstream")
    )


@stream
def a_test_pattern_edited_then_digested_in_another_process(stream: Stream) -> None:
    """Native source → Python effect → a second Python processor: the edit
    one child makes is read back by another, through the engine's memory."""
    _add_the_cross_process_edit(stream, skip_edit=False)


@stream
def a_test_pattern_left_unedited_then_digested_in_another_process(stream: Stream) -> None:
    """The cross-process edit's negative control: the effect leaves the pixels alone."""
    _add_the_cross_process_edit(stream, skip_edit=True)


STREAM_BY_SCENARIO = {
    "inverting_effect": a_test_pattern_into_an_inverting_effect,
    "cross_process_edit": a_test_pattern_edited_then_digested_in_another_process,
    "cross_process_edit_negative_control": (
        a_test_pattern_left_unedited_then_digested_in_another_process
    ),
}


if __name__ == "__main__":
    graph = compile_stream_to_graph(
        STREAM_BY_SCENARIO.get(sys.argv[1], one_pixel_exchange_probe)
    )
    runtime = tatolab.runtime.Runtime()
    runtime.load(graph)
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)
