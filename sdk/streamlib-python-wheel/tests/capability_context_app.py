# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that run one capability-context probe in its real placement.

Run as a real `python app.py`: the probe's hooks execute in a helper process,
and the observation reaches this app — and the test driving it — over the same
log forwarding every child's records ride.
"""

import sys

import streamlib

import capability_context_probes


def scenario_probe(probe_class_name: str, config: "dict | None" = None) -> None:
    """One probe, one graph."""
    runtime = streamlib.Runtime()
    runtime.add(getattr(capability_context_probes, probe_class_name), config=config)
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


def scenario_source_into_sink(source_class_name: str, sink_class_name: str) -> None:
    """A source into a sink, where the sink is the one that reports."""
    runtime = streamlib.Runtime()
    source = runtime.add(getattr(capability_context_probes, source_class_name))
    sink = runtime.add(getattr(capability_context_probes, sink_class_name))
    runtime.connect(
        source.output("bags_to_downstream"), sink.input("bags_from_upstream")
    )
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    scenario = sys.argv[1]
    if scenario == "configured_probe":
        scenario_probe("ConfigProbe", {"gain": 2.5, "label": "left"})
    elif scenario == "explicit_timestamp":
        scenario_source_into_sink("ExplicitlyStampedSource", "TimestampCollectingSink")
    elif scenario == "default_timestamp":
        scenario_source_into_sink("DefaultStampedSource", "TimestampCollectingSink")
    elif scenario == "worker_thread_source":
        scenario_source_into_sink("WorkerThreadSource", "WorkerThreadBagSink")
    else:
        scenario_probe(scenario)
