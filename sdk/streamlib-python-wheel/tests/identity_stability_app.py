# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One stream, launched three ways, adding one class.

`streamlib dev` executes this file under the name `__main__` too, then compiles
the file's sole `@stream` function and loads it. So the direct arms announce
themselves with an argument rather than with `__name__`: the launcher narrows
`sys.argv` to the entry file alone, and an app that keyed off `__name__` would
build a second runtime inside the launcher's process.

The direct arms stop after `load`. Identity is derived there, and `run()` would
buy the test nothing but a GPU context.
"""

import sys

import streamlib
from identity_stable_processor import IdentityStableProcessor
from streamlib import Stream, compile_stream_to_graph, stream

DIRECT_LAUNCH_ARGUMENT = "load-then-exit"


@stream
def identity_stability(stream: Stream) -> None:
    """The graph `streamlib dev` and the direct arms alike load."""
    stream.add(IdentityStableProcessor)


def load_then_exit() -> None:
    runtime = streamlib.Runtime()
    try:
        runtime.load(compile_stream_to_graph(identity_stability))
        print("MARKER:ADDED", flush=True)
    finally:
        runtime.shutdown()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__" and sys.argv[1:2] == [DIRECT_LAUNCH_ARGUMENT]:
    load_then_exit()
