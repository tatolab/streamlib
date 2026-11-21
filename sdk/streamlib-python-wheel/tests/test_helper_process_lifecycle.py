# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only


import json
import os
import resource
import statistics
import sys
import threading
import time

import pytest

from helper_process_test_support import (
    PROBE_MODULE,
    drive_lifecycle_on_a_thread,
    engine_shaped_link_wiring,
    frame_index_payload,
)
from tatolab.runtime import _engine
from tatolab.runtime import _node_interpreter_bootstrap as node_interpreter_bootstrap
from tatolab.runtime._function_node_run_as_a_standalone_node import (
    the_node_class_a_promoted_function_node_runs_as,
)
from tatolab.runtime._node_interpreter_bootstrap import (
    HelperProcessLifecycle,
    ParentProcessBridge,
    load_node_class,
)
from tatolab.stream import node

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)


def test_setup_answers_ready_and_run_answers_nothing_at_all(stand_in_parent):
    """A helper that replied to `run` would desynchronize every later command:
    the parent reads that reply as the answer to whatever it sends next."""
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:PassThroughProbe")
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


@node.function
def doubles_its_level(level: float = 0.5) -> float:
    return level * 2.0


@node.function
def labels_its_level(level: float = 0.5) -> dict:
    return {"label": "high" if level > 0.5 else "low"}


@pytest.mark.parametrize(
    ("function_node", "node_id", "ran_compiled"),
    [(doubles_its_level, "doubler-1", True), (labels_its_level, "labeller-1", False)],
)
def test_a_promoted_function_nodes_ready_says_how_its_own_helper_runs_it(
    stand_in_parent, function_node, node_id, ran_compiled
):
    """A function node promoted to a helper of its own is evaluated by no
    reader, so its own helper's `ready` is the only report of how it runs."""
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()

    def drive() -> None:
        link_data_access = _engine.open_node_link_data_access_for_helper_process()
        HelperProcessLifecycle(
            bridge,
            the_node_class_a_promoted_function_node_runs_as(
                function_node, f"{__name__}:{function_node.__name__}", node_id, link_data_access
            ),
            "R-helper-test",
            node_id,
            link_data_access,
        ).run_until_the_parent_is_done()

    lifecycle_thread = threading.Thread(target=drive, name="helper-lifecycle")
    lifecycle_thread.start()

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": None, "ports": {}})
    assert stand_in_parent.receive() == {
        "rpc": "ready",
        "function_nodes_ran_compiled": {node_id: ran_compiled},
    }

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


@pytest.mark.parametrize("evaluated_at_setup", [True, False])
def test_a_promoted_function_node_is_evaluated_as_it_sets_up_when_its_setup_command_says_so(
    stand_in_parent, evaluated_at_setup
):
    """`function_node_evaluated_at_setup` on the setup command has the helper call its
    promoted function node once before it answers `ready`, so a setting the node feeds is
    built with its value; without it the node is first called at its first run.

    Fail-without-fix: the helper ignores the flag, and the node is not called at its setup.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    calls_before_ready: "list[str]" = []

    def drive() -> None:
        link_data_access = _engine.open_node_link_data_access_for_helper_process()
        runner_class = the_node_class_a_promoted_function_node_runs_as(
            doubles_its_level, f"{__name__}:doubles_its_level", "doubler-1", link_data_access
        )
        process_of_the_runner = runner_class.process

        def process_counting_its_calls(runner, ctx) -> None:
            calls_before_ready.append("process")
            process_of_the_runner(runner, ctx)

        runner_class.process = process_counting_its_calls
        HelperProcessLifecycle(
            bridge, runner_class, "R-helper-test", "doubler-1", link_data_access
        ).run_until_the_parent_is_done()

    lifecycle_thread = threading.Thread(target=drive, name="helper-lifecycle")
    lifecycle_thread.start()

    stand_in_parent.send(
        {
            "cmd": "setup",
            "capability": "full",
            "config": None,
            "ports": {},
            "function_node_evaluated_at_setup": evaluated_at_setup,
        }
    )
    assert stand_in_parent.receive()["rpc"] == "ready"
    assert calls_before_ready == (["process"] if evaluated_at_setup else [])

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_node_that_cannot_set_itself_up_reports_the_failure(stand_in_parent):
    """`setup` is the one hook whose failure the parent is blocked on, so it
    must arrive as an error rather than be logged and answered `ready`."""
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:RefusesSetupProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    refusal = stand_in_parent.receive()
    assert refusal["rpc"] == "error"
    assert "cannot set itself up" in refusal["error"]

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)


def test_a_manual_node_whose_start_raised_fails_its_node(
    stand_in_parent, log_records_this_process_sends_its_stand_in_parent
):
    """`run` is unanswered, so a `start()` that raised reaches the parent as the
    node failing itself, carrying what `start()` raised.

    Fail-without-fix: the failure is only logged, no `fail_the_node` arrives,
    and the graph shows the node `Running`.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:RaisesInStartProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive() == {"rpc": "ready"}

    stand_in_parent.send({"cmd": "run", "execution": "manual", "interval_ms": 0})
    failure = stand_in_parent.receive()
    assert failure is not None, "the helper never failed its node"
    assert failure["rpc"] == "escalate_request"
    assert failure["op"] == "fail_the_node"
    assert failure["reason"] == 'start() raised: could not read "lower_third.riv"'
    stand_in_parent.send(
        {"rpc": "escalate_response", "request_id": failure["request_id"], "result": "ok"}
    )
    assert any(
        level == "error" and message == "start() raised"
        for level, message, _ in log_records_this_process_sends_its_stand_in_parent
    )

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_manual_node_not_ready_from_start_is_reported_and_its_config_update_reports_it_ready(
    stand_in_parent,
):
    """A `start()` that reports the node not ready sends the parent the reason
    in place of failing the node, and the node keeps running; a config update
    that reports it ready sends `null`.

    Fail-without-fix: no `set_not_ready_because` arrives and the graph shows
    nothing on the node.
    """
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:NotReadyInStartProbe")
    )

    def the_next_frame_other_than_a_log() -> dict:
        while True:
            frame = stand_in_parent.receive()
            assert frame is not None, "the helper sent the parent nothing"
            if frame.get("op") != "log":
                return frame

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert the_next_frame_other_than_a_log() == {"rpc": "ready"}
    stand_in_parent.send({"cmd": "run", "execution": "manual", "interval_ms": 0})

    not_ready = the_next_frame_other_than_a_log()
    assert not_ready["rpc"] == "escalate_request"
    assert not_ready["op"] == "set_not_ready_because"
    assert not_ready["not_ready_because"] == "`riv_path` is not set"
    stand_in_parent.send(
        {"rpc": "escalate_response", "request_id": not_ready["request_id"], "result": "ok"}
    )

    stand_in_parent.send({"cmd": "update_config", "config": {"gain": 2}})
    assert the_next_frame_other_than_a_log() == {"rpc": "ok"}
    ready = the_next_frame_other_than_a_log()
    assert ready["op"] == "set_not_ready_because"
    assert ready["not_ready_because"] is None
    stand_in_parent.send(
        {"rpc": "escalate_response", "request_id": ready["request_id"], "result": "ok"}
    )

    stand_in_parent.send({"cmd": "stop", "capability": "full"})
    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    answers = [the_next_frame_other_than_a_log()["rpc"] for _ in range(2)]
    assert answers == ["stopped", "done"]
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_running_manual_node_reported_from_another_thread_is_fed_transparent_frames_on_a_beat(
    stand_in_parent, monkeypatch
):
    """A manual node the runtime never calls is told of, and fed, from the wait
    for the next lifecycle command: a report from a thread of the node's own
    reaches the parent, its Image outputs are sent a frame on a steady beat
    while it is not ready, and none once it reports itself ready.

    Fail-without-fix: the wait blocks until the next command, so the report
    never reaches the parent and no frame is sent.
    """
    from helper_process_probes import CONTEXT_THE_REPORTED_FROM_ANOTHER_THREAD_PROBE_KEPT

    CONTEXT_THE_REPORTED_FROM_ANOTHER_THREAD_PROBE_KEPT.clear()
    transparent_frame_sends: "list[tuple[int, int]]" = []
    monkeypatch.setattr(
        node_interpreter_bootstrap.TransparentFramesOnTheImageOutputsOfANodeNotReady,
        "send_one_transparent_frame_on_each_image_output",
        lambda self, context, image_width, image_height: transparent_frame_sends.append(
            (image_width, image_height)
        ),
    )
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:ReportedFromAnotherThreadProbe")
    )

    def the_next_frame_other_than_a_log() -> dict:
        while True:
            frame = stand_in_parent.receive()
            assert frame is not None, "the helper sent the parent nothing"
            if frame.get("op") != "log":
                return frame

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert the_next_frame_other_than_a_log() == {"rpc": "ready"}
    stand_in_parent.send({"cmd": "run", "execution": "manual", "interval_ms": 0})
    deadline = time.monotonic() + 5.0
    while not CONTEXT_THE_REPORTED_FROM_ANOTHER_THREAD_PROBE_KEPT and time.monotonic() < deadline:
        time.sleep(0.01)
    context_of_the_node = CONTEXT_THE_REPORTED_FROM_ANOTHER_THREAD_PROBE_KEPT[0]
    assert transparent_frame_sends == [], "a node that is ready is sent no placeholder frame"

    context_of_the_node.report_not_ready(
        "the image asset `poster.png` is missing", image_width=64, image_height=32
    )
    not_ready = the_next_frame_other_than_a_log()
    assert not_ready["op"] == "set_not_ready_because"
    assert not_ready["not_ready_because"] == "the image asset `poster.png` is missing"
    stand_in_parent.send(
        {"rpc": "escalate_response", "request_id": not_ready["request_id"], "result": "ok"}
    )

    sends_when_reported = len(transparent_frame_sends)
    time.sleep(0.3)
    sends_over_the_wait = len(transparent_frame_sends) - sends_when_reported
    assert sends_over_the_wait >= 3, (
        f"a not-ready manual node was sent {sends_over_the_wait} frames in 0.3 s"
    )
    assert set(transparent_frame_sends) == {(64, 32)}

    context_of_the_node.report_ready()
    ready = the_next_frame_other_than_a_log()
    assert ready["op"] == "set_not_ready_because"
    assert ready["not_ready_because"] is None
    stand_in_parent.send(
        {"rpc": "escalate_response", "request_id": ready["request_id"], "result": "ok"}
    )
    time.sleep(0.05)
    sends_once_ready = len(transparent_frame_sends)
    time.sleep(0.2)
    assert len(transparent_frame_sends) == sends_once_ready, (
        "a node that reported itself ready is still sent placeholder frames"
    )

    stand_in_parent.send({"cmd": "stop", "capability": "full"})
    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    answers = [the_next_frame_other_than_a_log()["rpc"] for _ in range(2)]
    assert answers == ["stopped", "done"]
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_continuous_node_not_ready_is_reported_once_and_still_processed_until_it_reports_ready(
    stand_in_parent,
):
    """The first `process()` reporting the node not ready sends the reason once,
    later ones send nothing, `process()` keeps being called, and the first
    call reporting it ready sends `null`.

    Fail-without-fix: no `set_not_ready_because` arrives, or `process()` stops
    being called and the node can never report itself ready.
    """
    from helper_process_probes import (
        NOT_READY_UNTIL_TOLD_OTHERWISE_PROBE_IS_NOT_READY,
        PROCESS_CALLS_OF_THE_NOT_READY_UNTIL_TOLD_OTHERWISE_PROBE,
    )

    NOT_READY_UNTIL_TOLD_OTHERWISE_PROBE_IS_NOT_READY[0] = True
    PROCESS_CALLS_OF_THE_NOT_READY_UNTIL_TOLD_OTHERWISE_PROBE[0] = 0
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:NotReadyUntilToldOtherwiseProbe")
    )
    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive() == {"rpc": "ready"}
    stand_in_parent.send({"cmd": "run", "execution": "continuous", "interval_ms": 20})

    def the_next_escalate_request_other_than_a_log() -> dict:
        while True:
            frame = stand_in_parent.receive()
            assert frame is not None, "the helper sent the parent nothing"
            if frame.get("op") != "log":
                return frame

    not_ready = the_next_escalate_request_other_than_a_log()
    assert not_ready["op"] == "set_not_ready_because"
    assert not_ready["not_ready_because"] == "the image asset `poster.png` is missing"
    stand_in_parent.send(
        {"rpc": "escalate_response", "request_id": not_ready["request_id"], "result": "ok"}
    )

    calls_when_reported = PROCESS_CALLS_OF_THE_NOT_READY_UNTIL_TOLD_OTHERWISE_PROBE[0]
    deadline = time.monotonic() + 0.3
    while time.monotonic() < deadline:
        frame = stand_in_parent.receive(timeout_seconds=max(0.0, deadline - time.monotonic()))
        assert frame is None or frame.get("op") == "log", (
            f"a not-ready report after the first sent {frame}"
        )
    assert PROCESS_CALLS_OF_THE_NOT_READY_UNTIL_TOLD_OTHERWISE_PROBE[0] > calls_when_reported, (
        "process() is still called while the node is not ready"
    )

    NOT_READY_UNTIL_TOLD_OTHERWISE_PROBE_IS_NOT_READY[0] = False
    ready = the_next_escalate_request_other_than_a_log()
    assert ready["op"] == "set_not_ready_because"
    assert ready["not_ready_because"] is None
    stand_in_parent.send(
        {"rpc": "escalate_response", "request_id": ready["request_id"], "result": "ok"}
    )

    stand_in_parent.send({"cmd": "stop", "capability": "full"})
    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    answers = [the_next_escalate_request_other_than_a_log()["rpc"] for _ in range(2)]
    assert answers == ["stopped", "done"]
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_process_that_keeps_raising_is_reported_once_and_cleared_once_it_runs_cleanly(
    stand_in_parent,
):
    from helper_process_probes import RAISES_UNTIL_TOLD_NOT_TO_PROBE_RAISES

    RAISES_UNTIL_TOLD_NOT_TO_PROBE_RAISES[0] = True
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:RaisesUntilToldNotToProbe")
    )
    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive() == {"rpc": "ready"}
    stand_in_parent.send({"cmd": "run", "execution": "continuous", "interval_ms": 20})

    def the_next_escalate_request_other_than_a_log() -> dict:
        while True:
            frame = stand_in_parent.receive()
            assert frame is not None, "the helper sent the parent nothing"
            if frame.get("op") != "log":
                return frame

    raising = the_next_escalate_request_other_than_a_log()
    assert raising["rpc"] == "escalate_request"
    assert raising["op"] == "set_process_raising_because"
    assert raising["raising_because"].startswith("process() raised RuntimeError: broke on purpose (")
    assert "helper_process_probes.py, line " in raising["raising_because"]
    stand_in_parent.send(
        {"rpc": "escalate_response", "request_id": raising["request_id"], "result": "ok"}
    )

    deadline = time.monotonic() + 0.3
    while time.monotonic() < deadline:
        frame = stand_in_parent.receive(timeout_seconds=max(0.0, deadline - time.monotonic()))
        assert frame is None or frame.get("op") == "log", (
            f"a raise after the first of the run sent {frame}"
        )

    RAISES_UNTIL_TOLD_NOT_TO_PROBE_RAISES[0] = False
    ran_cleanly = the_next_escalate_request_other_than_a_log()
    assert ran_cleanly["op"] == "set_process_raising_because"
    assert ran_cleanly["raising_because"] is None
    stand_in_parent.send(
        {"rpc": "escalate_response", "request_id": ran_cleanly["request_id"], "result": "ok"}
    )

    stand_in_parent.send({"cmd": "stop", "capability": "full"})
    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    answers = [the_next_escalate_request_other_than_a_log()["rpc"] for _ in range(2)]
    assert answers == ["stopped", "done"]
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_function_node_raising_in_the_read_of_a_node_is_reported_on_the_function_node(
    stand_in_parent, log_records_this_process_sends_its_stand_in_parent
):
    """A function node whose call raises inside its reader's read is reported
    on the function node, once per run of raises, while the reader runs
    cleanly; a clean call clears it.

    Fail-without-fix: the raise reaches the reader's `process()`, which sends
    `set_process_raising_because` naming the reader, and nothing names the
    function node.
    """
    from function_node_test_nodes import an_island_of_one_node

    def an_island_feeding_the_level_its_pin_sets(level: float) -> dict:
        island = an_island_of_one_node(
            "level",
            f"{PROBE_MODULE}:builds_an_object_then_answers_its_level_raising_on_a_negative_one",
            None,
            "beat-1",
        )
        island["nodes"][0]["pins"] = {"level": level}
        return island

    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:ReadsTheIslandFeedingItsLevelProbe")
    )
    stand_in_parent.send(
        {
            "cmd": "setup",
            "capability": "full",
            "config": {},
            "ports": {"islands": [an_island_feeding_the_level_its_pin_sets(-1.0)]},
        }
    )
    assert stand_in_parent.receive() == {"rpc": "ready", "function_nodes_ran_compiled": {"beat-1": False}}
    stand_in_parent.send({"cmd": "run", "execution": "continuous", "interval_ms": 20})

    def the_next_escalate_request_other_than_a_log() -> dict:
        while True:
            frame = stand_in_parent.receive()
            assert frame is not None, "the helper sent the parent nothing"
            if frame.get("op") != "log":
                return frame

    raising = the_next_escalate_request_other_than_a_log()
    assert raising["op"] == "set_function_node_raising_because", raising
    assert raising["function_node"] == "beat-1"
    assert raising["raising_because"].startswith(
        "raised RuntimeError: the function node broke on purpose ("
    )
    assert "helper_process_probes.py, line " in raising["raising_because"]
    stand_in_parent.send(
        {"rpc": "escalate_response", "request_id": raising["request_id"], "result": "ok"}
    )

    deadline = time.monotonic() + 0.3
    while time.monotonic() < deadline:
        frame = stand_in_parent.receive(timeout_seconds=max(0.0, deadline - time.monotonic()))
        assert frame is None or frame.get("op") == "log", (
            f"a raise after the first of the run, or the reader's process(), sent {frame}"
        )

    logs_naming_the_function_node = [
        (level, attrs)
        for level, _, attrs in log_records_this_process_sends_its_stand_in_parent
        if (attrs or {}).get("function_node") == "beat-1"
    ]
    assert len(logs_naming_the_function_node) == 1, (
        f"the first raise of the run is logged once: {logs_naming_the_function_node}"
    )
    ((logged_level, logged_attrs),) = logs_naming_the_function_node
    assert logged_level == "error"
    assert logged_attrs["error"] == raising["raising_because"]
    assert "Traceback (most recent call last)" in logged_attrs["traceback"]
    assert "RuntimeError: the function node broke on purpose" in logged_attrs["traceback"]

    stand_in_parent.send(
        {"cmd": "feed_island", "island": an_island_feeding_the_level_its_pin_sets(0.5)}
    )
    assert the_next_escalate_request_other_than_a_log() == {
        "rpc": "link_wired",
        "link_id": "L-level",
    }
    ran_cleanly = the_next_escalate_request_other_than_a_log()
    assert ran_cleanly["op"] == "set_function_node_raising_because"
    assert ran_cleanly["function_node"] == "beat-1"
    assert ran_cleanly["raising_because"] is None
    stand_in_parent.send(
        {"rpc": "escalate_response", "request_id": ran_cleanly["request_id"], "result": "ok"}
    )

    stand_in_parent.send({"cmd": "stop", "capability": "full"})
    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    answers = [the_next_escalate_request_other_than_a_log()["rpc"] for _ in range(2)]
    assert answers == ["stopped", "done"]
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_raise_report_the_parent_refused_is_sent_again_on_the_next_call(stand_in_parent):
    """A refused `set_process_raising_because` leaves the parent's record
    unknown, so the next raise sends it again rather than nothing.

    Fail-without-fix: the helper counts the refused report as delivered, the
    raises after it send nothing, and the graph never shows the node raising.
    """
    from helper_process_probes import RAISES_UNTIL_TOLD_NOT_TO_PROBE_RAISES

    RAISES_UNTIL_TOLD_NOT_TO_PROBE_RAISES[0] = True
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:RaisesUntilToldNotToProbe")
    )
    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive() == {"rpc": "ready"}
    stand_in_parent.send({"cmd": "run", "execution": "continuous", "interval_ms": 20})

    def the_next_frame_other_than_a_log() -> dict:
        while True:
            frame = stand_in_parent.receive()
            assert frame is not None, "the helper sent the parent nothing"
            if frame.get("op") != "log":
                return frame

    refused = the_next_frame_other_than_a_log()
    assert refused["op"] == "set_process_raising_because"
    stand_in_parent.send(
        {
            "rpc": "escalate_response",
            "request_id": refused["request_id"],
            "result": "error",
            "message": "the escalate queue is full",
        }
    )

    sent_again = the_next_frame_other_than_a_log()
    assert sent_again["op"] == "set_process_raising_because"
    assert sent_again["raising_because"].startswith("process() raised RuntimeError: broke on purpose (")
    stand_in_parent.send(
        {"rpc": "escalate_response", "request_id": sent_again["request_id"], "result": "ok"}
    )

    stand_in_parent.send({"cmd": "stop", "capability": "full"})
    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    answers = [the_next_frame_other_than_a_log()["rpc"] for _ in range(2)]
    assert answers == ["stopped", "done"]
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()
    RAISES_UNTIL_TOLD_NOT_TO_PROBE_RAISES[0] = False


def test_a_clean_call_after_a_refused_raise_report_tells_the_parent_it_runs_cleanly(stand_in_parent):
    """A raise report the parent refused may or may not have been applied, so
    the clean call after it sends `null` rather than nothing.

    Fail-without-fix: the helper keeps its record of the parent as clean, the
    clean call sends nothing, and a report applied despite its error answer
    stays on the graph.
    """
    from helper_process_probes import RAISES_UNTIL_TOLD_NOT_TO_PROBE_RAISES

    RAISES_UNTIL_TOLD_NOT_TO_PROBE_RAISES[0] = True
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:RaisesUntilToldNotToProbe")
    )
    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive() == {"rpc": "ready"}
    stand_in_parent.send({"cmd": "run", "execution": "continuous", "interval_ms": 20})

    def the_next_frame_other_than_a_log() -> dict:
        while True:
            frame = stand_in_parent.receive()
            assert frame is not None, "the helper sent the parent nothing"
            if frame.get("op") != "log":
                return frame

    refused = the_next_frame_other_than_a_log()
    assert refused["op"] == "set_process_raising_because"
    RAISES_UNTIL_TOLD_NOT_TO_PROBE_RAISES[0] = False
    stand_in_parent.send(
        {
            "rpc": "escalate_response",
            "request_id": refused["request_id"],
            "result": "error",
            "message": "the escalate queue is full",
        }
    )

    ran_cleanly = the_next_frame_other_than_a_log()
    assert ran_cleanly["op"] == "set_process_raising_because"
    assert ran_cleanly["raising_because"] is None
    stand_in_parent.send(
        {"rpc": "escalate_response", "request_id": ran_cleanly["request_id"], "result": "ok"}
    )

    stand_in_parent.send({"cmd": "stop", "capability": "full"})
    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    answers = [the_next_frame_other_than_a_log()["rpc"] for _ in range(2)]
    assert answers == ["stopped", "done"]
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


@pytest.mark.skipif(sys.platform == "linux", reason="Linux adopts the fd rather than refusing")
def test_an_fd_shaped_raw_handle_refuses_by_name_off_linux(stand_in_parent):
    """The method exists on every floor, so the stub is one, and off Linux it
    names the Linux handle it cannot give and its IOSurface peer."""
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:ImportsADmaBufOffLinuxProbe")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    refusal = stand_in_parent.receive()
    assert refusal["rpc"] == "error"
    assert "import_dma_buf is Linux-only" in refusal["error"]
    assert "export_iosurface" in refusal["error"]

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)


def test_pause_and_resume_are_answered_and_tracked_without_an_engine(stand_in_parent):
    """`ctx.is_paused()` reads a leased engine view in the parent; a child has
    none, so it answers from what the parent last announced."""
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:PassThroughProbe")
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


def test_an_idle_reactive_helper_answers_a_command_the_moment_it_arrives(stand_in_parent):
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:PassThroughProbe")
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
            bridge, load_node_class(f"{PROBE_MODULE}:PassThroughProbe")
        )

        inbound_link_id = "L-high-fd-in"
        outbound_link_id = "L-high-fd-out"
        downstream = _engine.open_node_link_data_access_for_helper_process()
        node_interpreter_bootstrap.wire_link_data_access(
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

        upstream = _engine.open_node_link_data_access_for_helper_process()
        node_interpreter_bootstrap.wire_link_data_access(
            upstream,
            {"outputs": [engine_shaped_link_wiring("output", inbound_link_id)]},
        )
        # The helper is parked on its listener by now, so the message arrives
        # by waking the wait.
        time.sleep(0.2)
        upstream.write_to_output_port("frames_to_downstream", frame_index_payload(3))
        deadline = time.monotonic() + 5.0
        forwarded = None
        while forwarded is None and time.monotonic() < deadline:
            if downstream.any_input_port_has_data():
                forwarded = downstream.read_from_input_port("frames_from_upstream")
            else:
                time.sleep(0.01)
        assert forwarded is not None
        assert json.loads(bytes(forwarded)) == {"frame_index": 3, "tag": "untagged"}, (
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
        bridge, load_node_class(f"{PROBE_MODULE}:PassThroughProbe")
    )

    stand_in_parent.send({"cmd": "reticulate_splines"})
    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


THE_ENGINES_CONTINUOUS_INTERVAL_FLOOR_NANOSECONDS = 100_000


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
        bridge, load_node_class(f"{PROBE_MODULE}:ContinuousPacingProbe")
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


def test_a_continuous_node_runs_once_per_interval_longer_than_the_command_wait(
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


def test_a_paused_continuous_node_is_not_called_until_it_resumes(
    stand_in_parent, when_the_pacing_probe_processed_ns
):
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:ContinuousPacingProbe")
    )
    stand_in_parent.send({"cmd": "setup", "capability": "full", "config": {}, "ports": {}})
    assert stand_in_parent.receive() == {"rpc": "ready"}
    stand_in_parent.send({"cmd": "run", "execution": "continuous", "interval_ms": 20})
    time.sleep(0.2)

    stand_in_parent.send({"cmd": "on_pause", "capability": "limited"})
    assert stand_in_parent.receive() == {"rpc": "ok"}
    calls_when_paused = len(when_the_pacing_probe_processed_ns)
    time.sleep(0.3)
    calls_while_paused = len(when_the_pacing_probe_processed_ns) - calls_when_paused

    stand_in_parent.send({"cmd": "on_resume", "capability": "limited"})
    assert stand_in_parent.receive() == {"rpc": "ok"}
    calls_when_resumed = len(when_the_pacing_probe_processed_ns)
    time.sleep(0.2)
    calls_after_resuming = len(when_the_pacing_probe_processed_ns) - calls_when_resumed

    stand_in_parent.send({"cmd": "stop", "capability": "full"})
    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "stopped"
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()

    assert calls_when_paused > 0, "process() never ran before the pause"
    assert calls_while_paused == 0, f"process() ran {calls_while_paused} times while paused"
    assert calls_after_resuming > 0, "process() never ran after the resume"


def test_a_continuous_node_runs_at_the_start_rather_than_one_interval_in(
    stand_in_parent, when_the_pacing_probe_processed_ns
):
    run_sent_ns = run_the_pacing_probe(
        stand_in_parent, interval_ms=1000, running_seconds=0.6
    )

    assert when_the_pacing_probe_processed_ns, "process() never ran"
    first_call_after_run_ms = (when_the_pacing_probe_processed_ns[0] - run_sent_ns) / 1e6
    assert first_call_after_run_ms < 500, (
        f"the first process() came {first_call_after_run_ms:.0f} ms after run; a "
        f"continuous node runs at the start"
    )


def test_a_continuous_node_with_no_interval_never_runs_faster_than_the_floor(
    stand_in_parent, when_the_pacing_probe_processed_ns
):
    run_the_pacing_probe(stand_in_parent, interval_ms=0, running_seconds=0.5)

    process_calls = len(when_the_pacing_probe_processed_ns)
    assert process_calls >= 2, "the zero-interval loop never ran the node"
    measured_span_seconds = (
        when_the_pacing_probe_processed_ns[-1] - when_the_pacing_probe_processed_ns[0]
    ) / 1_000_000_000
    calls_per_second = (process_calls - 1) / measured_span_seconds
    floor_calls_per_second = (
        1_000_000_000 / THE_ENGINES_CONTINUOUS_INTERVAL_FLOOR_NANOSECONDS
    )
    assert calls_per_second <= floor_calls_per_second * 1.2, (
        f"a zero interval ran process() {calls_per_second:.0f} times a second; the "
        f"floor allows {floor_calls_per_second:.0f}"
    )


def reconfigure_a_set_up_probe(stand_in_parent, probe_name: str, configuration: dict) -> dict:
    """Set `probe_name` up, hand it `configuration`, and return the answer."""
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:{probe_name}")
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


def test_a_configuration_the_node_takes_is_answered_ok(stand_in_parent):
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
def test_a_configuration_the_node_does_not_take_is_refused_naming_the_cause(
    stand_in_parent, probe_name, the_cause_named
):
    answer = reconfigure_a_set_up_probe(stand_in_parent, probe_name, {"gain": 3})
    assert answer["rpc"] == "error"
    assert the_cause_named in answer["error"]
