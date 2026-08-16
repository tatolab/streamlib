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

from pathlib import Path
from types import TracebackType
from collections.abc import Callable, Mapping
from typing import Any, Literal, TypeVar, final, overload

from typing_extensions import disjoint_base

_EscalateResult = TypeVar("_EscalateResult")
_BagReadTarget = TypeVar("_BagReadTarget")

__all__ = [
    "AddedProcessor",
    "GpuContextFullAccess",
    "GpuContextLimitedAccess",
    "GpuSurfaceCheckOutLease",
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
    "TestBagCollector",
    "TestBagFeeder",
    "await_test_harness_bag",
    "close_test_harness_channel",
    "feed_test_harness_bag",
    "gpu_limited_access_of_the_typed_read_in_progress",
    "log_event",
    "monotonic_now_ns",
    "open_test_harness_channel",
    "runtime_log_directory",
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

@final
class TestBagFeeder:
    """`streamlib.testing`'s feeder endpoint: publishes bags a test queued.

    A marker type, like the media built-ins — never instantiated, resolved by
    `Runtime.add`. Native so that its queue lives in the app process, where the
    test reading it does.
    """

    # Keeps pytest from collecting the `Test*`-named class in user suites.
    __test__: Literal[False]

@final
class TestBagCollector:
    """`streamlib.testing`'s collector endpoint: records every bag produced."""

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

    # `bind_host` is `...` rather than its literal default because the binding
    # builds that string at call time, which is what the compiled signature
    # reports — the same shape `__exit__` below has.
    def host_control_plane(
        self,
        *,
        bind_host: str = ...,
        bind_port: int = 9000,
        node_name: str | None = None,
    ) -> None:
        """Host the control plane in this process, so the node is discoverable.

        Binds all interfaces (`0.0.0.0`) and port 9000 by default, incrementing
        the port on collision. Opt-in: a runtime that never calls this
        publishes no node-registry entry. Call it before `run()`.
        """

    def run(self) -> None:
        """Run the pipeline until Ctrl-C, SIGTERM or `shutdown()`, then tear down."""

    def wait_until_every_processor_is_running(self, *, timeout: float = 30.0) -> None:
        """Block until every processor in the graph is running.

        Call it before `run()` or from another thread while `run()` blocks — a
        graph that has not started yet is waited through, not refused. A Python
        processor is running once its helper process has registered and wired
        its ports; anything published into the graph before that is dropped by
        the link. Raises `RuntimeError` if a processor failed instead of
        starting, if `timeout` elapses, or if this runtime has already been
        shut down; and `ValueError` for a `timeout` that is negative, NaN, or
        too large to be a duration.
        """

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
    """One processor's links. The engine binds it; app code never builds one.

    Constructing one opens a helper process's own data plane, with its own
    iceoryx2 node — only `streamlib._helper` does that.
    """

    def __new__(cls) -> ProcessorLinkDataAccess: ...
    def wire_output_link(
        self,
        port_name: str,
        channel_service_name: str,
        dest_notify_service_name: str,
        expected_payload_bytes: int,
        max_payload_bytes_per_channel: int,
        max_queued_messages: int,
        max_subscribers: int,
        notify_max_notifiers: int,
        enable_safe_overflow: bool,
        link_id: str,
    ) -> None: ...
    def wire_input_link(
        self,
        port_name: str,
        channel_service_name: str,
        notify_service_name: str,
        read_mode: str,
        max_queued_messages: int,
        max_subscribers: int,
        notify_max_notifiers: int,
        enable_safe_overflow: bool,
        link_id: str,
    ) -> None: ...
    def unwire_output_link(self, port_name: str, link_id: str) -> None: ...
    def unwire_input_link(self, link_id: str) -> None: ...
    def input_listener_fd(self) -> int | None: ...
    def drain_input_listener(self) -> None: ...
    def any_input_port_has_data(self) -> bool: ...
    @overload
    def read_from_input_port(
        self, port_name: str, *, into: None = None
    ) -> Any | None: ...
    @overload
    def read_from_input_port(
        self, port_name: str, *, into: Callable[..., _BagReadTarget]
    ) -> _BagReadTarget | None: ...
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

    Built in the helper process the processor runs in; app code never
    constructs one.
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
    def processor_id(self) -> str: ...
    def is_paused(self) -> bool: ...
    def should_process(self) -> bool: ...
    @staticmethod
    def open_for_helper_process(
        configuration: Mapping[str, Any],
        link_data_access: ProcessorLinkDataAccess,
        runtime_id: str,
        processor_id: str,
        escalate_request_to_parent: Callable[[dict[str, Any]], dict[str, Any]] | None = None,
    ) -> RuntimeContextFullAccess: ...
    def limited_access_view_for_helper_process(self) -> RuntimeContextLimitedAccess: ...
    def note_pause_state_from_parent(self, paused: bool) -> None: ...

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
    def processor_id(self) -> str: ...
    def is_paused(self) -> bool: ...
    def should_process(self) -> bool: ...

@final
class LinkInputDataReader:
    """A processor's input ports, as `ctx.inputs`."""

    @overload
    def read(self, port_name: str, *, into: None = None) -> Any | None: ...
    @overload
    def read(
        self, port_name: str, *, into: Callable[..., _BagReadTarget]
    ) -> _BagReadTarget | None:
        """The next bag on `port_name`, read into `into`.

        The opt-in strictness dial. A TypedDict casts for free — the bag
        arrives as itself, unvalidated. A dataclass or pydantic model is
        constructed from the bag's entries, so a bag that does not fit raises
        here, at the consuming read.
        """

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
    ) -> GpuSurfaceHandle:
        """Acquire a pooled device texture, named by the surface id the engine minted.

        The id is the whole handle: a kernel dispatch binds it, and a downstream
        processor resolves it. The texture's memory is not mapped into this
        process, so its pixels are not addressable here.
        """
    def resolve_surface(self, surface_id: str) -> GpuSurfaceHandle: ...
    def claim_surface_against_producer_reuse(
        self, surface_id: str
    ) -> GpuSurfaceCheckOutLease:
        """Claim a published surface until the returned lease is dropped.

        The cheap half of `resolve_surface`: it holds the frame still without
        importing its memory, so an object that wants only the pixels it was
        handed to stay put can keep the lease in a field and let its own
        lifetime do the releasing.
        """

    def escalate(self, privileged_callback: Callable[[GpuContextFullAccess], _EscalateResult]) -> _EscalateResult:
        """Refuses: the callback's one atomic privileged scope cannot span a
        process boundary. The operations it wrapped are methods on this
        capability and on `ctx.gpu_full_access` — call them directly."""

@final
class GpuContextFullAccess:
    """The privileged GPU capability a full-access hook receives.

    Each method is its own escalate round trip to the parent, which runs the
    privileged work against the engine and answers with a handle.
    """

    def acquire_pixel_buffer(
        self, width: int, height: int, format: str = "bgra"
    ) -> GpuSurfaceHandle: ...
    def acquire_texture(
        self, width: int, height: int, format: str, usage: list[str]
    ) -> GpuSurfaceHandle:
        """Acquire a pooled device texture through the privileged path.

        The id is the whole handle: a kernel dispatch binds it, and a downstream
        processor resolves it. The texture's memory is not mapped into this
        process, so its pixels are not addressable here.
        """

    def create_compute_kernel(
        self,
        spirv: bytes,
        push_constant_size: int = 0,
        bindings: dict[str, str] | None = None,
    ) -> ComputeKernel:
        """Build a compute kernel from pre-compiled SPIR-V.

        Constructed once in `setup()`, dispatched per frame in `process()`. The
        engine reflects the shader at construction and takes its binding names
        from it — those names are what `dispatch` resolves against. Re-creating
        an identical kernel is free of compilation.

        `bindings` optionally asserts `{name: kind}` against reflection; each
        kind is one of `sampled_image`, `sampled_texture`, `storage_buffer`,
        `storage_image`, `uniform_buffer`.
        """

    def export_dma_buf(self, surface: GpuSurfaceHandle) -> tuple[int, int]:
        """Export a DMA-BUF file descriptor for `surface`, as `(fd, byte_size)`.

        The caller owns the fd and must close it, or hand it to something that
        takes ownership. Answered without leaving this process: the fds arrived
        over SCM_RIGHTS when the surface was checked out, and they are the same
        ones a host-side export would mint.
        """

    def import_dma_buf(
        self,
        fd: int,
        width: int,
        height: int,
        format: str = "bgra",
        byte_size: int | None = None,
    ) -> GpuSurfaceHandle:
        """Refuses from a Python processor: the surface registry a graph reads
        lives in the app process, and handing it an fd needs a wire that carries
        one. Exporting works — `export_dma_buf` answers from this process. See
        #1756."""

    def wait_device_idle(self) -> None: ...
    def escalate(self, privileged_callback: Callable[[GpuContextFullAccess], _EscalateResult]) -> _EscalateResult:
        """Refuses: the callback's one atomic privileged scope cannot span a
        process boundary. The operations it wrapped are methods on this
        capability and on `ctx.gpu_limited_access` — call them directly."""

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
class GpuSurfaceCheckOutLease:
    """A claim on a published surface, held for as long as this object is.

    While a claim is outstanding the pool never rehands that surface's slot to
    its producer, and dropping this object is the release — there is nothing to
    call. Claims are counted, so holding one and resolving the same surface for
    its pixels are independent.
    """

    @property
    def surface_id(self) -> str:
        """The surface this claim holds still."""

@final
class ComputeKernel:
    """A compute kernel the engine built and holds, dispatched by name.

    Constructed in `setup()` where the capability is Full, dispatched per frame
    in `process()`. No kernel handle string, fence, timeline or slot number
    reaches Python — the object is the handle.
    """

    @property
    def binding_names(self) -> list[str]:
        """The shader's own names for this kernel's bindings, in slot order."""

    def dispatch(
        self,
        bindings: dict[str, GpuSurfaceHandle | str],
        group_count: tuple[int, int, int],
        push_constants: bytes | None = None,
    ) -> None:
        """Dispatch, binding each of the shader's declared resources by name.

        Bindings never persist on the kernel, so every dispatch supplies all of
        them: there is no implicit default and no value carried over from the
        previous frame. Supplying an unknown name, omitting a declared one, or
        naming a surface of the wrong kind raises before anything is submitted.

        Returns when the GPU work has retired and the writes are visible.
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

def gpu_limited_access_of_the_typed_read_in_progress() -> GpuContextLimitedAccess | None:
    """The GPU capability of the `read(port, into=T)` currently constructing an
    object, or `None` when nothing is being read into a type.

    The same capability as `ctx.gpu_limited_access`, offered so a type can do
    per-frame work at construction that needs the engine — claiming the frame's
    surface against producer reuse is what the shipped `VideoFrame` does with
    it. Any class reachable through `into=` may call this; there is no
    registration, no marker and no privileged type.
    """

def monotonic_now_ns() -> int:
    """Current monotonic time in nanoseconds via `clock_gettime(CLOCK_MONOTONIC)`."""

def runtime_log_directory() -> Path:
    """The directory the engine writes its per-runtime JSONL logs into."""

def open_test_harness_channel(channel: str) -> None:
    """Open a test-harness channel; raises if the name is already in use."""

def close_test_harness_channel(channel: str) -> None:
    """Close a test-harness channel, dropping anything still queued on it."""

def feed_test_harness_bag(channel: str, bag: Any) -> None:
    """Queue one bag for delivery through `channel`'s feeder."""

def await_test_harness_bag(channel: str, timeout_seconds: float) -> Any | None:
    """The next bag collected on `channel`, or `None` if the wait ran out."""

def log_event(
    level: str, message: str, attrs: dict[str, Any] | None = None
) -> None:
    """Emit one record on the engine's log pipeline, with structured attrs."""

