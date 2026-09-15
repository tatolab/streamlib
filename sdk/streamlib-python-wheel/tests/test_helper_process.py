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
import shutil
import socket
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

    The envelope caps the notify service at one notifier (`notify_max_notifiers`
    is the destination's fan-in), so a notifier still held from the first
    connect makes the reconnect's `create_notifier` the second on a max-1
    service — `ExceedsMaxSupportedNotifiers`. The engine cannot reclaim these
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
    loop is selecting on — the listener owns it, and dropping the last
    subscriber drops the listener.

    Fail-without-fix: cache `input_listener_fd()` outside the loop (as
    `_run_reactive` did before this change) and the next `select` raises
    `OSError: [Errno 9] Bad file descriptor`, killing the child mid-run — or,
    once the OS recycles the number, silently waits on an unrelated object.
    Nothing between `_run_reactive` and `main()` catches it.

    A processor left with no inputs has nothing to wake it but the parent,
    which is exactly what it must fall back to.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
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
    stand_in_parent.send({"cmd": "run", "execution": "reactive", "interval_ms": 0})

    stand_in_parent.send(
        {"cmd": "unwire_link", "direction": "input", "link_id": link_id}
    )
    # The wait is load-bearing, not just the unanswered-command assertion: it
    # is what lets the loop go round again on the fd the unwire just closed,
    # several times over at a 0.1s poll interval. Send `teardown` immediately
    # instead and both commands drain in one batch, the loop leaves before it
    # ever selects again, and the bug hides.
    assert stand_in_parent.receive(timeout_seconds=0.5) is None

    # A loop that died of EBADF answers nothing from here on.
    stand_in_parent.send({"cmd": "on_pause", "capability": "limited"})
    assert stand_in_parent.receive() == {"rpc": "ok"}, (
        "the loop must survive losing the fd it was waiting on"
    )

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


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

    # A bag published before the child's subscriber is up is dropped by the
    # channel, and nothing announces when the child has acted on the wire, so
    # publish until one comes back.
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

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done", (
        "an unanswered wire must leave the next command's reply the next thing "
        "the parent reads"
    )
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


class _CountingLinkDataAccess:
    """Forwards to the real data plane and counts the loop's listener drains."""

    def __init__(self, real) -> None:
        self._real = real
        self.drain_calls = 0

    def drain_input_listener(self) -> None:
        self.drain_calls += 1
        self._real.drain_input_listener()

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
