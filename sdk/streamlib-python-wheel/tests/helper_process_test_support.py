# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The helpers more than one helper process test file uses."""

import json
import os
import threading

from tatolab.runtime._node_the_wheel_hosts import link_input_data_reader_of, link_output_data_writer_of
from tatolab.runtime import _engine
from tatolab.runtime._class_node_bound_pins import BoundOutputPin, bind_the_pins_of_a_class_node
from tatolab.runtime._node_hosting import class_node_members_of
from tatolab.runtime._node_interpreter_bootstrap import (
    HelperProcessLifecycle,
)
from tatolab.stream import EncodedVideo, OutputPin, node


PROBE_MODULE = "helper_process_probes"


def frame_index_payload(frame_index: int) -> bytes:
    """The JSON record the pass-through probes read and forward."""
    return json.dumps({"frame_index": frame_index}).encode()


@node(execution="manual")
class _UpstreamOfAPassThroughProbe:
    """The typed node upstream of a pass-through probe, written by the test in its place."""

    frames_to_downstream: OutputPin[EncodedVideo]


def the_pin_upstream_of_a_pass_through_probe(link_data_access) -> BoundOutputPin:
    """The `frames_to_downstream` pin of a typed node upstream of a pass-through probe, bound on
    `link_data_access`, so a test writes the probe each frame as a typed node does."""
    members = class_node_members_of(_UpstreamOfAPassThroughProbe)
    assert members is not None
    context = _engine.open_runtime_context_full_access_for_helper_process(
        {}, link_data_access, "R-helper-test", "upstream-of-the-probe"
    )
    pin = bind_the_pins_of_a_class_node(
        members,
        link_data_access=link_data_access,
        inputs=link_input_data_reader_of(context),
        outputs=link_output_data_writer_of(context),
        node_id="upstream-of-the-probe",
    )["frames_to_downstream"]
    assert isinstance(pin, BoundOutputPin)
    return pin


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
            "channel_service_name": channel_service_name,
            "dest_notify_service_name": notify_service_name,
            "expected_payload_bytes": 1024,
            "max_payload_bytes_per_channel": 1 << 20,
            "channel_service_creation_depth": 16,
            "max_subscribers": 2,
            "notify_max_notifiers": 1,
            "output_port_wiring_generation": 1,
        }
    return {
        "name": "frames_from_upstream",
        "link_id": link_id,
        "channel_service_name": channel_service_name,
        "inbound_link_name": channel_service_name,
        "notify_service_name": notify_service_name,
        "read_mode": "read_next_in_order",
        "channel_service_creation_depth": 16,
        "input_port_ring_depth": 16,
        "max_subscribers": 2,
        "notify_max_notifiers": 1,
        "loss_count_slot": 0,
        "wiring_generation": 1,
    }


def drive_lifecycle_on_a_thread(bridge, node_class):
    """Run the lifecycle loop off the test's own thread.

    iceoryx2's ports are `!Send`, so everything the loop touches has to be
    created and driven on the one thread that owns it — which is exactly how a
    real helper runs.
    """
    def drive() -> None:
        HelperProcessLifecycle(
            bridge,
            node_class,
            "R-helper-test",
            "P-helper-test",
            _engine.open_node_link_data_access_for_helper_process(),
        ).run_until_the_parent_is_done()

    lifecycle_thread = threading.Thread(target=drive, name="helper-lifecycle")
    lifecycle_thread.start()
    return lifecycle_thread
