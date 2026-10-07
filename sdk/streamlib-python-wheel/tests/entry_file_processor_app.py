# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that add a processor whose class no interpreter could import.

Run as its own `python <script>.py` process because `__main__` is the thing
under test: under pytest the entry module is the test runner, so a class's
`__module__` can only be `"__main__"` in an app actually launched as one.
"""

import sys
from pathlib import Path

import tatolab.runtime
from tatolab.stream import StreamBuilder, compile_stream_to_graph, node, stream

MARKER_PREFIX = "MARKER:"


@node(execution="continuous", interval_ms=1)
class EntryFileProcessor:
    """Declared in the entry file, which is exactly what makes it unhostable."""

    def process(self, ctx) -> None: ...


def marker(name: str) -> None:
    print(f"{MARKER_PREFIX}{name}", flush=True)


@stream
def a_processor_defined_in_the_entry_file(stream_builder: StreamBuilder) -> None:
    stream_builder.add(EntryFileProcessor)


@stream
def a_function_local_processor(stream_builder: StreamBuilder) -> None:
    def build_processor() -> type:
        @node(execution="continuous", interval_ms=1)
        class FunctionLocalProcessor:
            def process(self, ctx) -> None: ...

        return FunctionLocalProcessor

    stream_builder.add(build_processor())


@stream
def an_importable_processor(stream_builder: StreamBuilder) -> None:
    """The same stream, one import line different — the fix the refusal names."""
    from zero_argument_process_processor import ZeroArgumentProcess

    stream_builder.add(ZeroArgumentProcess)


def compile_reporting_its_refusal(stream_function) -> None:
    try:
        compile_stream_to_graph(stream_function)
    except ValueError as refusal:
        marker(f"REFUSED={refusal}".replace("\n", "\\n"))
    else:
        marker("ACCEPTED")
    marker("CLEAN_EXIT")


def scenario_entry_file_class_is_refused() -> None:
    compile_reporting_its_refusal(a_processor_defined_in_the_entry_file)


def scenario_function_local_class_is_refused() -> None:
    compile_reporting_its_refusal(a_function_local_processor)


def scenario_importable_class_is_accepted() -> None:
    graph = compile_stream_to_graph(an_importable_processor)
    runtime = tatolab.runtime.Runtime()
    runtime.load(
        graph,
        project_directory=Path(__file__).resolve().parent,
        interpreter=sys.executable,
    )
    marker("ACCEPTED")
    runtime.shutdown()
    marker("CLEAN_EXIT")


SCENARIOS = {
    "entry_file_class_is_refused": scenario_entry_file_class_is_refused,
    "function_local_class_is_refused": scenario_function_local_class_is_refused,
    "importable_class_is_accepted": scenario_importable_class_is_accepted,
}


if __name__ == "__main__":
    SCENARIOS[sys.argv[1]]()
