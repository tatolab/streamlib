# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""An output port's catalog entry, end to end on a running stream.

Each node runs in its own helper process. A Python node declares an entry and
`graph` and `/api/registry` show it under the port; a running node replaces it
over its bridge and the next `graph` shows the replacement; a reader reads its
upstream's current entry off its inbound link; and an entry hang's catalog
types do not read is refused naming the port. The engine seams under each are
gated by the `output_port_catalog_entries` and `port_catalog_entry` tests.
"""

from collections.abc import Callable
from typing import Any

import pytest

from catalog_entry_probes import DECLARED_ENTRY, REPLACEMENT_ENTRY
from catalog_entry_streams import (
    KEEPING_SOURCE_NAME,
    REPLACING_SOURCE_NAME,
    a_keeping_source_and_a_replacing_one_read_downstream,
)
from conftest import TatolabdUnderTest

pytestmark = pytest.mark.requires_gpu

GRAPH_READY_TIMEOUT_SECONDS = 90.0
MARKER_TIMEOUT_SECONDS = 30.0

DECLARED_RENDERING = {"section": DECLARED_ENTRY.section, "entry": DECLARED_ENTRY.to_dict()}
REPLACEMENT_RENDERING = {"section": REPLACEMENT_ENTRY.section, "entry": REPLACEMENT_ENTRY.to_dict()}


def catalog_entry_rendered_under(graph: "dict[str, Any]", node_name: str) -> Any:
    """What `graph` renders as `catalog_entry` under `node_name`'s `records` port."""
    (rendered_node,) = [node for node in graph["nodes"] if node["name"] == node_name]
    (records,) = [port for port in rendered_node["ports"]["outputs"] if port["name"] == "records"]
    return records.get("catalog_entry")


def test_declared_replaced_refused_and_read_downstream(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    tatolabd = start_tatolabd_running_stream(a_keeping_source_and_a_replacing_one_read_downstream)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    local_api.await_every_node_running(stream=stream_id, timeout=GRAPH_READY_TIMEOUT_SECONDS)

    refusal = tatolabd.await_marker("CATALOG_ENTRY_REFUSED", timeout=MARKER_TIMEOUT_SECONDS)
    tatolabd.await_marker("CATALOG_ENTRY_REPLACED", timeout=MARKER_TIMEOUT_SECONDS)
    graph = local_api.graph(stream_id)
    registry = local_api.registry(stream_id)

    read_downstream: "list[Any]" = []
    occurrence = 0
    while REPLACEMENT_RENDERING not in read_downstream:
        occurrence += 1
        read_downstream.append(
            tatolabd.await_marker(
                "UPSTREAM_CATALOG_ENTRY", timeout=MARKER_TIMEOUT_SECONDS, occurrence=occurrence
            )
        )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert catalog_entry_rendered_under(graph, KEEPING_SOURCE_NAME) == DECLARED_RENDERING
    assert catalog_entry_rendered_under(graph, REPLACING_SOURCE_NAME) == REPLACEMENT_RENDERING, (
        "the replacement the node made while running is the entry `graph` renders"
    )

    (keeping_type,) = [
        entry
        for entry in registry["nodes"]
        if entry["type"].endswith(":KeepsItsDeclaredCatalogEntry")
    ]
    assert keeping_type["outputs"][0]["catalog_entry"] == DECLARED_RENDERING

    assert "records" in refusal and "codec" in refusal, (
        f"a refused entry names the port and the field hang refused: {refusal!r}"
    )
    assert set(map(repr, read_downstream)) <= {repr(DECLARED_RENDERING), repr(REPLACEMENT_RENDERING)}, (
        f"the reader only ever reads an entry its upstream carried: {read_downstream}"
    )
