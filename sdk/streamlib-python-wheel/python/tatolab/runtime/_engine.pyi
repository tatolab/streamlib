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
from typing import Any

from tatolab.stream import _node_context_protocols as _stream_protocols

__all__ = [
    "capture_this_helper_processes_engine_log_records",
    "decode_tapped_channel_bag_frame_to_python_object",
    "drain_the_engine_log_records_this_helper_captured",
    "engine_build_id_compiled_into_this_extension",
    "limited_access_view_of_runtime_context_for_helper_process",
    "note_pause_state_from_parent_on_runtime_context",
    "open_node_link_data_access_for_helper_process",
    "open_runtime_context_full_access_for_helper_process",
]

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


