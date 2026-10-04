# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that run one ray-tracing-kernel probe in its real placement.

Run as a real `python app.py`: the probe builds its scene, its kernel and its
traced output from a helper process, and its observation reaches this app — and
the test driving it — over the child→parent log forwarding.
"""

import sys

import streamlib
from streamlib import Stream, compile_stream_to_graph, stream

import ray_tracing_kernel_probes


def _probe_class_named_on_the_command_line() -> type:
    return getattr(ray_tracing_kernel_probes, sys.argv[1])


@stream
def one_ray_tracing_kernel_probe(stream: Stream) -> None:
    """A kernel probe needs no upstream: it builds its own acceleration
    structures, acquires its own storage image and reports from `setup`."""
    stream.add(_probe_class_named_on_the_command_line())


if __name__ == "__main__":
    graph = compile_stream_to_graph(one_ray_tracing_kernel_probe)
    runtime = streamlib.Runtime()
    runtime.load(graph)
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)
