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


def answer_the_helper_for_a_moment(stand_in_parent: Any) -> None:
    """Answer each escalate request the helper sends for a moment, as its parent does; the helper
    sends nothing else unasked."""
    unasked = stand_in_parent.receive_answering_each_escalate(timeout_seconds=0.01)
    assert unasked is None, f"the helper sent {unasked!r} unasked"


def wait_until_the_probe_framed(
    stand_in_parent: Any, seen: "tuple[float, float]", timeout_s: float = 5.0
) -> None:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if what_the_probe_did()[-1:] == [("frame", seen)]:
            return
        answer_the_helper_for_a_moment(stand_in_parent)
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
    assert stand_in_parent.receive_answering_each_escalate() == {"rpc": "ready"}
    stand_in_parent.send({"cmd": "run", "execution": "continuous", "interval_ms": 5})
    wait_until_the_probe_framed(stand_in_parent, (0.5, 1.0))
    return lifecycle_thread


def test_a_value_set_live_rebuilds_only_the_computed_value_that_read_it(stand_in_parent: Any):
    """Mental-revert: run the body at each `set_input_values`, or rebuild every computed value
    at every write, and the body or the reading is recorded twice for the gain set."""
    lifecycle_thread = start_the_level_reading_probe(stand_in_parent)

    stand_in_parent.send({"cmd": "set_input_values", "input_values": {"level": 0.75}})
    wait_until_the_probe_framed(stand_in_parent, (0.75, 1.0))
    stand_in_parent.send(
        {"cmd": "set_input_values", "input_values": {"level": 0.75, "gain": 2.0}}
    )
    wait_until_the_probe_framed(stand_in_parent, (0.75, 2.0))

    assert done_but_frames(what_the_probe_did()) == [
        ("body", None),
        ("built", 0.5),
        ("built", 0.75),
        ("closed", 0.5),
    ]
    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive_answering_each_escalate()["rpc"] == "done"
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_stopping_the_node_closes_every_value_its_builds_made(stand_in_parent: Any):
    """Mental-revert: leave `stop` and `teardown` to the hooks a class defines, and the reading
    of 0.5 is never closed."""
    lifecycle_thread = start_the_level_reading_probe(stand_in_parent)

    stand_in_parent.send({"cmd": "stop", "capability": "full"})
    assert stand_in_parent.receive_answering_each_escalate()["rpc"] == "stopped"

    assert done_but_frames(what_the_probe_did()) == [
        ("body", None),
        ("built", 0.5),
        ("closed", 0.5),
    ]
    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive_answering_each_escalate()["rpc"] == "done"
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
    answer = stand_in_parent.receive_answering_each_escalate()
    assert answer["rpc"] == "error"
    assert refusal in answer["error"]

    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    assert stand_in_parent.receive_answering_each_escalate()["rpc"] == "done"
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

    running = NodeFunctionRunning(
        LevelReadingProbe, values_to_build_with={}, pins={}, bind_pins=lambda members: {}, slot_counts={}
    )
    with pytest.raises(NodeFunctionInputNotDeclared, match="declares no value input `volume`"):
        running.write_inputs({"volume": 1.0})
    running.setup(object())
    try:
        with pytest.raises(NodeFunctionBodyRanAgain, match="body has already run"):
            running.setup(object())
    finally:
        running.teardown(object())


def test_slots_that_do_not_build_leave_every_group_holding_the_slots_it_held():
    """Mental-revert: commit the count before the slots build, and the node says it holds the
    slot it refused, so a later reshape to that count is skipped as already held."""
    from node_function_probes import LayerNeedingAWeightProbe
    from tatolab.runtime._node_function_running import NodeFunctionRunning

    running = NodeFunctionRunning(
        LayerNeedingAWeightProbe,
        values_to_build_with={"layers.0.weight": 2.0},
        pins={},
        bind_pins=lambda members: {member.port: object() for member in members},
        slot_counts={"layers": 1},
    )

    with pytest.raises(ValueError, match="a layer needs its weight set"):
        running.hold_slot_counts({"layers": 2}, {})
    assert running.slot_counts() == {"layers": 1}
    running.hold_slot_counts({"layers": 2}, {"layers.1.weight": 4.0})
    assert running.slot_counts() == {"layers": 2}


def what_the_publishing_probe_did() -> "list[tuple[str, float]]":
    from node_function_probes import WHAT_THE_LEVEL_PUBLISHING_PROBE_DID

    return list(WHAT_THE_LEVEL_PUBLISHING_PROBE_DID)


def wait_until_the_publishing_probe_did(
    stand_in_parent: Any, done: "tuple[str, float]", timeout_s: float = 5.0
) -> None:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if done in what_the_publishing_probe_did():
            return
        answer_the_helper_for_a_moment(stand_in_parent)
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
    assert stand_in_parent.receive_answering_each_escalate() == {"rpc": "ready"}
    stand_in_parent.send({"cmd": "run", "execution": "manual", "interval_ms": 0})
    wait_until_the_publishing_probe_did(stand_in_parent, ("opened", 0.5))
    return lifecycle_thread


def stop_and_tear_down(stand_in_parent: Any, lifecycle_thread: Any) -> None:
    stand_in_parent.send({"cmd": "stop", "capability": "full"})
    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    answers = [stand_in_parent.receive_answering_each_escalate() for _ in range(2)]
    assert [answer and answer["rpc"] for answer in answers] == ["stopped", "done"]
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()


def test_a_node_pacing_itself_takes_a_value_set_live_at_its_next_tick(stand_in_parent: Any):
    """Mental-revert: drop the node function's arm from `_set_input_values`, and 0.75 never
    reaches the scope, so no publishing of it opens and the publishing of 0.5 is never closed."""
    lifecycle_thread = start_the_level_publishing_probe(stand_in_parent, {})

    stand_in_parent.send({"cmd": "set_input_values", "input_values": {"level": 0.75}})
    wait_until_the_publishing_probe_did(stand_in_parent, ("closed", 0.5))

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
    wait_until_the_publishing_probe_did(stand_in_parent, ("closed", 0.5))

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
        for _ in range(20):
            answer_the_helper_for_a_moment(stand_in_parent)
        stand_in_parent.send({"cmd": "on_pause"})
        answer = stand_in_parent.receive_answering_each_escalate(timeout_seconds=1.0)
        assert answer == {"rpc": "ok"}, f"the helper answered {answer!r} while a build is held"
        assert what_the_publishing_probe_did() == [("opened", 0.5)]
    finally:
        PUBLISHING_BUILDS_LET_GO.set()
    wait_until_the_publishing_probe_did(stand_in_parent, ("closed", 0.5))
    stop_and_tear_down(stand_in_parent, lifecycle_thread)


def test_a_node_pacing_itself_tells_its_parent_a_build_raised_by_its_own_kind(
    stand_in_parent: Any,
):
    """Mental-revert: leave a node function's builds out of what a tick tells the parent, and
    the parent never hears that the build of a negative level raised, so the node shows healthy
    while it publishes the level before."""
    lifecycle_thread = start_the_level_publishing_probe(stand_in_parent, {})
    escalates: "list[dict[str, Any]]" = []

    stand_in_parent.send({"cmd": "set_input_values", "input_values": {"level": -1.0}})
    deadline = time.monotonic() + 5.0
    while time.monotonic() < deadline and not any(
        build["latest_build_failed"] is not None
        for escalate in escalates
        if escalate["op"] == "set_computed_value_builds"
        for build in escalate["builds"]
    ):
        stand_in_parent.receive_answering_each_escalate(0.05, escalates)

    reports = [
        escalate["builds"] for escalate in escalates if escalate["op"] == "set_computed_value_builds"
    ]
    assert reports, f"the helper told its parent nothing of its builds; it sent {escalates!r}"
    (raised,) = reports[-1]
    assert raised["declared"].startswith("`LevelPublishingProbe.<locals>.publish_the_level` declared at ")
    from node_function_probes import __file__ as probes_file

    with open(probes_file) as probes:
        line = next(n for n, text in enumerate(probes, 1) if "def publish_the_level" in text)
    assert raised["declared"].endswith(f"node_function_probes.py:{line}")
    assert raised["has_a_value"] is True
    assert raised["building_again"] is False
    assert raised["latest_build_failed"] == {
        "kind": "raised",
        "why": "LevelNegative: the level is -1.0, below 0",
    }
    assert what_the_publishing_probe_did() == [("opened", 0.5)]
    stop_and_tear_down(stand_in_parent, lifecycle_thread)


def what_the_stacking_probe_did() -> "list[tuple[str, object]]":
    from node_function_probes import WHAT_THE_LAYER_STACKING_PROBE_DID

    return list(WHAT_THE_LAYER_STACKING_PROBE_DID)


def wait_until_the_stacking_probe_did(
    done: "tuple[str, object]", *, since: int, timeout_s: float = 5.0
) -> None:
    """Wait until the stacking probe did `done` after the first `since` things it did."""
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if done in what_the_stacking_probe_did()[since:]:
            return
        time.sleep(0.01)
    raise AssertionError(
        f"the stacking probe never did {done!r}; it did: {what_the_stacking_probe_did()[since:][-5:]}"
    )


def the_wiring_of_a_link_into_a_layer(slot: int, direction: str) -> "dict[str, Any]":
    """One end of a link into the stacking probe's `layers.<slot>.level`, as the engine's wiring
    path hands it a helper."""
    link_id = f"L-layer-{slot}-{os.getpid()}"
    wiring = the_wiring_of_a_link_into_the_level(link_id, direction)
    if direction == "input":
        wiring["name"] = f"layers.{slot}.level"
    return wiring


def a_source_of_levels_into_layer(slot: int) -> Any:
    """The writer of a source wired into the stacking probe's `layers.<slot>.level`."""
    source = _engine.open_node_link_data_access_for_helper_process()
    wire_link_data_access(source, {"outputs": [the_wiring_of_a_link_into_a_layer(slot, "output")]})
    return link_output_data_writer_of(
        _engine.open_runtime_context_full_access_for_helper_process(
            source, "R-helper-test", f"source-of-layer-{slot}"
        )
    )


def write_a_level(source_outputs: Any, level: float) -> None:
    samples = array.array("d", [level])
    source_outputs.write_channels_block(
        "level_to_the_probe",
        memoryview(samples).cast("B").cast("d", [1, 1]),
        start_sample=0,
        sample_rate=0,
    )


def counts_and_gains_framed(did: "list[tuple[str, object]]") -> "list[object]":
    """Each `(count, gains)` the frames read, each run of equal frames once, in order."""
    framed: "list[object]" = []
    for entry in did:
        if entry[0] == "frame" and framed[-1:] != [entry[1]]:
            framed.append(entry[1])
    return framed


def test_a_layer_wired_or_unwired_while_playing_gives_the_node_its_slots_and_rebuilds_nothing(
    stand_in_parent: Any,
):
    """Mental-revert: build the slots once at setup, as a class node's are, and the slot a link
    wired after setup reaches is never held, so no frame reads two layers or its level; or
    report no live slots in `ready`, and the runtime rebuilds the node for each one."""
    from node_function_probes import WHAT_THE_LAYER_STACKING_PROBE_DID

    WHAT_THE_LAYER_STACKING_PROBE_DID.clear()
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:LayerStackingProbe")
    )
    stand_in_parent.send(
        {
            "cmd": "setup",
            "capability": "full",
            "ports": {
                "inputs": [the_wiring_of_a_link_into_a_layer(0, "input")],
                "input_values": {"layers.0.gain": 2.0, "layers.1.gain": 3.0},
            },
        }
    )
    assert stand_in_parent.receive() == {"rpc": "ready", "takes_its_pin_group_slots_live": True}
    stand_in_parent.send({"cmd": "run", "execution": "continuous", "interval_ms": 5})
    wait_until_the_stacking_probe_did(("frame", (1, (2.0,))), since=0)

    second_layer = the_wiring_of_a_link_into_a_layer(1, "input")
    since = len(what_the_stacking_probe_did())
    stand_in_parent.send({"cmd": "wire_link", "direction": "input", "link": second_layer})
    assert stand_in_parent.receive() == {"rpc": "link_wired", "link_id": second_layer["link_id"]}
    wait_until_the_stacking_probe_did(("frame", (2, (2.0, 3.0))), since=since)
    since = len(what_the_stacking_probe_did())
    # Held until the probe reads the level, so its sender outlives the read.
    source_of_the_second_layer = a_source_of_levels_into_layer(1)
    write_a_level(source_of_the_second_layer, 0.75)
    wait_until_the_stacking_probe_did(("read", (1, 0.75)), since=since)

    since = len(what_the_stacking_probe_did())
    stand_in_parent.send(
        {"cmd": "set_input_values", "input_values": {"layers.0.gain": 2.0, "layers.1.gain": 5.0}}
    )
    wait_until_the_stacking_probe_did(("frame", (2, (2.0, 5.0))), since=since)

    since = len(what_the_stacking_probe_did())
    stand_in_parent.send(
        {
            "cmd": "unwire_link",
            "direction": "input",
            "port": "layers.1.level",
            "link_id": second_layer["link_id"],
        }
    )
    wait_until_the_stacking_probe_did(("frame", (1, (2.0,))), since=since)

    did = what_the_stacking_probe_did()
    assert [entry for entry in did if entry[0] in ("body", "counted")] == [
        ("body", None),
        ("counted", 1),
        ("counted", 2),
        ("counted", 1),
    ]
    # Between a slot held or dropped and the count built for it, a frame reads the slots with
    # the count before.
    framed = iter(counts_and_gains_framed(did))
    assert all(
        seen in framed for seen in [(1, (2.0,)), (2, (2.0, 3.0)), (2, (2.0, 5.0)), (1, (2.0,))]
    ), counts_and_gains_framed(did)
    stop_and_tear_down(stand_in_parent, lifecycle_thread)


def test_a_link_into_a_slot_that_does_not_build_is_refused_and_leaves_the_slots_as_they_were(
    stand_in_parent: Any, log_records_this_process_sends_its_stand_in_parent: Any
):
    """Mental-revert: answer the wire before the slot builds, and the parent hears `link_wired`
    for a slot the node never holds; commit the count before the slot builds, and every later
    reshape to that count is skipped as already held."""
    from node_function_probes import WHAT_THE_LAYER_STACKING_PROBE_DID

    WHAT_THE_LAYER_STACKING_PROBE_DID.clear()
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:LayerNeedingAWeightProbe")
    )
    stand_in_parent.send(
        {
            "cmd": "setup",
            "capability": "full",
            "ports": {
                "inputs": [the_wiring_of_a_link_into_a_layer(0, "input")],
                "input_values": {"layers.0.weight": 2.0},
            },
        }
    )
    assert stand_in_parent.receive() == {"rpc": "ready", "takes_its_pin_group_slots_live": True}
    stand_in_parent.send({"cmd": "run", "execution": "continuous", "interval_ms": 5})
    wait_until_the_stacking_probe_did(("weights", (2.0,)), since=0)

    second_layer = the_wiring_of_a_link_into_a_layer(1, "input")
    stand_in_parent.send({"cmd": "wire_link", "direction": "input", "link": second_layer})
    refused = stand_in_parent.receive()
    assert refused["rpc"] == "link_wire_failed", refused
    assert refused["link_id"] == second_layer["link_id"]
    assert refused["reason"] == "a layer needs its weight set", refused

    since = len(what_the_stacking_probe_did())
    stand_in_parent.send(
        {"cmd": "set_input_values", "input_values": {"layers.0.weight": 2.0, "layers.1.weight": 4.0}}
    )
    stand_in_parent.send({"cmd": "wire_link", "direction": "input", "link": second_layer})
    assert stand_in_parent.receive() == {"rpc": "link_wired", "link_id": second_layer["link_id"]}
    wait_until_the_stacking_probe_did(("weights", (2.0, 4.0)), since=since)
    stop_and_tear_down(stand_in_parent, lifecycle_thread)


def test_a_function_node_raising_in_the_tick_of_a_node_pacing_itself_is_reported_on_it(
    stand_in_parent: Any,
):
    """A function node island feeding the level of a node that paces itself, raising on the
    level its pin sets, is reported on the function node from the node's tick.

    Mental-revert: leave the manual tick reporting no function node's raise, and the raise its
    field read noted reaches the parent never, since a node that paces itself runs no frame."""
    from function_node_test_nodes import an_island_of_one_node
    from node_function_probes import WHAT_THE_LEVEL_PUBLISHING_PROBE_DID

    island = an_island_of_one_node(
        "level",
        "helper_process_probes:builds_an_object_then_answers_its_level_raising_on_a_negative_one",
        None,
        "beat-1",
    )
    island["nodes"][0]["pins"] = {"level": -1.0}
    WHAT_THE_LEVEL_PUBLISHING_PROBE_DID.clear()
    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(
        bridge, load_node_class(f"{PROBE_MODULE}:LevelPublishingProbe")
    )
    stand_in_parent.send(
        {
            "cmd": "setup",
            "capability": "full",
            "ports": {"islands": [island], "input_values": {"level": 0.5}},
        }
    )
    assert stand_in_parent.receive_answering_each_escalate() == {
        "rpc": "ready",
        "function_nodes_ran_compiled": {"beat-1": False},
    }
    stand_in_parent.send({"cmd": "run", "execution": "manual", "interval_ms": 0})

    deadline = time.monotonic() + 5.0
    raising = None
    while raising is None and time.monotonic() < deadline:
        frame = stand_in_parent.receive(timeout_seconds=max(0.0, deadline - time.monotonic()))
        if frame is None:
            break
        if frame.get("op") == "set_function_node_raising_because":
            raising = frame
        elif frame.get("rpc") == "escalate_request":
            stand_in_parent.send(
                {"rpc": "escalate_response", "request_id": frame["request_id"], "result": "ok"}
            )
    assert raising is not None, "the tick never reported the function node raising"
    assert raising["function_node"] == "beat-1"
    assert raising["raising_because"].startswith(
        "raised RuntimeError: the function node broke on purpose ("
    )
    stand_in_parent.send(
        {"rpc": "escalate_response", "request_id": raising["request_id"], "result": "ok"}
    )
    stop_and_tear_down(stand_in_parent, lifecycle_thread)
