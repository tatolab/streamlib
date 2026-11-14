# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""An output port's stream facts, end to end on a running stream.

Each node runs in its own helper process. A Python node declares an Image
frame rate and `graph` shows it as the port's stream facts; a running node
replaces a Packets port's facts over its bridge and the next `graph` shows the
replacement; a reader reads its upstream's current facts off its inbound link;
facts of another family are refused naming the port; and a Records port
renders `values_checked: false`. The engine seams under each are gated by the
`output_port_stream_facts` and `port_stream_facts` tests.
"""

from collections.abc import Callable
from typing import Any

import pytest

from conftest import TatolabdUnderTest
from stream_facts_probes import DECLARED_FRAMES_PER_SECOND, REPLACEMENT_STREAM_FACTS
from stream_facts_streams import (
    DECLARING_SOURCE_NAME,
    REPLACING_SOURCE_NAME,
    a_declaring_source_and_a_replacing_one_read_downstream,
)

pytestmark = pytest.mark.requires_gpu

GRAPH_READY_TIMEOUT_SECONDS = 90.0
MARKER_TIMEOUT_SECONDS = 30.0

DECLARED_RENDERING = {
    "family": "image",
    "frame_rate": {"numerator": DECLARED_FRAMES_PER_SECOND, "denominator": 1},
}
REPLACEMENT_RENDERING = REPLACEMENT_STREAM_FACTS.to_wire()


def output_port_rendered_under(
    graph: "dict[str, Any]", node_name: str, port_name: str
) -> "dict[str, Any]":
    """The output port `port_name` as `graph` renders it under `node_name`."""
    (rendered_node,) = [node for node in graph["nodes"] if node["name"] == node_name]
    (port,) = [port for port in rendered_node["ports"]["outputs"] if port["name"] == port_name]
    return port


def test_declared_replaced_refused_and_read_downstream(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    tatolabd = start_tatolabd_running_stream(a_declaring_source_and_a_replacing_one_read_downstream)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    local_api.await_every_node_running(stream=stream_id, timeout=GRAPH_READY_TIMEOUT_SECONDS)

    refusal = tatolabd.await_marker("STREAM_FACTS_REFUSED", timeout=MARKER_TIMEOUT_SECONDS)
    tatolabd.await_marker("STREAM_FACTS_REPLACED", timeout=MARKER_TIMEOUT_SECONDS)
    graph = local_api.graph(stream_id)
    registry = local_api.registry(stream_id)

    read_downstream: "list[Any]" = []
    occurrence = 0
    while REPLACEMENT_RENDERING not in read_downstream:
        occurrence += 1
        read_downstream.append(
            tatolabd.await_marker(
                "UPSTREAM_STREAM_FACTS", timeout=MARKER_TIMEOUT_SECONDS, occurrence=occurrence
            )
        )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    video = output_port_rendered_under(graph, DECLARING_SOURCE_NAME, "video")
    assert video.get("stream_facts") == DECLARED_RENDERING
    records = output_port_rendered_under(graph, DECLARING_SOURCE_NAME, "records")
    assert records.get("values_checked") is False
    assert "stream_facts" not in records
    encoded = output_port_rendered_under(graph, REPLACING_SOURCE_NAME, "encoded")
    assert encoded.get("stream_facts") == REPLACEMENT_RENDERING, (
        "the replacement the node made while running is the facts `graph` renders"
    )

    (declaring_type,) = [
        entry
        for entry in registry["nodes"]
        if entry["type"].endswith(":DeclaresItsFrameRateAndWritesRecords")
    ]
    (registry_records,) = [port for port in declaring_type["outputs"] if port["name"] == "records"]
    assert registry_records["values_checked"] is False

    assert {(node["name"], node["kind"]) for node in graph["nodes"]} == {
        (node["name"], "process") for node in graph["nodes"]
    }, "every `@node.process` class renders its kind on its graph node"
    assert declaring_type["kind"] == "process" and "language" not in declaring_type

    assert "encoded" in refusal and "Packets" in refusal and "Image" in refusal, (
        f"refused facts name the port and both families: {refusal!r}"
    )
    assert all(
        facts is None or facts == REPLACEMENT_RENDERING for facts in read_downstream
    ), f"the reader only ever reads facts its upstream carried: {read_downstream}"
