# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A node written as one function runs in its helper: the body once at setup, the function it
returned each frame; a value set live rebuilds only the computed value that read it, and
stopping the node closes every value its builds made. A node that paces itself returns no
per-frame function, and takes a value set live or given by its wire at its next tick."""

from __future__ import annotations

import array
import os
import time
from typing import Any

import pytest

from helper_process_test_support import drive_lifecycle_on_a_thread
from tatolab.runtime import _engine
from tatolab.runtime._hosted_node import wire_link_data_access
from tatolab.runtime._node_interpreter_bootstrap import ParentProcessBridge, load_node_class
from tatolab.runtime._node_the_wheel_hosts import link_output_data_writer_of

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)

PROBE_MODULE = "node_function_probes"


def what_the_probe_did() -> "list[tuple[str, object]]":
    from node_function_probes import WHAT_THE_LEVEL_READING_PROBE_DID

    return list(WHAT_THE_LEVEL_READING_PROBE_DID)


def wait_until_the_probe_framed(seen: "tuple[float, float]", timeout_s: float = 5.0) -> None:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if what_the_probe_did()[-1:] == [("frame", seen)]:
            return
        time.sleep(0.01)
    raise AssertionError(f"the probe never framed {seen!r}; it did: {what_the_probe_did()[-5:]}")


def done_but_frames(did: "list[tuple[str, object]]") -> "list[tuple[str, object]]":
    return [entry for entry in did if entry[0] != "frame"]


def start_the_level_reading_probe(stand_in_parent: Any) -> Any:
    from node_function_probes import WHAT_THE_LEVEL_READING_PROBE_DID

    WHAT_THE_LEVEL_READING_PROBE_DID.clear()
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:LevelReadingProbe")
    )
    stand_in_parent.send(
        {"cmd": "setup", "capability": "full", "ports": {"input_values": {"level": 0.5}}}
    )
    assert stand_in_parent.receive() == {"rpc": "ready"}
    stand_in_parent.send({"cmd": "run", "execution": "continuous", "interval_ms": 5})
    wait_until_the_probe_framed((0.5, 1.0))
    return lifecycle_thread


def test_a_value_set_live_rebuilds_only_the_computed_value_that_read_it(stand_in_parent: Any):
    """Mental-revert: run the body at each `set_input_values`, or rebuild every computed value
    at every write, and the body or the reading is recorded twice for the gain set."""
    lifecycle_thread = start_the_level_reading_probe(stand_in_parent)

    stand_in_parent.send({"cmd": "set_input_values", "input_values": {"level": 0.75}})
    wait_until_the_probe_framed((0.75, 1.0))
    stand_in_parent.send(
        {"cmd": "set_input_values", "input_values": {"level": 0.75, "gain": 2.0}}
    )
    wait_until_the_probe_framed((0.75, 2.0))

    assert done_but_frames(what_the_probe_did()) == [
        ("body", None),
        ("built", 0.5),
        ("built", 0.75),
        ("closed", 0.5),
    ]
    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_stopping_the_node_closes_every_value_its_builds_made(stand_in_parent: Any):
    """Mental-revert: leave `stop` and `teardown` to the hooks a class defines, and the reading
    of 0.5 is never closed."""
    lifecycle_thread = start_the_level_reading_probe(stand_in_parent)

    stand_in_parent.send({"cmd": "stop", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "stopped"

    assert done_but_frames(what_the_probe_did()) == [
        ("body", None),
        ("built", 0.5),
        ("closed", 0.5),
    ]
    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    assert done_but_frames(what_the_probe_did())[-1] == ("closed", 0.5)
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


@pytest.mark.parametrize(
    ("probe", "refusal"),
    [
        ("ReadsItsLevelInItsBodyProbe", "NodeScopeReadInTheBody"),
        ("ReturnsANumberProbe", "NodeFunctionBodyReturnedNoPerFrameFunction"),
        (
            "PacesItselfAndReturnsAFunctionProbe",
            "NodeFunctionPacingItselfReturnedAValue",
        ),
        (
            "PacesItselfAndReturnsANumberProbe",
            "NodeFunctionPacingItselfReturnedAValue",
        ),
    ],
)
def test_a_body_that_reads_an_input_or_returns_no_function_fails_its_setup(
    stand_in_parent: Any, probe: str, refusal: str
):
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:{probe}")
    )

    stand_in_parent.send({"cmd": "setup", "capability": "full", "ports": {}})
    answer = stand_in_parent.receive()
    assert answer["rpc"] == "error"
    assert refusal in answer["error"]

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)


def test_a_node_function_is_described_as_a_process_node_of_the_ports_it_declares():
    """Mental-revert: describe only a class or a function node, and the runtime refuses the node
    function as naming neither."""
    from node_function_probes import LevelReadingProbe
    from tatolab.runtime._node_class_description import describe_one_node_class

    described = describe_one_node_class(f"{PROBE_MODULE}:LevelReadingProbe")

    assert (described["kind"], described["short_name"]) == ("process", "LevelReadingProbe")
    assert described["execution"] == {"mode": "continuous", "interval_ms": 5}
    assert described["input_ports"] == getattr(LevelReadingProbe, "__tatolab_node_input_ports__")
    assert [port["name"] for port in described["input_ports"]] == ["level", "gain"]
    assert described["output_ports"] == []


def test_a_value_for_no_input_and_a_second_setup_are_refused_by_their_own_kinds():
    from node_function_probes import LevelReadingProbe
    from tatolab.runtime._node_function_running import (
        NodeFunctionBodyRanAgain,
        NodeFunctionInputNotDeclared,
        NodeFunctionRunning,
    )

    running = NodeFunctionRunning(LevelReadingProbe, values_to_build_with={}, pins={})
    with pytest.raises(NodeFunctionInputNotDeclared, match="declares no value input `volume`"):
        running.write_inputs({"volume": 1.0})
    running.setup(object())
    try:
        with pytest.raises(NodeFunctionBodyRanAgain, match="body has already run"):
            running.setup(object())
    finally:
        running.teardown(object())


def what_the_publishing_probe_did() -> "list[tuple[str, float]]":
    from node_function_probes import WHAT_THE_LEVEL_PUBLISHING_PROBE_DID

    return list(WHAT_THE_LEVEL_PUBLISHING_PROBE_DID)


def wait_until_the_publishing_probe_did(done: "tuple[str, float]", timeout_s: float = 5.0) -> None:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if done in what_the_publishing_probe_did():
            return
        time.sleep(0.01)
    raise AssertionError(
        f"the publishing probe never did {done!r}; it did: {what_the_publishing_probe_did()}"
    )


def the_wiring_of_a_link_into_the_level(link_id: str, direction: str) -> "dict[str, Any]":
    """One end of a link into the publishing probe's `level`, as the engine's wiring path hands
    it a helper."""
    channel_service_name = f"pacing{os.getpid()}_{link_id}/channel"
    shared = {
        "link_id": link_id,
        "channel_service_name": channel_service_name,
        "channel_service_creation_depth": 16,
        "max_subscribers": 2,
        "notify_max_notifiers": 1,
    }
    notify_service_name = f"pacing{os.getpid()}_{link_id}_dest/notify"
    if direction == "output":
        return {
            **shared,
            "name": "level_to_the_probe",
            "dest_notify_service_name": notify_service_name,
            "expected_payload_bytes": 1024,
            "max_payload_bytes_per_channel": 1 << 20,
            "output_port_wiring_generation": 1,
        }
    return {
        **shared,
        "name": "level",
        "inbound_link_name": channel_service_name,
        "notify_service_name": notify_service_name,
        "read_mode": "read_next_in_order",
        "input_port_ring_depth": 16,
        "loss_count_slot": 0,
        "wiring_generation": 1,
    }


def start_the_level_publishing_probe(stand_in_parent: Any, ports: "dict[str, Any]") -> Any:
    """The thread driving the probe, set up with `ports` and running on its own clock, its level
    at 0.5, once it opened its first publishing."""
    from node_function_probes import WHAT_THE_LEVEL_PUBLISHING_PROBE_DID

    WHAT_THE_LEVEL_PUBLISHING_PROBE_DID.clear()
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:LevelPublishingProbe")
    )
    stand_in_parent.send(
        {"cmd": "setup", "capability": "full", "ports": {**ports, "input_values": {"level": 0.5}}}
    )
    assert stand_in_parent.receive() == {"rpc": "ready"}
    stand_in_parent.send({"cmd": "run", "execution": "manual", "interval_ms": 0})
    wait_until_the_publishing_probe_did(("opened", 0.5))
    return lifecycle_thread


def stop_and_tear_down(stand_in_parent: Any, lifecycle_thread: Any) -> None:
    stand_in_parent.send({"cmd": "stop", "capability": "full"})
    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    answers = []
    while len(answers) < 2:
        frame = stand_in_parent.receive()
        assert frame is not None, "the helper did not answer stop and teardown"
        if frame.get("rpc") == "escalate_request":
            stand_in_parent.send(
                {"rpc": "escalate_response", "request_id": frame["request_id"], "result": "ok"}
            )
        elif frame.get("op") != "log":
            answers.append(frame["rpc"])
    assert answers == ["stopped", "done"]
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_node_pacing_itself_takes_a_value_set_live_at_its_next_tick(stand_in_parent: Any):
    """Mental-revert: drop the node function's arm from `_set_input_values`, and 0.75 never
    reaches the scope, so no publishing of it opens and the publishing of 0.5 is never closed."""
    lifecycle_thread = start_the_level_publishing_probe(stand_in_parent, {})

    stand_in_parent.send({"cmd": "set_input_values", "input_values": {"level": 0.75}})
    wait_until_the_publishing_probe_did(("closed", 0.5))

    assert what_the_publishing_probe_did() == [
        ("opened", 0.5),
        ("opened", 0.75),
        ("closed", 0.5),
    ]
    stop_and_tear_down(stand_in_parent, lifecycle_thread)
    assert what_the_publishing_probe_did()[-1] == ("closed", 0.75)


def test_a_node_pacing_itself_takes_a_value_its_wire_gives_at_its_next_tick(stand_in_parent: Any):
    """Mental-revert: leave the manual tick reading no wire, and 0.9 is never written to the
    scope, so no publishing of it opens."""
    link_id = f"L-level-{os.getpid()}"
    lifecycle_thread = start_the_level_publishing_probe(
        stand_in_parent, {"inputs": [the_wiring_of_a_link_into_the_level(link_id, "input")]}
    )
    source = _engine.open_node_link_data_access_for_helper_process()
    wire_link_data_access(
        source, {"outputs": [the_wiring_of_a_link_into_the_level(link_id, "output")]}
    )
    source_outputs = link_output_data_writer_of(
        _engine.open_runtime_context_full_access_for_helper_process(
            source, "R-helper-test", "source-of-the-level"
        )
    )

    level = array.array("d", [0.9])
    source_outputs.write_channels_block(
        "level_to_the_probe",
        memoryview(level).cast("B").cast("d", [1, 1]),
        start_sample=0,
        sample_rate=0,
    )
    wait_until_the_publishing_probe_did(("closed", 0.5))

    assert what_the_publishing_probe_did() == [
        ("opened", 0.5),
        ("opened", 0.9),
        ("closed", 0.5),
    ]
    stop_and_tear_down(stand_in_parent, lifecycle_thread)


def test_a_node_pacing_itself_answers_its_parent_while_a_build_is_held(stand_in_parent: Any):
    """Mental-revert: a tick that waited for the scope to settle would answer no lifecycle
    command while the build of 0.75 is held, so `on_pause` would go unanswered."""
    from node_function_probes import PUBLISHING_BUILDS_LET_GO

    lifecycle_thread = start_the_level_publishing_probe(stand_in_parent, {})
    PUBLISHING_BUILDS_LET_GO.clear()
    try:
        stand_in_parent.send({"cmd": "set_input_values", "input_values": {"level": 0.75}})
        time.sleep(0.2)
        stand_in_parent.send({"cmd": "on_pause"})
        answer = stand_in_parent.receive(timeout_seconds=1.0)
        while answer is not None and answer.get("op") == "log":
            answer = stand_in_parent.receive(timeout_seconds=1.0)
        assert answer == {"rpc": "ok"}, f"the helper answered {answer!r} while a build is held"
        assert what_the_publishing_probe_did() == [("opened", 0.5)]
    finally:
        PUBLISHING_BUILDS_LET_GO.set()
    wait_until_the_publishing_probe_did(("closed", 0.5))
    stop_and_tear_down(stand_in_parent, lifecycle_thread)
