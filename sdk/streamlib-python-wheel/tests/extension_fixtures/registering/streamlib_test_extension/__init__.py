# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A capability extension that loads: registers `test-capability` and returns."""

from typing import Any

#: Every host this hook was handed, in call order. Read by the in-process test;
#: an out-of-process app reads the markers below instead.
hosts_the_hook_was_handed: list[Any] = []


def load(host: Any) -> None:
    hosts_the_hook_was_handed.append(host)
    host.register_capability("test-capability", "1.4.2")
    print(f"MARKER:EXTENSION_HOOK_RAN_AS_{host.role}", flush=True)
