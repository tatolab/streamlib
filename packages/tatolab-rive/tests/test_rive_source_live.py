# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A `RiveSource` changes its file or its size while it plays: its body runs once, a new scene
opens while the one before keeps publishing, and the one before is closed once it is replaced; a
scene that cannot open leaves the one before publishing.

The node function runs on the wheel's node scope, as its helper runs it, with this wheel's native
scene stood in for by one that records each open and close (`native_scene_stand_in`).
"""

from __future__ import annotations

import time
from pathlib import Path
from typing import Any, Callable

import pytest

from native_scene_stand_in import WHAT_THE_SCENES_DID
from tatolab.rive.rive_source import (
    RiveFileMissing,
    RiveFileNotNamed,
    RiveSource,
    the_riv_file_named_by,
)
from tatolab.runtime._node_function_running import NodeFunctionRunning


class ContextLendingPorts:
    """Stands in for the full access context the body is handed: records each table it opens."""

    def __init__(self) -> None:
        self.tables_opened: "list[tuple[list[str], list[str]]]" = []

    def open_module_host_table(self, *, output_ports: "list[str]", input_ports: "list[str]") -> object:
        self.tables_opened.append((list(output_ports), list(input_ports)))
        return object()


def wait_until(done: Callable[[], bool], timeout_s: float = 5.0) -> None:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if done():
            return
        time.sleep(0.01)
    raise AssertionError(f"never done; the scenes did {WHAT_THE_SCENES_DID}")


@pytest.fixture
def riv_files(tmp_path: Path) -> "dict[str, str]":
    files = {}
    for name, content in (("first", "riv"), ("second", "riv"), ("refused", "refused")):
        path = tmp_path / f"{name}.riv"
        path.write_text(content)
        files[name] = str(path)
    return files


@pytest.fixture
def playing(riv_files: "dict[str, str]") -> "Any":
    """The node set up on `first.riv` at 640x360, once its scene opened."""
    WHAT_THE_SCENES_DID.clear()
    ctx = ContextLendingPorts()
    running = NodeFunctionRunning(
        RiveSource,
        values_to_build_with={"riv_path": riv_files["first"], "width": 640, "height": 360},
        pins={"data": object(), "video": object()},
        bind_pins=lambda members: {},
        slot_counts={},
    )
    running.setup(ctx)
    wait_until(lambda: WHAT_THE_SCENES_DID == [("opened", riv_files["first"], 640, 360)])
    yield running, ctx
    running.teardown(ctx)


def test_a_new_file_opens_a_new_scene_and_closes_the_one_before(
    playing: "Any", riv_files: "dict[str, str]"
) -> None:
    """Mental-revert: read `riv_path` once in the body, and a set opens no new scene; open the
    table in the build, and every scene lends the ports again."""
    running, ctx = playing

    running.write_inputs({"riv_path": riv_files["second"]})
    wait_until(lambda: len(WHAT_THE_SCENES_DID) == 3)

    assert WHAT_THE_SCENES_DID == [
        ("opened", riv_files["first"], 640, 360),
        ("opened", riv_files["second"], 640, 360),
        ("closed", riv_files["first"], 640, 360),
    ]
    assert ctx.tables_opened == [(["video"], ["data"])]


def test_a_new_size_opens_a_scene_of_that_size_and_closes_the_one_before(
    playing: "Any", riv_files: "dict[str, str]"
) -> None:
    running, _ = playing

    running.write_inputs({"width": 1280, "height": 720})
    wait_until(lambda: len(WHAT_THE_SCENES_DID) == 3)

    assert WHAT_THE_SCENES_DID[1:] == [
        ("opened", riv_files["first"], 1280, 720),
        ("closed", riv_files["first"], 640, 360),
    ]


def test_a_scene_that_cannot_open_leaves_the_one_before_publishing(
    playing: "Any", riv_files: "dict[str, str]"
) -> None:
    """Mental-revert: close the scene before the new one opens, and a file rive-runtime refuses
    leaves the node publishing nothing."""
    running, _ = playing
    gone = str(Path(riv_files["first"]).with_name("gone.riv"))

    for riv_path, why in (
        (riv_files["refused"], f"RiveFileRefused: RiveSource: {riv_files['refused']!r}: not a .riv file"),
        (gone, f"RiveFileMissing: RiveSource: the `.riv` file `riv_path` names, {gone}, is missing"),
        (None, "RiveFileNotNamed: RiveSource: `riv_path` is not set; it takes the `.riv` file to play"),
    ):
        running.write_inputs({"riv_path": riv_path})
        assert running._running is not None
        assert running._running.wait_until_settled(5.0)
        (scene,) = running.builds_of_each_computed_value()
        assert (scene.has_a_value, scene.last_build.failure) == (True, "raised")
        assert scene.last_build.why == why

    assert WHAT_THE_SCENES_DID == [("opened", riv_files["first"], 640, 360)]


def test_stopping_the_node_closes_its_scene(playing: "Any", riv_files: "dict[str, str]") -> None:
    running, ctx = playing

    running.stop(ctx)

    assert WHAT_THE_SCENES_DID[-1] == ("closed", riv_files["first"], 640, 360)


def test_an_unset_or_missing_file_is_refused_by_its_own_kind(tmp_path: Path) -> None:
    with pytest.raises(RiveFileNotNamed, match="`riv_path` is not set"):
        the_riv_file_named_by(None)
    with pytest.raises(RiveFileMissing, match="gone.riv"):
        the_riv_file_named_by(str(tmp_path / "gone.riv"))
