# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One app, launched three ways, adding one class.

`streamlib dev` executes this file under the name `__main__` too — it runs the
entry file exactly as `python app.py` does, then calls its `setup(rt)`. So the
direct arms announce themselves with an argument rather than with `__name__`:
the launcher narrows `sys.argv` to the entry file alone, and an app that keyed
off `__name__` would build a second runtime inside the launcher's process.

The direct arms stop after `add`. Identity is derived there, and `run()` would
buy the test nothing but a GPU context.
"""

import sys

import streamlib
from identity_stable_processor import IdentityStableProcessor

DIRECT_LAUNCH_ARGUMENT = "add-then-exit"


def setup(rt) -> None:
    """The pipeline, as `streamlib dev` and the direct arms alike build it."""
    rt.add(IdentityStableProcessor)
    print("MARKER:ADDED", flush=True)


def add_then_exit() -> None:
    runtime = streamlib.Runtime()
    try:
        setup(runtime)
    finally:
        runtime.shutdown()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__" and sys.argv[1:2] == [DIRECT_LAUNCH_ARGUMENT]:
    add_then_exit()
