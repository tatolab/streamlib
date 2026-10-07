# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One graph holding two classes from two modules.

Stops after `load`. Identity is derived there, and `run()` would buy the test
nothing but a GPU context.
"""

import sys
from pathlib import Path

import tatolab.runtime
from identity_stable_processor import IdentityStableProcessor
from second_identity_stable_processor import SecondIdentityStableProcessor
from tatolab.stream import StreamBuilder, compile_stream_to_graph, stream


@stream
def two_identity_stable_processors(stream_builder: StreamBuilder) -> None:
    """Both processors, in one graph."""
    stream_builder.add(IdentityStableProcessor)
    stream_builder.add(SecondIdentityStableProcessor)


def load_then_exit() -> None:
    runtime = tatolab.runtime.Runtime()
    try:
        runtime.load(
            compile_stream_to_graph(two_identity_stable_processors),
            project_directory=Path(__file__).resolve().parent,
            interpreter=sys.executable,
        )
        print("MARKER:ADDED", flush=True)
    finally:
        runtime.shutdown()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    load_then_exit()
