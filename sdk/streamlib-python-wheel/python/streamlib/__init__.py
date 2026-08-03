# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""StreamLib — a realtime streaming engine with Python authoring.

The engine runs in this interpreter's process: `Runtime()` boots it, `rt.add`
puts processors in its graph, `rt.connect` links them, and `rt.run()` blocks
until Ctrl-C with the GIL released.
"""

import atexit
import weakref

from . import log as log
from ._engine import AddedProcessor as AddedProcessor
from ._engine import ProcessorInputPortReference as ProcessorInputPortReference
from ._engine import ProcessorLinkDataAccess as ProcessorLinkDataAccess
from ._engine import ProcessorOutputPortReference as ProcessorOutputPortReference
from ._engine import Runtime as _NativeRuntime
from ._engine import media_clock_now_ns as media_clock_now_ns
from ._processor_declaration import LinkInputDataPort as LinkInputDataPort
from ._processor_declaration import LinkOutputDataPort as LinkOutputDataPort
from ._processor_declaration import processor as processor

__all__ = [
    "AddedProcessor",
    "LinkInputDataPort",
    "LinkOutputDataPort",
    "ProcessorInputPortReference",
    "ProcessorLinkDataAccess",
    "ProcessorOutputPortReference",
    "Runtime",
    "log",
    "media_clock_now_ns",
    "processor",
]


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
