# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only


from __future__ import annotations

import os
import re
import subprocess
import time
from pathlib import Path

#: The text of `tatolabd`'s line for each helper process it starts, ahead of its pid.
HELPER_PROCESS_STARTED_LOG_LINE_TEXT = "helper process started: pid="
#: `tatolabd`'s line for each helper process it starts.
HELPER_PROCESS_STARTED_LOG_LINE_PATTERN = re.compile(re.escape(HELPER_PROCESS_STARTED_LOG_LINE_TEXT) + r"(\d+)")

NODE_INTERPRETER_BOOTSTRAP_FILE_NAME = b"_node_interpreter_bootstrap.py"

PROCESS_POLL_INTERVAL_SECONDS = 0.05

PROC_FILESYSTEM_IS_MOUNTED = Path("/proc/self").exists()


def helper_process_ids_started_in(stderr_text: str) -> "list[int]":
    """The pid of every helper process `tatolabd` logged starting, in order."""
    return [int(process_id) for process_id in HELPER_PROCESS_STARTED_LOG_LINE_PATTERN.findall(stderr_text)]


def a_process_is_gone_within(process_id: int, budget_seconds: float) -> bool:
    """Whether `process_id` has stopped existing inside `budget_seconds`."""
    deadline = time.monotonic() + budget_seconds
    while True:
        try:
            os.kill(process_id, 0)
        except (ProcessLookupError, PermissionError):
            return True
        if time.monotonic() >= deadline:
            return False
        time.sleep(PROCESS_POLL_INTERVAL_SECONDS)


def every_process_still_alive_after(process_ids: "list[int]", budget_seconds: float) -> "list[int]":
    """The pids of `process_ids` still alive once one shared budget has run out."""
    deadline = time.monotonic() + budget_seconds
    return [
        process_id
        for process_id in process_ids
        if not a_process_is_gone_within(process_id, max(0.0, deadline - time.monotonic()))
    ]


def helper_process_is_still_alive(process_id: int) -> bool:
    if not _process_exists(process_id):
        return False
    if PROC_FILESYSTEM_IS_MOUNTED:
        try:
            with open(f"/proc/{process_id}/cmdline", "rb") as command_line:
                return NODE_INTERPRETER_BOOTSTRAP_FILE_NAME in command_line.read()
        except FileNotFoundError:
            return False
    listed = subprocess.run(
        ["ps", "-o", "command=", "-p", str(process_id)], capture_output=True, check=False
    )
    return NODE_INTERPRETER_BOOTSTRAP_FILE_NAME in listed.stdout


def parent_process_id_of(process_id: int) -> int:
    """`process_id`'s parent, or 0 once it is gone."""
    listed = subprocess.run(
        ["ps", "-o", "ppid=", "-p", str(process_id)], capture_output=True, text=True, check=False
    )
    return int(listed.stdout.strip() or 0)


def process_descends_from(process_id: int, ancestor_process_id: int) -> bool:
    """Whether `ancestor_process_id` is a proper ancestor of the live `process_id`."""
    walked_process_id = parent_process_id_of(process_id)
    while walked_process_id not in (ancestor_process_id, 0, 1):
        walked_process_id = parent_process_id_of(walked_process_id)
    return walked_process_id == ancestor_process_id


def assert_runs_in_a_process_of_its_own_beneath(
    node_interpreter_process_id: int, tatolabd_process_id: int
) -> None:
    """The live interpreter is not `tatolabd`, and `tatolabd` is its ancestor."""
    assert node_interpreter_process_id != tatolabd_process_id, (
        "the node interpreter runs inside tatolabd's own process"
    )
    assert process_descends_from(node_interpreter_process_id, tatolabd_process_id), (
        f"node interpreter {node_interpreter_process_id} does not descend from "
        f"tatolabd {tatolabd_process_id}"
    )


def _process_exists(process_id: int) -> bool:
    try:
        os.kill(process_id, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True
