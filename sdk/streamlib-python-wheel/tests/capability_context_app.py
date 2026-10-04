# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that run one capability-context probe in its real placement.

Run as a real `python app.py`: the probe's hooks execute in a helper process,
and the observation reaches this app — and the test driving it — over the same
log forwarding every child's records ride.
"""

import sys
from collections.abc import Callable

import streamlib
from streamlib import Stream, compile_stream_to_graph, stream

import capability_context_probes

SOURCE_AND_REPORTING_SINK_CLASS_NAMES_BY_SCENARIO = {
    "explicit_timestamp": ("ExplicitlyStampedSource", "TimestampCollectingSink"),
    "default_timestamp": ("DefaultStampedSource", "TimestampCollectingSink"),
    "worker_thread_source": ("WorkerThreadSource", "WorkerThreadBagSink"),
}


@stream
def one_capability_context_probe(stream: Stream) -> None:
    """The probe class `argv[1]` names, with no config."""
    stream.add(getattr(capability_context_probes, sys.argv[1]))


@stream
def one_config_probe_with_gain_and_label(stream: Stream) -> None:
    """A `ConfigProbe` configured with a gain and a label."""
    stream.add(
        capability_context_probes.ConfigProbe, config={"gain": 2.5, "label": "left"}
    )


@stream
def one_source_into_one_reporting_sink(stream: Stream) -> None:
    """The source `argv[1]` names into the sink that reports what it read."""
    source_class_name, sink_class_name = (
        SOURCE_AND_REPORTING_SINK_CLASS_NAMES_BY_SCENARIO[sys.argv[1]]
    )
    source = stream.add(getattr(capability_context_probes, source_class_name))
    sink = stream.add(getattr(capability_context_probes, sink_class_name))
    stream.connect(
        source.output("bags_to_downstream"), sink.input("bags_from_upstream")
    )


def run_stream_to_a_clean_exit(stream_function: Callable[[Stream], None]) -> None:
    """Load `stream_function`'s graph on a fresh `Runtime` and run it until stopped."""
    graph = compile_stream_to_graph(stream_function)
    runtime = streamlib.Runtime()
    runtime.load(graph)
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    scenario = sys.argv[1]
    if scenario == "configured_probe":
        run_stream_to_a_clean_exit(one_config_probe_with_gain_and_label)
    elif scenario in SOURCE_AND_REPORTING_SINK_CLASS_NAMES_BY_SCENARIO:
        run_stream_to_a_clean_exit(one_source_into_one_reporting_sink)
    else:
        run_stream_to_a_clean_exit(one_capability_context_probe)
