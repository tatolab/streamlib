# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""An app whose only processor defines `process` without the ctx parameter.

Run as its own `python <script>.py` process because the failure under test is
a log line: the engine's tracing writer binds this process's stdout at first
boot, so only a parent reading the pipe observes it reliably — capfd inside the
test process sees nothing once another test booted an engine first.
"""

import sys
from pathlib import Path

import tatolab.runtime
from tatolab.stream import StreamBuilder, compile_stream_to_graph, stream
from zero_argument_process_processor import ZeroArgumentProcess


@stream
def zero_argument_process(stream_builder: StreamBuilder) -> None:
    """The one processor whose `process` takes no ctx."""
    stream_builder.add(ZeroArgumentProcess)


if __name__ == "__main__":
    graph = compile_stream_to_graph(zero_argument_process)
    runtime = tatolab.runtime.Runtime()
    runtime.load(
        graph,
        project_directory=Path(__file__).resolve().parent,
        interpreter=sys.executable,
    )
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)
