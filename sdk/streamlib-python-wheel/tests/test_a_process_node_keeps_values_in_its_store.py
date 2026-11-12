# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`ctx.store()` sends each read, write and remove to the parent as its escalate op.

GPU-free: a stand-in parent keeps the values, as the parent's project store would.
"""

from typing import Any

import pytest

from tatolab.runtime import _engine

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)


class StandInParentKeepingNodeStoreValues:
    def __init__(self) -> None:
        self.values_by_key: dict[str, Any] = {}
        self.ops_received: list[dict[str, Any]] = []

    def __call__(self, op: dict[str, Any]) -> dict[str, Any]:
        self.ops_received.append(dict(op))
        if op["op"] == "write_node_store_key":
            self.values_by_key[op["key"]] = op["value"]
            return {}
        if op["op"] == "remove_node_store_key":
            self.values_by_key.pop(op["key"], None)
            return {}
        if op["key"] in self.values_by_key:
            return {"node_store_value": self.values_by_key[op["key"]]}
        return {}


def contexts_escalating_to(parent: StandInParentKeepingNodeStoreValues):
    full_access_context = _engine.open_runtime_context_full_access_for_helper_process(
        {},
        _engine.open_node_link_data_access_for_helper_process(),
        "runtime-under-test",
        "processor-under-test",
        parent,
    )
    return full_access_context, _engine.limited_access_view_of_runtime_context_for_helper_process(
        full_access_context
    )


def test_both_contexts_read_back_what_the_node_wrote_until_it_removes_it():
    parent = StandInParentKeepingNodeStoreValues()
    full_access_context, limited_access_context = contexts_escalating_to(parent)

    assert limited_access_context.store().read("calibration") is None
    full_access_context.store().write("calibration", {"gain": 0.75, "labels": ["left"]})
    assert limited_access_context.store().read("calibration") == {
        "gain": 0.75,
        "labels": ["left"],
    }
    limited_access_context.store().remove("calibration")
    assert full_access_context.store().read("calibration") is None

    assert [op["op"] for op in parent.ops_received] == [
        "read_node_store_key",
        "write_node_store_key",
        "read_node_store_key",
        "remove_node_store_key",
        "read_node_store_key",
    ]


def test_a_value_json_cannot_carry_is_refused_before_it_reaches_the_parent():
    parent = StandInParentKeepingNodeStoreValues()
    full_access_context, _ = contexts_escalating_to(parent)

    with pytest.raises(TypeError, match="JSON"):
        full_access_context.store().write("calibration", b"\x00\x01")
    assert parent.ops_received == []


def test_a_context_with_no_route_to_the_parent_refuses_by_name():
    context = _engine.open_runtime_context_full_access_for_helper_process(
        {},
        _engine.open_node_link_data_access_for_helper_process(),
        "runtime-under-test",
        "processor-under-test",
    )

    with pytest.raises(RuntimeError, match="no route to the parent"):
        context.store().read("calibration")
