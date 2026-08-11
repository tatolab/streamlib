# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""StreamLib — a realtime streaming engine with Python authoring.

The engine runs in this interpreter's process: `Runtime()` boots it, `rt.add`
puts processors in its graph, `rt.connect` links them, and `rt.run()` blocks
until Ctrl-C with the GIL released. Processors declare identity and ports with
`@processor` / `@input` / `@output` and receive a capability-typed context in
every lifecycle hook.
"""

import atexit
import weakref

from . import clock as clock
from . import log as log
from ._engine import AddedProcessor as AddedProcessor
from ._engine import GpuContextFullAccess as GpuContextFullAccess
from ._engine import GpuContextLimitedAccess as GpuContextLimitedAccess
from ._engine import GpuSurfaceHandle as GpuSurfaceHandle
from ._engine import LinkInputDataReader as LinkInputDataReader
from ._engine import LinkOutputDataWriter as LinkOutputDataWriter
from ._engine import MonotonicTimer as MonotonicTimer
from ._engine import ProcessorInputPortReference as ProcessorInputPortReference
from ._engine import ProcessorLinkDataAccess as ProcessorLinkDataAccess
from ._engine import ProcessorOutputPortReference as ProcessorOutputPortReference
from ._engine import CameraSource as CameraSource
from ._engine import DisplayWindow as DisplayWindow
from ._engine import Runtime as _NativeRuntime
from ._engine import RuntimeContextFullAccess as RuntimeContextFullAccess
from ._engine import RuntimeContextLimitedAccess as RuntimeContextLimitedAccess
from ._engine import TestPatternSource as TestPatternSource
from ._engine import media_clock_now_ns as media_clock_now_ns
from ._engine import monotonic_now_ns as monotonic_now_ns
from ._processor_declaration import input as input  # noqa: A004 — deliberate, see below
from ._processor_declaration import output as output
from ._processor_declaration import processor as processor
from .video_frame import ColorInfo as ColorInfo
from .video_frame import ContentLight as ContentLight
from .video_frame import MasteringDisplay as MasteringDisplay
from .video_frame import VideoFrame as VideoFrame

# `input` and `output` shadow the builtins at module scope on purpose — the
# authoring grammar reads `@input(...)` / `@output(...)`, matching the old SDK.
__all__ = [
    "AddedProcessor",
    "CameraSource",
    "ColorInfo",
    "ContentLight",
    "DisplayWindow",
    "GpuContextFullAccess",
    "GpuContextLimitedAccess",
    "GpuSurfaceHandle",
    "LinkInputDataReader",
    "LinkOutputDataWriter",
    "MasteringDisplay",
    "MonotonicTimer",
    "ProcessorInputPortReference",
    "ProcessorLinkDataAccess",
    "ProcessorOutputPortReference",
    "Runtime",
    "RuntimeContextFullAccess",
    "RuntimeContextLimitedAccess",
    "TestPatternSource",
    "VideoFrame",
    "clock",
    "input",
    "log",
    "media_clock_now_ns",
    "monotonic_now_ns",
    "output",
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
