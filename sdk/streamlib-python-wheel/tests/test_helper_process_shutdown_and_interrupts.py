# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""How a helper process shuts down: a link lost mid-wait, an interrupt, a
parent that goes away, and a closed channel.

Everything here drives the helper's own halves against a stand-in parent, so
what the loop does is asserted directly rather than inferred from a running
graph.
"""

import threading
import time

import pytest

from helper_process_test_support import (
    PROBE_MODULE,
    drive_lifecycle_on_a_thread,
    engine_shaped_link_wiring,
)
from tatolab.runtime._node_interpreter_bootstrap import (
    ParentProcessBridge,
    load_node_class,
)

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)


def test_a_reactive_helper_survives_losing_the_link_it_was_waiting_on(stand_in_parent):
    """Unwiring a reactive node's LAST inbound link closes the very fd its
    loop is polling — the listener owns it, and dropping the last subscriber
    drops the listener.

    Fail-without-fix: keep waiting on the listener's fd once it is gone and
    every later wait reports it invalid at once, so the loop spins where it
    should park.

    A node left with no inputs has nothing to wake it but the parent,
    which is exactly what it must fall back to.
    """

    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:PassThroughProbe")
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
    cpu_seconds_before_the_wait = time.process_time()
    # The wait is load-bearing, not just the unanswered-command assertion: it
    # is what lets the loop go round again on the fd the unwire just closed.
    # Send `teardown` immediately instead and both commands drain in one batch,
    # the loop leaves before it ever waits again, and the bug hides.
    assert stand_in_parent.receive(timeout_seconds=0.5) is None
    cpu_seconds_in_the_wait = time.process_time() - cpu_seconds_before_the_wait
    assert cpu_seconds_in_the_wait < 0.25, (
        f"the helper spent {cpu_seconds_in_the_wait:.2f} s of CPU in half a second with "
        f"no input left — it must park on the parent, not spin"
    )

    stand_in_parent.send({"cmd": "on_pause", "capability": "limited"})
    assert stand_in_parent.receive() == {"rpc": "ok"}, (
        "the loop must survive losing the fd it was waiting on"
    )

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


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
        bridge, load_node_class(f"{PROBE_MODULE}:InterruptedInProcessProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive() == {"rpc": "ready"}
    stand_in_parent.send({"cmd": "run", "execution": "continuous", "interval_ms": 0})
    deadline = time.monotonic() + 5.0
    while "process-interrupted" not in hooks_the_interrupt_probes_reached:
        assert time.monotonic() < deadline, "process() never took the interrupt"
        time.sleep(0.01)

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
        bridge, load_node_class(f"{PROBE_MODULE}:InterruptedInSetupProbe")
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


def test_a_parent_gone_after_an_interrupted_setup_still_leaves_its_teardown_run(
    stand_in_parent, hooks_the_interrupt_probes_reached
):
    """A parent that goes away can no longer send the `teardown` an interrupted
    `setup()` is owed, so the helper runs it on reading the end of the channel."""
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:InterruptedInSetupProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive()["rpc"] == "error"
    stand_in_parent.parent_end.close()

    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()
    assert hooks_the_interrupt_probes_reached == ["setup-interrupted", "teardown"]


def test_a_parent_gone_after_a_setup_that_raised_leaves_no_teardown_run(
    stand_in_parent, hooks_the_interrupt_probes_reached
):
    """A `setup()` that raised on its own keeps its no-teardown rule when the
    parent goes away before sending anything else.

    Fail-without-fix: the end of the channel ran `stop()` and `teardown()` on a
    node that never set up.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:RaisesInSetupProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive()["rpc"] == "error"
    stand_in_parent.parent_end.close()

    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()
    assert hooks_the_interrupt_probes_reached == ["setup-raised"]


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
