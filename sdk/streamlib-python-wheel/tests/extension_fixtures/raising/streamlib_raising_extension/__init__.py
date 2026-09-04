# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A capability extension whose hook fails, the way a missing driver would."""

from typing import Any

HOOK_FAILURE_MESSAGE = "this extension's stack could not be brought up"

#: How many times the loop has called this hook. A second call means the loop
#: retried a failure instead of re-raising the one it cached.
hook_call_count = 0


def load(host: Any) -> None:
    global hook_call_count
    hook_call_count += 1
    raise RuntimeError(HOOK_FAILURE_MESSAGE)
