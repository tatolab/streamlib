# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""StreamLib — a realtime streaming engine with Python authoring.

The engine runs in this interpreter's process: `Runtime()` boots it and
`rt.run()` blocks until Ctrl-C with the GIL released.
"""

import atexit
import weakref

from ._engine import Runtime as _NativeRuntime

__all__ = ["Runtime"]


# Engine threads must be joined before CPython finalizes. `Runtime.run()` does
# that on its own; this covers the paths where `run()` never returns normally —
# an exception between construction and `run()`, or an interpreter exiting while
# a Runtime is still referenced, where `__del__` ordering is not guaranteed.
_live_runtimes: "weakref.WeakSet[Runtime]" = weakref.WeakSet()


class Runtime(_NativeRuntime):
    """The engine, running in this process."""

    def __init__(self) -> None:
        super().__init__()
        _live_runtimes.add(self)


@atexit.register
def _shut_down_live_runtimes() -> None:
    # Copied first: `shutdown()` can drop the last reference to a Runtime, and
    # mutating the WeakSet while iterating it would raise.
    for runtime in list(_live_runtimes):
        runtime.shutdown()
