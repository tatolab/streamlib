# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that add a processor whose class no interpreter could import.

Run as a real `python app.py` because `__main__` is the thing under test: under
pytest the entry module is the test runner, so a class's `__module__` can only
be `"__main__"` in an app actually launched as one.
"""

import sys

import streamlib
from streamlib import Stream, compile_stream_to_graph, node, stream

MARKER_PREFIX = "MARKER:"


@node(execution="continuous", interval_ms=1)
class EntryFileProcessor:
    """Declared in the entry file, which is exactly what makes it unhostable."""

    def process(self, ctx) -> None: ...


def marker(name: str) -> None:
    print(f"{MARKER_PREFIX}{name}", flush=True)


@stream
def entry_file_class(stream: Stream) -> None:
    stream.add(EntryFileProcessor)


@stream
def function_local_class(stream: Stream) -> None:
    def build_processor() -> type:
        @node(execution="continuous", interval_ms=1)
        class FunctionLocalProcessor:
            def process(self, ctx) -> None: ...

        return FunctionLocalProcessor

    stream.add(build_processor())


@stream
def importable_class(stream: Stream) -> None:
    """The same stream, one import line different — the fix the refusal names."""
    from zero_argument_process_processor import ZeroArgumentProcess

    stream.add(ZeroArgumentProcess)


def compile_reporting_its_refusal(stream_function) -> None:
    try:
        compile_stream_to_graph(stream_function)
    except ValueError as refusal:
        marker(f"REFUSED={refusal}".replace("\n", "\\n"))
    else:
        marker("ACCEPTED")
    marker("CLEAN_EXIT")


def scenario_entry_file_class_is_refused() -> None:
    compile_reporting_its_refusal(entry_file_class)


def scenario_function_local_class_is_refused() -> None:
    compile_reporting_its_refusal(function_local_class)


def scenario_importable_class_is_accepted() -> None:
    graph = compile_stream_to_graph(importable_class)
    runtime = streamlib.Runtime()
    runtime.load(graph)
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
