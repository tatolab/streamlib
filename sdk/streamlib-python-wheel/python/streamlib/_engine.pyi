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
from collections.abc import Callable, Mapping
from typing import Any, Literal, TypeVar, final

from typing_extensions import disjoint_base

_EscalateResult = TypeVar("_EscalateResult")

__all__ = [
    "AddedProcessor",
    "GpuContextFullAccess",
    "GpuContextLimitedAccess",
    "GpuSurfaceHandle",
    "LinkInputDataReader",
    "LinkOutputDataWriter",
    "MonotonicTimer",
    "ProcessorInputPortReference",
    "ProcessorLinkDataAccess",
    "ProcessorOutputPortReference",
    "CameraSource",
    "DisplayWindow",
    "Runtime",
    "RuntimeContextFullAccess",
    "RuntimeContextLimitedAccess",
    "TestPatternSource",
    "log_event",
    "media_clock_now_ns",
    "monotonic_now_ns",
]

@final
class CameraSource:
    """Native built-in block: live V4L2 camera capture (Linux).

    A marker type — pass the class itself to `Runtime.add`
    (`rt.add(CameraSource, config={"device_id": "/dev/video0"})`); it is
    never instantiated and its per-frame path never enters the interpreter.
    Camera→GPU transport auto-selects zero-copy DMA-BUF or CPU upload.
    """

@final
class DisplayWindow:
    """Native built-in block: video frames in a vsync'd window (Linux).

    A marker type — pass the class itself to `Runtime.add`
    (`rt.add(DisplayWindow, config={"title": "My app", "scaling": "fit"})`);
    it is never instantiated and its per-frame path never enters the
    interpreter. `scaling` is `"fit"`, `"fill"`, or `"stretch"`.

    One window per process today: the display owns the process-wide event
    loop, so a second DisplayWindow logs an error and drains its input
    without showing anything.
    """

@final
class TestPatternSource:
    """Native built-in block: SMPTE-style color bars, no hardware.

    A marker type — pass the class itself to `Runtime.add`
    (`rt.add(TestPatternSource, config={"width": 1280, "height": 720})`);
    it is never instantiated and its per-frame path never enters the
    interpreter.
    """

    # Keeps pytest from collecting the `Test*`-named class in user suites.
    __test__: Literal[False]

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
    """One processor's links. The engine binds it; app code never builds one."""

    def read_from_input_port(self, port_name: str) -> Any | None: ...
    def read_from_input_port_with_timestamp(
        self, port_name: str
    ) -> tuple[Any, int] | tuple[None, None]: ...
    def input_port_has_data(self, port_name: str) -> bool: ...
    def write_to_output_port(
        self,
        port_name: str,
        bag: Mapping[str, Any],
        timestamp_ns: int | None = None,
    ) -> None: ...

@final
class RuntimeContextFullAccess:
    """Privileged runtime context handed to `setup` / `teardown` / `start` / `stop`.

    Lease-bound members are only valid during the hook that received the
    context; touching them afterwards raises `RuntimeError`.
    """

    @property
    def config(self) -> dict[str, Any]: ...
    @property
    def time(self) -> int: ...
    @property
    def inputs(self) -> LinkInputDataReader: ...
    @property
    def outputs(self) -> LinkOutputDataWriter: ...
    @property
    def gpu_limited_access(self) -> GpuContextLimitedAccess: ...
    @property
    def gpu_full_access(self) -> GpuContextFullAccess: ...
    @property
    def runtime_id(self) -> str: ...
    @property
    def processor_id(self) -> str | None: ...
    def is_paused(self) -> bool: ...
    def should_process(self) -> bool: ...

@final
class RuntimeContextLimitedAccess:
    """Restricted runtime context handed to `process` / `on_pause` / `on_resume`.

    `gpu_full_access` is deliberately absent — reaching for it raises
    `AttributeError`, mirroring the Rust capability split.
    """

    @property
    def config(self) -> dict[str, Any]: ...
    @property
    def time(self) -> int: ...
    @property
    def inputs(self) -> LinkInputDataReader: ...
    @property
    def outputs(self) -> LinkOutputDataWriter: ...
    @property
    def gpu_limited_access(self) -> GpuContextLimitedAccess: ...
    @property
    def runtime_id(self) -> str: ...
    @property
    def processor_id(self) -> str | None: ...
    def is_paused(self) -> bool: ...
    def should_process(self) -> bool: ...

@final
class LinkInputDataReader:
    """A processor's input ports, as `ctx.inputs`."""

    def read(self, port_name: str) -> Any | None: ...
    def read_with_timestamp(
        self, port_name: str
    ) -> tuple[Any, int] | tuple[None, None]: ...
    def has_data(self, port_name: str) -> bool: ...

@final
class LinkOutputDataWriter:
    """A processor's output ports, as `ctx.outputs`."""

    def write(
        self,
        port_name: str,
        bag: Mapping[str, Any],
        timestamp_ns: int | None = None,
    ) -> None:
        """Publish one bag to every downstream link on `port_name`.

        Writes past a `lossless` link's ceiling block; on other profiles an
        over-ceiling write is silently dropped.
        """

@final
class GpuContextLimitedAccess:
    """Non-allocating GPU capability, valid for the whole processor life."""

    def acquire_pixel_buffer(
        self, width: int, height: int, format: str = "bgra"
    ) -> GpuSurfaceHandle: ...
    def acquire_texture(
        self, width: int, height: int, format: str, usage: list[str]
    ) -> GpuSurfaceHandle: ...
    def resolve_surface(self, surface_id: str) -> GpuSurfaceHandle: ...
    def escalate(self, privileged_callback: Callable[[GpuContextFullAccess], _EscalateResult]) -> _EscalateResult:
        """Run the callback with a temporary full-access capability, camera-pattern style."""

@final
class GpuContextFullAccess:
    """Privileged GPU capability, valid only while a full-access hook runs."""

    def acquire_pixel_buffer(
        self, width: int, height: int, format: str = "bgra"
    ) -> GpuSurfaceHandle: ...
    def acquire_texture(
        self, width: int, height: int, format: str, usage: list[str]
    ) -> GpuSurfaceHandle: ...
    def export_dma_buf(self, surface: GpuSurfaceHandle) -> tuple[int, int]:
        """Export a DMA-BUF file descriptor for `surface`, as `(fd, byte_size)`.

        The caller owns the fd and must close it, or hand it to something that
        takes ownership. Only an ordinary pixel buffer can answer — a
        device-exchange buffer is OPAQUE_FD-flavoured.
        """

    def import_dma_buf(
        self,
        fd: int,
        width: int,
        height: int,
        format: str = "bgra",
        byte_size: int | None = None,
    ) -> GpuSurfaceHandle:
        """Import a DMA-BUF file descriptor as a surface this graph can read.

        Takes ownership of `fd` on success — the driver adopts it and it must
        not be closed afterwards. On failure the fd is still the caller's.
        """

    def wait_device_idle(self) -> None: ...

@final
class GpuSurfaceHandle:
    """An owned GPU surface, and the pixels behind it."""

    @property
    def surface_id(self) -> str: ...
    @property
    def width(self) -> int: ...
    @property
    def height(self) -> int: ...
    @property
    def format(self) -> str: ...
    @property
    def bytes_per_row(self) -> int:
        """Row pitch in bytes, including any padding the allocation carries."""

    @property
    def base_address(self) -> int | None:
        """Base address of the host mapping, or None when not locked."""

    def close(self) -> None:
        """Release the underlying GPU resource. Idempotent."""

    def __enter__(self) -> GpuSurfaceHandle: ...
    def __exit__(
        self,
        exception_type: type[BaseException] | None = ...,
        exception: BaseException | None = ...,
        traceback: TracebackType | None = ...,
    ) -> Literal[False]: ...
    def lock(self, read_only: bool = True) -> None:
        """Open CPU access, declaring read or write intent.

        Performs no wait — ordering against the producer comes from
        publication, since a source finishes its GPU work before it sends the
        frame on. `read_only=False` marks an exported tensor writable.
        """

    def unlock(self) -> None:
        """Close CPU access, publishing any pending device-side write back
        into the surface first. Idempotent.
        """

    def as_numpy(self) -> Any:
        """A numpy view sharing memory with the surface. Requires a lock."""

    def __dlpack_device__(self) -> tuple[int, int]: ...
    def __dlpack__(
        self,
        stream: Any | None = ...,
        max_version: tuple[int, int] | None = ...,
        dl_device: tuple[int, int] | None = ...,
        copy: bool | None = ...,
    ) -> Any:
        """A DLPack capsule over the pixels. Requires a lock.

        A graph frame's natural side is the device: with a usable CUDA
        runtime the tensor is GPU-resident (one engine-side blit into an
        exportable staging buffer — zero CPU copies, never claimed
        copy-free); otherwise, or with `dl_device=(1, 0)`, it is the host
        mapping. A writable device tensor's edits publish back to the
        surface at `unlock()`.

        The tensor may outlive this handle: it holds its own share of the
        surface, so the pool slot is not reused until the tensor is released.
        """

@final
class MonotonicTimer:
    """Drift-free periodic timer backed by `timerfd_create(CLOCK_MONOTONIC)`.

    The first absolute deadline is `now + interval`, then `TFD_TIMER_ABSTIME`
    repeats, so ticks never accumulate drift.
    """

    def __new__(cls, interval_ns: int) -> MonotonicTimer: ...
    @property
    def interval_ns(self) -> int: ...
    def wait(self, timeout_ms: int = 100) -> int:
        """Wait up to `timeout_ms` for the next tick.

        Returns a positive expiration count when a tick fired, 0 on timeout,
        -1 once closed.
        """

    def close(self) -> None:
        """Release the timer's file descriptor. Idempotent."""

    def __enter__(self) -> MonotonicTimer: ...
    def __exit__(
        self,
        exception_type: type[BaseException] | None = ...,
        exception: BaseException | None = ...,
        traceback: TracebackType | None = ...,
    ) -> Literal[False]: ...

def monotonic_now_ns() -> int:
    """Current monotonic time in nanoseconds via `clock_gettime(CLOCK_MONOTONIC)`."""

def media_clock_now_ns() -> int:
    """The clock the engine stamps bags with, in nanoseconds.

    Not the system-wide `CLOCK_MONOTONIC` epoch — the origin is this process's
    engine start, so a value from one process means nothing in another.
    """

def log_event(
    level: str, message: str, attrs: dict[str, Any] | None = None
) -> None:
    """Emit one record on the engine's log pipeline, with structured attrs."""
