# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The streams `test_processor_owned_window.py` starts on `tatolabd`.

A `DisplayWindow` rides alongside every probe that gets a window at all. Two
windows on `tatolabd`'s one event pump is the arrangement that pump exists
for — the probe owning one of them runs in its own processor interpreter — and
a processor-owned window that only worked as the sole window would be a
regression nobody would see with one on screen.

The headless stream has no `DisplayWindow`: it is started with no display
server, where one would fail for the same reason and take the stream down
before the probe could report the refusal it is testing.
"""

import tatolab.stream
from tatolab.stream import StreamBuilder, stream

import processor_owned_window_probes

# Deliberately not a superstring of the probe's own window title: a test
# looking one of them up by name would otherwise find both.
DISPLAY_TITLE = "streamlib harness — the pipeline's own display"


def _add_a_test_pattern_into_the_probe_and_a_display_window(
    stream_builder: StreamBuilder, probe_class: type
) -> None:
    """The arrangement a debug window is really used in: the pipeline's own
    display up, and a processor's window beside it."""
    source = stream_builder.add(
        tatolab.stream.TestPatternSource, config={"width": 640, "height": 480}
    )
    probe = stream_builder.add(probe_class)
    display = stream_builder.add(tatolab.stream.DisplayWindow, config={"title": DISPLAY_TITLE})
    stream_builder.connect(source.output("video"), probe.input("video_from_upstream"))
    stream_builder.connect(source.output("video"), display.input("video"))


@stream
def every_argument_shape_reaches_the_window_probe_beside_a_display_window(
    stream_builder: StreamBuilder,
) -> None:
    _add_a_test_pattern_into_the_probe_and_a_display_window(
        stream_builder, processor_owned_window_probes.EveryArgumentShapeReachesTheWindowProbe
    )


@stream
def an_owner_closing_its_own_window_probe_beside_a_display_window(
    stream_builder: StreamBuilder,
) -> None:
    _add_a_test_pattern_into_the_probe_and_a_display_window(
        stream_builder, processor_owned_window_probes.AnOwnerClosingItsOwnWindowProbe
    )


@stream
def showing_something_that_names_no_surface_is_refused_probe_beside_a_display_window(
    stream_builder: StreamBuilder,
) -> None:
    _add_a_test_pattern_into_the_probe_and_a_display_window(
        stream_builder,
        processor_owned_window_probes.ShowingSomethingThatNamesNoSurfaceIsRefusedProbe,
    )


@stream
def a_frame_describing_its_colour_reaches_the_window_probe_beside_a_display_window(
    stream_builder: StreamBuilder,
) -> None:
    _add_a_test_pattern_into_the_probe_and_a_display_window(
        stream_builder,
        processor_owned_window_probes.AFrameDescribingItsColourReachesTheWindowProbe,
    )


@stream
def a_process_that_can_get_no_window_probe_alone_off_a_test_pattern(
    stream_builder: StreamBuilder,
) -> None:
    """The test pattern into the headless probe, alone."""
    source = stream_builder.add(
        tatolab.stream.TestPatternSource, config={"width": 640, "height": 480}
    )
    probe = stream_builder.add(
        processor_owned_window_probes.AProcessThatCanGetNoWindowRefusesAtSetupProbe
    )
    stream_builder.connect(source.output("video"), probe.input("video_from_upstream"))
