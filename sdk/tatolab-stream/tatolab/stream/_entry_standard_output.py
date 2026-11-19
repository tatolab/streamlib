# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
# no-print-in-python ignores this file: it points stdout at stderr so stdout carries only what
# the runtime parses; neither stream is a log event.

"""The standard output of an entry the runtime runs in a project's interpreter, kept for what it parses.

An entry imports the project's code, which may print. Each such write is carried to stderr, so
stdout holds only what the entry itself writes there for the runtime to parse.
"""

from __future__ import annotations

import os
import sys

__all__ = ["carry_every_later_stdout_write_to_stderr"]


def carry_every_later_stdout_write_to_stderr() -> int:
    """Point fd 1 and `sys.stdout` at stderr for the rest of the process, returning a
    descriptor on the real stdout, the only way the entry's output reaches it."""
    sys.stdout.flush()
    entry_stdout_descriptor = os.dup(sys.stdout.fileno())
    os.dup2(sys.stderr.fileno(), sys.stdout.fileno())
    sys.stdout = sys.stderr
    return entry_stdout_descriptor
