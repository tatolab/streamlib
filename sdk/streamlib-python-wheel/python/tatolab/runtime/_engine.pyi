# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Type stubs for the compiled engine module's private bootstrap surface.

A type checker and an editor can read nothing out of `_engine.abi3.so`, so this
file describes the native names only the runtime's own Python reaches — the
engine, the bootstrap's calls, the test harness and the local API client.

What a node is handed while it runs is declared once, in `tatolab.stream`: its
classes as `typing.Protocol`s and its functions as runtime-backed functions.
None of it is declared here, save `NodeLinkDataAccess` and
`RuntimeContextFullAccess`, which the bootstrap constructs and so appear typed
as the class of their Protocol, declaring no member of their own.

Two gates keep this honest against the built module in CI.
`tests/runtime_backed_protocol_conformance.py` holds every native class and
function `tatolab.stream` declares to its declaration member for member, and
holds every name the module exports to exactly one declaration, here or
there. `mypy.stubtest` checks the rest of this file, skipping the names the
conformance gate prints as its allowlist.
"""

import sys
from collections.abc import Mapping
from pathlib import Path
from types import TracebackType
from typing import Any, ClassVar, Literal, final

from tatolab.stream import _node_context_protocols as _stream_protocols

from typing_extensions import Self, disjoint_base

__all__ = [
    "LocalApiMcpClient",
    "LocalApiMcpRequestRefused",
    "LocalApiMcpServerUnreachable",
    "LocalApiMcpToolCallFailed",
    "NodeLinkDataAccess",
    "CapabilityExtensionHost",
    "Runtime",
    "RuntimeContextFullAccess",
    "TestBagCollector",
    "TestBagFeeder",
    "await_test_harness_bag",
    "capability_extension_host_for_the_app_process",
    "capability_extension_host_for_the_helper_process",
    "capture_this_helper_processes_engine_log_records",
    "close_test_harness_channel",
    "decode_tapped_channel_bag_frame_to_python_object",
    "drain_the_engine_log_records_this_helper_captured",
    "engine_build_id_compiled_into_this_extension",
    "feed_test_harness_bag",
    "log_event",
    "open_test_harness_channel",
    "processor_class_import_paths_in_this_processes_catalog",
    "runtime_log_directory",
]

@final
class TestBagFeeder:
    """`tatolab.runtime.testing`'s feeder endpoint: publishes bags a test queued.

    A marker type — never instantiated, passed to
    `stream_builder.add`. Native so that its queue lives in the app process, where the
    test reading it does.
    """

    type: ClassVar[str]

    # Keeps pytest from collecting the `Test*`-named class in user suites.
    __test__: Literal[False]

@final
class TestBagCollector:
    """`tatolab.runtime.testing`'s collector endpoint: records every bag produced."""

    type: ClassVar[str]

    # Keeps pytest from collecting the `Test*`-named class in user suites.
    __test__: Literal[False]

@disjoint_base
class Runtime:
    """The engine, running in this process."""

    def __new__(
        cls,
        *,
        runtime_name: str | None = None,
    ) -> Self:
        """Build the engine, named `runtime_name`.

        The name is the first chunk of every tap channel this runtime serves,
        `<runtime name>/<node name>/<port>`, and the name on its registry
        row that `--node` matches. It belongs to the runtime and is stable
        across runs of one app. It is non-empty, carries none of `/ * $ # ?`,
        and does not begin with `@`; spaces and unicode are fine. A name
        breaking that is refused here, naming the character.

        Left out, the engine reads `STREAMLIB_RUNTIME_NAME`, and failing that
        names the runtime `<hostname>-<app directory name>-<id>`, where the id
        hashes the app directory's full path — so two checkouts of one app on
        one machine differ and every run of one checkout matches. `streamlib
        run` and `dev` pass their `--runtime-name` through to here.

        A name is never auto-suffixed, so it does not depend on start order.
        Nothing refuses a name another runtime already holds — two runs from
        one directory both start — and `--node` refuses a name two live
        runtimes hold, naming both.
        """

    def load(self, graph: Mapping[str, Any], *, name: str | None = None) -> None:
        """Load a graph into this Runtime before `run()`.

        `graph` is the mapping `compile_stream_to_graph` returns, or a graph
        `streamlib graph` rendered; anything not a mapping raises `TypeError`.
        It is converted to JSON: a tuple reads as a list, NaN and infinity as
        null, and what JSON cannot carry — a set, bytes, a nested mapping that
        is not a dict, a key that is not a str, an int wider than 64 bits, a
        str that cannot be encoded as UTF-8, containers nested more than 128
        deep, as one holding itself is — raises `TypeError` or `ValueError`
        saying the graph is not JSON data, with the converter's own error as
        `__cause__`. `name`, when given, is a str that encodes as UTF-8. The
        stream's name — `name` when given, else the graph's own `stream` — is
        cast the way a node name is, and one casting to nothing raises
        `ValueError`. A Runtime takes exactly one `load`: a second raises
        `RuntimeError` naming the stream already loaded, the earlier refusal,
        or the load still underway on another thread. An empty graph raises
        `RuntimeError`, naming the stream when it has one; one that does not
        parse, and one the engine refuses — a key it does not read, an unknown
        `type` (named beside the runtime's own version), a built-in this
        platform does not have (naming the platform), a setting a built-in
        does not take or a value of the wrong kind for one (naming the node and
        the setting), a taken node name, a link to a port no node has — raise
        `RuntimeError` with the engine's own text. A refused load can leave part
        of its graph behind, so every refused call is recorded — save one
        refused because this Runtime is already running or shut down, which
        `run()` refuses anyway — and so is a panic inside the load; `run()` then
        raises naming the load's own refusal or panic, else the first refusal
        recorded, and raises while a load is still underway. Construct a new
        Runtime and load a corrected graph.
        """

    def host_control_plane(self) -> None:
        """Host the control plane in this process, so the node is discoverable.

        Serves the control API on
        `<runtime directory>/local-api-<runtime_id>.sock`, a Unix socket only
        this user can open, and on nothing else: control is reachable only on
        its machine. Opt-in: a runtime that never calls this publishes no
        node-registry entry. Call it before `run()`.

        The entry it publishes carries the runtime's own name, which
        `streamlib nodes` lists and `--node` resolves; the control plane never
        names the runtime, so there is nothing to pass here.
        """

    def run(self) -> None:
        """Run the pipeline until Ctrl-C, SIGTERM, SIGHUP or `shutdown()`, then tear down.

        Call it from the main thread. It owns SIGINT, SIGTERM and SIGHUP from
        startup until the engine is dropped, then hands them back to Python.
        The first interrupt stops the graph gracefully: every processor stops at
        once, and each Python processor's `stop()` and `teardown()` run. The
        second forces it: every helper process group is terminated without its
        `teardown()`, and a native processor still inside its callback is
        abandoned. The third kills every helper process group and exits the
        process with status 130 at once. `shutdown()` is the first step only,
        however often it is called.

        Raises `RuntimeError` naming each processor, by node name and id,
        whose thread ignored shutdown past its budget and was abandoned — the
        engine then stays alive beneath it until the process exits. A forced
        shutdown that abandoned nothing returns normally. A teardown still hung
        after about fifteen seconds ends the process with status 124.

        On macOS the main thread drives the window event pump while this
        blocks, so a `DisplayWindow` opens only under `run()`. There SIGINT and
        SIGTERM are never handed back to Python, and SIGHUP is not owned.
        """

    def wait_until_every_node_is_running(self, *, timeout: float = 30.0) -> None:
        """Block until every node in the graph is running.

        Call it before `run()` or from another thread while `run()` blocks — a
        graph that has not started yet is waited through, not refused. A Python
        node is running once its helper process has registered and wired
        its ports; anything published into the graph before that is dropped by
        the link. Raises `RuntimeError` if a node failed instead of
        starting — carrying that node's own refusal text, so a built-in
        that refused at setup is read by name — if `timeout` elapses, or if
        this runtime has already been shut down; and `ValueError` for a
        `timeout` that is negative, NaN, or too large to be a duration.
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
class CapabilityExtensionHost:
    """What a capability extension's `load(host)` hook is handed.

    A wheel declares its hook as a `streamlib.extensions` entry point in its
    `pyproject.toml`, and the engine calls it once in every process taking an
    engine role — the app process as `Runtime()` is constructed, and each
    helper process before the processor's own module is imported. App code
    never constructs one.

    A hook is expected to be cheap and to do no I/O: bring a runtime or a
    device library up, register the capability's name, and return. It must not
    connect, open a device, or block — the app is waiting on `Runtime()` and a
    helper is inside its registration budget. Raising from a hook fails the
    process it was loading into, by design: an extension that half loaded is
    worse than one that refused.
    """

    @property
    def role(self) -> Literal["app", "helper"]:
        """Which role this process takes."""

    def register_capability(self, name: str, version: str) -> None:
        """Declare a capability this wheel brought up.

        The name is unique across every installed distribution: a second
        distribution registering one already taken is refused, naming both. In
        the app process the registration renders under `extensions` in
        `streamlib graph`; in a helper it is the process's own record.
        """

# The two native classes the bootstrap constructs itself. Each is typed as the
# class of its `tatolab.stream` Protocol and declares nothing of its own: a stub
# class deriving from the Protocol would inherit its bodiless members as
# abstract, and neither checker lets an abstract class be constructed. The
# conformance gate holds each native constructor to the no-argument one this
# declares.
NodeLinkDataAccess: type[_stream_protocols.NodeLinkDataAccess]
RuntimeContextFullAccess: type[_stream_protocols.RuntimeContextFullAccess]

class LocalApiMcpServerUnreachable(Exception):
    """Nothing answered MCP on the local API socket."""

class LocalApiMcpRequestRefused(Exception):
    """The node answered and refused the MCP request."""

class LocalApiMcpToolCallFailed(Exception):
    """The tool ran and reported a failure, or answered with no text."""

@final
class LocalApiMcpClient:
    """An MCP client of one running node, over its local API socket.

    Connecting sends `server/discover` at the latest revision; a node that
    does not answer raises `LocalApiMcpServerUnreachable`, one that refuses
    raises `LocalApiMcpRequestRefused`. Every call releases the GIL.
    """

    def __new__(cls, local_api_socket_path: str, timeout_seconds: float) -> LocalApiMcpClient: ...
    def call_tool(self, tool_name: str, arguments_json: str) -> str:
        """Call `tool_name` with a JSON object of arguments; answer the text its result carries.

        A tool that ran and failed, or answered no text, raises
        `LocalApiMcpToolCallFailed`; a call the node refused outright raises
        `LocalApiMcpRequestRefused`.
        """

    def close(self) -> None:
        """End the client's connection. Idempotent."""

    def __enter__(self) -> LocalApiMcpClient: ...
    def __exit__(
        self,
        exception_type: type[BaseException] | None = ...,
        exception: BaseException | None = ...,
        traceback: TracebackType | None = ...,
    ) -> Literal[False]: ...

def decode_tapped_channel_bag_frame_to_python_object(
    framed_bag_bytes: bytes,
) -> Any:
    """Decode one raw bag a `tap` forwarded — transport-framed msgpack — into
    ordinary Python data.

    The bytes a tap hands back are the channel's wire bytes verbatim, header
    included; this reads exactly the payload the header declares. Refuses a bag
    shorter than its own declared length rather than returning the prefix that
    did arrive, and one whose containers nest more than 128 deep.
    """

def capability_extension_host_for_the_app_process(
    distribution: str,
) -> CapabilityExtensionHost:
    """Mint the host `distribution`'s hook is handed in the app process."""

def capability_extension_host_for_the_helper_process(
    distribution: str,
) -> CapabilityExtensionHost:
    """Mint the host `distribution`'s hook is handed in a helper process."""

def processor_class_import_paths_in_this_processes_catalog() -> list[str]:
    """Every processor class import path in the calling process's catalog.

    What `GET /api/registry` renders, readable in a process that serves no
    control plane. A Python class appears here once a graph naming it loads;
    decorating it registers nothing.
    """

def engine_build_id_compiled_into_this_extension() -> str:
    """The build id of the engine compiled into this extension:
    `<crate version>+<git sha>.<per-build nonce>`, the sha `unknown` where the
    build had no git checkout.

    A helper process compares it with the id its parent handed it in
    `STREAMLIB_ENGINE_BUILD_ID` and refuses to start on any difference, so two
    builds of one commit are still two ids.
    """

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

def capture_this_helper_processes_engine_log_records() -> None:
    """Start capturing this helper process's engine `tracing` records,
    iceoryx2's own included, into the ring
    `drain_the_engine_log_records_this_helper_captured` empties.

    Called by `tatolab.runtime._helper` once its channel to the parent is up and
    before it opens anything, and by nothing else. Raises on a second call and
    on a process that already has a `tracing` subscriber.
    """

def drain_the_engine_log_records_this_helper_captured(
    wait_seconds: float,
) -> tuple[list[dict[str, Any]], int]:
    """The engine records captured so far and how many the ring dropped since
    the last drain, waiting up to `wait_seconds` for the first record.

    Each record carries `level`, `target`, `message`, `pipeline_id`,
    `processor_id`, `rhi_op`, `attrs` and
    `emitted_at_wall_clock_nanoseconds`. The wait releases the GIL.
    """

if sys.platform == "darwin":
    __all__ += [
        "note_this_helper_processes_callbacks_returned_after_its_parent_went_away",
        "watch_for_this_helper_processes_parent_going_away",
    ]

    def watch_for_this_helper_processes_parent_going_away(parent_channel_fd: int) -> None:
        """Arm this helper's watch on its parent: kqueue `NOTE_EXIT` on the
        parent's pid, and the surface-share service's dead-name notification.

        macOS only — Linux binds a helper to its parent with
        `PR_SET_PDEATHSIG`. Either signal shuts `parent_channel_fd` down, so
        the helper reads the end of its channel and runs `stop` and
        `teardown()`; whatever is still alive about six and a half seconds
        later has its process group killed. Called by `tatolab.runtime._helper`
        before any processor code runs, and by nothing else. Raises on a
        second call and when the pid watch cannot be armed.
        """

    def note_this_helper_processes_callbacks_returned_after_its_parent_went_away() -> None:
        """Say the helper's `stop` rung returned after its parent went away,
        which spares its callback the interrupt."""


