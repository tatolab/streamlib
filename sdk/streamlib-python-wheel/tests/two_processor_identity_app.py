# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One graph holding two classes from two modules.

Stops after `add`. Identity is derived there, and `run()` would buy the test
nothing but a GPU context.
"""

import streamlib
from identity_stable_processor import IdentityStableProcessor
from second_identity_stable_processor import SecondIdentityStableProcessor


def setup(rt) -> None:
    """Both processors, in one graph."""
    rt.add(IdentityStableProcessor)
    rt.add(SecondIdentityStableProcessor)
    print("MARKER:ADDED", flush=True)


def add_then_exit() -> None:
    runtime = streamlib.Runtime()
    try:
        setup(runtime)
    finally:
        runtime.shutdown()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    add_then_exit()
