# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A stream whose Python nodes run in processor interpreters started from the
interpreter named on the command line.

Run as its own `python <script>.py <scenario> <interpreter>` process: the claim
is about which interpreter a child runs in, and only a parent can name one.
"""

import sys
from pathlib import Path

import tatolab.runtime
from processor_interpreter_lend_probes import (
    CountsBagsFromUpstreamSink,
    ReportsItsInterpreterSource,
)
from tatolab.stream import StreamBuilder, compile_stream_to_graph, stream

MARKER_PREFIX = "MARKER:"


def marker(name: str) -> None:
    print(f"{MARKER_PREFIX}{name}", flush=True)


@stream
def interpreter_reporting_source_into_counting_sink(stream_builder: StreamBuilder) -> None:
    """A source announcing its interpreter into a sink counting what it received."""
    source = stream_builder.add(ReportsItsInterpreterSource)
    sink = stream_builder.add(CountsBagsFromUpstreamSink)
    stream_builder.connect(
        source.output("bags_to_downstream"), sink.input("bags_from_upstream")
    )


def scenario_nodes_run_in_the_named_interpreter(interpreter: str) -> None:
    graph = compile_stream_to_graph(interpreter_reporting_source_into_counting_sink)
    runtime = tatolab.runtime.Runtime()
    runtime.load(
        graph,
        project_directory=Path(__file__).resolve().parent,
        interpreter=interpreter,
    )
    marker(f"LEND_DIRECTORY={Path(tatolab.runtime.__file__).resolve().parent.parent.parent}")
    runtime.run()
    marker("CLEAN_EXIT")


SCENARIOS = {
    "nodes_run_in_the_named_interpreter": scenario_nodes_run_in_the_named_interpreter,
}


if __name__ == "__main__":
    SCENARIOS[sys.argv[1]](sys.argv[2])
