# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The parent bridge a helper process speaks: frames, log records, the
releases an answer it stopped waiting on owes, and the escalate socket it
inherits.

Everything here drives the helper's own halves against a stand-in parent, so
what the loop does is asserted directly rather than inferred from a running
graph.
"""

import gc
import os
import socket
import threading
import time
from typing import cast

import pytest

from helper_process_test_support import (
    PROBE_MODULE,
    drive_lifecycle_on_a_thread,
)
from tatolab.runtime import _engine
from tatolab.runtime import _parent_process_bridge as parent_process_bridge
from tatolab.runtime import _processor_interpreter_bootstrap as processor_interpreter_bootstrap
from tatolab.runtime._processor_interpreter_bootstrap import (
    ParentProcessBridge,
    ParentProcessLogSink,
    load_processor_class,
)

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)


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
    """A helper has no engine in it, so `tatolab.stream.log` cannot hand a record to
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


class StandInBridgeWatchingTheSequenceLock:
    """A bridge that records whether the sink still held its sequence lock.

    The race this guards is not reproducible on demand — CPython rarely
    switches threads between numbering a record and writing it — so the test
    below asserts the invariant that makes the race impossible instead of
    trying to lose it.
    """

    def __init__(self, sequence_lock_of_the_sink_under_test) -> None:
        self._sequence_lock = sequence_lock_of_the_sink_under_test
        self.frames_sent_without_the_sequence_lock: "list[dict]" = []
        self.sequence_numbers_sent: "list[int]" = []
        self._order_lock = threading.Lock()

    def send(self, message: dict) -> None:
        # `locked()` is true for any holder, and the only thread that can be
        # inside this call is the one that took it.
        holding_the_sequence_lock = self._sequence_lock.locked()
        with self._order_lock:
            if not holding_the_sequence_lock:
                self.frames_sent_without_the_sequence_lock.append(message)
            self.sequence_numbers_sent.append(int(message["source_seq"]))


def test_a_record_goes_out_under_the_lock_that_numbered_it(stand_in_parent):
    """A helper has two threads sending records — its processor's and the
    engine-log forwarder's — and one sequence between them. A number taken
    before a frame that goes out after the next one's would put the sequence
    backwards on the wire, where a reader reads a step backwards as a new
    helper process and a jump as records lost. Numbering and sending under one
    lock is what makes that impossible.

    Fail-without-fix: number under the lock and send outside it, and every
    frame here arrives with the lock already released.
    """
    sink = ParentProcessLogSink(
        cast(ParentProcessBridge, None), "P-helper-test"
    )
    bridge = StandInBridgeWatchingTheSequenceLock(sink._sequence_lock)
    sink._bridge = cast(ParentProcessBridge, bridge)
    records_each_sender_sends = 20

    def send_the_processors_records() -> None:
        for _ in range(records_each_sender_sends):
            sink("info", "from the processor", None)

    def send_the_forwarders_records() -> None:
        for _ in range(records_each_sender_sends):
            sink.send_a_captured_engine_record(
                {
                    "level": "warn",
                    "target": "iceoryx2",
                    "message": "from the forwarder",
                    "pipeline_id": None,
                    "processor_id": None,
                    "rhi_op": None,
                    "attrs": {},
                    "emitted_at_wall_clock_nanoseconds": 1_700_000_000_000_000_000,
                }
            )

    senders = [
        threading.Thread(target=send_the_processors_records),
        threading.Thread(target=send_the_forwarders_records),
    ]
    for sender in senders:
        sender.start()
    for sender in senders:
        sender.join(timeout=30.0)
        assert not sender.is_alive()

    assert not bridge.frames_sent_without_the_sequence_lock, (
        "a record numbered under the lock and sent outside it can reach the parent "
        "after a later one: "
        f"{bridge.frames_sent_without_the_sequence_lock}"
    )
    assert bridge.sequence_numbers_sent == list(
        range(1, records_each_sender_sends * 2 + 1)
    ), (
        "one sequence covers both senders, climbing in wire order with no number "
        f"spent twice: {bridge.sequence_numbers_sent}"
    )


@pytest.mark.parametrize(
    ("escalate_op", "release_op", "released_id_field"),
    [
        ("acquire_pixel_buffer", "release_handle", "handle_id"),
        ("acquire_storage_buffer", "release_handle", "handle_id"),
        ("register_acceleration_structure_blas", "release_handle", "handle_id"),
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
    with pytest.raises(processor_interpreter_bootstrap.EscalateRequestError):
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
    with pytest.raises(processor_interpreter_bootstrap.EscalateRequestError):
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
    slot = processor_interpreter_bootstrap._PendingEscalateResponse()
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


@pytest.mark.linux_only_capability(reason="MoltenVK has no VK_KHR_ray_tracing_pipeline")
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


@pytest.mark.linux_only_capability(reason="MoltenVK has no VK_KHR_ray_tracing_pipeline")
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
    monkeypatch.setenv("STREAMLIB_SURFACE_SOCKET", "/nonexistent/streamlib-surface.sock")
    decode_the_frame = parent_process_bridge._decode_frame_payload

    def decode_the_frame_collecting_garbage_on_the_marker(payload: bytes):
        frame = decode_the_frame(payload)
        if frame.get("cmd") == "collect_garbage_on_the_reader":
            gc.collect()
        return frame

    monkeypatch.setattr(
        parent_process_bridge,
        "_decode_frame_payload",
        decode_the_frame_collecting_garbage_on_the_marker,
    )
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    context = _engine.open_runtime_context_full_access_for_helper_process(
        {},
        _engine.open_node_link_data_access_for_helper_process(),
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
