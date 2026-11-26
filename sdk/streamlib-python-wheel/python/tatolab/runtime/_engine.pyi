# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Type stubs for the compiled engine module's private bootstrap surface.

A type checker and an editor can read nothing out of `_engine.abi3.so`, so this
file describes the native names only the runtime's own Python reaches — the
bootstrap's calls and the tapped-bag decoder.

What a node is handed while it runs is declared once, in `tatolab.stream`: its
classes as `typing.Protocol`s and its functions as runtime-backed functions.
None of it is declared here: the bootstrap reaches those classes only through
the functions that open them, typed as their Protocols.

Two gates keep this honest against the built module in CI.
`tests/runtime_backed_protocol_conformance.py` holds every native class and
function `tatolab.stream` declares to its declaration member for member, and
holds every name the module exports to exactly one declaration, here or
there. `mypy.stubtest` checks the rest of this file, skipping the names the
conformance gate prints as its allowlist.
"""

import sys
from collections.abc import Callable
from typing import Any, Literal, final

from tatolab.stream import _node_context_protocols as _stream_protocols

__all__ = [
    "MODULE_HOST_TABLE_ABI_MAJOR",
    "MODULE_HOST_TABLE_ABI_MINOR",
    "FunctionNodeEvaluatedAsAStandaloneNode",
    "LastBuildOfANodeScopeComputedValue",
    "NodeScopeBeingDeclared",
    "NodeScopeComputedValue",
    "NodeScopeComputedValueNotYetBuilt",
    "NodeScopeHandleOfAnotherScope",
    "NodeScopeInput",
    "NodeScopeReadOutsideABuildOrAFrame",
    "RunningNodeScope",
    "capture_this_helper_processes_engine_log_records",
    "computed",
    "drain_the_engine_log_records_this_helper_captured",
    "engine_build_id_compiled_into_this_extension",
    "feed_an_input_port_from_a_function_node_island",
    "hand_an_unread_message_on",
    "limited_access_view_of_runtime_context_for_helper_process",
    "not_ready_report_on_runtime_context",
    "note_execution_mode_from_parent_on_runtime_context",
    "note_pause_state_from_parent_on_runtime_context",
    "note_stop_request_from_parent_on_runtime_context",
    "open_node_link_data_access_for_helper_process",
    "open_runtime_context_full_access_for_helper_process",
    "queue_a_fire_of_each_trigger_input",
    "read_each_unlinked_input_port_as_its_pin_value",
    "read_the_control_value_at_an_input_port",
    "read_the_named_value_at_an_input_port",
    "read_the_parts_at_an_input_port",
    "read_the_stored_value_at_an_input_port",
    "read_the_message_at_an_input_port_unread",
    "run_the_scheduling_loop_of_this_helper",
    "stop_feeding_an_input_port_from_a_function_node_island",
    "take_each_function_node_raise_newly_noted",
    "why_each_function_node_its_islands_call_is_raising",
    "write_the_named_value_on_an_output_port",
    "write_the_stored_value_on_an_output_port",
]

MODULE_HOST_TABLE_ABI_MAJOR: int
"""The module host table major this runtime serves; a module's native code built for another is refused."""

MODULE_HOST_TABLE_ABI_MINOR: int
"""The module host table minor this runtime serves; a module's native code built for a newer one is refused."""

@final
class FunctionNodeEvaluatedAsAStandaloneNode:
    """A function node a helper runs as a standalone node, once it is promoted out of its readers' islands."""

    def __new__(
        cls,
        *,
        link_data_access: _stream_protocols.NodeLinkDataAccess,
        node_id: str,
        called_through: int | object,
        input_count: int,
        config_values: list[float],
        config_value_indices_set_by_the_inputs_past_its_own: list[int],
        initial_state_values: list[float],
        output_count: int,
        outputs_that_may_hand_out_nothing: list[int] = ...,
        inputs_carrying_an_unread_message: list[int] = ...,
        outputs_carrying_an_unread_message: list[int] = ...,
        inputs_carried_in_parts: list[tuple[int, str]] = ...,
        outputs_carried_in_parts: list[tuple[int, str]] = ...,
    ) -> FunctionNodeEvaluatedAsAStandaloneNode:
        """`called_through` is a compiled evaluate entry's address, or the object that runs the body as Python.

        Of a compiled node's `input_count` inputs, the last set the config values these indices name.
        """
    def evaluate(
        self,
        input_values: list[float | dict[str, Any] | bytes | _stream_protocols.UnreadMessage | None],
    ) -> list[float | dict[str, Any] | bytes | _stream_protocols.UnreadMessage | None]:
        """Call the node with `input_values` at the stream's time now, answering its outputs, a `float`, a `dict`, the `bytes` of a named type, the `UnreadMessage` it hands on, or `None` for one that hands out nothing."""

class NodeScopeReadOutsideABuildOrAFrame(RuntimeError):
    """`.get()` reads only in a computed value's build or in the per-frame function."""

class NodeScopeHandleOfAnotherScope(RuntimeError):
    """The handle belongs to another node's scope than the build or frame reading it."""

class NodeScopeComputedValueNotYetBuilt(RuntimeError):
    """A build read a computed value that has no value yet; it builds again once that value is built."""

@final
class NodeScopeInput:
    """An input of a Python node's scope, read with `get()` in a build or a frame."""

    def get(self) -> Any:
        """The input's value: in a build, as the build started, recorded as read; in a frame, as of the frame's boundary."""

@final
class NodeScopeComputedValue:
    """A value computed from a Python node's inputs, read with `get()` in a build or a frame."""

    def get(self) -> Any:
        """The computed value: in a build, as the build started, recorded as read; in a frame, always a value."""

def computed(build: Callable[[], Any]) -> NodeScopeComputedValue:
    """Declare a value `build` computes from what it reads with `get()`, into the scope being declared on this thread.

    `build` runs on the scope's worker thread, holding the GIL only while it runs. A value it
    replaces is closed there with its `close()`, if it has one, once nothing in a scope holds it;
    an object that came in as an input's value is never closed.
    """

@final
class NodeScopeBeingDeclared:
    """A Python node's scope while its body declares it: its inputs, then its computed values."""

    def __new__(cls) -> NodeScopeBeingDeclared: ...
    def input(self, value: Any) -> NodeScopeInput:
        """An input holding `value` until it is written."""
    def declare(self, body: Callable[[], Any]) -> Any:
        """Call the node's body once, with every `computed` it calls declared into this scope, and answer what it returned."""
    def start(self) -> RunningNodeScope:
        """Start the scope's worker, which builds every computed value from here on."""

@final
class LastBuildOfANodeScopeComputedValue:
    """How the last build of a computed value went, for a node's diagnostics."""

    @property
    def state(self) -> Literal["not_yet_built", "built", "failed"]:
        """`"not_yet_built"`, `"built"` or `"failed"`."""
    @property
    def builds(self) -> int:
        """How many builds have run."""
    @property
    def failure(self) -> Literal["raised", "read_a_computed_value_not_yet_built", "panicked"] | None:
        """Of a failed build: `"raised"`, `"read_a_computed_value_not_yet_built"` or `"panicked"`."""
    @property
    def why(self) -> str | None:
        """Of a failed build: why, as the exception or the panic said it."""

@final
class RunningNodeScope:
    """A Python node's running scope: sets and wires write its inputs, and the frame thread runs each frame through it."""

    def write(self, input: NodeScopeInput, value: Any) -> None:
        """Write `value` to `input`; a value equal to the one it holds builds nothing."""
    def run_frame(self, frame: Callable[[], object]) -> bool:
        """Take the values built since the last frame, then call `frame` with `get()` reading them, once every computed value has its first value; whether `frame` ran."""
    def last_build_of(self, computed_value: NodeScopeComputedValue) -> LastBuildOfANodeScopeComputedValue:
        """How the last build of `computed_value` went."""
    def wait_until_settled(self, timeout_seconds: float) -> bool:
        """Wait up to `timeout_seconds` until no computed value is left to build; whether it settled."""
    def close(self) -> None:
        """Wait for a frame running on another thread, stop the worker once its build in flight returns, and close every computed value. Refused inside a frame of this scope."""

def feed_an_input_port_from_a_function_node_island(
    *,
    link_data_access: _stream_protocols.NodeLinkDataAccess,
    input_port: str,
    function_nodes: list[
        tuple[
            str,
            int,
            int | object,
            list[tuple[tuple[int, int] | None, float | None]],
            list[float],
            list[float],
            int,
            list[int],
        ]
    ],
    island_output: tuple[int, int],
    how_each_node_carries_its_values: list[dict[str, Any]] = ...,
) -> None:
    """Feed `input_port` from an island of function nodes, evaluated at each read.

    Each node, after the nodes it reads, is its id, the breadcrumb number its
    calls are noted under, its compiled evaluate entry's address or the object
    whose `evaluate(input_values, now_ns)` runs its body as Python,
    where each input takes its value (an earlier node's output, or `None` and its
    pin value), its config values, its initial state values, its output count,
    and the index among its config values each input past its own sets.
    `island_output` names the node and output the port reads. An island
    already feeding the port is replaced, and each stateful node it shares
    resumes from its state. `how_each_node_carries_its_values` names, per
    node, the inputs a link from outside the island feeds, the inputs that
    take `None`, the inputs and outputs carrying an `UnreadMessage`, and the
    outputs that may hand out nothing.
    """

def stop_feeding_an_input_port_from_a_function_node_island(
    *,
    link_data_access: _stream_protocols.NodeLinkDataAccess,
    input_port: str,
) -> bool:
    """Stop feeding `input_port` from the island feeding it; whether one did."""

def take_each_function_node_raise_newly_noted(
    *,
    link_data_access: _stream_protocols.NodeLinkDataAccess,
) -> list[tuple[str, str, str | None]]:
    """Each first raise of a run of a function node's raises noted since the last call, in the order noted: the node's id, what it raised as the graph shows it, and its Python traceback, `None` for a call refused outside Python."""

def why_each_function_node_its_islands_call_is_raising(
    *,
    link_data_access: _stream_protocols.NodeLinkDataAccess,
) -> dict[str, str]:
    """What each function node this helper's islands call raised at its last call, by the node's id; a node that ran cleanly is left out."""

def read_the_message_at_an_input_port_unread(
    *,
    link_data_access: _stream_protocols.NodeLinkDataAccess,
    input_port: str,
) -> _stream_protocols.UnreadMessage | None:
    """The message `input_port` holds for this read, handed back unread; `None` when it holds none."""

def read_the_control_value_at_an_input_port(
    *,
    link_data_access: _stream_protocols.NodeLinkDataAccess,
    node_id: str,
    input_port: str,
) -> float | None:
    """The number `input_port` holds for this read, as an island reads one: a Channels block's first sample, f32 or f64; `None` when none arrived."""

def read_the_parts_at_an_input_port(
    *,
    link_data_access: _stream_protocols.NodeLinkDataAccess,
    node_id: str,
    input_port: str,
    value_type: str,
) -> tuple[float, ...] | None:
    """The parts of the Color, Vector, Rectangle or Transform `input_port` holds for this read, as an island reads them off a control message; `None` when none arrived."""

def read_the_stored_value_at_an_input_port(
    *,
    link_data_access: _stream_protocols.NodeLinkDataAccess,
    node_id: str,
    input_port: str,
    pin_type: str,
) -> dict[str, Any] | None:
    """The record `{"value": ...}` of the String, Menu, Asset or List of `pin_type` (JSON) `input_port` holds for this read; `None` when none arrived."""

def read_the_bundle_frame_at_an_input_port(
    *,
    inputs: _stream_protocols.LinkInputDataReader,
    node_id: str,
    input_port: str,
    pin_type: str,
) -> dict[str, Any] | None:
    """Each member of the bundle holding a Texture of `pin_type` (JSON) `input_port` holds for this read, by name: a small member its stored JSON form, a Texture as it was read; `None` when none arrived."""

def write_the_bundle_frame_on_an_output_port(
    *,
    outputs: _stream_protocols.LinkOutputDataWriter,
    output_port: str,
    pin_type: str,
    small_members: str,
    textures: dict[str, Any],
    timestamp_ns: int | None = None,
) -> None:
    """Write a bundle holding a Texture of `pin_type` (JSON) on `output_port` as one Bundle frame: `small_members` (JSON) by name, and each Texture by name."""

def write_the_stored_value_on_an_output_port(
    *,
    link_data_access: _stream_protocols.NodeLinkDataAccess,
    output_port: str,
    pin_type: str,
    record: dict[str, Any],
) -> None:
    """Write `record`, `{"value": ...}` of a String, Menu, Asset or List of `pin_type` (JSON), on `output_port` as that type packs."""

def read_the_named_value_at_an_input_port(
    *,
    link_data_access: _stream_protocols.NodeLinkDataAccess,
    node_id: str,
    input_port: str,
) -> bytes | None:
    """The bytes of the value of a named type `input_port` holds for this read, off a Bytes message; `None` when none arrived."""

def write_the_named_value_on_an_output_port(
    *,
    link_data_access: _stream_protocols.NodeLinkDataAccess,
    output_port: str,
    payload: bytes,
) -> None:
    """Write `payload`, the bytes a value of a named type packs as, on `output_port` as a Bytes message."""

def hand_an_unread_message_on(
    *,
    link_data_access: _stream_protocols.NodeLinkDataAccess,
    output_port: str,
    message: _stream_protocols.UnreadMessage,
) -> None:
    """Write `message` on `output_port` exactly as it arrived, its descriptor, stamp and group boundary unchanged."""

def an_unread_message_carries_what_another_does(
    *,
    message: _stream_protocols.UnreadMessage,
    other: _stream_protocols.UnreadMessage,
) -> bool:
    """Whether `message` carries what `other` does: the same payload, described the same way, whatever each was stamped with."""

def read_each_unlinked_input_port_as_its_pin_value(
    *,
    link_data_access: _stream_protocols.NodeLinkDataAccess,
    pin_values: dict[str, float],
) -> None:
    """Read each input port of `pin_values` as its pin value while no link feeds it, in place of the pin values read before; a port left out reads only its links again."""

def queue_a_fire_of_each_trigger_input(
    *,
    link_data_access: _stream_protocols.NodeLinkDataAccess,
    fired: list[str],
) -> None:
    """Queue one fire of each Trigger input of `fired`, in order and stamped with this helper's stream time, for its node to read as a Trigger message; an input named twice fires twice."""

def open_node_link_data_access_for_helper_process() -> _stream_protocols.NodeLinkDataAccess:
    """A helper process's own data plane, opened in the iceoryx2 domain its parent handed it.

    Raises `RuntimeError` in a process its parent handed no domain root.
    """

def open_runtime_context_full_access_for_helper_process(
    link_data_access: _stream_protocols.NodeLinkDataAccess,
    runtime_id: str,
    node_id: str,
    escalate_request_to_parent: Callable[[dict[str, Any]], dict[str, Any]] | None = None,
    release_to_parent_without_waiting: Callable[[dict[str, Any]], None] | None = None,
) -> _stream_protocols.RuntimeContextFullAccess:
    """The context a helper process hands its own node's privileged hooks.

    With both callables and the surface-share socket the parent's environment
    names, the GPU surface works here; without any of them, GPU calls refuse by
    name.
    """

def limited_access_view_of_runtime_context_for_helper_process(
    full_access_context: _stream_protocols.RuntimeContextFullAccess,
) -> _stream_protocols.RuntimeContextLimitedAccess:
    """The limited-access view of the same node: same links and pause state."""

def note_pause_state_from_parent_on_runtime_context(
    full_access_context: _stream_protocols.RuntimeContextFullAccess, paused: bool
) -> None:
    """Record on a helper's context the pause state its parent just announced."""

def note_execution_mode_from_parent_on_runtime_context(
    full_access_context: _stream_protocols.RuntimeContextFullAccess, execution_mode: str
) -> None:
    """Record on a helper's context the execution mode its parent runs the node
    in, refusing one that drives `process()` from inputs lent to native code."""

def not_ready_report_on_runtime_context(
    full_access_context: _stream_protocols.RuntimeContextFullAccess,
) -> tuple[str, int, int] | None:
    """What the node last reported on its context: `(reason, placeholder image
    width, placeholder image height)` while it is not ready, `None` while it is."""

def note_stop_request_from_parent_on_runtime_context(
    full_access_context: _stream_protocols.RuntimeContextFullAccess, stop_requested: bool
) -> None:
    """Record on a helper's context whether its parent asked the node to stop."""

def run_the_scheduling_loop_of_this_helper(
    *,
    node_interpreter: Any,
    link_data_access: _stream_protocols.NodeLinkDataAccess,
    lifecycle_command_arrival_fd: int,
    execution_mode: str,
    interval_ms: int,
    paced_by_real_time: bool,
    stream_clock_may_step: bool,
    paused: bool,
) -> None:
    """Run the hosted node on the engine's scheduling loop for
    `execution_mode`, `"reactive"` or `"continuous"`, until a lifecycle command
    stops it, raising what any callback into `node_interpreter` raised."""

def engine_build_id_compiled_into_this_extension() -> str:
    """The build id of the engine compiled into this extension:
    `<crate version>+<git sha>.<per-build nonce>`, the sha `unknown` where the
    build had no git checkout.

    A helper process compares it with the id its parent handed it in
    `STREAMLIB_ENGINE_BUILD_ID` and refuses to start on any difference, so two
    builds of one commit are still two ids.
    """

def capture_this_helper_processes_engine_log_records() -> None:
    """Start capturing this helper process's engine `tracing` records,
    iceoryx2's own included, into the ring
    `drain_the_engine_log_records_this_helper_captured` empties.

    Called by the node interpreter bootstrap once its channel to the
    parent is up and before it opens anything, and by nothing else. Raises on a
    second call and on a process that already has a `tracing` subscriber.
    """

def drain_the_engine_log_records_this_helper_captured(
    wait_seconds: float,
) -> tuple[list[dict[str, Any]], int]:
    """The engine records captured so far and how many the ring dropped since
    the last drain, waiting up to `wait_seconds` for the first record.

    Each record carries `level`, `target`, `message`, `pipeline_id`,
    `node_run_id`, `rhi_op`, `attrs` and
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
        later has its process group killed. Called by the node
        interpreter bootstrap before any node code runs, and by nothing
        else. Raises on a second call and when the pid watch cannot be armed.
        """

    def note_this_helper_processes_callbacks_returned_after_its_parent_went_away() -> None:
        """Say the helper's `stop` rung returned after its parent went away,
        which spares its callback the interrupt."""


