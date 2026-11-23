# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The lifecycle of a helper running a function node promoted to a process of its own."""

import threading

import pytest

from tatolab.runtime import _engine
from tatolab.runtime._function_node_run_as_a_standalone_node import (
    the_node_class_a_promoted_function_node_runs_as,
)
from tatolab.runtime._node_interpreter_bootstrap import (
    HelperProcessLifecycle,
    ParentProcessBridge,
)
from tatolab.stream import node

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)


@node
def doubles_its_level(level: float = 0.5) -> float:
    return level * 2.0


@node
def says_if_high_in_python(level: float = 0.5) -> float:
    """Builds an object, which keeps its body from compiling, so it runs as Python."""
    repr(object())
    return 1.0 if level > 0.5 else 0.0


@pytest.mark.parametrize(
    ("function_node", "node_id", "ran_compiled"),
    [(doubles_its_level, "doubler-1", True), (says_if_high_in_python, "sayer-1", False)],
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
