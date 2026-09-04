# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A capability extension whose hook fails, the way a missing driver would."""

from typing import Any

HOOK_FAILURE_MESSAGE = "this extension's stack could not be brought up"


def load(host: Any) -> None:
    raise RuntimeError(HOOK_FAILURE_MESSAGE)
