# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""`ImageFile` reports itself not ready while it names no asset or cannot read the one it names, and tries again at each `process()`."""

from __future__ import annotations

from core_nodes.image_file import ImageFile, ImageFileConfig


class AssetContentPathThatFailsRecordingReadinessReports:
    """A node context whose `asset_content_path` raises, recording the reason the node last
    reported itself not ready, or `None` while it is ready."""

    gpu_limited_access = None

    def __init__(self) -> None:
        self.not_ready_because: "str | None" = None
        self.asset_content_path_calls = 0

    def report_not_ready(self, reason: str, **_placeholder_image_size: int) -> None:
        self.not_ready_because = reason

    def report_ready(self) -> None:
        self.not_ready_because = None

    def asset_content_path(self, asset: str) -> str:
        self.asset_content_path_calls += 1
        raise RuntimeError(f"asset {asset} is not readable yet")


def test_an_image_file_naming_no_asset_reports_itself_not_ready_naming_the_setting():
    image_file = ImageFile(ImageFileConfig())
    context = AssetContentPathThatFailsRecordingReadinessReports()
    image_file.process(context)
    assert context.not_ready_because is not None
    assert "set its `asset` config" in context.not_ready_because
    assert context.asset_content_path_calls == 0


def test_an_unreadable_asset_reports_the_node_not_ready_and_is_tried_again_at_each_process():
    image_file = ImageFile(ImageFileConfig(asset="0f0e"))
    context = AssetContentPathThatFailsRecordingReadinessReports()
    image_file.process(context)
    assert context.not_ready_because == (
        "ImageFile cannot read its image asset: asset 0f0e is not readable yet"
    )
    image_file.process(context)
    assert context.asset_content_path_calls == 2, "the publish stays pending after a failure"
