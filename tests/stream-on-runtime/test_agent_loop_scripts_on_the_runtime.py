# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Each agent loop script, replayed over MCP against a runtime unit holding one running stream.

The project carries every node file a script's task names, and its stream runs one test
pattern source; the script learns everything else from the session.
"""

from __future__ import annotations

import asyncio
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from agent_loop_script_runner import (
    every_agent_loop_script_path,
    load_agent_loop_script,
    play_agent_loop_script,
)
from cli_launch_projects import NODE_READY_TIMEOUT_SECONDS
from conftest import TatolabdUnderTest
from stream_runs_on_tatolabd import TatolabRunOfAProject
from test_cli_launch import STREAM_WITH_ONE_TEST_PATTERN_SOURCE

pytestmark = pytest.mark.requires_gpu

AGENT_LOOP_SESSION_TIMEOUT_SECONDS = 180.0
CLEAN_EXIT_TIMEOUT_SECONDS = 60.0

#: The sink `build_a_graph`'s task names as `nodes.frame_sink:FrameSink`.
FRAME_SINK_SOURCE = '''\
from tatolab.stream import Image, Input, RuntimeContextLimitedAccess, node


@node.process
class FrameSink:
    """Takes each frame and keeps nothing."""

    video_from_upstream = Input(Image, delivery="newest")

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.inputs.read(self.video_from_upstream)
'''

#: The function node `build_a_graph`'s task names as `nodes.clamp:Clamp`, whose number input a
#: picture output does not connect to.
CLAMP_SOURCE = '''\
from tatolab.stream import node


@node.function
def Clamp(x: float = 0.0, low: float = 0.0, high: float = 1.0) -> float:  # noqa: N802
    """`x` held between `low` and `high`."""
    return min(max(x, low), high)
'''

#: The files of the project every script runs on.
AGENT_LOOP_PROJECT_FILES = {
    "nodes/__init__.py": "",
    "nodes/clamp.py": CLAMP_SOURCE,
    "nodes/frame_sink.py": FRAME_SINK_SOURCE,
    "stream.py": STREAM_WITH_ONE_TEST_PATTERN_SOURCE,
}


@pytest.mark.parametrize(
    "script_path", every_agent_loop_script_path(), ids=lambda script_path: script_path.stem
)
def test_an_agent_loop_script_plays_through_on_a_runtime_unit(
    script_path: Path,
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
) -> None:
    script = load_agent_loop_script(script_path)
    project_directory = make_tatolab_project(AGENT_LOOP_PROJECT_FILES)
    tatolabd = start_tatolabd()
    tatolab_run = tatolabd.run_stream(TatolabRunOfAProject(working_directory=project_directory))
    tatolab_run.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)
    local_api = tatolabd.local_api_client()

    answers: "dict[str, Any]" = local_api.answer_over_mcp(
        lambda client: asyncio.wait_for(
            play_agent_loop_script(client, script), AGENT_LOOP_SESSION_TIMEOUT_SECONDS
        ),
        timeout=AGENT_LOOP_SESSION_TIMEOUT_SECONDS,
    )

    assert list(answers) == [step.name for step in script.steps]
    assert tatolab_run.stop_the_stream(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0
