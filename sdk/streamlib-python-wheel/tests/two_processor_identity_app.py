# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One graph holding two classes from two modules.

Stops after `load`. Identity is derived there, and `run()` would buy the test
nothing but a GPU context.
"""

import streamlib
from identity_stable_processor import IdentityStableProcessor
from second_identity_stable_processor import SecondIdentityStableProcessor
from streamlib import Stream, compile_stream_to_graph, stream


@stream
def two_identity_stable_processors(stream: Stream) -> None:
    """Both processors, in one graph."""
    stream.add(IdentityStableProcessor)
    stream.add(SecondIdentityStableProcessor)


def load_then_exit() -> None:
    runtime = streamlib.Runtime()
    try:
        runtime.load(compile_stream_to_graph(two_identity_stable_processors))
        print("MARKER:ADDED", flush=True)
    finally:
        runtime.shutdown()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    load_then_exit()
