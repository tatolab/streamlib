# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What a test sees of the helper processes beneath a `tatolabd`, from outside them.

A helper — the processor interpreter running one Python node — is started by
`tatolabd`, which logs `[<node>] helper process started: pid=<pid>, ...` for
each. The processes looked for here are never the test's own children, so
their lifetime is read with signal 0 rather than a wait.
"""

from __future__ import annotations

import os
import re
import subprocess
import time

#: `tatolabd`'s line for each helper process it starts.
HELPER_PROCESS_STARTED_LOG_LINE_PATTERN = re.compile(r"helper process started: pid=(\d+)")

#: The file every processor interpreter runs, by path, from the lend.
PROCESSOR_INTERPRETER_BOOTSTRAP_FILE_NAME = b"_processor_interpreter_bootstrap.py"

PROCESS_POLL_INTERVAL_SECONDS = 0.05


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
    """Whether `process_id` is still a live processor interpreter.

    The command line is checked, not just the pid: pids are reused, and a
    recycled one would otherwise read as a leaked helper.
    """
    if not _process_exists(process_id):
        return False
    try:
        with open(f"/proc/{process_id}/cmdline", "rb") as command_line:
            return PROCESSOR_INTERPRETER_BOOTSTRAP_FILE_NAME in command_line.read()
    except FileNotFoundError:
        return False
    except OSError:
        listed = subprocess.run(
            ["ps", "-o", "command=", "-p", str(process_id)], capture_output=True, check=False
        )
        return PROCESSOR_INTERPRETER_BOOTSTRAP_FILE_NAME in listed.stdout


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


def _process_exists(process_id: int) -> bool:
    try:
        os.kill(process_id, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True
