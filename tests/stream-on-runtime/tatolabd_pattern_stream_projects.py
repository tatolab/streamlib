# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The pattern stream projects, and the `tatolabd` restarts, both `tatolabd` hosting test files use."""

from __future__ import annotations

import re
from collections.abc import Callable
from pathlib import Path
from typing import Any

from conftest import TatolabdUnderTest
from local_api_client import LocalApiClient
from runtime_unit_under_test import MachineDirectoriesUnderATestRoot
from stream_runs_on_tatolabd import (
    graph_document_path_of,
    stream_asset_held_at,
    stream_id_of_the_stream_in,
    write_the_project_file_unless_there,
)


#: A load is the project's graph documents read, and on the runtime's first start its GPU context.
STREAM_RUNNING_TIMEOUT_SECONDS = 90.0
TATOLABD_EXIT_TIMEOUT_SECONDS = 60.0


#: The streams of the two projects, each the sole graph document of its
#: own project and the default display name of its stream.
ALPHA_STREAM_FUNCTION = "alpha_pattern"
BRAVO_STREAM_FUNCTION = "bravo_pattern"

#: Every project stream here: a test pattern into a node that reads its frames.
PROJECT_STREAM_SOURCE_TEMPLATE = '''\
from frame_reader import PatternFrameReader
from tatolab.stream import StreamBuilder, graph_generator
from tatolab.testpattern import TestPatternSource


@graph_generator
def {stream_function_name}(stream_builder: StreamBuilder) -> None:
    pattern = stream_builder.add(
        TestPatternSource, name="pattern", config={{"width": 320, "height": 180}}
    )
    reader = stream_builder.add(PatternFrameReader, name="reader")
    stream_builder.connect(pattern.output("video"), reader.input("video_from_upstream"))
'''
PROJECT_FRAME_READER_SOURCE = '''\
from tatolab.stream import Image, Input, RuntimeContextLimitedAccess, node


@node.process
class PatternFrameReader:
    """Reads each frame it receives and keeps none."""

    video_from_upstream = Input(Image, delivery="newest")

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.inputs.read(self.video_from_upstream)
'''
PROJECT_STREAM_NODE_COUNT = 2


#: `tatolabd`'s line once it serves, naming the re-load it queued for each project.
RELOADS_QUEUED_AT_THE_START_LOG_LINE_PATTERN = re.compile(
    r"the runtime is serving: (?P<project_count>\d+) project re-loads queued "
    r"\((?P<actions>[^)]*)\)"
)


def await_the_reloads_queued_at_the_start(
    tatolabd: TatolabdUnderTest,
    project_count: int,
    *,
    timeout: float = STREAM_RUNNING_TIMEOUT_SECONDS,
) -> "list[dict[str, Any]]":
    """Wait for `tatolabd` to serve, assert it queued one re-load for each of
    `project_count` projects, and follow each to `done`; return each as
    `describe_stream_action` ended it."""
    tatolabd.await_stderr_containing("the runtime is serving: ", timeout=timeout)
    queued = RELOADS_QUEUED_AT_THE_START_LOG_LINE_PATTERN.search(tatolabd.stderr_text)
    assert queued is not None, tatolabd.recent_stderr()
    actions = [action for action in queued.group("actions").split(", ") if action]
    assert len(actions) == int(queued.group("project_count")) == project_count, queued.group(0)
    local_api = tatolabd.local_api_client()
    reloads = [local_api.stream_action_once_ended(action, timeout=timeout) for action in actions]
    assert all(reload["state"] == "done" for reload in reloads), reloads
    return reloads


def make_project_running_a_pattern_stream(
    make_tatolab_project: "Callable[..., Path]", stream_function_name: str
) -> Path:
    """A project directory of its own whose one graph document holds the pattern stream,
    resolved."""
    return make_tatolab_project(
        {
            "stream.py": PROJECT_STREAM_SOURCE_TEMPLATE.format(
                stream_function_name=stream_function_name
            ),
            "frame_reader.py": PROJECT_FRAME_READER_SOURCE
        },
        directory_name=stream_function_name.replace("_", "-"),
    ).resolve()


def location_of_the_pattern_project(project_directory: Path, stream_name: str) -> "dict[str, Any]":
    """The location the runtime records the project's pattern stream at."""
    return {
        "project_directory": str(project_directory),
        "project": write_the_project_file_unless_there(project_directory)["project"],
        "stream_asset": stream_asset_held_at(graph_document_path_of(project_directory, stream_name)),
    }


def stream_id_of_the_pattern_project(project_directory: Path, stream_name: str) -> str:
    """The stream id the runtime keys and addresses the project's pattern stream by."""
    return stream_id_of_the_stream_in(project_directory, stream_name)


def listed_streams_by_id(local_api: LocalApiClient) -> "dict[str, dict[str, Any]]":
    """`list_streams`, keyed by stream id."""
    return {listed["stream"]: listed for listed in local_api.list_streams()}


def listing_of(
    stream_id: str,
    stream_function_name: str,
    state: str,
    project_directory: Path,
    node_count: "int | None",
) -> "dict[str, Any]":
    """One `list_streams` entry of a pattern stream that is not failed, shown under its
    function's name, its graph loaded and `running` or `stopped`."""
    return {
        "stream": stream_id,
        "display_name": stream_function_name,
        "location": location_of_the_pattern_project(project_directory, stream_function_name),
        "state": state,
        "condition": {
            "load": "loaded",
            "playback": {"running": "playing", "stopped": "stopped"}[state],
            "health": "healthy",
            "diagnostics": [],
        },
        "node_count": node_count,
        "failed_because": None,
        "capture_devices_whose_access_was_never_asked_for": [],
        "action": None,
        "step": None,
    }


def stream_record_path(machine_directories: MachineDirectoriesUnderATestRoot, stream_id: str) -> Path:
    """`<state>/streams/<stream id>.json`."""
    return machine_directories.streams_directory / f"{stream_id}.json"


def restart_tatolabd(
    tatolabd: TatolabdUnderTest, start_tatolabd: "Callable[..., TatolabdUnderTest]"
) -> TatolabdUnderTest:
    """A Ctrl-C to `tatolabd`, its clean exit, and a new `tatolabd` under the same machine root."""
    stop_tatolabd_cleanly(tatolabd)
    return start_tatolabd()


def stop_tatolabd_cleanly(tatolabd: TatolabdUnderTest) -> None:
    """A Ctrl-C to `tatolabd`, and its clean exit."""
    tatolabd.interrupt()
    assert tatolabd.await_exit(timeout=TATOLABD_EXIT_TIMEOUT_SECONDS) == 0, tatolabd.recent_stderr()
