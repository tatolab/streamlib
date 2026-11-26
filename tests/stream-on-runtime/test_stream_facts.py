# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""An output port's stream facts, end to end on a running stream.

Each node runs in its own helper process. A Python node states a Texture
frame rate on its first tick and `graph` shows it as the port's stream facts,
named by the port's pin type beside their family; a running node replaces an
Encoded video port's facts over its bridge and the next `graph` shows the
replacement; a reader reads its upstream's current facts off its inbound link;
and facts of another type are refused naming the port. The engine seams under each are gated by the
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
    "type": "texture",
    "frame_rate": {"numerator": DECLARED_FRAMES_PER_SECOND, "denominator": 1},
}
REPLACEMENT_READ_DOWNSTREAM = REPLACEMENT_STREAM_FACTS.to_wire()
REPLACEMENT_RENDERING = {
    "type": "encoded_video",
    **{key: value for key, value in REPLACEMENT_READ_DOWNSTREAM.items() if key != "family"},
}


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

    tatolabd.await_marker("FRAME_RATE_STATED", timeout=MARKER_TIMEOUT_SECONDS)
    refusal = tatolabd.await_marker("STREAM_FACTS_REFUSED", timeout=MARKER_TIMEOUT_SECONDS)
    tatolabd.await_marker("STREAM_FACTS_REPLACED", timeout=MARKER_TIMEOUT_SECONDS)
    graph = local_api.graph(stream_id)

    read_downstream: "list[Any]" = []
    occurrence = 0
    while REPLACEMENT_READ_DOWNSTREAM not in read_downstream:
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
    assert "stream_facts" not in records
    encoded = output_port_rendered_under(graph, REPLACING_SOURCE_NAME, "encoded")
    assert encoded.get("stream_facts") == REPLACEMENT_RENDERING, (
        "the replacement the node made while running is the facts `graph` renders"
    )

    assert {(node["name"], node["kind"]) for node in graph["nodes"]} == {
        (node["name"], "process") for node in graph["nodes"]
    }, "every class node renders its kind on its graph node"
    assert all("language" not in node for node in graph["nodes"])

    assert "encoded" in refusal and "Encoded video" in refusal and "Texture" in refusal, (
        f"refused facts name the port, its pin type and the facts' type: {refusal!r}"
    )
    assert all(
        facts is None or facts == REPLACEMENT_READ_DOWNSTREAM for facts in read_downstream
    ), f"the reader only ever reads facts its upstream carried: {read_downstream}"
