# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A capability extension that loads in the app process and fails in a helper.

Two things at once: `role` is what tells a hook which process it is in, and a
helper-side failure has to be reachable without the app process refusing first.
"""

from typing import Any

HOOK_FAILURE_MESSAGE = "this extension has no stack to bring up in a child"


def load(host: Any) -> None:
    if host.role == "helper":
        raise RuntimeError(HOOK_FAILURE_MESSAGE)
    host.register_capability("app-only-capability", "0.5.0")
