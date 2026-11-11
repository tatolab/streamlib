# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Every node `tatolab-ops` declares in `tatolab.nodes` is one a stream's interpreter describes."""

from __future__ import annotations

from importlib import metadata

import pytest


def test_every_node_tatolab_ops_declares_is_described_by_a_streams_interpreter(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    bootstrap = pytest.importorskip("tatolab.runtime._processor_interpreter_bootstrap")
    from tatolab.stream import log

    monkeypatch.setattr(log, "_helper_process_sink", lambda level, message, attrs: None)
    node_import_paths_tatolab_ops_declares = [
        entry_point.value
        for entry_point in metadata.distribution("tatolab-ops").entry_points
        if entry_point.group == bootstrap.INSTALLED_MODULE_NODES_ENTRY_POINT_GROUP
    ]
    assert node_import_paths_tatolab_ops_declares

    refusals_by_import_path: dict[str, str] = {}
    for import_path in node_import_paths_tatolab_ops_declares:
        try:
            described_node_type = bootstrap.describe_one_processor_class(import_path)
        except bootstrap.ProcessorClassNotDescribable as refusal:
            refusals_by_import_path[import_path] = str(refusal)
            continue
        assert described_node_type["import_path"] == import_path

    assert refusals_by_import_path == {}
