# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""The `@graph_generator` functions `test_helper_shutdown_ladder.py` runs from the suite project."""

from tatolab.stream import StreamBuilder, graph_generator

from interpreter_lifecycle_processors import (
    AsleepInItsCallbackAndSlowToTearDownProbe,
    AsleepInItsCallbackProbe,
    AsleepInItsCallbackRecordingItsTeardownProbe,
    StartsAProcessThatOutlivesItProbe,
    ThirtySecondImportProbe,
    WorkerKeepingTeardownGoingProbe,
)


@graph_generator
def one_processor_asleep_in_its_callback(stream_builder: StreamBuilder) -> None:
    """One processor that sleeps in `process()`."""
    stream_builder.add(AsleepInItsCallbackProbe)


@graph_generator
def two_processors_asleep_recording_their_teardown(stream_builder: StreamBuilder) -> None:
    """Two processors asleep in `process()`, each recording its own teardown."""
    for _ in range(2):
        stream_builder.add(AsleepInItsCallbackRecordingItsTeardownProbe)


@graph_generator
def three_processors_slow_to_tear_down(stream_builder: StreamBuilder) -> None:
    """Three processors asleep in `process()`, each three seconds over its teardown."""
    for _ in range(3):
        stream_builder.add(AsleepInItsCallbackAndSlowToTearDownProbe)


@graph_generator
def a_teardown_only_a_forced_shutdown_cuts_short(stream_builder: StreamBuilder) -> None:
    """One processor with a thirty-second teardown and a forked worker ignoring SIGTERM."""
    stream_builder.add(WorkerKeepingTeardownGoingProbe)


@graph_generator
def a_process_the_stream_started_outlives_it(stream_builder: StreamBuilder) -> None:
    """One processor that starts a process outliving `tatolabd`, holding every descriptor it can."""
    stream_builder.add(StartsAProcessThatOutlivesItProbe)


@graph_generator
def a_helper_still_importing(stream_builder: StreamBuilder) -> None:
    """One processor whose module takes thirty seconds to import in its helper."""
    stream_builder.add(ThirtySecondImportProbe)
