# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A Python node's compute kernel built in a computed value that reads its `width` input is built
again for each of a hundred widths set while its stream plays, on the limited GPU capability:
frames dispatch each new kernel in the process the node set up in, every kernel replaced is closed
and handed back to the engine, a kernel created in the node's body keeps dispatching, and the
stream stays healthy.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from helper_placement_observation import StartTatolabdRunningStream
from stream_runs_on_tatolabd import write_a_hand_written_graph_project

#: The width the probe is saved with, and the hundred it is set to after.
SAVED_WIDTH = 8
WIDTHS_SET_WHILE_PLAYING = list(range(SAVED_WIDTH + 1, SAVED_WIDTH + 101))

GRAPH_HOLDING_THE_PROBE = {
    "stream": "main",
    "nodes": [
        {
            "name": "probe",
            "type": "computed_kernel_probe_nodes:RebuildsAKernelSizedByItsWidth",
            "input_values": {"width": SAVED_WIDTH},
        }
    ],
    "links": [],
}

#: What the helper logs when the parent did not take a release it was handed.
A_RELEASE_THE_PARENT_DID_NOT_TAKE = "the parent did not take a release from this helper"


@pytest.mark.requires_gpu
def test_a_computed_kernel_is_rebuilt_for_each_width_set_while_playing_and_each_replaced_one_released(
    start_tatolabd_running_stream: StartTatolabdRunningStream, tmp_path: Path
):
    """Mental-revert: kernel creation left on the full capability fails the build with an
    AttributeError and no frame ever dispatches; a replaced kernel nothing closes reports no
    `KERNEL_CLOSED`; a set that rebuilds the node reports a second `PROBE_SETUP`."""
    project = write_a_hand_written_graph_project(
        tmp_path / "project", GRAPH_HOLDING_THE_PROBE
    )
    tatolabd = start_tatolabd_running_stream(project.tatolab_run)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    tatolabd.await_marker("BODY_KERNEL_CREATED")
    first_dispatch = tatolabd.await_marker("FRAME_DISPATCHED_WIDTH")
    assert first_dispatch["width"] == SAVED_WIDTH, first_dispatch
    local_api = tatolabd.local_api_client()
    (probe,) = [
        node
        for node in local_api.graph(stream=stream_id)["nodes"]
        if node["name"] == "probe"
    ]

    for change, width in enumerate(WIDTHS_SET_WHILE_PLAYING, start=1):
        local_api.call_tool(
            "transact",
            {
                "operations": [
                    {
                        "operation": "set_node_input_value",
                        "stream": stream_id,
                        "node": probe["id"],
                        "input": "width",
                        "value": width,
                    }
                ]
            },
        )
        built = tatolabd.await_marker("KERNEL_BUILT", occurrence=change + 1)
        assert built["width"] == width, (
            f"change {change} built a kernel for {built['width']}, not {width}"
        )
        # Awaited before the next set, so no build replaces one a frame never took.
        tatolabd.await_marker("FRAME_DISPATCHED_WIDTH", occurrence=change + 1)

    last_width = WIDTHS_SET_WHILE_PLAYING[-1]
    tatolabd.await_marker("KERNEL_CLOSED", occurrence=len(WIDTHS_SET_WHILE_PLAYING))
    dispatched = tatolabd.marker_payloads("FRAME_DISPATCHED_WIDTH")
    assert [payload["width"] for payload in dispatched] == [
        SAVED_WIDTH,
        *WIDTHS_SET_WHILE_PLAYING,
    ], "frames dispatch each kernel, in the order its width was set"
    closed = tatolabd.marker_payloads("KERNEL_CLOSED")
    assert sorted(payload["width"] for payload in closed) == [
        SAVED_WIDTH,
        *WIDTHS_SET_WHILE_PLAYING[:-1],
    ], f"every replaced kernel is closed once, and the one in use is not: {closed}"
    assert len({payload["pid"] for payload in [*dispatched, *closed]}) == 1, (
        "the kernels are built, dispatched and closed in the process the probe set up in"
    )
    assert len(tatolabd.marker_payloads("PROBE_SETUP")) == 1, tatolabd.recent_stderr()
    assert A_RELEASE_THE_PARENT_DID_NOT_TAKE not in tatolabd.stderr_text, (
        tatolabd.recent_stderr()
    )
    (listed,) = [
        listed for listed in local_api.list_streams() if listed["stream"] == stream_id
    ]
    assert listed["state"] == "running", listed
    assert listed["condition"]["health"] == "healthy", listed
    print(
        f"frame dispatch seconds after the kernel for {last_width}:",
        tatolabd.marker_payloads("DISPATCH_SECONDS")[-1],
    )
