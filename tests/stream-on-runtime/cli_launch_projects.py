# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The projects, and the stream loads, both CLI launch test files use."""

from __future__ import annotations

from pathlib import Path

from conftest import (
    TatolabdUnderTest,
    TatolabRunOrDevOnTheRuntime,
)
from runtime_unit_under_test import STREAM_ON_RUNTIME_SUITE_DIRECTORY
from stream_runs_on_tatolabd import (
    TatolabRunOfAProject,
    stream_id_of_the_stream_in,
)


# A load is the project's graph documents read + the describe of its Python types + the
# engine's GPU context on the runtime's first start.
NODE_READY_TIMEOUT_SECONDS = 90.0
CLEAN_EXIT_TIMEOUT_SECONDS = 60.0


FIRST_FRAME_REPORTER_MODULE = STREAM_ON_RUNTIME_SUITE_DIRECTORY / "first_frame_reporter.py"

STREAM_WITH_HELPER_PLACED_NODES_TEMPLATE = '''\
from first_frame_reporter import ReportsItsProcessOnFirstFrame
from tatolab.stream import StreamBuilder, graph_generator
from tatolab.testpattern import TestPatternSource


@graph_generator
def main(stream_builder: StreamBuilder) -> None:
    source = stream_builder.add(TestPatternSource, input_values={"width": 320, "height": 180})
    for _ in range(%d):
        reporter = stream_builder.add(ReportsItsProcessOnFirstFrame)
        stream_builder.connect(source.output("video"), reporter.input("video_from_upstream"))
'''

LIVE_HELPER_MARKER_NAME = "LIVE"


#: The one stream every project here holds.
STREAM_NAME_OF_EVERY_PROJECT = "main"


def stream_id_of_the_project(app_directory: Path) -> str:
    """The stream id the runtime keys and addresses the project's one stream by."""
    return stream_id_of_the_stream_in(app_directory, STREAM_NAME_OF_EVERY_PROJECT)


def the_stream_stopped_log_line(stream_id: str) -> str:
    """The engine's line once the stream's teardown has finished."""
    return f"[stop] The stream `{stream_id}` stopped"


def project_files_with_helper_placed_nodes(helper_count: int) -> "dict[str, str]":
    """A project whose `stream.py` wires `helper_count` copies of the reporter to one test pattern source."""
    return {
        "stream.py": STREAM_WITH_HELPER_PLACED_NODES_TEMPLATE % helper_count,
        FIRST_FRAME_REPORTER_MODULE.name: FIRST_FRAME_REPORTER_MODULE.read_text(encoding="utf-8"),
    }


def load_the_projects_stream(
    tatolabd: TatolabdUnderTest, app_directory: Path, verb: str = "open"
) -> TatolabRunOrDevOnTheRuntime:
    """`tatolab <verb>` from `app_directory`, started on `tatolabd`."""
    return tatolabd.run_stream(TatolabRunOfAProject(working_directory=app_directory), verb=verb)
