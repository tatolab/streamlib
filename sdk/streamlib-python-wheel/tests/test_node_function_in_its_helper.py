# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A node written as one function runs in its helper: the body once at setup, the function it
returned each frame; a value set live rebuilds only the computed value that read it, and
stopping the node closes every value its builds made."""

from __future__ import annotations

import time
from typing import Any

import pytest

from helper_process_test_support import drive_lifecycle_on_a_thread
from tatolab.runtime._node_interpreter_bootstrap import ParentProcessBridge, load_node_class

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
