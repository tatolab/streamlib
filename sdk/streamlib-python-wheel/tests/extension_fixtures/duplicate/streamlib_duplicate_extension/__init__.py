# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A second distribution claiming the name `streamlib-test-extension` took."""

from typing import Any


def load(host: Any) -> None:
    host.register_capability("test-capability", "2.0.0")
