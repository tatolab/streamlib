# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

import pytest

from helper_process_test_support import drive_lifecycle_on_a_thread
from tatolab.runtime._node_interpreter_bootstrap import ParentProcessBridge

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)


def test_a_running_function_node_pool_takes_a_late_carry_it_is_handed(stand_in_parent):
    """The engine sets a pool up with the carries its input values name, and hands a running
    pool every carry it names, its late ones among them; the pool takes them without a restart
    and answers `ok`.

    Fail-without-fix: the pool is set up with no carry, so the late ones are refused as ports it
    does not hold.
    """
    from tatolab.runtime._function_node_pool import FunctionNodePool

    bridge = ParentProcessBridge(stand_in_parent.child_end)
    bridge.start_reading()
    lifecycle_thread = drive_lifecycle_on_a_thread(bridge, FunctionNodePool)

    def the_next_frame_other_than_a_log() -> dict:
        while True:
            frame = stand_in_parent.receive()
            assert frame is not None, "the helper sent the parent nothing"
            if frame.get("op") != "log":
                return frame

    first_carry = {"input_port": "island_0", "output_port": "island_0_output", "family": "channels"}
    late_carry = {"input_port": "island_1", "output_port": "island_1_output", "family": "records"}
    stand_in_parent.send(
        {
            "cmd": "setup",
            "capability": "full",
            "input_values": {"carries": [first_carry]},
            "ports": {},
        }
    )
    assert the_next_frame_other_than_a_log() == {"rpc": "ready"}

    stand_in_parent.send(
        {
            "cmd": "carry_links",
            "carries": [first_carry, late_carry],
            "output_ports_keeping_their_last_message": ["island_1_output"],
        }
    )
    assert the_next_frame_other_than_a_log() == {"rpc": "ok"}

    stand_in_parent.send({"cmd": "stop", "capability": "full"})
    stand_in_parent.send({"cmd": "teardown", "capability": "full"})
    answers = [the_next_frame_other_than_a_log()["rpc"] for _ in range(2)]
    assert answers == ["stopped", "done"]
    lifecycle_thread.join(timeout=5.0)
    assert not lifecycle_thread.is_alive()
