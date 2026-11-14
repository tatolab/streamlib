# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""A helper's context names the stream and node it hosts, as its parent named them at spawn.

GPU-free: the context is opened directly on a data plane with no link wired.
"""

import pytest

from tatolab.runtime import _engine

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)


def test_both_contexts_name_the_stream_and_node_the_parent_named(
    monkeypatch: pytest.MonkeyPatch,
):
    monkeypatch.setenv("STREAMLIB_NODE_NAME", "desk-camera")
    monkeypatch.setenv("STREAMLIB_STREAM_ID", "main")
    monkeypatch.setenv("STREAMLIB_STREAM_PROJECT_DIRECTORY", "/home/someone/apps/desk")
    full_access_context = _engine.open_runtime_context_full_access_for_helper_process(
        {},
        _engine.open_node_link_data_access_for_helper_process(),
        "runtime-under-test",
        "processor-under-test",
    )
    limited_access_context = _engine.limited_access_view_of_runtime_context_for_helper_process(
        full_access_context
    )

    for context in (full_access_context, limited_access_context):
        assert (context.node_name, context.stream_id, context.stream_project_directory) == (
            "desk-camera",
            "main",
            "/home/someone/apps/desk",
        )
        assert context.node_id == "processor-under-test"


def test_a_context_no_parent_spawned_names_nothing(monkeypatch: pytest.MonkeyPatch):
    for variable in (
        "STREAMLIB_NODE_NAME",
        "STREAMLIB_STREAM_ID",
        "STREAMLIB_STREAM_PROJECT_DIRECTORY",
    ):
        monkeypatch.delenv(variable, raising=False)
    context = _engine.open_runtime_context_full_access_for_helper_process(
        {},
        _engine.open_node_link_data_access_for_helper_process(),
        "runtime-under-test",
        "processor-under-test",
    )

    assert (context.node_name, context.stream_id, context.stream_project_directory) == ("", "", "")
