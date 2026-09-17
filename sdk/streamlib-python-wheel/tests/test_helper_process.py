# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The runtime loop a Python processor's own child process runs.

Everything here drives the helper's own halves — the framed socket, the class
loader, the lifecycle machine — against a stand-in parent, so what the loop
does is asserted directly rather than inferred from a running graph.
"""

import gc
import json
import os
import select
import shutil
import resource
import socket
import statistics
import struct
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import cast

import pytest

from streamlib import _helper
from streamlib._engine import engine_build_id_compiled_into_this_extension
from streamlib._helper import (
    HelperProcessLifecycle,
    HelperProcessProtocolError,
    ParentProcessBridge,
    ParentProcessLogSink,
    load_processor_class,
)

pytestmark = pytest.mark.usefixtures("private_iceoryx2_domain_for_this_test_process")

FRAME_LENGTH_PREFIX = struct.Struct(">I")

PROBE_MODULE = "helper_process_probes"


class StandInParent:
    """The parent half of the socketpair, speaking the same framing.

    Reads go through `select` on the raw socket rather than a buffered file
    with a timeout: one timed-out read poisons a socket file object for good
    ("cannot read from timed out object"), and several tests here deliberately
    wait for a reply that must not come.
    """

    def __init__(self) -> None:
        self.parent_end, self.child_end = socket.socketpair()

    def send(self, message: dict) -> None:
        payload = json.dumps(message).encode("utf-8")
        self.parent_end.sendall(FRAME_LENGTH_PREFIX.pack(len(payload)) + payload)

    def receive(self, timeout_seconds: float = 5.0):
        """The next frame, or `None` if none arrives inside the deadline."""
        deadline = time.monotonic() + timeout_seconds
        length_prefix = self._read_exactly(FRAME_LENGTH_PREFIX.size, deadline)
        if length_prefix is None:
            return None
        (payload_length,) = FRAME_LENGTH_PREFIX.unpack(length_prefix)
        payload = self._read_exactly(payload_length, deadline)
        if payload is None:
            return None
        return json.loads(payload.decode("utf-8"))

    def _read_exactly(self, byte_count: int, deadline: float):
        collected = bytearray()
        while len(collected) < byte_count:
            remaining_seconds = deadline - time.monotonic()
            if remaining_seconds <= 0:
                return None
            readable, _, _ = select.select([self.parent_end], [], [], remaining_seconds)
            if not readable:
                return None
            chunk = self.parent_end.recv(byte_count - len(collected))
            if not chunk:
                return None
            collected.extend(chunk)
        return bytes(collected)

    def close(self) -> None:
        self.parent_end.close()


@pytest.fixture
def stand_in_parent():
    parent = StandInParent()
    try:
        yield parent
    finally:
        parent.close()


# =============================================================================
# Loading the class by import path
# =============================================================================


def test_a_module_scope_class_loads_from_its_import_path():
    loaded = load_processor_class(f"{PROBE_MODULE}:PassThroughProbe")
    assert loaded.__name__ == "PassThroughProbe"


def test_a_nested_class_resolves_through_the_whole_dotted_qualname():
    """`rt.add` deliberately admits `Outer.Inner` because a fresh interpreter
    can reach it — which it only can if the loader walks every segment. A
    single `getattr` on the joined qualname raises instead."""
    loaded = load_processor_class(f"{PROBE_MODULE}:OuterProbe.InnerProbe")
    assert loaded.__qualname__ == "OuterProbe.InnerProbe"


def test_an_unresolvable_import_path_names_the_segment_that_failed():
    with pytest.raises(HelperProcessProtocolError) as refusal:
        load_processor_class(f"{PROBE_MODULE}:OuterProbe.NoSuchProbe")
    assert "NoSuchProbe" in str(refusal.value)


def test_an_import_path_without_a_qualname_is_refused():
    with pytest.raises(HelperProcessProtocolError) as refusal:
        load_processor_class(PROBE_MODULE)
    assert "module:qualname" in str(refusal.value)


# =============================================================================
# The wiring envelope
# =============================================================================


def engine_shaped_link_wiring(direction: str, link_id: str) -> dict:
    """One entry of the envelope the compiler's wiring path emits.

    Field-for-field what `wire_subprocess_source` / `wire_subprocess_dest`
    build — a key renamed on either side is a `KeyError` in a child that has
    already been spawned, which is why this suite reads the same names.
    """
    channel_service_name = f"phelper{os.getpid()}_{link_id}/frames_to_downstream"
    notify_service_name = f"phelper{os.getpid()}_{link_id}_dest/notify"
    if direction == "output":
        return {
            "name": "frames_to_downstream",
            "link_id": link_id,
            "enable_safe_overflow": True,
            "channel_service_name": channel_service_name,
            "dest_notify_service_name": notify_service_name,
            "expected_payload_bytes": 1024,
            "max_payload_bytes_per_channel": 1 << 20,
            "channel_service_creation_depth": 16,
            "max_subscribers": 2,
            "notify_max_notifiers": 1,
            "output_port_wiring_generation": 1,
        }
    return {
        "name": "frames_from_upstream",
        "link_id": link_id,
        "enable_safe_overflow": True,
        "channel_service_name": channel_service_name,
        "notify_service_name": notify_service_name,
        "read_mode": "read_next_in_order",
        "channel_service_creation_depth": 16,
        "input_port_ring_depth": 16,
        "max_subscribers": 2,
        "notify_max_notifiers": 1,
        "loss_count_slot": 0,
        "wiring_generation": 1,
    }


def test_a_helper_handed_no_iceoryx2_domain_root_refuses_to_start_by_name(
    monkeypatch: pytest.MonkeyPatch,
):
    """A helper opens its node only in the domain its parent resolved. One that
    guessed a root instead — from its working directory, iceoryx2's defaults or
    a config file — would open in a domain the parent is not in, where no bag
    ever arrives and nothing errors.

    Fail-without-fix: fall back to any root when the variable is absent and the
    constructor below returns a data plane that silently reaches nobody.
    """
    from streamlib import ProcessorLinkDataAccess

    monkeypatch.delenv("STREAMLIB_ICEORYX2_DOMAIN_ROOT")

    with pytest.raises(RuntimeError, match="STREAMLIB_ICEORYX2_DOMAIN_ROOT is not set"):
        ProcessorLinkDataAccess()


def test_a_declared_port_with_no_link_reads_empty_and_drops_writes():
    """A processor added to a running graph is set up before any link reaches
    it, and a link may be taken away again later. Its declared ports stay
    usable throughout: a read finds nothing, a write goes nowhere, and neither
    raises. Only a port the class never declared is refused, by name.

    Fail-without-fix: without the declaration the write below raises on every
    frame a live-added effect sees before its output is connected.
    """
    from streamlib import ProcessorLinkDataAccess

    link_data_access = ProcessorLinkDataAccess()
    link_data_access.declare_ports(["frames_from_upstream"], ["frames_to_downstream"])

    assert link_data_access.read_from_input_port("frames_from_upstream") is None
    assert link_data_access.input_port_has_data("frames_from_upstream") is False
    link_data_access.write_to_output_port("frames_to_downstream", {"frame_index": 1})

    with pytest.raises(RuntimeError, match="never_declared"):
        link_data_access.read_from_input_port("never_declared")
    with pytest.raises(RuntimeError, match="never_declared"):
        link_data_access.write_to_output_port("never_declared", {"frame_index": 1})


def test_a_helper_opens_its_own_ports_from_the_envelope_the_engine_sends():
    """The whole point of the wiring envelope: two helpers open their own ends
    of one channel from what the parent sent, and a bag crosses between them.

    Both planes live on this thread because iceoryx2's ports are `!Send`, and
    the destination is wired first because a send with no subscriber attached
    is dropped.
    """
    from streamlib import ProcessorLinkDataAccess

    link_id = "L-envelope-test"
    destination = ProcessorLinkDataAccess()
    _helper.wire_link_data_access(
        destination, {"inputs": [engine_shaped_link_wiring("input", link_id)]}
    )
    source = ProcessorLinkDataAccess()
    _helper.wire_link_data_access(
        source, {"outputs": [engine_shaped_link_wiring("output", link_id)]}
    )

    source.write_to_output_port("frames_to_downstream", {"frame_index": 11})

    assert destination.any_input_port_has_data()
    assert destination.read_from_input_port("frames_from_upstream") == {
        "frame_index": 11
    }


def test_a_helper_opens_the_channel_at_its_creation_depth_and_reads_at_its_own_ports_depth():
    """A helper opens a channel at the creation depth the envelope names and
    reads through a ring its own port's depth, so a shallow port that opens a
    channel first never sizes it for the deeper one that joins.

    Ten bags published while neither reads: the four-deep port reads four, the
    sixteen-deep port reads all ten. Fail-without-fix: open the service at the
    port's ring depth and the second destination's open is refused
    (`DoesNotSupportRequestedMinBufferSize`).
    """
    from streamlib import ProcessorLinkDataAccess

    bags_published_while_neither_reads = 10
    shallow_wiring = engine_shaped_link_wiring("input", "L-shallow-port")
    shallow_wiring["input_port_ring_depth"] = 4
    deep_wiring = engine_shaped_link_wiring("input", "L-deep-port")
    deep_wiring["channel_service_name"] = shallow_wiring["channel_service_name"]
    deep_wiring["notify_service_name"] = f"{shallow_wiring['notify_service_name']}_deep"
    source_wiring = engine_shaped_link_wiring("output", "L-shallow-port")
    source_wiring["channel_service_name"] = shallow_wiring["channel_service_name"]
    for wiring in (shallow_wiring, deep_wiring, source_wiring):
        wiring["max_subscribers"] = 3

    shallow_destination = ProcessorLinkDataAccess()
    _helper.wire_link_data_access(shallow_destination, {"inputs": [shallow_wiring]})
    deep_destination = ProcessorLinkDataAccess()
    _helper.wire_link_data_access(deep_destination, {"inputs": [deep_wiring]})
    source = ProcessorLinkDataAccess()
    _helper.wire_link_data_access(source, {"outputs": [source_wiring]})

    for frame_index in range(bags_published_while_neither_reads):
        source.write_to_output_port("frames_to_downstream", {"frame_index": frame_index})

    def every_bag_read(destination: ProcessorLinkDataAccess) -> list:
        read = []
        while (bag := destination.read_from_input_port("frames_from_upstream")) is not None:
            read.append(bag["frame_index"])
        return read

    assert every_bag_read(shallow_destination) == [6, 7, 8, 9]
    assert every_bag_read(deep_destination) == list(range(bags_published_while_neither_reads))


def test_a_helper_publishes_to_a_destination_that_wants_no_notification():
    """An empty `dest_notify_service_name` is the engine saying this
    destination never drains a listener — a self-driven sink like
    `DisplayWindow`, which polls its mailboxes from its own render thread.

    The helper must wire the link for data only. Before #1764 it opened a
    notify service unconditionally, so the empty name failed the child's whole
    `setup` on an invalid iceoryx2 service name — reached through the MVP
    graph's own `helper -> DisplayWindow` link.
    """
    from streamlib import ProcessorLinkDataAccess

    link_id = "L-no-notify"
    destination = ProcessorLinkDataAccess()
    _helper.wire_link_data_access(
        destination, {"inputs": [engine_shaped_link_wiring("input", link_id)]}
    )

    source = ProcessorLinkDataAccess()
    source_wiring = engine_shaped_link_wiring("output", link_id)
    source_wiring["dest_notify_service_name"] = ""
    _helper.wire_link_data_access(source, {"outputs": [source_wiring]})

    source.write_to_output_port("frames_to_downstream", {"frame_index": 12})

    assert destination.any_input_port_has_data(), (
        "dropping the notifier must not touch data delivery"
    )
    assert destination.read_from_input_port("frames_from_upstream") == {
        "frame_index": 12
    }


def test_a_disconnected_links_ports_are_free_for_its_reconnect():
    """The child half of #1554: a disconnect must release the ports this
    process opened, or the same link cannot be wired a second time.

    This envelope caps the notify service at one notifier (the engine sends its
    fixed inbound-link cap; the fixture shrinks it), so a notifier still held
    from the first connect makes the reconnect's `create_notifier` the second
    on a max-1 service — `ExceedsMaxSupportedNotifiers`. The engine cannot reclaim these
    from the parent: the publisher, notifier and subscriber all belong here.

    Fail-without-fix: make `unwire_output_link` / `unwire_input_link` no-ops
    (the behaviour before this fix, when `close_iceoryx2_service` skipped
    out-of-process endpoints entirely) and the second `connect()` raises.

    The Python-surface mirror of the engine's
    `disconnect_reconnect_cycle_reclaims_notifier_and_data_service`.
    """
    from streamlib import ProcessorLinkDataAccess

    link_id = "L-reconnect-cycle"
    destination = ProcessorLinkDataAccess()
    source = ProcessorLinkDataAccess()

    # The destination is wired first both times: a send with no subscriber
    # attached is dropped.
    def connect() -> None:
        _helper.wire_link_data_access(
            destination, {"inputs": [engine_shaped_link_wiring("input", link_id)]}
        )
        _helper.wire_link_data_access(
            source, {"outputs": [engine_shaped_link_wiring("output", link_id)]}
        )

    def disconnect() -> None:
        _helper.unwire_link_data_access(
            source,
            {
                "direction": "output",
                "port": "frames_to_downstream",
                "link_id": link_id,
            },
        )
        _helper.unwire_link_data_access(
            destination, {"direction": "input", "link_id": link_id}
        )

    connect()
    disconnect()
    connect()

    source.write_to_output_port("frames_to_downstream", {"frame_index": 13})
    assert destination.read_from_input_port("frames_from_upstream") == {
        "frame_index": 13
    }, "the reconnected link must carry data through freshly opened ports"


def test_unwiring_a_link_in_an_unknown_direction_touches_neither_plane():
    """A direction this side does not know is a parent that has drifted from
    this protocol. It must take nothing down and release nothing — guessing a
    direction would drop a port that is still carrying frames.

    Fail-without-fix: route the unknown direction to `unwire_input_link`
    instead of warning (the plausible mis-implementation, since the child's
    plane is fully built either way) and the read below raises
    `RuntimeError: Link error: Unknown input port` — the mailbox went with
    the subscriber that was dropped out from under it.
    """
    from streamlib import ProcessorLinkDataAccess

    link_id = "L-unknown-direction"
    destination = ProcessorLinkDataAccess()
    _helper.wire_link_data_access(
        destination, {"inputs": [engine_shaped_link_wiring("input", link_id)]}
    )
    source = ProcessorLinkDataAccess()
    _helper.wire_link_data_access(
        source, {"outputs": [engine_shaped_link_wiring("output", link_id)]}
    )

    _helper.unwire_link_data_access(
        source,
        {
            "direction": "sideways",
            "port": "frames_to_downstream",
            "link_id": link_id,
        },
    )
    _helper.unwire_link_data_access(
        destination, {"direction": "sideways", "link_id": link_id}
    )

    source.write_to_output_port("frames_to_downstream", {"frame_index": 14})
    assert destination.read_from_input_port("frames_from_upstream") == {
        "frame_index": 14
    }, "an unrecognised direction must leave both ends of the link wired"


# =============================================================================
# The framed socket
# =============================================================================


def test_a_frame_written_by_the_parent_arrives_as_a_lifecycle_command(
    stand_in_parent,
):
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    stand_in_parent.send({"cmd": "stop", "capability": "full"})
    assert bridge.next_lifecycle_command() == {"cmd": "stop", "capability": "full"}


def test_an_escalate_response_never_reaches_the_lifecycle_queue(stand_in_parent):
    """Frames are classified on their `rpc` tag. Route escalate traffic to the
    lifecycle queue instead and the next `setup` handshake reads a GPU reply
    where it expected `ready`."""
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    stand_in_parent.send({"rpc": "escalate_response", "request_id": "r-1", "result": "ok"})
    stand_in_parent.send({"cmd": "teardown"})
    assert bridge.next_lifecycle_command() == {"cmd": "teardown"}


def test_a_closed_channel_surfaces_as_a_command_of_none(stand_in_parent):
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    stand_in_parent.close()
    assert bridge.next_lifecycle_command() is None


def test_a_log_record_rides_the_escalate_log_op_to_the_parent(stand_in_parent):
    """A helper has no engine in it, so `streamlib.log` cannot hand a record to
    one — it travels to the parent's pipeline as a fire-and-forget escalate op."""
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    sink = ParentProcessLogSink(bridge, "P-helper-test")
    sink("info", "a frame arrived", {"width": 1920})

    record = stand_in_parent.receive()
    assert record["rpc"] == "escalate_request"
    assert record["op"] == "log"
    assert record["source"] == "python"
    assert record["level"] == "info"
    assert record["message"] == "a frame arrived"
    assert record["processor_id"] == "P-helper-test"
    assert record["attrs"] == {"width": 1920}
    assert record["intercepted"] is False


def test_log_sequence_numbers_are_per_process_monotonic(stand_in_parent):
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    sink = ParentProcessLogSink(bridge, "P-helper-test")
    sink("info", "first", None)
    sink("info", "second", None)
    assert int(stand_in_parent.receive()["source_seq"]) == 1
    assert int(stand_in_parent.receive()["source_seq"]) == 2


# =============================================================================
# Releases
# =============================================================================


@pytest.mark.parametrize(
    ("escalate_op", "release_op", "released_id_field"),
    [
        ("acquire_pixel_buffer", "release_handle", "handle_id"),
        ("register_acceleration_structure_blas", "release_handle", "handle_id"),
        ("create_processor_owned_window", "close_processor_owned_window", "window_id"),
    ],
)
def test_what_an_answer_the_helper_stopped_waiting_on_created_is_released(
    stand_in_parent, escalate_op, release_op, released_id_field
):
    """An escalate keeps running in the app process after the helper's wait
    for it ends, and what its answer creates belongs to nobody.

    Fail-without-fix: the late answer is logged as one nothing waits on and
    dropped, so no release reaches the parent and what it created stays until
    the helper stops.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    with pytest.raises(_helper.EscalateRequestError):
        bridge.request_from_parent({"op": escalate_op}, timeout_seconds=0.1)
    request = stand_in_parent.receive()
    assert request["op"] == escalate_op

    stand_in_parent.send(
        {
            "rpc": "escalate_response",
            "request_id": request["request_id"],
            "result": "ok",
            "handle_id": "created-after-the-wait-ended",
        }
    )

    release = stand_in_parent.receive()
    assert release is not None, "nothing released what the late answer created"
    assert release["op"] == release_op
    assert release[released_id_field] == "created-after-the-wait-ended"


def test_a_late_refusal_created_nothing_so_nothing_is_released(stand_in_parent):
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    with pytest.raises(_helper.EscalateRequestError):
        bridge.request_from_parent({"op": "acquire_pixel_buffer"}, timeout_seconds=0.1)
    request = stand_in_parent.receive()

    stand_in_parent.send(
        {
            "rpc": "escalate_response",
            "request_id": request["request_id"],
            "result": "err",
            "message": "the pool is at its cap",
        }
    )

    assert stand_in_parent.receive(timeout_seconds=0.5) is None


def test_an_answer_delivered_before_the_channel_closes_is_still_its_callers(
    stand_in_parent,
):
    """An answer already in its slot belongs to its caller even when the
    channel closes before the caller reads it; otherwise what the answer
    created is neither handed back nor released."""
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    slot = _helper._PendingEscalateResponse()
    bridge._pending_escalate_responses["r-delivered"] = slot
    answer = {
        "rpc": "escalate_response",
        "request_id": "r-delivered",
        "result": "ok",
        "handle_id": "created-before-the-close",
    }

    assert bridge._deliver_escalate_response(answer)
    bridge._wake_every_pending_escalate_caller()

    assert slot.arrived.is_set()
    assert slot.message == answer


def test_teardown_is_answered_only_after_the_releases_its_hook_owed(
    stand_in_parent, monkeypatch
):
    """The parent gives a helper up the moment it answers `teardown`, and a
    release still queued then is never taken. An acceleration structure lives
    on the app process's GPU context rather than on this helper's handles, so
    nothing else ever frees it.

    Fail-without-fix: `done` goes out while the structure's release is still
    queued, so the parent reads it before the release is answered.
    """
    monkeypatch.setenv("STREAMLIB_SURFACE_SOCKET", "/nonexistent/streamlib-surface.sock")
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_processor_class(f"{PROBE_MODULE}:ReleasesAStructureInTeardownProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    register = stand_in_parent.receive()
    assert register["op"] == "register_acceleration_structure_blas"
    stand_in_parent.send(
        {
            "rpc": "escalate_response",
            "request_id": register["request_id"],
            "result": "ok",
            "handle_id": "blas-released-in-teardown",
        }
    )
    assert stand_in_parent.receive() == {"rpc": "ready"}

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    release = stand_in_parent.receive()
    assert release is not None and release.get("op") == "release_handle", (
        f"teardown answered before the structure's release went out: {release}"
    )
    assert release["handle_id"] == "blas-released-in-teardown"
    assert stand_in_parent.receive(timeout_seconds=0.3) is None, (
        "teardown was answered while its release was still unanswered"
    )
    stand_in_parent.send(
        {
            "rpc": "escalate_response",
            "request_id": release["request_id"],
            "result": "ok",
            "handle_id": "blas-released-in-teardown",
        }
    )
    assert stand_in_parent.receive() == {"rpc": "done"}
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_an_interrupt_during_teardowns_wait_for_releases_still_answers_teardown(
    stand_in_parent, monkeypatch
):
    """The shutdown ladder's interrupt can land while teardown waits for its
    releases to go out; the parent is still owed `done`, or it waits out its
    budget and kills a helper that had finished."""
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()

    def interrupted_while_waiting(timeout_seconds: float) -> bool:
        raise KeyboardInterrupt

    monkeypatch.setattr(
        bridge, "wait_for_every_release_queued_so_far", interrupted_while_waiting
    )
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_processor_class(f"{PROBE_MODULE}:PassThroughProbe")
    )
    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive() == {"rpc": "ready"}

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})

    assert stand_in_parent.receive() == {"rpc": "done"}
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_release_a_finalizer_owes_on_the_bridge_reader_never_holds_the_reader(
    stand_in_parent, monkeypatch
):
    """The collector runs a cycle's finalizers on whichever thread crossed its
    threshold, and the reader allocates for every frame it decodes, so a GPU
    handle's release can come due on the reader itself.

    Fail-without-fix: the handle's drop waits for its release's answer on the
    reader, the one thread that delivers answers, so every frame behind it —
    the parent's next command included — waits out the escalate timeout.
    """
    from streamlib import ProcessorLinkDataAccess, RuntimeContextFullAccess

    monkeypatch.setenv("STREAMLIB_SURFACE_SOCKET", "/nonexistent/streamlib-surface.sock")
    decode_the_frame = _helper._decode_frame_payload

    def decode_the_frame_collecting_garbage_on_the_marker(payload: bytes):
        frame = decode_the_frame(payload)
        if frame.get("cmd") == "collect_garbage_on_the_reader":
            gc.collect()
        return frame

    monkeypatch.setattr(
        _helper, "_decode_frame_payload", decode_the_frame_collecting_garbage_on_the_marker
    )
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    context = RuntimeContextFullAccess.open_for_helper_process(
        {},
        ProcessorLinkDataAccess(),
        "R-helper-test",
        "P-helper-test",
        bridge.request_from_parent,
        bridge.release_to_parent_without_waiting,
    )

    built_handles: list = []
    builder = threading.Thread(
        target=lambda: built_handles.append(
            context.gpu_full_access.build_triangles_blas(
                [0.0] * 9, [0, 1, 2], label="freed-on-the-reader"
            )
        ),
        name="builds-the-structure",
    )
    builder.start()
    register = stand_in_parent.receive()
    assert register["op"] == "register_acceleration_structure_blas"
    stand_in_parent.send(
        {
            "rpc": "escalate_response",
            "request_id": register["request_id"],
            "result": "ok",
            "handle_id": "blas-freed-on-the-reader",
        }
    )
    builder.join(timeout=5.0)
    assert len(built_handles) == 1, "the structure was never built"

    gc.disable()
    try:
        # Reachable only through a cycle, so only the collector frees it — and
        # the marker below runs the collector on the reader.
        cycle: list = [built_handles.pop()]
        cycle.append(cycle)
        del cycle
        stand_in_parent.send({"cmd": "collect_garbage_on_the_reader"})
        stand_in_parent.send({"cmd": "teardown"})

        release = stand_in_parent.receive()
        assert release is not None, "the freed structure was never released"
        assert release["op"] == "release_handle"
        assert release["handle_id"] == "blas-freed-on-the-reader"
        stand_in_parent.send(
            {
                "rpc": "escalate_response",
                "request_id": release["request_id"],
                "result": "ok",
                "handle_id": "blas-freed-on-the-reader",
            }
        )

        delivered: list = []
        deadline = time.monotonic() + 5.0
        while len(delivered) < 2 and time.monotonic() < deadline:
            was_waiting, command = bridge.next_lifecycle_command_if_waiting()
            if was_waiting:
                delivered.append(command)
            else:
                time.sleep(0.01)
        assert [command["cmd"] for command in delivered] == [
            "collect_garbage_on_the_reader",
            "teardown",
        ], "the reader stalled behind the release a finalizer owed on it"
    finally:
        gc.enable()


# =============================================================================
# The lifecycle machine
# =============================================================================


def drive_lifecycle_on_a_thread(bridge, processor_class):
    """Run the lifecycle loop off the test's own thread.

    iceoryx2's ports are `!Send`, so everything the loop touches has to be
    created and driven on the one thread that owns it — which is exactly how a
    real helper runs.
    """
    from streamlib import ProcessorLinkDataAccess

    def drive() -> None:
        HelperProcessLifecycle(
            bridge,
            processor_class,
            "R-helper-test",
            "P-helper-test",
            ProcessorLinkDataAccess(),
        ).run_until_the_parent_is_done()

    lifecycle_thread = threading.Thread(target=drive, name="helper-lifecycle")
    lifecycle_thread.start()
    return lifecycle_thread


def drive_lifecycle_on_a_thread_and_hand_it_back(bridge, processor_class):
    """The same, with the lifecycle object once its thread has built it, so a
    test can swap in a counting data plane after `setup` has handed the real
    one to the engine's context."""
    from streamlib import ProcessorLinkDataAccess

    lifecycle_holder: "list[HelperProcessLifecycle]" = []
    lifecycle_built = threading.Event()

    def drive() -> None:
        lifecycle = HelperProcessLifecycle(
            bridge,
            processor_class,
            "R-helper-test",
            "P-helper-test",
            ProcessorLinkDataAccess(),
        )
        lifecycle_holder.append(lifecycle)
        lifecycle_built.set()
        lifecycle.run_until_the_parent_is_done()

    lifecycle_thread = threading.Thread(target=drive, name="helper-lifecycle")
    lifecycle_thread.start()
    assert lifecycle_built.wait(timeout=5.0), "the lifecycle thread never built its loop"
    return lifecycle_thread, lifecycle_holder[0]


def test_setup_answers_ready_and_run_answers_nothing_at_all(stand_in_parent):
    """A helper that replied to `run` would desynchronize every later command:
    the parent reads that reply as the answer to whatever it sends next."""
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_processor_class(f"{PROBE_MODULE}:PassThroughProbe")
    )

    stand_in_parent.send(
        {"cmd": "setup", "capability": "full", "config": {"tag": "probe"}, "ports": {}}
    )
    assert stand_in_parent.receive() == {"rpc": "ready"}

    stand_in_parent.send({"cmd": "run", "execution": "reactive", "interval_ms": 0})
    assert stand_in_parent.receive(timeout_seconds=0.5) is None

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_processor_that_cannot_set_itself_up_reports_the_failure(stand_in_parent):
    """`setup` is the one hook whose failure the parent is blocked on, so it
    must arrive as an error rather than be logged and answered `ready`."""
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_processor_class(f"{PROBE_MODULE}:RefusesSetupProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    refusal = stand_in_parent.receive()
    assert refusal["rpc"] == "error"
    assert "cannot set itself up" in refusal["error"]

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)


def test_pause_and_resume_are_answered_and_tracked_without_an_engine(stand_in_parent):
    """`ctx.is_paused()` reads a leased engine view in the parent; a child has
    none, so it answers from what the parent last announced."""
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_processor_class(f"{PROBE_MODULE}:PassThroughProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive()["rpc"] == "ready"

    stand_in_parent.send({"cmd": "on_pause", "capability": "limited"})
    assert stand_in_parent.receive()["rpc"] == "ok"
    stand_in_parent.send({"cmd": "on_resume", "capability": "limited"})
    assert stand_in_parent.receive()["rpc"] == "ok"

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)


def test_unwire_link_is_dispatched_mid_run_and_answers_nothing(stand_in_parent):
    """The parent sends this from its compiler while it holds the graph write
    lock, so it cannot wait for a reply — and a reply nobody reads would be
    taken as the answer to the next command, exactly like one to `run`.

    It must also reach a child that is already in its execution loop, which is
    the only state a disconnect can arrive in.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_processor_class(f"{PROBE_MODULE}:PassThroughProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive()["rpc"] == "ready"
    stand_in_parent.send({"cmd": "run", "execution": "reactive", "interval_ms": 0})

    stand_in_parent.send(
        {
            "cmd": "unwire_link",
            "direction": "output",
            "port": "frames_to_downstream",
            "link_id": "L-mid-run",
        }
    )
    assert stand_in_parent.receive(timeout_seconds=0.5) is None

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done", (
        "an unanswered unwire must leave the next command's reply the next thing "
        "the parent reads"
    )
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_reactive_helper_survives_losing_the_link_it_was_waiting_on(stand_in_parent):
    """Unwiring a reactive processor's LAST inbound link closes the very fd its
    loop is polling — the listener owns it, and dropping the last subscriber
    drops the listener.

    Fail-without-fix: cache `input_listener_fd()` outside the loop and every
    later wait polls a closed descriptor, which `poll` reports invalid at once,
    so the loop spins where it should park — or, once the OS recycles the
    number, silently waits on an unrelated object.

    A processor left with no inputs has nothing to wake it but the parent,
    which is exactly what it must fall back to.
    """
    from streamlib import ProcessorLinkDataAccess

    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread, lifecycle = drive_lifecycle_on_a_thread_and_hand_it_back(
        bridge, load_processor_class(f"{PROBE_MODULE}:PassThroughProbe")
    )

    link_id = "L-reactive-unwire"
    stand_in_parent.send(
        {
            "cmd": "setup",
            "capability": "full",
            "config": {},
            "ports": {"inputs": [engine_shaped_link_wiring("input", link_id)]},
        }
    )
    assert stand_in_parent.receive()["rpc"] == "ready"
    counting = _CountingLinkDataAccess(lifecycle._link_data_access)
    # A duck-typed stand-in: the loop only ever calls methods on it.
    lifecycle._link_data_access = cast(ProcessorLinkDataAccess, counting)
    stand_in_parent.send({"cmd": "run", "execution": "reactive", "interval_ms": 0})

    stand_in_parent.send(
        {"cmd": "unwire_link", "direction": "input", "link_id": link_id}
    )
    # The wait is load-bearing, not just the unanswered-command assertion: it
    # is what lets the loop go round again on the fd the unwire just closed.
    # Send `teardown` immediately instead and both commands drain in one batch,
    # the loop leaves before it ever waits again, and the bug hides.
    assert stand_in_parent.receive(timeout_seconds=0.5) is None
    assert counting.readiness_checks < 50, (
        f"the loop asked for data {counting.readiness_checks} times in half a second "
        f"with no input left — it must park on the parent, not spin"
    )

    stand_in_parent.send({"cmd": "on_pause", "capability": "limited"})
    assert stand_in_parent.receive() == {"rpc": "ok"}, (
        "the loop must survive losing the fd it was waiting on"
    )

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_an_idle_reactive_helper_answers_a_command_the_moment_it_arrives(stand_in_parent):
    """A reactive helper parked on its listener wakes for a parent's command as
    well as for a notify, so an idle processor hears `stop` or `on_pause` at
    once rather than when its wait next times out.

    Fail-without-fix: wait on the listener alone with the old 100 ms bound and
    each command sent to the parked loop waits out most of that bound, putting
    the median round trip near 100 ms.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_processor_class(f"{PROBE_MODULE}:PassThroughProbe")
    )
    stand_in_parent.send(
        {
            "cmd": "setup",
            "capability": "full",
            "config": {},
            "ports": {"inputs": [engine_shaped_link_wiring("input", "L-idle-commands")]},
        }
    )
    assert stand_in_parent.receive()["rpc"] == "ready"
    stand_in_parent.send({"cmd": "run", "execution": "reactive", "interval_ms": 0})

    round_trip_seconds = []
    for index in range(20):
        verb = "on_pause" if index % 2 == 0 else "on_resume"
        sent_at = time.monotonic()
        stand_in_parent.send({"cmd": verb, "capability": "limited"})
        assert stand_in_parent.receive() == {"rpc": "ok"}
        round_trip_seconds.append(time.monotonic() - sent_at)

    median_round_trip_seconds = statistics.median(round_trip_seconds)
    assert median_round_trip_seconds < 0.05, (
        f"an idle helper took a median {median_round_trip_seconds * 1000:.1f} ms to answer; "
        f"a command must wake its wait"
    )

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_reactive_helper_whose_descriptors_sit_above_1024_keeps_running(stand_in_parent):
    """A live unwire and rewire recreates the listener at the lowest free
    descriptor, which in a long-running app with many open files can be 1024
    or above. The loop must keep waiting on it.

    Here every descriptor below 1024 is held before the helper opens anything,
    so its listener and its command-arrival pipe both land above.

    Fail-without-fix: wait with `select.select`, which refuses any descriptor
    of 1024 or above with `ValueError: filedescriptor out of range in
    select()`; it escapes the loop, the helper's thread dies, and neither the
    bag nor the `on_pause` below is ever answered.
    """
    from streamlib import ProcessorLinkDataAccess

    soft_limit, hard_limit = resource.getrlimit(resource.RLIMIT_NOFILE)
    descriptors_needed = 2048
    if hard_limit != resource.RLIM_INFINITY and hard_limit < descriptors_needed:
        pytest.skip(f"the hard descriptor limit {hard_limit} leaves no room above 1024")
    resource.setrlimit(
        resource.RLIMIT_NOFILE, (max(soft_limit, descriptors_needed), hard_limit)
    )
    descriptors_holding_every_number_below_1024: "list[int]" = []
    try:
        while (
            not descriptors_holding_every_number_below_1024
            or descriptors_holding_every_number_below_1024[-1] < 1024
        ):
            descriptors_holding_every_number_below_1024.append(
                os.open(os.devnull, os.O_RDONLY)
            )

        bridge = ParentProcessBridge(stand_in_parent.child_end)
        assert bridge.lifecycle_command_arrival_fd() > 1024
        bridge.start_reading()
        lifecycle_thread = drive_lifecycle_on_a_thread(
            bridge, load_processor_class(f"{PROBE_MODULE}:PassThroughProbe")
        )

        inbound_link_id = "L-high-fd-in"
        outbound_link_id = "L-high-fd-out"
        downstream = ProcessorLinkDataAccess()
        _helper.wire_link_data_access(
            downstream,
            {"inputs": [engine_shaped_link_wiring("input", outbound_link_id)]},
        )
        stand_in_parent.send(
            {
                "cmd": "setup",
                "capability": "full",
                "config": {},
                "ports": {
                    "inputs": [engine_shaped_link_wiring("input", inbound_link_id)],
                    "outputs": [engine_shaped_link_wiring("output", outbound_link_id)],
                },
            }
        )
        assert stand_in_parent.receive()["rpc"] == "ready"
        stand_in_parent.send({"cmd": "run", "execution": "reactive", "interval_ms": 0})

        upstream = ProcessorLinkDataAccess()
        _helper.wire_link_data_access(
            upstream,
            {"outputs": [engine_shaped_link_wiring("output", inbound_link_id)]},
        )
        # The helper is parked on its listener by now, so the bag arrives by
        # waking the wait — the path that raised.
        time.sleep(0.2)
        upstream.write_to_output_port("frames_to_downstream", {"frame_index": 3})
        deadline = time.monotonic() + 5.0
        forwarded = None
        while forwarded is None and time.monotonic() < deadline:
            if downstream.any_input_port_has_data():
                forwarded = downstream.read_from_input_port("frames_from_upstream")
            else:
                time.sleep(0.01)
        assert forwarded == {"frame_index": 3, "tag": "untagged"}, (
            "a notify on a listener above descriptor 1024 must wake the helper"
        )

        stand_in_parent.send({"cmd": "on_pause", "capability": "limited"})
        assert stand_in_parent.receive() == {"rpc": "ok"}, (
            "a command must reach a helper waiting on descriptors above 1024"
        )

        stand_in_parent.send({"cmd": "teardown", "capability": "full"})
        assert stand_in_parent.receive()["rpc"] == "done"
        lifecycle_thread.join(timeout=5.0)
        assert not lifecycle_thread.is_alive()
    finally:
        for descriptor in descriptors_holding_every_number_below_1024:
            os.close(descriptor)
        resource.setrlimit(resource.RLIMIT_NOFILE, (soft_limit, hard_limit))


def test_an_unknown_lifecycle_command_is_survived(stand_in_parent):
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_processor_class(f"{PROBE_MODULE}:PassThroughProbe")
    )

    stand_in_parent.send({"cmd": "reticulate_splines"})
    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


# =============================================================================
# The shutdown ladder, helper half
# =============================================================================


@pytest.fixture
def hooks_the_interrupt_probes_reached():
    """The probes' shared record, emptied around each test that reads it."""
    from helper_process_probes import HOOKS_THE_INTERRUPT_PROBES_REACHED

    HOOKS_THE_INTERRUPT_PROBES_REACHED.clear()
    yield HOOKS_THE_INTERRUPT_PROBES_REACHED
    HOOKS_THE_INTERRUPT_PROBES_REACHED.clear()


def test_an_interrupt_inside_a_callback_still_leaves_stop_and_teardown_to_run(
    stand_in_parent, hooks_the_interrupt_probes_reached
):
    """The parent's ladder interrupts a callback that outran its one-second
    budget, and the plan has `stop()` and `teardown()` still running after
    it — only the bag in flight is lost.

    Fail-without-fix: `call_hook` catches `Exception`, which a
    `KeyboardInterrupt` is not, so it leaves the loop, the helper's `main`
    and the process, and neither hook is ever reached.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_processor_class(f"{PROBE_MODULE}:InterruptedInProcessProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive() == {"rpc": "ready"}
    stand_in_parent.send({"cmd": "run", "execution": "continuous", "interval_ms": 0})

    # Both rungs at once, the way the ladder sends them.
    stand_in_parent.send({"cmd": "stop", "capability": "full"})
    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "stopped"
    assert stand_in_parent.receive()["rpc"] == "done"

    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()
    assert hooks_the_interrupt_probes_reached == [
        "process-interrupted",
        "stop",
        "teardown",
    ]


def test_an_interrupted_setup_is_answered_and_still_gets_its_teardown(
    stand_in_parent, hooks_the_interrupt_probes_reached
):
    """The plan reads literally — any callback interrupted at shutdown, `setup()`
    included, is followed by `teardown()`. A `setup()` that raises on its own
    keeps its own no-teardown rule, which lives in the parent: it never sends
    the command.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_processor_class(f"{PROBE_MODULE}:InterruptedInSetupProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    refusal = stand_in_parent.receive()
    assert refusal["rpc"] == "error"
    assert "interrupted" in refusal["error"]

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"

    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()
    assert hooks_the_interrupt_probes_reached == ["setup-interrupted", "teardown"]


def test_a_closed_channel_answers_every_later_read_rather_than_the_first(
    stand_in_parent,
):
    """The end of the channel is sticky.

    Fail-without-fix: the reader thread puts exactly one `None` on the queue,
    so a mid-run drain that consumes it leaves the outer `queue.get()` with
    nothing left to receive and no writer left to send — blocked for the life
    of the process, which is the helper that will not quit.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    stand_in_parent.parent_end.close()

    # The drain consumes the end-of-channel the way a mid-run loop does, once
    # the reader thread has seen it.
    deadline = time.monotonic() + 5.0
    while bridge.next_lifecycle_command_if_waiting() != (True, None):
        assert time.monotonic() < deadline, "the reader never reported the closed channel"
        time.sleep(0.01)

    read_after_the_end: list = []

    def read_once() -> None:
        read_after_the_end.append(bridge.next_lifecycle_command())

    blocked_reader = threading.Thread(target=read_once, name="reads-after-the-end")
    blocked_reader.start()
    blocked_reader.join(timeout=5.0)
    assert not blocked_reader.is_alive(), (
        "the second read blocked forever on a channel that had already ended"
    )
    assert read_after_the_end == [None]
    assert bridge.next_lifecycle_command_if_waiting() == (True, None)


def test_a_command_the_parent_sent_before_letting_go_is_still_delivered(
    stand_in_parent,
):
    """The end of the channel is sticky, not a gate in front of what is queued.

    Fail-without-fix: latch the end and answer it ahead of the queue, and the
    `stop` and `teardown` a parent wrote on its way out are discarded — which
    is the whole of a shutdown for a helper whose parent closed promptly.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    stand_in_parent.send({"cmd": "stop", "capability": "full"})
    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    stand_in_parent.parent_end.close()

    deadline = time.monotonic() + 5.0
    delivered = []
    while len(delivered) < 3:
        assert time.monotonic() < deadline, f"only got {delivered}"
        delivered.append(bridge.next_lifecycle_command())

    assert [command["cmd"] for command in delivered[:2]] == ["stop", "teardown"]
    assert delivered[2] is None


def test_the_escalate_socket_is_not_inherited_by_anything_the_helper_starts(
    monkeypatch,
):
    """The plan gives a helper's descendants no descriptor of its own. The parent
    cleared `FD_CLOEXEC` on this fd to hand it over, and `socket.socket(fileno=)`
    leaves the flag exactly as it found it, so an `os.system` or `posix_spawn`
    child would inherit the channel every privileged operation rides.
    """
    parent_end, child_end = socket.socketpair()
    inherited_fd = child_end.detach()
    os.set_inheritable(inherited_fd, True)
    monkeypatch.setenv("STREAMLIB_ESCALATE_FD", str(inherited_fd))

    bridge = ParentProcessBridge.open_from_inherited_fd()
    try:
        assert not os.get_inheritable(inherited_fd)
    finally:
        bridge._socket.close()
        parent_end.close()


def test_a_link_wired_after_setup_opens_its_port_mid_run(stand_in_parent):
    """A processor added to a running graph is set up with no links and gets
    its first ones from later connects. The engine hands each one over as a
    `wire_link` the child acts on from inside its execution loop — parked on
    the parent, because with no input it has nothing else to wait on — and
    bags then cross both new links.

    Fail-without-fix: drop the `wire_link` arm from `_dispatch` and the child
    logs an unknown command, its input never opens, and the bag published
    below never comes back.
    """
    from streamlib import ProcessorLinkDataAccess

    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_processor_class(f"{PROBE_MODULE}:PassThroughProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive()["rpc"] == "ready"
    stand_in_parent.send({"cmd": "run", "execution": "reactive", "interval_ms": 0})

    inbound_link_id = "L-wired-late-in"
    outbound_link_id = "L-wired-late-out"
    # The far end of the child's output is opened first, so nothing the child
    # publishes is dropped for want of a subscriber.
    downstream = ProcessorLinkDataAccess()
    _helper.wire_link_data_access(
        downstream,
        {"inputs": [engine_shaped_link_wiring("input", outbound_link_id)]},
    )
    stand_in_parent.send(
        {
            "cmd": "wire_link",
            "direction": "output",
            "link": engine_shaped_link_wiring("output", outbound_link_id),
        }
    )
    stand_in_parent.send(
        {
            "cmd": "wire_link",
            "direction": "input",
            "link": engine_shaped_link_wiring("input", inbound_link_id),
        }
    )
    upstream = ProcessorLinkDataAccess()
    _helper.wire_link_data_access(
        upstream,
        {"outputs": [engine_shaped_link_wiring("output", inbound_link_id)]},
    )

    wire_answers = {
        answer["link_id"]: answer
        for answer in (stand_in_parent.receive(), stand_in_parent.receive())
    }

    # A bag published before the child's subscriber is up is dropped by the
    # channel, so publish until one comes back.
    deadline = time.monotonic() + 10.0
    forwarded = None
    while forwarded is None and time.monotonic() < deadline:
        upstream.write_to_output_port("frames_to_downstream", {"frame_index": 7})
        time.sleep(0.05)
        if downstream.any_input_port_has_data():
            forwarded = downstream.read_from_input_port("frames_from_upstream")
    assert forwarded == {"frame_index": 7, "tag": "untagged"}, (
        "a bag must cross the input wired after setup and come back over the "
        "output wired after setup"
    )

    assert wire_answers == {
        outbound_link_id: {"rpc": "link_wired", "link_id": outbound_link_id},
        inbound_link_id: {"rpc": "link_wired", "link_id": inbound_link_id},
    }, "each wire is answered for the link it names, and both ports did open"

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done", (
        "a wire's answer rides its own link-scoped tag, so the next lifecycle "
        "command's reply is still the next lifecycle frame the parent reads"
    )
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_wire_the_helper_cannot_open_is_answered_with_the_reason(stand_in_parent):
    """The engine reports a link `wired` only on this answer, so a port that
    could not open has to say so rather than only logging: the reason is what
    `graph` renders against the link, and the caller of a live `connect` reads
    it there.

    The refusal here is a port whose ring is deeper than the channel it is
    joining, which iceoryx2 refuses at `create_subscriber` — the shape a live
    connect onto a shallower channel takes.

    Fail-without-fix: log the failure and answer nothing, and the parent waits
    on an answer that never comes while `graph` reports the link pending
    forever.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_processor_class(f"{PROBE_MODULE}:PassThroughProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive()["rpc"] == "ready"
    stand_in_parent.send({"cmd": "run", "execution": "reactive", "interval_ms": 0})

    unopenable = engine_shaped_link_wiring("input", "L-unopenable")
    unopenable["channel_service_creation_depth"] = 4
    unopenable["input_port_ring_depth"] = 64
    stand_in_parent.send({"cmd": "wire_link", "direction": "input", "link": unopenable})

    answer = stand_in_parent.receive()
    assert answer["rpc"] == "link_wire_failed", (
        f"a port that could not open must be refused rather than reported open; got {answer}"
    )
    assert answer["link_id"] == "L-unopenable"
    assert "frames_from_upstream" in answer["reason"], (
        "`graph` renders this reason, so it has to name what could not open; "
        f"got {answer['reason']!r}"
    )

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done", (
        "a refusal is link-scoped too, so it never becomes the reply to the "
        "next lifecycle command"
    )
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_wire_after_a_failed_setup_is_refused_rather_than_opened(stand_in_parent):
    """The engine can hand a link over while `setup` is still running, and the
    child reads it only once `setup` has answered. A processor whose setup
    failed opens no port for it, so the link must read error rather than wired.

    Fail-without-fix: open the port anyway and the child answers `link_wired`
    for a processor that never came up.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_processor_class(f"{PROBE_MODULE}:RefusesSetupProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    stand_in_parent.send(
        {
            "cmd": "wire_link",
            "direction": "output",
            "link": engine_shaped_link_wiring("output", "L-during-a-failed-setup"),
        }
    )
    assert stand_in_parent.receive()["rpc"] == "error"

    answer = stand_in_parent.receive()
    assert answer["rpc"] == "link_wire_failed", answer
    assert answer["link_id"] == "L-during-a-failed-setup"
    assert "setup did not succeed" in answer["reason"]

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_wire_in_an_unknown_direction_is_refused_rather_than_left_unanswered(
    stand_in_parent,
):
    """A direction the helper cannot act on is still a link the parent is
    waiting on. Answered as a refusal, the link reaches `error` in `graph`;
    left unanswered it would sit pending for the life of the runtime.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_processor_class(f"{PROBE_MODULE}:PassThroughProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive()["rpc"] == "ready"
    stand_in_parent.send({"cmd": "run", "execution": "reactive", "interval_ms": 0})

    stand_in_parent.send(
        {
            "cmd": "wire_link",
            "direction": "sideways",
            "link": engine_shaped_link_wiring("input", "L-sideways"),
        }
    )

    answer = stand_in_parent.receive()
    assert answer["rpc"] == "link_wire_failed"
    assert answer["link_id"] == "L-sideways"

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


class _CountingLinkDataAccess:
    """Forwards to the real data plane and counts the loop's listener drains."""

    def __init__(self, real) -> None:
        self._real = real
        self.drain_calls = 0
        self.readiness_checks = 0

    def drain_input_listener(self) -> None:
        self.drain_calls += 1
        self._real.drain_input_listener()

    def any_input_port_has_data(self) -> bool:
        self.readiness_checks += 1
        return self._real.any_input_port_has_data()

    def __getattr__(self, name: str):
        return getattr(self._real, name)


def test_a_helper_that_cannot_keep_up_still_drains_its_listener_every_pass(stand_in_parent):
    """A processor slower than its upstream never goes idle, so a loop that
    drains only when idle leaves the listener's datagram socket to fill within
    seconds — after which every upstream notify fails and is logged, one
    warning per frame for the rest of the run. The drain has to happen on
    every pass.

    Fail-without-fix: move the drain back under the idle branch and the burst
    below is processed with a drain or two at most, all after it ended.
    """
    from streamlib import ProcessorLinkDataAccess

    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_holder: "list[HelperProcessLifecycle]" = []

    def drive() -> None:
        lifecycle = HelperProcessLifecycle(
            bridge,
            load_processor_class(f"{PROBE_MODULE}:SlowPassThroughProbe"),
            "R-helper-test",
            "P-helper-test",
            ProcessorLinkDataAccess(),
        )
        lifecycle_holder.append(lifecycle)
        lifecycle.run_until_the_parent_is_done()

    lifecycle_thread = threading.Thread(target=drive, name="helper-lifecycle")
    lifecycle_thread.start()

    inbound_link_id = "L-slow-in"
    outbound_link_id = "L-slow-out"
    downstream = ProcessorLinkDataAccess()
    _helper.wire_link_data_access(
        downstream,
        {"inputs": [engine_shaped_link_wiring("input", outbound_link_id)]},
    )
    stand_in_parent.send(
        {
            "cmd": "setup",
            "capability": "full",
            "config": {},
            "ports": {
                "inputs": [engine_shaped_link_wiring("input", inbound_link_id)],
                "outputs": [engine_shaped_link_wiring("output", outbound_link_id)],
            },
        }
    )
    assert stand_in_parent.receive()["rpc"] == "ready"
    # Swapped in after setup, which handed the real object to the engine's
    # context; from here on only the loop reads it.
    (lifecycle,) = lifecycle_holder
    counting = _CountingLinkDataAccess(lifecycle._link_data_access)
    # A duck-typed stand-in: the loop only ever calls methods on it.
    lifecycle._link_data_access = cast(ProcessorLinkDataAccess, counting)
    stand_in_parent.send({"cmd": "run", "execution": "reactive", "interval_ms": 0})

    upstream = ProcessorLinkDataAccess()
    _helper.wire_link_data_access(
        upstream,
        {"outputs": [engine_shaped_link_wiring("output", inbound_link_id)]},
    )
    # Faster than the probe processes, so its input never runs dry during the
    # burst and the loop never reaches its idle branch.
    for index in range(80):
        upstream.write_to_output_port("frames_to_downstream", {"frame_index": index})
        time.sleep(0.001)

    forwarded = 0
    quiet_since = time.monotonic()
    deadline = time.monotonic() + 10.0
    while time.monotonic() < deadline and time.monotonic() - quiet_since < 0.3:
        if downstream.any_input_port_has_data():
            if downstream.read_from_input_port("frames_from_upstream") is not None:
                forwarded += 1
                quiet_since = time.monotonic()
            continue
        time.sleep(0.01)
    assert forwarded >= 8, f"the probe forwarded only {forwarded} bags of the burst"
    assert counting.drain_calls >= 8, (
        f"the listener was drained {counting.drain_calls} times while {forwarded} bags "
        f"were processed back to back — it must be drained on every pass"
    )

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


# =============================================================================
# The continuous loop's pacing
# =============================================================================


@pytest.fixture
def when_the_pacing_probe_processed_ns():
    """The pacing probe's record, emptied around each test that reads it."""
    from helper_process_probes import WHEN_THE_PACING_PROBE_PROCESSED_NS

    WHEN_THE_PACING_PROBE_PROCESSED_NS.clear()
    yield WHEN_THE_PACING_PROBE_PROCESSED_NS
    WHEN_THE_PACING_PROBE_PROCESSED_NS.clear()


def run_the_pacing_probe(stand_in_parent, interval_ms: int, running_seconds: float) -> int:
    """Set the pacing probe up, run it continuous at `interval_ms` for
    `running_seconds`, then stop and tear it down the way the ladder does.

    Returns when `run` was sent, in monotonic nanoseconds.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_processor_class(f"{PROBE_MODULE}:ContinuousPacingProbe")
    )
    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive() == {"rpc": "ready"}

    run_sent_ns = time.monotonic_ns()
    stand_in_parent.send(
        {"cmd": "run", "execution": "continuous", "interval_ms": interval_ms}
    )
    time.sleep(running_seconds)

    stand_in_parent.send({"cmd": "stop", "capability": "full"})
    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "stopped"
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()
    return run_sent_ns


def test_a_continuous_processor_runs_once_per_interval_longer_than_the_command_wait(
    stand_in_parent, when_the_pacing_probe_processed_ns
):
    """A 250 ms interval over 1.1 s is a call at the start and four ticks, so
    five `process()` calls.

    Fail-without-fix: the loop reads the timer wait's own 100 ms timeout as a
    tick and calls `process()` after every one, so the probe runs eleven or
    more times — about ten a second, whatever interval above 100 ms it asked for.
    """
    run_the_pacing_probe(stand_in_parent, interval_ms=250, running_seconds=1.1)

    process_calls = len(when_the_pacing_probe_processed_ns)
    assert 4 <= process_calls <= 6, (
        f"a 250 ms interval ran process() {process_calls} times in 1.1 s; five is right"
    )


def test_a_continuous_processor_runs_at_the_start_rather_than_one_interval_in(
    stand_in_parent, when_the_pacing_probe_processed_ns
):
    """The native runner calls a continuous processor at once and then paces
    it, so a Python source's first bag does not trail a Rust one's by an
    interval."""
    run_sent_ns = run_the_pacing_probe(
        stand_in_parent, interval_ms=1000, running_seconds=0.6
    )

    assert when_the_pacing_probe_processed_ns, "process() never ran"
    first_call_after_run_ms = (when_the_pacing_probe_processed_ns[0] - run_sent_ns) / 1e6
    assert first_call_after_run_ms < 500, (
        f"the first process() came {first_call_after_run_ms:.0f} ms after run; a "
        f"continuous processor runs at the start"
    )


def test_a_continuous_processor_with_no_interval_never_runs_faster_than_the_floor(
    stand_in_parent, when_the_pacing_probe_processed_ns
):
    """An interval of zero runs as often as the engine's floor allows and no
    more, so a processor with nothing to do does not take a whole core.

    Fail-without-fix: the zero-interval loop calls `process()` back to back
    with no wait at all, hundreds of thousands of times a second.
    """
    run_the_pacing_probe(stand_in_parent, interval_ms=0, running_seconds=0.5)

    process_calls = len(when_the_pacing_probe_processed_ns)
    assert process_calls >= 2, "the zero-interval loop never ran the processor"
    measured_span_seconds = (
        when_the_pacing_probe_processed_ns[-1] - when_the_pacing_probe_processed_ns[0]
    ) / 1_000_000_000
    calls_per_second = (process_calls - 1) / measured_span_seconds
    floor_calls_per_second = (
        1_000_000_000 / _helper.CONTINUOUS_INTERVAL_FLOOR_NANOSECONDS
    )
    assert calls_per_second <= floor_calls_per_second * 1.2, (
        f"a zero interval ran process() {calls_per_second:.0f} times a second; the "
        f"floor allows {floor_calls_per_second:.0f}"
    )


# =============================================================================
# Reconfiguration
# =============================================================================


def reconfigure_a_set_up_probe(stand_in_parent, probe_name: str, configuration: dict) -> dict:
    """Set `probe_name` up, hand it `configuration`, and return the answer."""
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_processor_class(f"{PROBE_MODULE}:{probe_name}")
    )
    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    stand_in_parent.receive()

    stand_in_parent.send({"cmd": "update_config", "config": configuration})
    answer = stand_in_parent.receive()

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()
    return answer


def test_a_configuration_the_processor_takes_is_answered_ok(stand_in_parent):
    answer = reconfigure_a_set_up_probe(
        stand_in_parent, "TakesReconfigurationProbe", {"gain": 3}
    )
    assert answer == {"rpc": "ok"}


@pytest.mark.parametrize(
    ("probe_name", "the_cause_named"),
    [
        ("PassThroughProbe", "cannot be reconfigured while running"),
        ("RefusesReconfigurationProbe", "a gain of 3 is out of range"),
        ("RefusesSetupProbe", "setup did not succeed"),
    ],
)
def test_a_configuration_the_processor_does_not_take_is_refused_naming_the_cause(
    stand_in_parent, probe_name, the_cause_named
):
    """The parent reports a refused update to its caller and keeps the graph's
    previous configuration, which it can only do if the refusal reaches it.

    Fail-without-fix: the helper logs the refusal and answers `ok`, so the
    parent reports a configuration the processor never took.
    """
    answer = reconfigure_a_set_up_probe(stand_in_parent, probe_name, {"gain": 3})
    assert answer["rpc"] == "error"
    assert the_cause_named in answer["error"]


# =============================================================================
# Bootstrap refusals
# =============================================================================


def test_a_helper_started_without_its_channel_says_so(monkeypatch):
    monkeypatch.delenv(_helper.ESCALATE_FD_ENV, raising=False)
    with pytest.raises(HelperProcessProtocolError) as refusal:
        ParentProcessBridge.open_from_inherited_fd()
    assert _helper.ESCALATE_FD_ENV in str(refusal.value)


def test_a_non_numeric_channel_fd_is_refused_by_value(monkeypatch):
    monkeypatch.setenv(_helper.ESCALATE_FD_ENV, "not-an-fd")
    with pytest.raises(HelperProcessProtocolError) as refusal:
        ParentProcessBridge.open_from_inherited_fd()
    assert "not-an-fd" in str(refusal.value)


#: A build id no build of this checkout mints: its nonce is all zeros.
ENGINE_BUILD_ID_OF_ANOTHER_BUILD = (
    "0.0.1+0123456789abcdef0123456789abcdef01234567.00000000000000000000000000000000"
)

#: Long enough for a cold interpreter to import the whole wheel.
SECONDS_A_REAL_HELPER_HAS_TO_START = 30.0


@pytest.fixture
def empty_iceoryx2_domain_root():
    """A domain root of the helper's own, empty until an iceoryx2 node opens in
    it — which is how a test sees whether one did. Short, under `/tmp`, so a
    helper that did get as far as a node is not refused on the socket budget
    instead."""
    domain_root = Path(tempfile.mkdtemp(prefix="sl-build-id-", dir="/tmp"))
    try:
        yield domain_root
    finally:
        shutil.rmtree(domain_root, ignore_errors=True)


def start_a_real_helper_process(
    parent: StandInParent,
    domain_root: Path,
    parent_engine_build_id: "str | None",
) -> subprocess.Popen:
    """`python -m streamlib._helper` as the spawn host starts it — its channel,
    its class, its domain — handed `parent_engine_build_id`, or no id at all.

    Everything a helper needs to reach its own iceoryx2 node is supplied, so a
    helper that let its start through would open one and wait on `parent`."""
    environment = {
        name: value
        for name, value in os.environ.items()
        if name != _helper.ENGINE_BUILD_ID_ENV
    }
    if parent_engine_build_id is not None:
        environment[_helper.ENGINE_BUILD_ID_ENV] = parent_engine_build_id
    environment.update(
        {
            _helper.ENTRYPOINT_ENV: f"{PROBE_MODULE}:PassThroughProbe",
            _helper.PROCESSOR_ID_ENV: "Pbuildid",
            _helper.ESCALATE_FD_ENV: str(parent.child_end.fileno()),
            "STREAMLIB_ICEORYX2_DOMAIN_ROOT": str(domain_root),
            "PYTHONPATH": os.pathsep.join(
                entry
                for entry in (str(Path(__file__).parent), os.environ.get("PYTHONPATH"))
                if entry
            ),
        }
    )
    return subprocess.Popen(
        [sys.executable, "-m", "streamlib._helper"],
        env=environment,
        pass_fds=[parent.child_end.fileno()],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def standard_error_of_a_helper_that_refused_its_start(helper: subprocess.Popen) -> str:
    try:
        _, standard_error = helper.communicate(timeout=SECONDS_A_REAL_HELPER_HAS_TO_START)
    except subprocess.TimeoutExpired:
        helper.kill()
        _, standard_error = helper.communicate()
        pytest.fail(f"the helper let its start through and ran on:\n{standard_error}")
    assert helper.returncode == 1, standard_error
    return standard_error


def test_a_helper_that_imported_another_engine_build_refuses_before_it_opens_anything(
    stand_in_parent, empty_iceoryx2_domain_root
):
    """A stale `streamlib` earlier on a helper's `sys.path`, or an engine built
    against another iceoryx2, otherwise fails every service open as a
    corrupted service. The refusal is on raw stderr, naming both builds,
    because the log channel does not exist yet — and no node has opened.

    Fail-without-fix: skip the comparison and this helper opens its node in
    the domain root and waits on its parent until the timeout.
    """
    helper = start_a_real_helper_process(
        stand_in_parent, empty_iceoryx2_domain_root, ENGINE_BUILD_ID_OF_ANOTHER_BUILD
    )

    standard_error = standard_error_of_a_helper_that_refused_its_start(helper)

    assert (
        f"this helper imported engine build {engine_build_id_compiled_into_this_extension()}"
        in standard_error
    ), standard_error
    assert (
        f"its parent is engine build {ENGINE_BUILD_ID_OF_ANOTHER_BUILD}" in standard_error
    ), standard_error
    assert list(empty_iceoryx2_domain_root.iterdir()) == []
    assert stand_in_parent.receive(timeout_seconds=0.1) is None


def test_a_helper_handed_no_engine_build_id_refuses_rather_than_passing(
    stand_in_parent, empty_iceoryx2_domain_root
):
    """An absent id is not a pass: a helper that cannot tell whether its engine
    is its parent's does not start.

    Fail-without-fix: treat an unset variable as agreement — the check this
    replaced did — and this helper opens its node and waits.
    """
    helper = start_a_real_helper_process(
        stand_in_parent, empty_iceoryx2_domain_root, parent_engine_build_id=None
    )

    standard_error = standard_error_of_a_helper_that_refused_its_start(helper)

    assert f"{_helper.ENGINE_BUILD_ID_ENV} is not set" in standard_error, standard_error
    assert engine_build_id_compiled_into_this_extension() in standard_error
    assert list(empty_iceoryx2_domain_root.iterdir()) == []


def test_a_helper_handed_its_own_engine_build_id_starts_and_opens_its_node(
    stand_in_parent, empty_iceoryx2_domain_root
):
    """The control arm: parent and helper importing one wheel start exactly as
    before — the helper reports itself started on its channel with its node
    open in the domain it was handed."""
    helper = start_a_real_helper_process(
        stand_in_parent,
        empty_iceoryx2_domain_root,
        engine_build_id_compiled_into_this_extension(),
    )
    try:
        deadline = time.monotonic() + SECONDS_A_REAL_HELPER_HAS_TO_START
        started = None
        while started is None and time.monotonic() < deadline:
            frame = stand_in_parent.receive(timeout_seconds=deadline - time.monotonic())
            if frame is None:
                break
            if frame.get("op") == "log" and frame.get("message") == "helper process started":
                started = frame
        assert started is not None, "the helper never reported itself started"
        assert (empty_iceoryx2_domain_root / "nodes").is_dir()
    finally:
        helper.kill()
        helper.communicate()


def test_the_helper_module_is_runnable_as_a_module():
    """The parent execs `python -m streamlib._helper`; a module without a
    `__main__` guard would exec cleanly and do nothing."""
    helper_source = os.path.join(os.path.dirname(_helper.__file__), "_helper.py")
    with open(helper_source, encoding="utf-8") as source:
        assert '__name__ == "__main__"' in source.read()
