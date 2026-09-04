# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A capability extension whose hook is still running when a second thread arrives.

A hook that brings a real stack up takes time, which is the whole window a
second `Runtime()` can be constructed in. This one holds that window open on
demand so the race is a deterministic test rather than a lucky interleaving.
"""

import threading
from typing import Any

#: Set once the hook has been entered.
hook_has_been_entered = threading.Event()

#: The hook returns when the test sets this.
hook_may_return = threading.Event()

#: How many times the loop has called this hook.
hook_call_count = 0


def load(host: Any) -> None:
    global hook_call_count
    hook_call_count += 1
    hook_has_been_entered.set()
    hook_may_return.wait(timeout=30.0)
    host.register_capability("blocking-capability", "0.9.0")
