# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The runtime loop a Python processor's own child process runs.

Everything here drives the helper's own halves — the framed socket, the class
loader, the lifecycle machine — against a stand-in parent, so what the loop
does is asserted directly rather than inferred from a running graph.
"""

import json
import os
import select
import socket
import struct
import threading
import time

import pytest

from streamlib import _helper
from streamlib._helper import (
    HelperProcessLifecycle,
    HelperProcessProtocolError,
    ParentProcessBridge,
    ParentProcessLogSink,
    load_processor_class,
    schema_ident_segments,
)

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


def test_a_wire_schema_flattens_into_the_six_segments_the_binding_takes():
    assert schema_ident_segments(
        {
            "org": "tatolab",
            "package": "media",
            "type": "VideoFrame",
            "version": {"major": 1, "minor": 2, "patch": 3},
        }
    ) == ("tatolab", "media", "VideoFrame", 1, 2, 3)


def test_a_wildcard_port_carries_no_schema():
    """The engine sends `null` for a port that declared no schema; the channel
    then carries no routing tag."""
    assert schema_ident_segments(None) is None


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
            "schema": None,
            "expected_payload_bytes": 1024,
            "max_payload_bytes_per_channel": 1 << 20,
            "max_queued_messages": 8,
            "max_subscribers": 2,
            "notify_max_notifiers": 1,
        }
    return {
        "name": "frames_from_upstream",
        "link_id": link_id,
        "enable_safe_overflow": True,
        "channel_service_name": channel_service_name,
        "notify_service_name": notify_service_name,
        "read_mode": "read_next_in_order",
        "max_queued_messages": 8,
        "max_subscribers": 2,
        "notify_max_notifiers": 1,
    }


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
    ready = stand_in_parent.receive()
    assert ready["rpc"] == "ready"
    assert ready["protocol_version"] == _helper.PROTOCOL_VERSION

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


def test_the_helper_module_is_runnable_as_a_module():
    """The parent execs `python -m streamlib._helper`; a module without a
    `__main__` guard would exec cleanly and do nothing."""
    helper_source = os.path.join(os.path.dirname(_helper.__file__), "_helper.py")
    with open(helper_source, encoding="utf-8") as source:
        assert '__name__ == "__main__"' in source.read()
