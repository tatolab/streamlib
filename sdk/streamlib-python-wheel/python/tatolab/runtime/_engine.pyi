# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

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
from collections.abc import Callable, Mapping
from typing import Any, final

from tatolab.stream import _node_context_protocols as _stream_protocols

__all__ = [
    "MODULE_HOST_TABLE_ABI_MAJOR",
    "MODULE_HOST_TABLE_ABI_MINOR",
    "FunctionNodeEvaluatedAsItsHelpersProcessor",
    "capture_this_helper_processes_engine_log_records",
    "drain_the_engine_log_records_this_helper_captured",
    "engine_build_id_compiled_into_this_extension",
    "feed_an_input_port_from_a_function_node_island",
    "limited_access_view_of_runtime_context_for_helper_process",
    "note_execution_mode_from_parent_on_runtime_context",
    "note_pause_state_from_parent_on_runtime_context",
    "note_stop_request_from_parent_on_runtime_context",
    "open_node_link_data_access_for_helper_process",
    "open_runtime_context_full_access_for_helper_process",
    "read_each_unlinked_input_port_as_its_pin_value",
    "run_the_scheduling_loop_of_this_helper",
    "stop_feeding_an_input_port_from_a_function_node_island",
]

MODULE_HOST_TABLE_ABI_MAJOR: int
"""The module host table major this runtime serves; a module's native code built for another is refused."""

MODULE_HOST_TABLE_ABI_MINOR: int
"""The module host table minor this runtime serves; a module's native code built for a newer one is refused."""

@final
class FunctionNodeEvaluatedAsItsHelpersProcessor:
    """The function node a helper runs as its own processor, once it is promoted out of its readers' islands."""

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
    ) -> FunctionNodeEvaluatedAsItsHelpersProcessor:
        """`called_through` is a compiled evaluate entry's address, or the object that runs the body as Python.

        Of a compiled node's `input_count` inputs, the last set the config values these indices name.
        """
    def evaluate(self, input_values: list[float | dict[str, Any]]) -> list[float | dict[str, Any]]:
        """Call the node with `input_values` at the stream's time now, answering its outputs, a `float` or a `dict` each."""

def feed_an_input_port_from_a_function_node_island(
    *,
    link_data_access: _stream_protocols.NodeLinkDataAccess,
    input_port: str,
    function_nodes: list[
        tuple[
            str,
            int,
            int | object,
            list[tuple[tuple[int, int] | None, float]],
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

def read_each_unlinked_input_port_as_its_pin_value(
    *,
    link_data_access: _stream_protocols.NodeLinkDataAccess,
    pin_values: dict[str, float],
) -> None:
    """Read each input port of `pin_values` as its pin value while no link feeds it, in place of the pin values read before; a port left out reads only its links again."""

def open_node_link_data_access_for_helper_process() -> _stream_protocols.NodeLinkDataAccess:
    """A helper process's own data plane, opened in the iceoryx2 domain its parent handed it.

    Raises `RuntimeError` in a process its parent handed no domain root.
    """

def open_runtime_context_full_access_for_helper_process(
    configuration: Mapping[str, Any],
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
    """The limited-access view of the same node: same configuration, links and pause state."""

def note_pause_state_from_parent_on_runtime_context(
    full_access_context: _stream_protocols.RuntimeContextFullAccess, paused: bool
) -> None:
    """Record on a helper's context the pause state its parent just announced."""

def note_execution_mode_from_parent_on_runtime_context(
    full_access_context: _stream_protocols.RuntimeContextFullAccess, execution_mode: str
) -> None:
    """Record on a helper's context the execution mode its parent runs the node
    in, refusing one that drives `process()` from inputs lent to native code."""

def note_stop_request_from_parent_on_runtime_context(
    full_access_context: _stream_protocols.RuntimeContextFullAccess, stop_requested: bool
) -> None:
    """Record on a helper's context whether its parent asked the node to stop."""

def run_the_scheduling_loop_of_this_helper(
    *,
    processor_interpreter: Any,
    link_data_access: _stream_protocols.NodeLinkDataAccess,
    lifecycle_command_arrival_fd: int,
    execution_mode: str,
    interval_ms: int,
    paced_by_real_time: bool,
    stream_clock_may_step: bool,
    paused: bool,
) -> None:
    """Run the hosted processor on the engine's scheduling loop for
    `execution_mode`, `"reactive"` or `"continuous"`, until a lifecycle command
    stops it, raising what any callback into `processor_interpreter` raised."""

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

    Called by the processor interpreter bootstrap once its channel to the
    parent is up and before it opens anything, and by nothing else. Raises on a
    second call and on a process that already has a `tracing` subscriber.
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
        later has its process group killed. Called by the processor
        interpreter bootstrap before any processor code runs, and by nothing
        else. Raises on a second call and when the pid watch cannot be armed.
        """

    def note_this_helper_processes_callbacks_returned_after_its_parent_went_away() -> None:
        """Say the helper's `stop` rung returned after its parent went away,
        which spares its callback the interrupt."""


