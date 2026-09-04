# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The capability-extension hook pip records and the engine runs at startup.

Declared as this wheel's `streamlib.extensions` entry point, so it runs once in
every process that takes an engine role — the app process as `Runtime()` is
constructed, and each helper before the processor's own module is imported. That
second call site is the one that matters here: a QUIC session runs in the
helper, so the stack it needs has to be up in the helper.
"""

from __future__ import annotations

import importlib.metadata

from streamlib import CapabilityExtensionHost

from . import _native

CAPABILITY_NAME = "moq"


def load(host: CapabilityExtensionHost) -> None:
    """Bring up the transport stack and register the capability.

    No connection and no I/O: the app is waiting on `Runtime()` and a helper is
    inside its registration budget. Opening a session is a processor's own work,
    on its first bag.
    """
    _native.bring_up_the_transport_stack()
    host.register_capability(
        CAPABILITY_NAME, importlib.metadata.version("streamlib-moq")
    )
