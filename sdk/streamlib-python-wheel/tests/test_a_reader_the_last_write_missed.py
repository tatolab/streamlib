# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`ctx.outputs.has_a_reader_the_last_write_missed`: a reader that connects
after a write is told apart, so a node that writes only on change can write
again for it.

GPU-free. The link is a real iceoryx2 channel, its source end wired before its
destination's subscriber opens, the order the engine wires a new link in.
"""

import os

import pytest

from tatolab.runtime import _engine

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)

INPUT_PORT = "picture"
OUTPUT_PORT = "picture_to_downstream"


def test_a_reader_subscribing_after_a_write_is_told_apart_until_a_write_reaches_it(
    request: pytest.FixtureRequest,
):
    unique = f"latereader{os.getpid()}_{request.node.name}"
    channel_service_name = f"{unique}/picture"
    notify_service_name = f"{unique}_dest/notify"
    source = _engine.open_node_link_data_access_for_helper_process()
    source.wire_output_link(
        OUTPUT_PORT, channel_service_name, notify_service_name,
        1024, 1 << 20, 8, 2, 1, f"L-{unique}",
    )  # fmt: skip
    outputs = _engine.open_runtime_context_full_access_for_helper_process(
        {}, source, "runtime-under-test", "processor-under-test"
    ).outputs
    outputs.write(OUTPUT_PORT, b"before any reader")
    assert not outputs.has_a_reader_the_last_write_missed(OUTPUT_PORT)

    destination = _engine.open_node_link_data_access_for_helper_process()
    destination.wire_input_link(
        INPUT_PORT, channel_service_name, channel_service_name,
        notify_service_name,
        "read_next_in_order", 8, 2, 2, 1, f"L-{unique}",
    )  # fmt: skip
    inputs = _engine.open_runtime_context_full_access_for_helper_process(
        {}, destination, "runtime-under-test", "processor-under-test-downstream"
    ).inputs
    assert outputs.has_a_reader_the_last_write_missed(OUTPUT_PORT)
    assert inputs.read(INPUT_PORT) is None, "the channel keeps no history for a late reader"

    outputs.write(OUTPUT_PORT, b"again")
    assert not outputs.has_a_reader_the_last_write_missed(OUTPUT_PORT)
    received = inputs.read(INPUT_PORT)
    assert received is not None and bytes(received) == b"again"


def test_an_undeclared_output_port_is_refused_by_name():
    outputs = _engine.open_runtime_context_full_access_for_helper_process(
        {},
        _engine.open_node_link_data_access_for_helper_process(),
        "runtime-under-test",
        "processor-under-test",
    ).outputs
    with pytest.raises(Exception, match="nowhere"):
        outputs.has_a_reader_the_last_write_missed("nowhere")
