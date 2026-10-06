# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The engine, running in this interpreter's process.

`Runtime()` boots it, `rt.load(graph)` puts a stream's graph in it, and
`rt.run()` blocks until Ctrl-C with the GIL released. `_engine` is the native
extension behind it and behind every runtime-backed name `tatolab.stream`
re-exports.
"""

import atexit
import os
import weakref
from typing import Optional

from ._bundled_vulkan_driver import point_the_vulkan_loader_at_the_bundled_driver

# Before `_engine` loads, so no Vulkan instance can predate the driver search.
point_the_vulkan_loader_at_the_bundled_driver(os.environ)

from ._capability_extensions import (
    load_installed_capability_extensions_once_per_process,
)
from ._engine import CapabilityExtensionHost as CapabilityExtensionHost
from ._engine import capability_extension_host_for_the_app_process
from ._engine import Runtime as _NativeRuntime

__all__ = [
    "CapabilityExtensionHost",
    "Runtime",
]


# Engine threads must be joined before CPython finalizes. `Runtime.run()` does
# that on its own; this covers the paths where `run()` never returns normally —
# an exception between construction and `run()`, or an interpreter exiting while
# a Runtime is still referenced, where `__del__` ordering is not guaranteed.
_live_runtimes: "weakref.WeakSet[Runtime]" = weakref.WeakSet()


class Runtime(_NativeRuntime):
    """The engine, running in this process."""

    def __init__(
        self,
        *,
        runtime_name: Optional[str] = None,
    ) -> None:
        # Every constructor value is declared so this subclass accepts it and
        # unused because the engine already has it: a `#[pyclass]`'s
        # constructor is `__new__`, which `type.__call__` hands the arguments
        # before it calls this. A value missing here is refused at the call,
        # whatever the engine's own signature says — so this list and
        # `_engine.pyi`'s move together.
        del runtime_name
        super().__init__()
        # Registered before the hooks run, not after: a hook that raises leaves
        # a constructed engine behind whose threads still need joining, and the
        # `atexit` teardown below only reaches a Runtime it knows about.
        _live_runtimes.add(self)
        load_installed_capability_extensions_once_per_process(
            capability_extension_host_for_the_app_process
        )


@atexit.register
def _shut_down_live_runtimes() -> None:
    # Copied first: `shutdown()` can drop the last reference to a Runtime, and
    # mutating the WeakSet while iterating it would raise.
    for runtime in list(_live_runtimes):
        runtime.shutdown()
