# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Type stubs for the compiled engine module.

A type checker and an editor can read nothing out of `_engine.abi3.so`, so this
file is the only description of the native surface they get — without it,
`rt.add` offers no completion and `Runtime` resolves as unknown.

Hand-maintained, and kept honest by `mypy.stubtest`, which imports the built
module and compares it against this file in CI. `pyright --verifytypes` does not
catch that: it scores annotation completeness, so a stub describing a method the
binary no longer exports still reads as complete.
"""

from types import TracebackType
from typing import Any, Literal, final

from typing_extensions import disjoint_base

__all__ = [
    "AddedProcessor",
    "ProcessorInputPortReference",
    "ProcessorLinkDataAccess",
    "ProcessorOutputPortReference",
    "Runtime",
    "log_event",
    "media_clock_now_ns",
]

@disjoint_base
class Runtime:
    """The engine, running in this process."""

    def __init__(self) -> None: ...
    def add(
        self,
        processor_class: type,
        *,
        config: dict[str, Any] | None = None,
        display_name: str | None = None,
    ) -> AddedProcessor:
        """Add a processor class to the graph, configured with `config`."""

    def connect(
        self, source: ProcessorOutputPortReference, destination: ProcessorInputPortReference
    ) -> None:
        """Link one processor's output port to another's input port."""

    def run(self) -> None:
        """Run the pipeline until Ctrl-C, SIGTERM or `shutdown()`, then tear down."""

    def shutdown(self) -> None:
        """Ask the pipeline to stop. Safe from any thread; idempotent."""

    def __enter__(self) -> Runtime: ...
    # `Literal[False]`, not `bool`: `__exit__` never suppresses the exception,
    # and saying so is what lets a checker know that code after a `with` block
    # only runs when the block completed.
    def __exit__(
        self,
        exception_type: type[BaseException] | None = ...,
        exception: BaseException | None = ...,
        traceback: TracebackType | None = ...,
    ) -> Literal[False]: ...

@final
class AddedProcessor:
    """A processor in the graph."""

    @property
    def processor_id(self) -> str: ...
    @property
    def display_name(self) -> str: ...
    def output(self, port_name: str) -> ProcessorOutputPortReference: ...
    def input(self, port_name: str) -> ProcessorInputPortReference: ...
    def __repr__(self) -> str: ...

@final
class ProcessorOutputPortReference:
    """The producing end of a link."""

    def __repr__(self) -> str: ...

@final
class ProcessorInputPortReference:
    """The consuming end of a link."""

    def __repr__(self) -> str: ...

@final
class ProcessorLinkDataAccess:
    """One processor's links. The engine binds it to a port; app code never builds one."""

    def read_from_input_port(self, port_name: str) -> Any | None: ...
    def input_port_has_data(self, port_name: str) -> bool: ...
    def write_to_output_port(self, port_name: str, bag: Any) -> None: ...

def media_clock_now_ns() -> int:
    """The clock the engine stamps bags with, in nanoseconds."""

def log_event(level: str, target: str, message: str) -> None:
    """Emit one record on the engine's log pipeline."""
