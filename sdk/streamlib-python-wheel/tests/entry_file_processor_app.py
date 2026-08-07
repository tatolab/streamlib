# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that add a processor whose class no interpreter could import.

Run as a real `python app.py` because `__main__` is the thing under test: under
pytest the entry module is the test runner, so a class's `__module__` can only
be `"__main__"` in an app actually launched as one.
"""

import sys

import streamlib
from streamlib import processor

MARKER_PREFIX = "MARKER:"


@processor(execution="continuous", interval_ms=1)
class EntryFileProcessor:
    """Declared in the entry file, which is exactly what makes it unhostable."""

    def process(self, ctx) -> None: ...


def marker(name: str) -> None:
    print(f"{MARKER_PREFIX}{name}", flush=True)


def scenario_entry_file_class_is_refused() -> None:
    runtime = streamlib.Runtime()
    try:
        runtime.add(EntryFileProcessor)
    except ValueError as refusal:
        marker(f"REFUSED={refusal}".replace("\n", "\\n"))
    else:
        marker("ACCEPTED")
    runtime.shutdown()
    marker("CLEAN_EXIT")


def scenario_function_local_class_is_refused() -> None:
    def build_processor() -> type:
        @processor(execution="continuous", interval_ms=1)
        class FunctionLocalProcessor:
            def process(self, ctx) -> None: ...

        return FunctionLocalProcessor

    runtime = streamlib.Runtime()
    try:
        runtime.add(build_processor())
    except ValueError as refusal:
        marker(f"REFUSED={refusal}".replace("\n", "\\n"))
    else:
        marker("ACCEPTED")
    runtime.shutdown()
    marker("CLEAN_EXIT")


def scenario_importable_class_is_accepted() -> None:
    """The same app, one import line different — the fix the refusal names."""
    from zero_argument_process_processor import ZeroArgumentProcess

    runtime = streamlib.Runtime()
    runtime.add(ZeroArgumentProcess)
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
