# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that run one compute-kernel probe in its real placement.

Run as its own `python <script>.py` process: the probe builds and dispatches
its kernel from a helper process, and its observation reaches this app — and
the test driving it — over the child→parent log forwarding.
"""

import sys
from pathlib import Path

import tatolab.runtime
from tatolab.stream import StreamBuilder, compile_stream_to_graph, stream

import compute_kernel_probes


def _probe_class_named_on_the_command_line() -> type:
    return getattr(compute_kernel_probes, sys.argv[1])


@stream
def one_standalone_compute_kernel_probe(stream_builder: StreamBuilder) -> None:
    """A kernel probe needs no upstream: it acquires both surfaces itself and
    reports from `setup`."""
    stream_builder.add(_probe_class_named_on_the_command_line())


if __name__ == "__main__":
    graph = compile_stream_to_graph(one_standalone_compute_kernel_probe)
    runtime = tatolab.runtime.Runtime()
    runtime.load(
        graph,
        project_directory=Path(__file__).resolve().parent,
        interpreter=sys.executable,
    )
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)
